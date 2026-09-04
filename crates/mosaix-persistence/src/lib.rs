//! Ordered, SQLite-backed persistence boundary.
//!
//! This module owns every SQL connection. Callers exchange domain-oriented
//! revisions and health snapshots; rusqlite types never cross this boundary.
//!
//! Two rules shape the whole module (ADR 0025). Storage failure never stops
//! live window management, so every fallible operation reports health rather
//! than panicking or unwinding into the reducer. And an unusable database is
//! never repaired automatically -- a corrupt file, a newer schema, or a
//! failed migration is preserved byte-for-byte until a user asks for
//! [`Persistence::reset`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mosaix_domain::identity::WindowEvidence;
use mosaix_domain::undo::{
    now_unix, UndoMember, UndoTransaction, UndoTransactionDraft, UndoTransactionId,
};
use mosaix_domain::{ApplicationId, Rect, WindowRole};
use rusqlite::Connection;
use thiserror::Error;

/// How long a write waits for another process's lock before reporting a
/// failed write. The worker is Mosaix's only writer, so contention comes
/// from outside -- a backup agent, an inspecting `sqlite3` shell. Waiting
/// briefly absorbs those; waiting longer would stall the ordered queue
/// behind a lock we do not own.
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// One numbered, forward-only schema step.
///
/// Migrations are applied in ascending order, each inside its own
/// transaction that also advances `PRAGMA user_version`. There is no
/// downgrade path: a database written by a newer build is refused rather
/// than rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: i32,
    pub statements: &'static str,
}

/// The schema, in order. Per the spec, tables arrive with the slice that
/// consumes them -- speculative tables would become a permanent
/// compatibility obligation for no live reader.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        statements: "CREATE TABLE persistence_metadata (
                         singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                         last_durable_revision INTEGER NOT NULL
                     );
                     INSERT INTO persistence_metadata (singleton, last_durable_revision)
                         VALUES (1, 0);",
    },
    // Persistent undo (ADR 0024). Members cascade from their transaction,
    // so consuming a transaction cannot leave rows describing a command
    // that no longer exists. There is deliberately no title column: the
    // evidence model has nowhere to put one.
    Migration {
        version: 2,
        statements: "CREATE TABLE undo_transaction (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         command TEXT NOT NULL,
                         recorded_at_unix INTEGER NOT NULL,
                         topology_fingerprint TEXT NOT NULL,
                         durable_revision INTEGER NOT NULL
                     );
                     CREATE TABLE undo_member (
                         transaction_id INTEGER NOT NULL
                             REFERENCES undo_transaction (id) ON DELETE CASCADE,
                         ordinal INTEGER NOT NULL,
                         prior_x INTEGER NOT NULL,
                         prior_y INTEGER NOT NULL,
                         prior_width INTEGER NOT NULL,
                         prior_height INTEGER NOT NULL,
                         prior_display_fingerprint TEXT NOT NULL,
                         application_id TEXT NOT NULL,
                         executable_path TEXT,
                         native_class TEXT,
                         role TEXT NOT NULL,
                         launch_order INTEGER NOT NULL,
                         last_x INTEGER NOT NULL,
                         last_y INTEGER NOT NULL,
                         last_width INTEGER NOT NULL,
                         last_height INTEGER NOT NULL,
                         evidence_display_fingerprint TEXT NOT NULL,
                         PRIMARY KEY (transaction_id, ordinal)
                     );
                     CREATE INDEX undo_transaction_recorded_at
                         ON undo_transaction (recorded_at_unix);",
    },
];

/// The newest schema this build understands.
pub const SUPPORTED_SCHEMA_VERSION: i32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// How many undo transactions history keeps at most (ADR 0024). Fixed in
/// the first release: a configurable bound would be a promise about how
/// much behavioural history Mosaix retains, and that is a decision worth
/// making once rather than per user.
pub const MAX_UNDO_TRANSACTIONS: u32 = 100;

/// How long an undo transaction may live, in seconds. Seven days.
pub const UNDO_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// The schema steps this build ships, in ascending order.
pub const fn migrations() -> &'static [Migration] {
    MIGRATIONS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceHealth {
    Healthy {
        last_durable_revision: u64,
    },
    Degraded {
        last_durable_revision: u64,
        reason: PersistenceFailure,
    },
}

impl Default for PersistenceHealth {
    fn default() -> Self {
        Self::Healthy {
            last_durable_revision: 0,
        }
    }
}

