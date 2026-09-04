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

    let preserved = outcome
        .preserved
        .expect("the unusable database is preserved");
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

fn evidence(application: &str, launch_order: u32) -> mosaix_domain::WindowEvidence {
    mosaix_domain::WindowEvidence {
        application_id: mosaix_domain::ApplicationId(application.to_owned()),
        executable_path: Some(format!("C:/apps/{application}")),
        native_class: Some("Chrome_WidgetWin_1".to_owned()),
        role: mosaix_domain::WindowRole::Normal,
        launch_order,
        last_placement: mosaix_domain::Rect::new(960, 0, 960, 1080),
        display_fingerprint: "DISPLAY1".to_owned(),
    }
}

fn draft(command: &str, applications: &[&str]) -> mosaix_domain::UndoTransactionDraft {
    mosaix_domain::UndoTransactionDraft {
        command: command.to_owned(),
        recorded_at_unix: 1_756_000_000,
        topology_fingerprint: "DISPLAY1@0,0 1920x1080 scale=1".to_owned(),
        durable_revision: 12,
        members: applications
            .iter()
            .enumerate()
            .map(|(index, application)| mosaix_domain::UndoMember {
                ordinal: index as u32,
                prior_placement: mosaix_domain::Rect::new(0, 0, 800, 600),
                prior_display_fingerprint: "DISPLAY1".to_owned(),
                evidence: evidence(application, index as u32),
            })
            .collect(),
        prior_trees: Vec::new(),
    }
}

#[test]
fn a_transaction_keeps_the_arrangement_it_reshaped_across_a_restart() {
    let temporary = TempDatabase::new("undo-tree");
    let mut recorded = draft("resize-left", &["Code.exe", "firefox.exe"]);
    recorded.prior_trees = vec![mosaix_domain::UndoTreeSnapshot {
        display_fingerprint: "DISPLAY1".to_owned(),
        tree: stored_tree(&["Code.exe", "firefox.exe"]),
    }];

    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.record_transaction(&recorded).expect("records");
    }

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = restarted.newest_transaction().unwrap().unwrap();

    assert_eq!(
        loaded.prior_trees, recorded.prior_trees,
        "undo must be able to put the structure back, not only the windows"
    );
}

#[test]
fn consuming_a_transaction_removes_its_arrangement_snapshot_too() {
    let temporary = TempDatabase::new("undo-tree-consume");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let mut recorded = draft("swap-left", &["Code.exe"]);
    recorded.prior_trees = vec![mosaix_domain::UndoTreeSnapshot {
        display_fingerprint: "DISPLAY1".to_owned(),
        tree: stored_tree(&["Code.exe"]),
    }];
    let id = store.record_transaction(&recorded).unwrap();

    assert!(store.consume_transaction(id).unwrap());

    let connection = rusqlite::Connection::open(temporary.path()).unwrap();
    let remaining: i64 = connection
        .query_row("SELECT COUNT(*) FROM undo_tree", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "snapshots cascade with their transaction");
}

#[test]
fn a_recorded_transaction_survives_a_restart_intact() {
    let temporary = TempDatabase::new("undo-restart");
    let recorded = draft("snap-left", &["Code.exe"]);

    let id = {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store
            .record_transaction(&recorded)
            .expect("the transaction records")
    };

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = restarted
        .newest_transaction()
        .expect("history is readable")
        .expect("the transaction is still there");

    assert_eq!(loaded.id, id);
    assert_eq!(loaded.command, recorded.command);
    assert_eq!(loaded.recorded_at_unix, recorded.recorded_at_unix);
    assert_eq!(loaded.topology_fingerprint, recorded.topology_fingerprint);
    assert_eq!(loaded.durable_revision, recorded.durable_revision);
    assert_eq!(loaded.members, recorded.members);
}

#[test]
fn a_multi_window_transaction_survives_a_restart_with_every_member() {
    let temporary = TempDatabase::new("undo-multi-restart");
    let recorded = draft(
        "apply-layout halves",
        &["Code.exe", "firefox.exe", "wt.exe"],
    );

    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.record_transaction(&recorded).expect("records");
    }

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = restarted.newest_transaction().unwrap().unwrap();

    assert_eq!(
        loaded.members, recorded.members,
        "a transaction that loses a member on reload would undo only part of a command"
    );
    assert_eq!(
        loaded
            .members
            .iter()
            .map(|member| member.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "members come back in their recorded order"
    );
}

