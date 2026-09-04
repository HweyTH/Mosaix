//! The current-session recovery ledger (ADR 0023): a small SQLite file
//! beside the state database that records, before any native parking
//! effect, everything needed to put a window back and everything needed
//! to refuse a handle that no longer means the same window.
//!
//! It is its own file rather than tables in the state database on
//! purpose. The state database holds cross-session identity and never a
//! native handle; the ledger holds native handles and is read by the
//! out-of-process restore command while the agent may be dead or
//! half-started. Keeping them apart means a corrupt or newer state
//! database, which startup refuses to touch, never blocks recovery of
//! parked windows -- and a ledger the restore command is reading never
//! contends with the ordered worker for the database lock.
//!
//! Every write commits before returning. SQLite's default journal mode
//! syncs on commit, so a returned entry id is a durable entry, and that
//! is the acknowledgement the engine waits for before it authorises the
//! parking effect the entry describes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mosaix_domain::recovery::{
    plan_recovery, HandleVerdict, LiveHandleEvidence, ProcessInstance, RecoveryDraft,
    RecoveryEntry, RecoveryEntryId, RecoveryOutcome, ShowState,
};
use mosaix_domain::{ApplicationId, Rect};
use rusqlite::Connection;

use crate::{PersistenceError, PersistenceFailure};

const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// The ledger schema, forward-only like the state database's.
const LEDGER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS recovery_entry (
                                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                                 session_id TEXT NOT NULL,
                                 native_handle INTEGER NOT NULL,
                                 process_id INTEGER NOT NULL,
                                 process_creation_time INTEGER NOT NULL,
                                 application_id TEXT NOT NULL,
                                 executable_path TEXT,
                                 native_class TEXT,
                                 original_display_fingerprint TEXT NOT NULL,
                                 visible_x INTEGER NOT NULL,
                                 visible_y INTEGER NOT NULL,
                                 visible_width INTEGER NOT NULL,
                                 visible_height INTEGER NOT NULL,
                                 normal_x INTEGER NOT NULL,
                                 normal_y INTEGER NOT NULL,
                                 normal_width INTEGER NOT NULL,
                                 normal_height INTEGER NOT NULL,
                                 show_state TEXT NOT NULL,
                                 recorded_at_unix INTEGER NOT NULL,
                                 parked INTEGER NOT NULL DEFAULT 0,
                                 restored INTEGER NOT NULL DEFAULT 0
                             );
                             CREATE INDEX IF NOT EXISTS recovery_entry_open
                                 ON recovery_entry (parked, restored);";

/// Where the ledger lives: beside the state database.
pub fn default_ledger_path() -> Option<PathBuf> {
    crate::default_database_path().map(|state| state.with_file_name("recovery-ledger.db"))
}

#[derive(Debug)]
pub struct RecoveryLedger {
    connection: Connection,
    degraded: Option<PersistenceFailure>,
}