/// Why durability is not currently promised. The string form is a stable
/// machine-readable reason code shared by IPC and the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceFailure {
    OpenFailed,
    CorruptOrUnreadable,
    NewerSchema,
    MigrationFailed,
    WriteFailed,
}

impl PersistenceFailure {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OpenFailed => "open_failed",
            Self::CorruptOrUnreadable => "corrupt_or_unreadable",
            Self::NewerSchema => "newer_schema",
            Self::MigrationFailed => "migration_failed",
            Self::WriteFailed => "write_failed",
        }
    }

    /// Whether an operator must act before durability can return. Startup
    /// failures need an explicit repair or reset; a failed write recovers
    /// on its own once ordered writes succeed again.
    pub const fn requires_explicit_recovery(&self) -> bool {
        match self {
            Self::CorruptOrUnreadable | Self::NewerSchema | Self::MigrationFailed => true,
            Self::OpenFailed | Self::WriteFailed => false,
        }
    }
}

impl std::fmt::Display for PersistenceFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::OpenFailed => "the state database could not be opened",
            Self::CorruptOrUnreadable => "the state database is corrupt or unreadable",
            Self::NewerSchema => "the state database was written by a newer Mosaix",
            Self::MigrationFailed => "a state database migration failed",
            Self::WriteFailed => "a state database write failed",
        })
    }
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database uses newer schema version {found}; this build supports {supported}")]
    NewerSchema { found: i32, supported: i32 },
    #[error("database could not be opened: {0}")]
    Open(#[source] rusqlite::Error),
    #[error("database could not be read safely: {0}")]
    Read(#[source] rusqlite::Error),
    #[error("database failed its integrity check: {report}")]
    Corrupt { report: String },
    #[error("database migration to version {version} failed: {source}")]
    Migration {
        version: i32,
        #[source]
        source: rusqlite::Error,
    },
    #[error("database write failed: {0}")]
    Write(#[source] rusqlite::Error),
    #[error("database directory could not be created: {0}")]
    Directory(#[source] std::io::Error),
    #[error("database file could not be moved aside: {0}")]
    Quarantine(#[source] std::io::Error),
}

impl PersistenceError {
    pub fn failure(&self) -> PersistenceFailure {
        match self {
            Self::NewerSchema { .. } => PersistenceFailure::NewerSchema,
            Self::Open(_) | Self::Directory(_) | Self::Quarantine(_) => {
                PersistenceFailure::OpenFailed
            }
            Self::Read(_) | Self::Corrupt { .. } => PersistenceFailure::CorruptOrUnreadable,
            Self::Migration { .. } => PersistenceFailure::MigrationFailed,
            Self::Write(_) => PersistenceFailure::WriteFailed,
        }
    }
}

/// Where the per-user state database lives.
///
/// Resolved here rather than in each binary so the agent and the CLI can
/// never disagree about which file they are talking about. Both locations
/// are the platform's per-user application-data directory, which carries
/// the user-only protection the spec asks for without Mosaix setting an
/// ACL of its own. `None` means this platform has no location yet, and so
/// no durable state -- not that the location failed to be created.
pub fn default_database_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join("Mosaix").join("state.db"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Mosaix")
                .join("state.db")
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// What [`Persistence::reset`] did, so a caller can tell the user where
/// their old database went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetOutcome {
    /// Where the previous database was moved, or `None` if there was none.
    pub preserved: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Persistence {
    connection: Connection,
    schema_version: i32,
    last_durable_revision: u64,
    /// Set by a failed write and cleared by the next successful one. While
    /// set, the store makes no new durability promise.
    degraded: Option<PersistenceFailure>,
}

impl Persistence {
    /// Opens (creating if absent) the database at `path` and brings it to
    /// [`SUPPORTED_SCHEMA_VERSION`].
    pub fn open(path: &Path) -> Result<Self, PersistenceError> {
        Self::open_with_migrations(path, MIGRATIONS)
    }

    /// The migration seam. Production callers want [`Persistence::open`];
    /// this exists so migration ordering and rollback can be exercised
    /// against schema steps that do not have to be real.
    pub fn open_with_migrations(
        path: &Path,
        migrations: &[Migration],
    ) -> Result<Self, PersistenceError> {
        // The state database sits beside the configuration directory but
        // does not depend on it having been created, so it makes its own
        // parent rather than assuming another subsystem already did.
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(PersistenceError::Directory)?;
        }
        let connection = Connection::open(path).map_err(PersistenceError::Open)?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(PersistenceError::Open)?;

        // Reading the header is also the first proof that this file is a
        // database at all: a garbage file fails here rather than later,
        // mid-migration, with the file half-rewritten.
        let mut version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(PersistenceError::Read)?;
        let report: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(PersistenceError::Read)?;
        if report != "ok" {
            return Err(PersistenceError::Corrupt { report });
        }

        let supported = migrations.last().map_or(0, |migration| migration.version);
        if version > supported {
            return Err(PersistenceError::NewerSchema {
                found: version,
                supported,
            });
        }
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(PersistenceError::Open)?;

        let already_applied = version;
        for migration in migrations
            .iter()
            .filter(move |step| step.version > already_applied)
        {
            let transaction = connection
                .unchecked_transaction()
                .map_err(|source| PersistenceError::Migration {
                    version: migration.version,
                    source,
                })?;
            // The version bump rides inside the same transaction, so a
            // migration that fails halfway leaves neither its tables nor
            // its version number behind.
            transaction
                .execute_batch(&format!(
                    "{}\nPRAGMA user_version = {};",
                    migration.statements, migration.version
                ))
                .map_err(|source| PersistenceError::Migration {
                    version: migration.version,
                    source,
                })?;
            transaction
                .commit()
                .map_err(|source| PersistenceError::Migration {
                    version: migration.version,
                    source,
                })?;
            version = migration.version;
        }

        let last_durable_revision = connection
            .query_row(
                "SELECT last_durable_revision FROM persistence_metadata WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(PersistenceError::Read)?;

        Ok(Self {
            connection,
            schema_version: version,
            last_durable_revision,
            degraded: None,
        })
    }

    /// Moves an unusable database aside and creates a fresh one. This is
    /// the explicit action that startup refuses to take by itself; the
    /// previous file is preserved, never deleted.
    pub fn reset(path: &Path) -> Result<ResetOutcome, PersistenceError> {
        let preserved = if path.exists() {
            let quarantine = quarantine_path(path);
            std::fs::rename(path, &quarantine).map_err(PersistenceError::Quarantine)?;
            // A rollback journal left beside the old database describes a
            // file that is no longer there, so it travels with it.
            for suffix in ["-journal", "-wal", "-shm"] {
                let sidecar = sidecar_path(path, suffix);
                if sidecar.exists() {
                    let _ = std::fs::rename(&sidecar, sidecar_path(&quarantine, suffix));
                }
            }
            Some(quarantine)
        } else {
            None
        };
        Self::open(path)?;
        Ok(ResetOutcome { preserved })
    }

    /// The schema version this database is now at.
    pub const fn schema_version(&self) -> i32 {
        self.schema_version
    }

    pub fn health(&self) -> PersistenceHealth {
        match self.degraded {
            Some(reason) => PersistenceHealth::Degraded {
                last_durable_revision: self.last_durable_revision,
                reason,
            },
            None => PersistenceHealth::Healthy {
                last_durable_revision: self.last_durable_revision,
            },
        }
    }

    /// Records `revision` as durable. On failure the recorded revision is
    /// left where it was -- the caller's live arrangement is already
    /// committed in memory and stays there, but no durability is claimed
    /// for it until a later write succeeds.
    pub fn commit_revision(
        &mut self,
        revision: u64,
    ) -> Result<PersistenceHealth, PersistenceError> {
        if revision <= self.last_durable_revision {
            return Ok(self.health());
        }
        match self.connection.execute(
            "UPDATE persistence_metadata SET last_durable_revision = ?1 WHERE singleton = 1",
            [revision],
        ) {
            Ok(_) => {
                self.last_durable_revision = revision;
                self.degraded = None;
                Ok(self.health())
            }
            Err(source) => {
                self.degraded = Some(PersistenceFailure::WriteFailed);
                Err(PersistenceError::Write(source))
            }
        }
    }

    /// Stores one undo transaction and every window it moved, atomically.
    ///
    /// A half-written transaction would be worse than none at all -- undo
    /// would preflight members it could see and silently omit the ones it
    /// could not -- so the parent row and its members share one SQL
    /// transaction.
    pub fn record_transaction(
        &mut self,
        draft: &UndoTransactionDraft,
    ) -> Result<UndoTransactionId, PersistenceError> {
        let outcome = self.write(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO undo_transaction
                     (command, recorded_at_unix, topology_fingerprint, durable_revision)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    draft.command,
                    draft.recorded_at_unix,
                    draft.topology_fingerprint,
                    draft.durable_revision,
                ],
            )?;
            let id = UndoTransactionId(transaction.last_insert_rowid());
            for member in &draft.members {
                transaction.execute(
                    "INSERT INTO undo_member (
                         transaction_id, ordinal,
                         prior_x, prior_y, prior_width, prior_height,
                         prior_display_fingerprint,
                         application_id, executable_path, native_class, role, launch_order,
                         last_x, last_y, last_width, last_height,
                         evidence_display_fingerprint
                     ) VALUES (
                         ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16, ?17
                     )",
                    rusqlite::params![
                        id.0,
                        member.ordinal,
                        member.prior_placement.x,
                        member.prior_placement.y,
                        member.prior_placement.width,
                        member.prior_placement.height,
                        member.prior_display_fingerprint,
                        member.evidence.application_id.0,
                        member.evidence.executable_path,
                        member.evidence.native_class,
                        member.evidence.role.code(),
                        member.evidence.launch_order,
                        member.evidence.last_placement.x,
                        member.evidence.last_placement.y,
                        member.evidence.last_placement.width,
                        member.evidence.last_placement.height,
                        member.evidence.display_fingerprint,
                    ],
                )?;
            }
            // Pruning rides inside the insert's transaction, so history is
            // never observably over its bounds and a failed prune cannot
            // leave the new transaction stored without it.
            prune_within(&transaction, draft.recorded_at_unix)?;
            transaction.commit()?;
            Ok(id)
        })?;
        Ok(outcome)
    }

    /// Applies both retention bounds and answers how many transactions
    /// were removed. `now_unix` is a parameter rather than a clock read so
    /// the boundary is testable and the caller decides what "now" means.
    pub fn prune_history(&mut self, now_unix: i64) -> Result<usize, PersistenceError> {
        self.write(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let removed = prune_within(&transaction, now_unix)?;
            transaction.commit()?;
            Ok(removed)
        })
    }

    /// The transaction undo would examine next, or `None` when history is
    /// empty. Undo never looks past this one (ADR 0024).
    pub fn newest_transaction(&self) -> Result<Option<UndoTransaction>, PersistenceError> {
        let header = self
            .connection
            .query_row(
                "SELECT id, command, recorded_at_unix, topology_fingerprint, durable_revision
                 FROM undo_transaction ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        UndoTransactionId(row.get(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u64>(4)?,
                    ))
                },
            )
            .optional_row()?;
        let Some((id, command, recorded_at_unix, topology_fingerprint, durable_revision)) = header
        else {
            return Ok(None);
        };
        Ok(Some(UndoTransaction {
            id,
            command,
            recorded_at_unix,
            topology_fingerprint,
            durable_revision,
            members: self.members_of(id)?,
        }))
    }

    fn members_of(&self, id: UndoTransactionId) -> Result<Vec<UndoMember>, PersistenceError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT ordinal,
                        prior_x, prior_y, prior_width, prior_height, prior_display_fingerprint,
                        application_id, executable_path, native_class, role, launch_order,
                        last_x, last_y, last_width, last_height, evidence_display_fingerprint
                 FROM undo_member WHERE transaction_id = ?1 ORDER BY ordinal",
            )
            .map_err(PersistenceError::Read)?;
        let members = statement
            .query_map([id.0], |row| {
                Ok(UndoMember {
                    ordinal: row.get(0)?,
                    prior_placement: Rect::new(row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
                    prior_display_fingerprint: row.get(5)?,
                    evidence: WindowEvidence {
                        application_id: ApplicationId(row.get(6)?),
                        executable_path: row.get(7)?,
                        native_class: row.get(8)?,
                        role: WindowRole::from_code(&row.get::<_, String>(9)?),
                        launch_order: row.get(10)?,
                        last_placement: Rect::new(
                            row.get(11)?,
                            row.get(12)?,
                            row.get(13)?,
                            row.get(14)?,
                        ),
                        display_fingerprint: row.get(15)?,
                    },
                })
            })
            .map_err(PersistenceError::Read)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::Read)?;
        Ok(members)
    }

    /// Removes a transaction after it has been successfully undone.
    /// Answers whether there was one to remove, so a caller cannot mistake
    /// "already gone" for "consumed".
    pub fn consume_transaction(
        &mut self,
        id: UndoTransactionId,
    ) -> Result<bool, PersistenceError> {
        self.write(|connection| {
            let removed = connection.execute("DELETE FROM undo_transaction WHERE id = ?1", [id.0])?;
            Ok(removed > 0)
        })
    }

    /// Runs a write, mapping any failure onto the same sticky degradation
    /// [`Persistence::commit_revision`] uses. Every durable write in this
    /// module goes through here so that one failed write cannot leave the
    /// store claiming health it does not have.
    fn write<T>(
        &mut self,
        action: impl FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    ) -> Result<T, PersistenceError> {
        match action(&self.connection) {
            Ok(value) => {
                self.degraded = None;
                Ok(value)
            }
            Err(source) => {
                self.degraded = Some(PersistenceFailure::WriteFailed);
                Err(PersistenceError::Write(source))
            }
        }
    }
}

