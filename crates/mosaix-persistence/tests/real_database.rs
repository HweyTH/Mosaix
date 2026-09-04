//! Behaviour of the persistence boundary against real temporary SQLite
//! databases. The spec forbids a mock adapter here: migrations, rollback,
//! locking, and corruption are properties of SQLite itself, so a fake
//! would only assert that our test double agrees with our production code.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use mosaix_persistence::{
    Migration, Persistence, PersistenceError, PersistenceFailure, PersistenceHealth,
    SUPPORTED_SCHEMA_VERSION,
};

/// A uniquely named database directory that removes itself on drop, so a
/// failing assertion never leaks state into the next run.
struct TempDatabase {
    directory: PathBuf,
}

impl TempDatabase {
    fn new(label: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mosaix-persistence-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temporary directory is creatable");
        Self { directory }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("state.db")
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn user_version(path: &Path) -> i32 {
    let connection = rusqlite::Connection::open(path).expect("database opens for inspection");
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version is readable")
}

fn table_names(path: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(path).expect("database opens for inspection");
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("schema query prepares");
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("schema query runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema rows read");
    names
}

#[test]
fn creating_a_database_applies_every_migration_in_order() {
    let temporary = TempDatabase::new("create");

    let store = Persistence::open(&temporary.path()).expect("a fresh database opens");

    assert_eq!(store.schema_version(), SUPPORTED_SCHEMA_VERSION);
    assert_eq!(
        store.health(),
        PersistenceHealth::Healthy {
            last_durable_revision: 0
        }
    );
    assert_eq!(user_version(&temporary.path()), SUPPORTED_SCHEMA_VERSION);
}

#[test]
fn migrations_are_numbered_forward_only_and_strictly_ascending() {
    let versions: Vec<i32> = mosaix_persistence::migrations()
        .iter()
        .map(|migration| migration.version)
        .collect();

    assert!(
        !versions.is_empty(),
        "the first schema must ship at least one migration"
    );
    assert!(
        versions.windows(2).all(|pair| pair[0] < pair[1]),
        "migrations must be strictly ascending, found {versions:?}"
    );
    assert_eq!(
        versions.first().copied(),
        Some(1),
        "migration numbering starts at 1"
    );
    assert_eq!(
        versions.last().copied(),
        Some(SUPPORTED_SCHEMA_VERSION),
        "the supported version is the newest migration"
    );
}

#[test]
fn an_older_database_is_migrated_up_without_replaying_applied_migrations() {
    let temporary = TempDatabase::new("upgrade");
    let first_only = &mosaix_persistence::migrations()[..1];

    let partial = Persistence::open_with_migrations(&temporary.path(), first_only)
        .expect("the first migration applies");
    assert_eq!(partial.schema_version(), 1);
    drop(partial);

    let upgraded = Persistence::open(&temporary.path()).expect("the remaining migrations apply");

    assert_eq!(upgraded.schema_version(), SUPPORTED_SCHEMA_VERSION);
    assert_eq!(user_version(&temporary.path()), SUPPORTED_SCHEMA_VERSION);
}

#[test]
fn restart_keeps_the_last_durable_revision() {
    let temporary = TempDatabase::new("restart");

    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store.commit_revision(42).expect("revision commits");
    drop(store);

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");

    assert_eq!(
        restarted.health(),
        PersistenceHealth::Healthy {
            last_durable_revision: 42
        }
    );
}

#[test]
fn a_failing_migration_rolls_back_and_leaves_the_database_at_its_previous_version() {
    let temporary = TempDatabase::new("rollback");
    let broken: &[Migration] = &[
        Migration {
            version: 1,
            statements: mosaix_persistence::migrations()[0].statements,
        },
        Migration {
            version: 2,
            statements: "CREATE TABLE doomed (id INTEGER PRIMARY KEY);
                         CREATE TABLE doomed (id INTEGER PRIMARY KEY);",
        },
    ];

    let error = Persistence::open_with_migrations(&temporary.path(), broken)
        .expect_err("the second migration fails");

    assert_eq!(error.failure(), PersistenceFailure::MigrationFailed);
    assert_eq!(
        user_version(&temporary.path()),
        1,
        "a failed migration must not advance the recorded schema version"
    );
    assert!(
        !table_names(&temporary.path()).contains(&"doomed".to_owned()),
        "a failed migration must roll back every statement it ran"
    );
}

#[test]
fn a_newer_schema_is_refused_and_the_database_is_preserved_untouched() {
    let temporary = TempDatabase::new("newer");
    let future_version = SUPPORTED_SCHEMA_VERSION + 98;
    {
        let connection = rusqlite::Connection::open(temporary.path()).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE evidence (id INTEGER PRIMARY KEY);
                 PRAGMA user_version = {future_version};"
            ))
            .unwrap();
    }
    let before = fs::read(temporary.path()).expect("database is readable");