#[test]
fn undo_history_reads_newest_first() {
    let temporary = TempDatabase::new("undo-newest");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");

    store
        .record_transaction(&draft("snap-left", &["Code.exe"]))
        .unwrap();
    let newest = store
        .record_transaction(&draft("snap-right", &["firefox.exe"]))
        .unwrap();

    let loaded = store
        .newest_transaction()
        .unwrap()
        .expect("history is not empty");

    assert_eq!(loaded.id, newest);
    assert_eq!(loaded.command, "snap-right");
}

#[test]
fn consuming_a_transaction_removes_it_and_its_members() {
    let temporary = TempDatabase::new("undo-consume");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let older = store
        .record_transaction(&draft("snap-left", &["Code.exe"]))
        .unwrap();
    let newest = store
        .record_transaction(&draft("snap-right", &["firefox.exe"]))
        .unwrap();

    assert!(store.consume_transaction(newest).unwrap());

    let remaining = store
        .newest_transaction()
        .unwrap()
        .expect("the older one survives");
    assert_eq!(remaining.id, older);
    let orphaned: i64 = rusqlite::Connection::open(temporary.path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM undo_member WHERE transaction_id = ?1",
            [newest.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphaned, 0, "a consumed transaction leaves no member rows");
}

#[test]
fn consuming_a_transaction_that_is_already_gone_reports_that_it_did_nothing() {
    let temporary = TempDatabase::new("undo-consume-missing");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let id = store
        .record_transaction(&draft("snap-left", &["Code.exe"]))
        .unwrap();

    assert!(store.consume_transaction(id).unwrap());
    assert!(
        !store.consume_transaction(id).unwrap(),
        "consuming twice must not silently claim success"
    );
    assert_eq!(store.newest_transaction().unwrap(), None);
}

#[test]
fn stored_undo_records_contain_no_window_titles() {
    let temporary = TempDatabase::new("undo-privacy");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store
        .record_transaction(&draft("snap-left", &["Code.exe"]))
        .unwrap();
    drop(store);

    let bytes = fs::read(temporary.path()).expect("database is readable");
    let text = String::from_utf8_lossy(&bytes);

    // The evidence model has no title field at all, so the assertion that
    // matters is structural: no column exists that could hold one.
    let columns: Vec<String> = {
        let connection = rusqlite::Connection::open(temporary.path()).unwrap();
        let mut statement = connection
            .prepare("PRAGMA table_info(undo_member)")
            .unwrap();
        let names = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        names
    };
    assert!(
        !columns.iter().any(|column| column.contains("title")),
        "undo records must have nowhere to store a window title, found {columns:?}"
    );
    assert!(
        !text.contains("a document nobody should be storing"),
        "no captured title may reach the database file"
    );
}

fn dated_draft(command: &str, recorded_at_unix: i64) -> mosaix_domain::UndoTransactionDraft {
    mosaix_domain::UndoTransactionDraft {
        prior_trees: Vec::new(),
        recorded_at_unix,
        ..draft(command, &["Code.exe"])
    }
}

fn transaction_count(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM undo_transaction", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn member_count(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM undo_member", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn history_keeps_only_the_newest_hundred_transactions() {
    let temporary = TempDatabase::new("retention-count");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let now = 1_756_000_000;

    for index in 0..105 {
        store
            .record_transaction(&dated_draft(&format!("snap-{index}"), now))
            .expect("records");
    }

    assert_eq!(
        transaction_count(&temporary.path()),
        mosaix_persistence::MAX_UNDO_TRANSACTIONS as i64
    );
    assert_eq!(
        member_count(&temporary.path()),
        mosaix_persistence::MAX_UNDO_TRANSACTIONS as i64,
        "pruning a transaction takes its members with it"
    );
    assert_eq!(
        store.newest_transaction().unwrap().unwrap().command,
        "snap-104",
        "the newest command is the one that survives, not the oldest"
    );
}

#[test]
fn history_drops_transactions_older_than_the_retention_window() {
    let temporary = TempDatabase::new("retention-age");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let now = 1_756_000_000;
    let window = mosaix_persistence::UNDO_RETENTION_SECONDS;

    // Exactly at the boundary, one second inside it, and one second past.
    store
        .record_transaction(&dated_draft("too-old", now - window - 1))
        .unwrap();
    store
        .record_transaction(&dated_draft("exactly-at-the-edge", now - window))
        .unwrap();
    store
        .record_transaction(&dated_draft("inside", now - window + 1))
        .unwrap();
    assert_eq!(transaction_count(&temporary.path()), 3);

    let pruned = store.prune_history(now).expect("pruning succeeds");

    assert_eq!(pruned, 1, "only the entry past the window goes");
    let surviving: Vec<String> = {
        let connection = rusqlite::Connection::open(temporary.path()).unwrap();
        let mut statement = connection
            .prepare("SELECT command FROM undo_transaction ORDER BY id")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert_eq!(surviving, vec!["exactly-at-the-edge", "inside"]);
}

#[test]
fn pruning_is_durable_across_a_restart() {
    let temporary = TempDatabase::new("retention-restart");
    let now = 1_756_000_000;
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store
            .record_transaction(&dated_draft("ancient", now - 400_000_000))
            .unwrap();
        store
            .record_transaction(&dated_draft("recent", now))
            .unwrap();
        store.prune_history(now).unwrap();
    }

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");

    assert_eq!(transaction_count(&temporary.path()), 1);
    assert_eq!(
        restarted.newest_transaction().unwrap().unwrap().command,
        "recent"
    );
}

#[test]
fn pruning_an_already_bounded_history_changes_nothing() {
    let temporary = TempDatabase::new("retention-idempotent");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let now = 1_756_000_000;
    store
        .record_transaction(&dated_draft("recent", now))
        .unwrap();

    assert_eq!(store.prune_history(now).unwrap(), 0);
    assert_eq!(store.prune_history(now).unwrap(), 0);
    assert_eq!(transaction_count(&temporary.path()), 1);
}

fn stored_tree(applications: &[&str]) -> mosaix_domain::PersistedTree {
    use mosaix_domain::tree::{Child, Node, SplitAxis};

    let mut children: Vec<Child<mosaix_domain::WindowEvidence>> = Vec::new();
    for (index, application) in applications.iter().enumerate() {
        children.push(Child {
            weight: 1.0 + index as f64,
            node: Node::window(evidence(application, index as u32)),
        });
    }
    mosaix_domain::PersistedTree::from_root(Node::Split {
        axis: SplitAxis::Vertical,
        children,
    })
}

#[test]
fn an_arrangement_survives_a_restart_with_its_axes_and_weights() {
    let temporary = TempDatabase::new("tree-restart");
    let tree = stored_tree(&["Code.exe", "firefox.exe", "wt.exe"]);

    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_tree("DISPLAY1", &tree).expect("the tree stores");
    }

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = restarted.load_trees().expect("arrangements are readable");

    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded.get("DISPLAY1"),
        Some(&tree),
        "topology, axes, weights, and leaf evidence all survive"
    );
}

