//! Ordered, SQLite-backed persistence boundary.
//!
//! This module owns every SQL connection. Callers exchange domain-oriented
//! revisions and health snapshots; rusqlite types never cross this boundary.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rusqlite::Connection;
use thiserror::Error;

const CURRENT_SCHEMA_VERSION: i32 = 1;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceFailure {
    OpenFailed,
    CorruptOrUnreadable,
    NewerSchema,
    MigrationFailed,
    WriteFailed,
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database uses newer schema version {found}; this build supports {supported}")]
    NewerSchema { found: i32, supported: i32 },
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

pub struct Persistence {
    connection: Connection,
    last_durable_revision: u64,
}

impl Persistence {
    pub fn open(path: &Path) -> Result<Self, PersistenceError> {
        let connection = Connection::open(path)?;
        let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(PersistenceError::NewerSchema {
                found: version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        if version < CURRENT_SCHEMA_VERSION {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS persistence_metadata (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    last_durable_revision INTEGER NOT NULL
                );
                INSERT OR IGNORE INTO persistence_metadata (singleton, last_durable_revision)
                    VALUES (1, 0);
                PRAGMA user_version = 1;",
            )?;
            transaction.commit()?;
        }
        let last_durable_revision = connection.query_row(
            "SELECT last_durable_revision FROM persistence_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(Self {
            connection,
            last_durable_revision,
        })
    }

    pub fn health(&self) -> PersistenceHealth {
        PersistenceHealth::Healthy {
            last_durable_revision: self.last_durable_revision,
        }
    }

    pub fn commit_revision(
        &mut self,
        revision: u64,
    ) -> Result<PersistenceHealth, PersistenceError> {
        if revision <= self.last_durable_revision {
            return Ok(self.health());
        }
        self.connection.execute(
            "UPDATE persistence_metadata SET last_durable_revision = ?1 WHERE singleton = 1",
            [revision],
        )?;
        self.last_durable_revision = revision;
        Ok(self.health())
    }
}

#[derive(Debug)]
enum WorkerMessage {
    Commit { revision: u64 },
    Stop,
}

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
                        let health = match persistence.commit_revision(revision) {
                            Ok(health) => health,
                            Err(_) => PersistenceHealth::Degraded {
                                last_durable_revision: persistence.last_durable_revision,
                                reason: PersistenceFailure::WriteFailed,
                            },
                        };
                        let _ = update_sender.send(health);
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

    pub fn commit(&self, revision: u64) -> Result<(), ()> {
        self.sender
            .send(WorkerMessage::Commit { revision })
            .map_err(|_| ())
    }

    pub fn next_update(&self) -> Result<PersistenceHealth, ()> {
        self.updates.recv().map_err(|_| ())
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
    fn opens_a_database_and_records_the_first_durable_revision() {
        let path =
            std::env::temp_dir().join(format!("mosaix-persistence-{}.db", std::process::id()));
        let store = Persistence::open(&path).expect("database opens");
        assert_eq!(
            store.health(),
            PersistenceHealth::Healthy {
                last_durable_revision: 0
            }
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn restart_keeps_the_last_durable_revision() {
        let path = std::env::temp_dir().join(format!(
            "mosaix-persistence-restart-{}.db",
            std::process::id()
        ));
        let mut store = Persistence::open(&path).unwrap();
        store.commit_revision(42).unwrap();
        drop(store);
        let restarted = Persistence::open(&path).unwrap();
        assert_eq!(
            restarted.health(),
            PersistenceHealth::Healthy {
                last_durable_revision: 42
            }
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn newer_schema_is_refused_without_resetting_the_database() {
        let path = std::env::temp_dir().join(format!(
            "mosaix-persistence-newer-{}.db",
            std::process::id()
        ));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        drop(connection);
        assert!(matches!(
            Persistence::open(&path),
            Err(PersistenceError::NewerSchema { found: 99, .. })
        ));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .unwrap(),
            99
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn worker_acknowledges_revisions_in_order() {
        let path = std::env::temp_dir().join(format!(
            "mosaix-persistence-worker-{}.db",
            std::process::id()
        ));
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
        std::fs::remove_file(path).ok();
    }
}
