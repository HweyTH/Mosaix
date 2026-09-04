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
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    statements: "CREATE TABLE persistence_metadata (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     last_durable_revision INTEGER NOT NULL
                 );
                 INSERT INTO persistence_metadata (singleton, last_durable_revision)
                     VALUES (1, 0);",
}];

/// The newest schema this build understands.
pub const SUPPORTED_SCHEMA_VERSION: i32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

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

#[derive(Debug)]
enum WorkerMessage {
    Commit { revision: u64 },
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
    updates: Receiver<PersistenceHealth>,
    join: Option<JoinHandle<()>>,
}

impl PersistenceWorker {
    pub fn start(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let mut persistence = Persistence::open(path.as_ref())?;
        let (sender, receiver) = mpsc::channel();
        let (update_sender, updates) = mpsc::channel();
        let join = thread::spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    WorkerMessage::Stop => break,
                    WorkerMessage::Commit { revision } => {
                        // The store itself tracks whether the write landed,
                        // so the health it reports here is the truth rather
                        // than this thread's guess at it.
                        if let Err(error) = persistence.commit_revision(revision) {
                            tracing::warn!(
                                %error,
                                revision,
                                "state write failed; live window management continues \
                                 without a durability promise"
                            );
                        }
                        let _ = update_sender.send(persistence.health());
                    }
                }
            }
        });
        Ok(Self {
            sender,
            updates,
            join: Some(join),
        })
    }

    pub fn commit(&self, revision: u64) -> Result<(), WorkerStopped> {
        self.sender
            .send(WorkerMessage::Commit { revision })
            .map_err(|_| WorkerStopped)
    }

    pub fn next_update(&self) -> Result<PersistenceHealth, WorkerStopped> {
        self.updates.recv().map_err(|_| WorkerStopped)
    }

    pub fn stop(mut self) {
        let _ = self.sender.send(WorkerMessage::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
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
        worker.commit(1).unwrap();
        worker.commit(2).unwrap();
        assert_eq!(
            worker.next_update().unwrap(),
            PersistenceHealth::Healthy {
                last_durable_revision: 1
            }
        );
        assert_eq!(
            worker.next_update().unwrap(),
            PersistenceHealth::Healthy {
                last_durable_revision: 2
            }
        );
        worker.stop();

        std::fs::remove_dir_all(&directory).ok();
    }
}