#[test]
fn a_dormant_leaf_survives_a_restart_with_its_evidence_and_age() {
    use mosaix_domain::tree::{Child, DormantPosition, Node, SplitAxis};

    let temporary = TempDatabase::new("tree-dormant");
    let tree = mosaix_domain::PersistedTree::from_root(Node::Split {
        axis: SplitAxis::Horizontal,
        children: vec![
            Child {
                weight: 1.0,
                node: Node::window(evidence("Code.exe", 0)),
            },
            Child {
                weight: 1.0,
                node: Node::dormant(DormantPosition {
                    evidence: evidence("firefox.exe", 0),
                    since_unix: 1_756_000_000,
                }),
            },
        ],
    });

    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_tree("DISPLAY1", &tree).expect("the tree stores");
    }
    let restarted = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = restarted.load_trees().expect("arrangements are readable");

    assert_eq!(loaded.get("DISPLAY1"), Some(&tree));
    let dormant = loaded["DISPLAY1"].dormant_positions();
    assert_eq!(dormant.len(), 1);
    assert_eq!(dormant[0].1.since_unix, 1_756_000_000);
}

#[test]
fn saving_an_arrangement_replaces_the_one_that_display_held() {
    let temporary = TempDatabase::new("tree-replace");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");

    store
        .save_tree("DISPLAY1", &stored_tree(&["Code.exe"]))
        .unwrap();
    let replacement = stored_tree(&["firefox.exe", "wt.exe"]);
    store.save_tree("DISPLAY1", &replacement).unwrap();

    let loaded = store.load_trees().unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "a display holds one arrangement, not a history"
    );
    assert_eq!(loaded["DISPLAY1"], replacement);
}