impl RecoveryLedger {
    pub fn open(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(PersistenceError::Directory)?;
        }
        let connection = Connection::open(path).map_err(PersistenceError::Open)?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(PersistenceError::Open)?;
        let report: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(PersistenceError::Read)?;
        if report != "ok" {
            return Err(PersistenceError::Corrupt { report });
        }
        connection
            .execute_batch(LEDGER_SCHEMA)
            .map_err(|source| PersistenceError::Migration { version: 1, source })?;
        Ok(Self {
            connection,
            degraded: None,
        })
    }

    /// Whether the last write landed. A ledger that could not write makes
    /// no promise, and the engine authorises no parking on its say-so.
    pub const fn degraded(&self) -> Option<PersistenceFailure> {
        self.degraded
    }

    /// Records `draft` and returns its id once it is durable. The window
    /// has not been parked yet: [`RecoveryLedger::mark_parked`] follows
    /// the native effect, so an entry recorded for a park that never
    /// happened describes a window that never moved.
    pub fn record(&mut self, draft: &RecoveryDraft) -> Result<RecoveryEntryId, PersistenceError> {
        self.write(|connection| {
            connection.execute(
                "INSERT INTO recovery_entry (
                     session_id, native_handle, process_id, process_creation_time,
                     application_id, executable_path, native_class,
                     original_display_fingerprint,
                     visible_x, visible_y, visible_width, visible_height,
                     normal_x, normal_y, normal_width, normal_height,
                     show_state, recorded_at_unix
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
                 )",
                rusqlite::params![
                    draft.session_id,
                    draft.native_handle as i64,
                    draft.process.process_id,
                    draft.process.creation_time as i64,
                    draft.application_id.0,
                    draft.executable_path,
                    draft.native_class,
                    draft.original_display_fingerprint,
                    draft.visible_bounds.x,
                    draft.visible_bounds.y,
                    draft.visible_bounds.width,
                    draft.visible_bounds.height,
                    draft.normal_bounds.x,
                    draft.normal_bounds.y,
                    draft.normal_bounds.width,
                    draft.normal_bounds.height,
                    draft.show_state.code(),
                    draft.recorded_at_unix,
                ],
            )?;
            Ok(RecoveryEntryId(connection.last_insert_rowid()))
        })
    }

    /// Records that the parking effect `id` authorised was carried out.
    pub fn mark_parked(&mut self, id: RecoveryEntryId) -> Result<bool, PersistenceError> {
        self.write(|connection| {
            let changed =
                connection.execute("UPDATE recovery_entry SET parked = 1 WHERE id = ?1", [id.0])?;
            Ok(changed > 0)
        })
    }

    /// Records that the window `id` describes is back in visible
    /// geometry, by whoever put it there.
    pub fn mark_restored(&mut self, id: RecoveryEntryId) -> Result<bool, PersistenceError> {
        self.write(|connection| {
            let changed = connection.execute(
                "UPDATE recovery_entry SET restored = 1 WHERE id = ?1",
                [id.0],
            )?;
            Ok(changed > 0)
        })
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> Result<Vec<RecoveryEntry>, PersistenceError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, session_id, native_handle, process_id, process_creation_time,
                        application_id, executable_path, native_class,
                        original_display_fingerprint,
                        visible_x, visible_y, visible_width, visible_height,
                        normal_x, normal_y, normal_width, normal_height,
                        show_state, recorded_at_unix, parked, restored
                 FROM recovery_entry ORDER BY id",
            )
            .map_err(PersistenceError::Read)?;
        let rows = statement
            .query_map([], |row| {
                Ok(RecoveryEntry {
                    id: RecoveryEntryId(row.get(0)?),
                    draft: RecoveryDraft {
                        session_id: row.get(1)?,
                        native_handle: row.get::<_, i64>(2)? as isize,
                        process: ProcessInstance {
                            process_id: row.get(3)?,
                            creation_time: row.get::<_, i64>(4)? as u64,
                        },
                        application_id: ApplicationId(row.get(5)?),
                        executable_path: row.get(6)?,
                        native_class: row.get(7)?,
                        original_display_fingerprint: row.get(8)?,
                        visible_bounds: Rect::new(
                            row.get(9)?,
                            row.get(10)?,
                            row.get(11)?,
                            row.get(12)?,
                        ),
                        normal_bounds: Rect::new(
                            row.get(13)?,
                            row.get(14)?,
                            row.get(15)?,
                            row.get(16)?,
                        ),
                        show_state: ShowState::from_code(&row.get::<_, String>(17)?)
                            .unwrap_or(ShowState::Normal),
                        recorded_at_unix: row.get(18)?,
                    },
                    parked: row.get::<_, i64>(19)? != 0,
                    restored: row.get::<_, i64>(20)? != 0,
                })
            })
            .map_err(PersistenceError::Read)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::Read)?;
        Ok(rows)
    }

    /// The entries that still describe a parked window.
    pub fn open_entries(&self) -> Result<Vec<RecoveryEntry>, PersistenceError> {
        Ok(self
            .entries()?
            .into_iter()
            .filter(RecoveryEntry::is_open)
            .collect())
    }

    /// Drops entries that are history: restored, or recorded but never
    /// parked by a session that is not this one. Open entries are never
    /// dropped by anything but a restore.
    pub fn prune(&mut self, current_session: &str) -> Result<usize, PersistenceError> {
        self.write(|connection| {
            let removed = connection.execute(
                "DELETE FROM recovery_entry
                 WHERE restored = 1 OR (parked = 0 AND session_id <> ?1)",
                [current_session],
            )?;
            Ok(removed)
        })
    }

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

/// Restores every open entry whose handle verifies, and reports what
/// happened to each. The one recovery routine, shared by the agent's
/// startup pass and the out-of-process restore command, so the two can
/// never disagree about which handles may be touched (ADR 0023).
///
/// `probe` says what a handle names now; `restore` puts a verified
/// window back and answers with the platform's reason if it could not.
/// A stale, reused, or ambiguous handle is reported and left alone.
pub fn recover_parked_windows(
    ledger: &mut RecoveryLedger,
    probe: impl Fn(isize) -> Option<LiveHandleEvidence>,
    mut restore: impl FnMut(&RecoveryEntry) -> Result<(), String>,
) -> Result<Vec<RecoveryOutcome>, PersistenceError> {
    let entries = ledger.open_entries()?;
    let plan = plan_recovery(&entries, probe);
    let mut outcomes = Vec::with_capacity(plan.len());
    for (entry_id, verdict) in plan {
        let entry = entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .expect("planned from these entries");
        let (restored, failure) = match &verdict {
            HandleVerdict::Verified => match restore(entry) {
                Ok(()) => {
                    ledger.mark_restored(entry_id)?;
                    (true, None)
                }
                Err(reason) => (false, Some(reason)),
            },
            _ => (false, None),
        };
        outcomes.push(RecoveryOutcome {
            entry_id,
            native_handle: entry.draft.native_handle,
            application_id: entry.draft.application_id.clone(),
            verdict,
            restored,
            failure,
        });
    }
    Ok(outcomes)
}
