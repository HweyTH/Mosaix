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