/// `query_row` treats "no rows" as an error; every caller here treats it as
/// an empty history. This turns that one case into `None` and leaves every
/// other failure alone.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>, PersistenceError>;
}

impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional_row(self) -> Result<Option<T>, PersistenceError> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(source) => Err(PersistenceError::Read(source)),
        }
    }
}

/// Enforces both retention bounds in one statement, so whichever binds
/// first does. Members follow their transaction out through the foreign
/// key's cascade rather than a second delete that could be skipped.
fn prune_within(connection: &Connection, now_unix: i64) -> Result<usize, rusqlite::Error> {
    let cutoff = now_unix.saturating_sub(UNDO_RETENTION_SECONDS);
    connection.execute(
        "DELETE FROM undo_transaction
         WHERE recorded_at_unix < ?1
            OR id NOT IN (
                SELECT id FROM undo_transaction ORDER BY id DESC LIMIT ?2
            )",
        rusqlite::params![cutoff, MAX_UNDO_TRANSACTIONS],
    )
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A name that does not collide with an earlier quarantine, so repeated
/// resets never overwrite the evidence from the first one.
fn quarantine_path(path: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    for attempt in 0..1_000 {
        let suffix = if attempt == 0 {
            format!(".quarantine-{stamp}")
        } else {
            format!(".quarantine-{stamp}-{attempt}")
        };
        let candidate = sidecar_path(path, &suffix);
        if !candidate.exists() {
            return candidate;
        }
    }
    sidecar_path(path, &format!(".quarantine-{stamp}-last"))
}

/// A durable write for the worker to perform, in the order submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceRequest {
    /// Record committed state through `revision` as durable.
    Commit { revision: u64 },
    /// Store one explicit command's reversible placements.
    RecordUndoTransaction(UndoTransactionDraft),
    /// Remove a transaction that has just been undone.
    ConsumeUndoTransaction(UndoTransactionId),
    /// Apply the retention bounds against the current clock.
    ///
    /// Recording already prunes, so this exists for the case recording
    /// cannot reach: an agent left running with no commands issued, whose
    /// newest transaction would otherwise age past the window and still be
    /// offered.
    PruneHistory,
}