#[test]
fn saving_an_empty_arrangement_forgets_the_display_rather_than_storing_emptiness() {
    let temporary = TempDatabase::new("tree-empty");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store
        .save_tree("DISPLAY1", &stored_tree(&["Code.exe"]))
        .unwrap();

    store
        .save_tree("DISPLAY1", &mosaix_domain::PersistedTree::new())
        .unwrap();

    assert!(
        store.load_trees().unwrap().is_empty(),
        "an empty stored arrangement would restore emptiness over a fresh one"
    );
}

#[test]
fn each_display_keeps_its_own_arrangement() {
    let temporary = TempDatabase::new("tree-per-display");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");

    store
        .save_tree("DISPLAY1", &stored_tree(&["Code.exe"]))
        .unwrap();
    store
        .save_tree("DISPLAY2", &stored_tree(&["firefox.exe"]))
        .unwrap();

    let loaded = store.load_trees().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_ne!(loaded["DISPLAY1"], loaded["DISPLAY2"]);
}

#[test]
fn an_unreadable_arrangement_does_not_cost_the_others() {
    let temporary = TempDatabase::new("tree-unreadable");
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store
            .save_tree("DISPLAY1", &stored_tree(&["Code.exe"]))
            .unwrap();
        store
            .save_tree("DISPLAY2", &stored_tree(&["firefox.exe"]))
            .unwrap();
    }
    rusqlite::Connection::open(temporary.path())
        .unwrap()
        .execute(
            "UPDATE container_tree SET tree = 'not a tree' WHERE display_fingerprint = ?1",
            ["DISPLAY1"],
        )
        .unwrap();

    let store = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = store.load_trees().expect("the load itself still succeeds");

    assert_eq!(
        loaded.keys().collect::<Vec<_>>(),
        vec!["DISPLAY2"],
        "one unreadable display must not cost the arrangement of the others"
    );
}

#[test]
fn stored_arrangements_contain_no_window_titles() {
    let temporary = TempDatabase::new("tree-privacy");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store
        .save_tree("DISPLAY1", &stored_tree(&["Code.exe"]))
        .unwrap();
    drop(store);

    let bytes = fs::read(temporary.path()).expect("database is readable");

    assert!(
        !String::from_utf8_lossy(&bytes).contains("a document nobody should be storing"),
        "leaf identity is evidence, and evidence has no title field"
    );
}

#[test]
fn a_locked_database_degrades_the_write_and_recovers_when_the_lock_clears() {
    let temporary = TempDatabase::new("locked");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store
        .commit_revision(1)
        .expect("the first revision commits");

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
    store
        .commit_revision(7)
        .expect("the first revision commits");

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

fn stored_workspace(
    name: &str,
    origin: mosaix_domain::WorkspaceOrigin,
    displayed: Option<&str>,
    tree: Option<mosaix_domain::PersistedTree>,
) -> mosaix_domain::PersistedWorkspace {
    mosaix_domain::PersistedWorkspace {
        name: mosaix_domain::WorkspaceName::new(name).unwrap(),
        origin,
        displayed_fingerprint: displayed.map(str::to_owned),
        tree,
    }
}

#[test]
fn a_workspace_survives_a_restart_with_its_display_origin_and_tree() {
    let temporary = TempDatabase::new("workspace-restart");
    let dev = stored_workspace(
        "dev",
        mosaix_domain::WorkspaceOrigin::Command,
        Some("DISPLAY1"),
        Some(stored_tree(&["Code.exe", "wt.exe"])),
    );
    let chat = stored_workspace(
        "chat",
        mosaix_domain::WorkspaceOrigin::Configuration,
        None,
        None,
    );
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_workspace(&dev).unwrap();
        store.save_workspace(&chat).unwrap();
    }

    let restarted = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = restarted.load_workspaces().unwrap();

    assert_eq!(loaded, vec![chat, dev], "read back in name order");
}