    let error = Persistence::open(&temporary.path()).expect_err("a newer schema is refused");

    assert!(matches!(
        error,
        PersistenceError::NewerSchema { found, supported }
            if found == future_version && supported == SUPPORTED_SCHEMA_VERSION
    ));
    assert_eq!(error.failure(), PersistenceFailure::NewerSchema);
    assert_eq!(
        fs::read(temporary.path()).expect("database is still readable"),
        before,
        "refusing a newer schema must not rewrite a single byte"
    );
}

#[test]
fn a_corrupt_database_is_refused_and_preserved_untouched() {
    let temporary = TempDatabase::new("corrupt");
    {
        let mut file = fs::File::create(temporary.path()).expect("file is creatable");
        file.write_all(b"this is emphatically not a SQLite database")
            .expect("garbage is writable");
    }
    let before = fs::read(temporary.path()).expect("database is readable");

    let error = Persistence::open(&temporary.path()).expect_err("a corrupt database is refused");

    assert_eq!(error.failure(), PersistenceFailure::CorruptOrUnreadable);
    assert_eq!(
        fs::read(temporary.path()).expect("database is still readable"),
        before,
        "refusing a corrupt database must not rewrite a single byte"
    );
}

#[test]
fn resetting_is_explicit_and_preserves_the_unusable_database() {
    let temporary = TempDatabase::new("reset");
    {
        let mut file = fs::File::create(temporary.path()).expect("file is creatable");
        file.write_all(b"this is emphatically not a SQLite database")
            .expect("garbage is writable");
    }
    let corrupt_bytes = fs::read(temporary.path()).expect("database is readable");

    let outcome = Persistence::reset(&temporary.path()).expect("an explicit reset succeeds");

    let preserved = outcome.preserved.expect("the unusable database is preserved");
    assert_eq!(
        fs::read(&preserved).expect("preserved copy is readable"),
        corrupt_bytes,
        "reset must move the unusable database aside byte-for-byte"
    );
    let store = Persistence::open(&temporary.path()).expect("the fresh database opens");
    assert_eq!(store.schema_version(), SUPPORTED_SCHEMA_VERSION);
    assert_eq!(
        store.health(),
        PersistenceHealth::Healthy {
            last_durable_revision: 0
        }
    );
}

#[test]
fn a_locked_database_degrades_the_write_and_recovers_when_the_lock_clears() {
    let temporary = TempDatabase::new("locked");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store.commit_revision(1).expect("the first revision commits");

    let blocker = rusqlite::Connection::open(temporary.path()).expect("blocking connection opens");
    blocker
        .execute_batch("BEGIN EXCLUSIVE")
        .expect("an exclusive lock is taken");

    let error = store
        .commit_revision(2)
        .expect_err("a locked database cannot accept a write");
    assert_eq!(error.failure(), PersistenceFailure::WriteFailed);
    assert_eq!(
        store.health(),
        PersistenceHealth::Degraded {
            last_durable_revision: 1,
            reason: PersistenceFailure::WriteFailed
        },
        "a failed write must not promise durability it did not achieve"
    );

    blocker
        .execute_batch("ROLLBACK")
        .expect("the lock is released");

    let health = store
        .commit_revision(2)
        .expect("the retried write succeeds once the lock clears");
    assert_eq!(
        health,
        PersistenceHealth::Healthy {
            last_durable_revision: 2
        }
    );
}

#[test]
fn a_write_failure_never_advances_the_durable_revision() {
    let temporary = TempDatabase::new("no-false-promise");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store.commit_revision(7).expect("the first revision commits");

    let blocker = rusqlite::Connection::open(temporary.path()).expect("blocking connection opens");
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let _ = store.commit_revision(8);
    blocker.execute_batch("ROLLBACK").unwrap();
    drop(store);

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");

    assert_eq!(
        restarted.health(),
        PersistenceHealth::Healthy {
            last_durable_revision: 7
        },
        "the revision whose write failed must not survive a restart"
    );
}