/// What the worker reports after each request: how durability now stands,
/// and what undo would find. Both travel together so the agent never
/// publishes a health state and an undo history from different moments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceUpdate {
    pub health: PersistenceHealth,
    pub newest_undo: Option<UndoTransaction>,
}

#[derive(Debug)]
enum WorkerMessage {
    Request(PersistenceRequest),
    Stop,
}

/// The worker thread is gone, so nothing further will be made durable.
///
/// Distinct from [`PersistenceHealth::Degraded`]: that is a live worker
/// reporting it could not write, this is no worker at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the persistence worker has stopped")]
pub struct WorkerStopped;

pub struct PersistenceWorker {
    sender: Sender<WorkerMessage>,
    updates: Receiver<PersistenceUpdate>,
    join: Option<JoinHandle<()>>,
}

impl PersistenceWorker {
    pub fn start(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let mut persistence = Persistence::open(path.as_ref())?;

        // History that aged out while the agent was not running is dropped
        // before anyone is told what undo would do, so a stale transaction
        // is never offered even once.
        if let Err(error) = persistence.prune_history(now_unix()) {
            tracing::warn!(%error, "undo history could not be pruned at startup");
        }

        let (sender, receiver) = mpsc::channel();
        let (update_sender, updates) = mpsc::channel();

        // The first update is sent before any request arrives, so a caller
        // learns what undo history the database already holds without
        // having to write something first.
        let _ = update_sender.send(snapshot_of(&persistence));

        let join = thread::spawn(move || {
            while let Ok(message) = receiver.recv() {
                let WorkerMessage::Request(request) = message else {
                    break;
                };
                // The store itself tracks whether each write landed, so the
                // health reported here is the truth rather than this
                // thread's guess at it.
                let outcome = match &request {
                    PersistenceRequest::Commit { revision } => {
                        persistence.commit_revision(*revision).map(|_| ())
                    }
                    PersistenceRequest::RecordUndoTransaction(draft) => {
                        persistence.record_transaction(draft).map(|_| ())
                    }
                    PersistenceRequest::ConsumeUndoTransaction(id) => {
                        persistence.consume_transaction(*id).map(|_| ())
                    }
                    PersistenceRequest::PruneHistory => {
                        persistence.prune_history(now_unix()).map(|_| ())
                    }
                };
                if let Err(error) = outcome {
                    tracing::warn!(
                        %error,
                        ?request,
                        "state write failed; live window management continues \
                         without a durability promise"
                    );
                }
                let _ = update_sender.send(snapshot_of(&persistence));
            }
        });
        Ok(Self {
            sender,
            updates,
            join: Some(join),
        })
    }