#[test]
fn saving_a_workspace_again_moves_it_rather_than_duplicating_it() {
    let temporary = TempDatabase::new("workspace-move");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store
        .save_workspace(&stored_workspace(
            "dev",
            mosaix_domain::WorkspaceOrigin::Command,
            Some("DISPLAY1"),
            Some(stored_tree(&["Code.exe"])),
        ))
        .unwrap();

    let moved = stored_workspace(
        "dev",
        mosaix_domain::WorkspaceOrigin::Command,
        Some("DISPLAY2"),
        Some(stored_tree(&["Code.exe"])),
    );
    store.save_workspace(&moved).unwrap();

    assert_eq!(store.load_workspaces().unwrap(), vec![moved]);
}

#[test]
fn an_empty_tree_is_stored_as_no_tree() {
    let temporary = TempDatabase::new("workspace-empty-tree");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    store
        .save_workspace(&stored_workspace(
            "dev",
            mosaix_domain::WorkspaceOrigin::Command,
            None,
            Some(mosaix_domain::PersistedTree::new()),
        ))
        .unwrap();

    let loaded = store.load_workspaces().unwrap();
    assert_eq!(loaded[0].tree, None);
}

#[test]
fn deleting_a_workspace_removes_its_row_and_reports_whether_it_existed() {
    let temporary = TempDatabase::new("workspace-delete");
    let mut store = Persistence::open(&temporary.path()).expect("database opens");
    let name = mosaix_domain::WorkspaceName::new("dev").unwrap();
    store
        .save_workspace(&stored_workspace(
            "dev",
            mosaix_domain::WorkspaceOrigin::Command,
            None,
            None,
        ))
        .unwrap();

    assert!(store.delete_workspace(&name).unwrap());
    assert!(!store.delete_workspace(&name).unwrap());
    assert!(store.load_workspaces().unwrap().is_empty());
}

#[test]
fn an_unreadable_workspace_tree_costs_only_that_tree() {
    let temporary = TempDatabase::new("workspace-unreadable");
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store
            .save_workspace(&stored_workspace(
                "dev",
                mosaix_domain::WorkspaceOrigin::Command,
                Some("DISPLAY1"),
                Some(stored_tree(&["Code.exe"])),
            ))
            .unwrap();
    }
    {
        let connection = rusqlite::Connection::open(temporary.path()).unwrap();
        connection
            .execute(
                "UPDATE workspace SET tree = 'not a tree' WHERE name = 'dev'",
                [],
            )
            .unwrap();
    }

    let store = Persistence::open(&temporary.path()).expect("database reopens");
    let loaded = store.load_workspaces().unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].displayed_fingerprint.as_deref(), Some("DISPLAY1"));
    assert_eq!(loaded[0].tree, None);
}

#[test]
fn stored_workspaces_contain_no_window_titles() {
    let temporary = TempDatabase::new("workspace-titles");
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store
            .save_workspace(&stored_workspace(
                "dev",
                mosaix_domain::WorkspaceOrigin::Command,
                Some("DISPLAY1"),
                Some(stored_tree(&["Code.exe"])),
            ))
            .unwrap();
    }
    let bytes = fs::read(temporary.path()).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("title"),
        "a stored workspace has no title column and its tree evidence has no title field"
    );
}

// ---- The recovery ledger (issue #58) ----------------------------------

fn ledger_draft(session: &str, handle: isize, pid: u32) -> mosaix_domain::RecoveryDraft {
    mosaix_domain::RecoveryDraft {
        session_id: session.to_owned(),
        native_handle: handle,
        process: mosaix_domain::ProcessInstance {
            process_id: pid,
            creation_time: 133_000_000_000_000_000,
        },
        application_id: mosaix_domain::ApplicationId("code.exe".to_owned()),
        executable_path: Some("C:/apps/code.exe".to_owned()),
        native_class: Some("Chrome_WidgetWin_1".to_owned()),
        original_display_fingerprint: "DISPLAY1".to_owned(),
        visible_bounds: mosaix_domain::Rect::new(10, 20, 800, 600),
        normal_bounds: mosaix_domain::Rect::new(30, 40, 700, 500),
        show_state: mosaix_domain::ShowState::Maximized,
        recorded_at_unix: 1_756_000_000,
    }
}

fn ledger_path(temporary: &TempDatabase) -> PathBuf {
    temporary.path().with_file_name("recovery-ledger.db")
}