    /// Queues one durable write. Requests are performed in the order they
    /// are submitted, which is what makes a consume that follows a record
    /// safe to submit without waiting.
    pub fn submit(&self, request: PersistenceRequest) -> Result<(), WorkerStopped> {
        self.sender
            .send(WorkerMessage::Request(request))
            .map_err(|_| WorkerStopped)
    }

    pub fn commit(&self, revision: u64) -> Result<(), WorkerStopped> {
        self.submit(PersistenceRequest::Commit { revision })
    }

    pub fn next_update(&self) -> Result<PersistenceUpdate, WorkerStopped> {
        self.updates.recv().map_err(|_| WorkerStopped)
    }

    /// The next update if one is already waiting. Lets a caller drain
    /// everything the worker has said without blocking its own loop.
    pub fn try_next_update(&self) -> Option<PersistenceUpdate> {
        self.updates.try_recv().ok()
    }

    pub fn stop(mut self) {
        let _ = self.sender.send(WorkerMessage::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Health and undo history read together, so the two can never describe
/// different moments. A history that cannot be read is reported as empty
/// rather than as a stale earlier answer.
fn snapshot_of(persistence: &Persistence) -> PersistenceUpdate {
    let newest_undo = match persistence.newest_transaction() {
        Ok(newest) => newest,
        Err(error) => {
            tracing::warn!(%error, "undo history could not be read");
            None
        }
    };
    PersistenceUpdate {
        health: persistence.health(),
        newest_undo,
    }
}

impl Drop for PersistenceWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkerMessage::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_codes_are_stable_and_distinct() {
        let codes = [
            PersistenceFailure::OpenFailed.code(),
            PersistenceFailure::CorruptOrUnreadable.code(),
            PersistenceFailure::NewerSchema.code(),
            PersistenceFailure::MigrationFailed.code(),
            PersistenceFailure::WriteFailed.code(),
        ];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len(), "reason codes must be distinct");
        assert_eq!(PersistenceFailure::NewerSchema.code(), "newer_schema");
    }

    #[test]
    fn only_startup_failures_demand_an_explicit_repair() {
        assert!(PersistenceFailure::CorruptOrUnreadable.requires_explicit_recovery());
        assert!(PersistenceFailure::NewerSchema.requires_explicit_recovery());
        assert!(PersistenceFailure::MigrationFailed.requires_explicit_recovery());
        assert!(
            !PersistenceFailure::WriteFailed.requires_explicit_recovery(),
            "a failed write recovers when ordered writes recover"
        );
    }

    #[test]
    fn worker_acknowledges_revisions_in_order() {
        let directory =
            std::env::temp_dir().join(format!("mosaix-persistence-worker-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("state.db");

        let worker = PersistenceWorker::start(&path).unwrap();
        assert_eq!(
            worker.next_update().unwrap(),
            PersistenceUpdate {
                health: PersistenceHealth::Healthy {
                    last_durable_revision: 0
                },
                newest_undo: None,
            },
            "the worker states what the database already holds before any write"
        );

        worker.commit(1).unwrap();
        worker.commit(2).unwrap();
        assert_eq!(
            worker.next_update().unwrap().health,
            PersistenceHealth::Healthy {
                last_durable_revision: 1
            }
        );
        assert_eq!(
            worker.next_update().unwrap().health,
            PersistenceHealth::Healthy {
                last_durable_revision: 2
            }
        );
        worker.stop();

        std::fs::remove_dir_all(&directory).ok();
    }
}