#[test]
fn a_recorded_entry_is_durable_before_the_id_is_returned_and_survives_a_restart() {
    let temporary = TempDatabase::new("ledger-record");
    let draft = ledger_draft("s1", 0x1234, 77);
    let id = {
        let mut ledger =
            mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
        let id = ledger.record(&draft).unwrap();
        ledger.mark_parked(id).unwrap();
        id
    };

    // Reopened cold, as the restore command does after a crash: the
    // entry is there with every field, and it is open.
    let ledger = mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
    let entries = ledger.entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].draft, draft);
    assert!(entries[0].parked);
    assert!(!entries[0].restored);
    assert_eq!(ledger.open_entries().unwrap().len(), 1);
}

#[test]
fn an_entry_recorded_but_never_parked_is_not_open_and_is_pruned_by_a_later_session() {
    let temporary = TempDatabase::new("ledger-unparked");
    let mut ledger = mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
    ledger.record(&ledger_draft("s1", 1, 7)).unwrap();

    assert!(
        ledger.open_entries().unwrap().is_empty(),
        "a window that never left visible geometry needs no recovery"
    );
    assert_eq!(
        ledger.prune("s1").unwrap(),
        0,
        "the recording session keeps it"
    );
    assert_eq!(ledger.prune("s2").unwrap(), 1, "a later session drops it");
}

#[test]
fn a_clean_exit_marks_entries_restored_and_the_next_session_finds_nothing_to_recover() {
    let temporary = TempDatabase::new("ledger-clean-exit");
    {
        let mut ledger =
            mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
        let id = ledger.record(&ledger_draft("s1", 1, 7)).unwrap();
        ledger.mark_parked(id).unwrap();
        ledger.mark_restored(id).unwrap();
    }
    let mut ledger = mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
    assert!(ledger.open_entries().unwrap().is_empty());
    assert_eq!(ledger.prune("s2").unwrap(), 1);
    assert!(ledger.entries().unwrap().is_empty());
}

#[test]
fn an_interrupted_write_leaves_no_partial_entry() {
    let temporary = TempDatabase::new("ledger-interrupted");
    let path = ledger_path(&temporary);
    mosaix_persistence::RecoveryLedger::open(&path).unwrap();
    {
        // A transaction that never commits, as a process killed mid-write
        // leaves behind: SQLite rolls it back on the next open.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("BEGIN;").unwrap();
        connection
            .execute(
                "INSERT INTO recovery_entry (
                     session_id, native_handle, process_id, process_creation_time,
                     application_id, original_display_fingerprint,
                     visible_x, visible_y, visible_width, visible_height,
                     normal_x, normal_y, normal_width, normal_height,
                     show_state, recorded_at_unix, parked
                 ) VALUES ('s1', 1, 7, 0, 'code.exe', 'D', 0, 0, 1, 1, 0, 0, 1, 1, 'normal', 1, 1)",
                [],
            )
            .unwrap();
        // Dropped without COMMIT.
    }
    let ledger = mosaix_persistence::RecoveryLedger::open(&path).unwrap();
    assert!(ledger.entries().unwrap().is_empty());
}

#[test]
fn recovery_restores_only_verified_handles_and_reports_the_rest_untouched() {
    let temporary = TempDatabase::new("ledger-recover");
    let mut ledger = mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
    let verified = ledger.record(&ledger_draft("s1", 100, 7)).unwrap();
    let stale = ledger.record(&ledger_draft("s1", 200, 8)).unwrap();
    let reused = ledger.record(&ledger_draft("s1", 300, 9)).unwrap();
    let ambiguous_a = ledger.record(&ledger_draft("s0", 400, 10)).unwrap();
    let ambiguous_b = ledger.record(&ledger_draft("s1", 400, 10)).unwrap();
    for id in [verified, stale, reused, ambiguous_a, ambiguous_b] {
        ledger.mark_parked(id).unwrap();
    }

    let mut restored_handles = Vec::new();
    let outcomes = mosaix_persistence::recover_parked_windows(
        &mut ledger,
        |handle| match handle {
            100 => Some(mosaix_domain::LiveHandleEvidence {
                process: mosaix_domain::ProcessInstance {
                    process_id: 7,
                    creation_time: 133_000_000_000_000_000,
                },
                native_class: Some("Chrome_WidgetWin_1".to_owned()),
            }),
            300 => Some(mosaix_domain::LiveHandleEvidence {
                process: mosaix_domain::ProcessInstance {
                    process_id: 9,
                    creation_time: 1,
                },
                native_class: Some("Notepad".to_owned()),
            }),
            400 => Some(mosaix_domain::LiveHandleEvidence {
                process: mosaix_domain::ProcessInstance {
                    process_id: 10,
                    creation_time: 133_000_000_000_000_000,
                },
                native_class: Some("Chrome_WidgetWin_1".to_owned()),
            }),
            _ => None,
        },
        |entry| {
            restored_handles.push(entry.draft.native_handle);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(
        restored_handles,
        vec![100],
        "only the verified handle was touched"
    );
    let by_id = |id: mosaix_domain::RecoveryEntryId| {
        outcomes
            .iter()
            .find(|outcome| outcome.entry_id == id)
            .unwrap()
    };
    assert_eq!(
        by_id(verified).verdict,
        mosaix_domain::HandleVerdict::Verified
    );
    assert!(by_id(verified).restored);
    assert_eq!(by_id(stale).verdict, mosaix_domain::HandleVerdict::Stale);
    assert!(matches!(
        by_id(reused).verdict,
        mosaix_domain::HandleVerdict::Reused { .. }
    ));
    assert_eq!(
        by_id(ambiguous_a).verdict,
        mosaix_domain::HandleVerdict::Ambiguous { claimants: 2 }
    );
    let open: Vec<_> = ledger
        .open_entries()
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(
        open,
        vec![stale, reused, ambiguous_a, ambiguous_b],
        "unverified entries stay open as evidence rather than being forgotten"
    );
}

#[test]
fn a_failed_restore_keeps_the_entry_open_and_reports_the_reason() {
    let temporary = TempDatabase::new("ledger-restore-fails");
    let mut ledger = mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
    let id = ledger.record(&ledger_draft("s1", 100, 7)).unwrap();
    ledger.mark_parked(id).unwrap();

    let outcomes = mosaix_persistence::recover_parked_windows(
        &mut ledger,
        |_| {
            Some(mosaix_domain::LiveHandleEvidence {
                process: mosaix_domain::ProcessInstance {
                    process_id: 7,
                    creation_time: 133_000_000_000_000_000,
                },
                native_class: None,
            })
        },
        |_| Err("SetWindowPlacement failed".to_owned()),
    )
    .unwrap();

    assert!(!outcomes[0].restored);
    assert_eq!(
        outcomes[0].failure.as_deref(),
        Some("SetWindowPlacement failed")
    );
    assert_eq!(ledger.open_entries().unwrap().len(), 1);
}

#[test]
fn the_worker_acknowledges_a_ledger_write_with_its_token_and_refuses_without_a_ledger() {
    let temporary = TempDatabase::new("ledger-worker");
    let worker = mosaix_persistence::PersistenceWorker::start_with_ledger(
        temporary.path(),
        Some(&ledger_path(&temporary)),
    )
    .unwrap();
    let _first = worker.next_update().unwrap();

    worker
        .submit(mosaix_persistence::PersistenceRequest::RecordRecovery {
            token: 41,
            draft: Box::new(ledger_draft("s1", 5, 7)),
        })
        .unwrap();
    let update = worker.next_update().unwrap();
    assert_eq!(update.recovery_acknowledged.len(), 1);
    assert_eq!(update.recovery_acknowledged[0].0, 41);
    let id = update.recovery_acknowledged[0]
        .1
        .expect("the entry is durable");
    worker
        .submit(mosaix_persistence::PersistenceRequest::MarkParked(id))
        .unwrap();
    let _ = worker.next_update().unwrap();
    worker.stop();

    let ledger = mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
    assert_eq!(ledger.open_entries().unwrap().len(), 1);

    let without = mosaix_persistence::PersistenceWorker::start(temporary.path()).unwrap();
    let _first = without.next_update().unwrap();
    without
        .submit(mosaix_persistence::PersistenceRequest::RecordRecovery {
            token: 42,
            draft: Box::new(ledger_draft("s1", 6, 7)),
        })
        .unwrap();
    let update = without.next_update().unwrap();
    assert_eq!(update.recovery_acknowledged, vec![(42, None)]);
    without.stop();
}

#[test]
fn the_ledger_stores_no_window_titles() {
    let temporary = TempDatabase::new("ledger-titles");
    {
        let mut ledger =
            mosaix_persistence::RecoveryLedger::open(&ledger_path(&temporary)).unwrap();
        ledger.record(&ledger_draft("s1", 1, 7)).unwrap();
    }
    let bytes = fs::read(ledger_path(&temporary)).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("title"));
}
