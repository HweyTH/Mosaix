//! Integration test: real filesystem I/O and `notify`-based watching for a
//! config directory.
//!
//! Mirrors `mosaix-platform-windows/tests/enumerate_windows.rs`'s precedent
//! for an OS-touching integration test (this one touches the real
//! filesystem and a real `notify` watcher instead of Win32 APIs). Uses a
//! hand-rolled unique directory under `std::env::temp_dir()` rather than
//! pulling in a `tempfile`/`tempdir` crate, per this project's "no new
//! libraries without approval" convention -- each test gets its own
//! directory and cleans it up itself.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mosaix_config::{
    ensure_default_config, fallback_config, load, watch, Command, ConfigEvent, KeyCombo,
};

/// A fresh, empty directory under the system temp dir, unique to this test
/// process and call site.
fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mosaix-config-test-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("failed to create test temp directory");
    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

const VALID_EDIT: &str = r#"
version = 1

[hotkeys]
snap-left = "ctrl+shift+left"
snap-right = "ctrl+alt+right"
snap-top = "ctrl+alt+up"
snap-bottom = "ctrl+alt+down"

[gaps]
outer = 8
inner = 4

[behavior]
"#;

const INVALID_EDIT: &str = "version = 1\n[hotkeys\n";

#[test]
fn first_run_against_an_empty_directory_writes_a_working_default_config() {
    let dir = temp_dir("default-generation");

    ensure_default_config(&dir).expect("should create the default config directory and file");

    assert!(
        dir.join("config.toml").is_file(),
        "config.toml should have been written"
    );
    assert!(
        dir.join("profiles").is_dir(),
        "profiles/ should have been created"
    );

    let set = load(&dir)
        .expect("a freshly-created directory should be readable")
        .expect("the generator's own default content should pass validation");
    assert_eq!(
        set.base,
        fallback_config(),
        "default-generated content should match the fallback constant"
    );
    assert!(set.profiles.is_empty());

    // Calling it again on an already-populated directory must not disturb
    // the existing config.toml.
    fs::write(dir.join("config.toml"), VALID_EDIT).unwrap();
    ensure_default_config(&dir).expect("should be a no-op on an existing directory");
    let content = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert_eq!(
        content, VALID_EDIT,
        "an existing config.toml must never be overwritten"
    );

    cleanup(&dir);
}

#[test]
fn a_valid_edit_saved_while_watching_is_delivered_as_a_changed_event() {
    let dir = temp_dir("valid-edit");
    ensure_default_config(&dir).expect("should create the default config");

    let (watcher, events) = watch(dir.clone()).expect("watcher should start");

    fs::write(dir.join("config.toml"), VALID_EDIT).expect("failed to write the edited config");

    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("expected a config event after the debounced edit");

    match event {
        ConfigEvent::Changed(set) => {
            assert_eq!(
                set.base.hotkeys.get(&Command::SnapLeft),
                Some(&KeyCombo::parse("ctrl+shift+left").unwrap()),
                "the reload should reflect the edited binding"
            );
            assert_eq!(set.base.gaps.outer, 8);
        }
        ConfigEvent::Rejected(errors) => {
            panic!("expected the valid edit to be accepted, got: {errors:?}")
        }
    }

    watcher.stop();
    cleanup(&dir);
}

#[test]
fn an_invalid_edit_is_rejected_and_a_later_fix_recovers() {
    let dir = temp_dir("invalid-edit");
    ensure_default_config(&dir).expect("should create the default config");

    let (watcher, events) = watch(dir.clone()).expect("watcher should start");

    fs::write(dir.join("config.toml"), INVALID_EDIT).expect("failed to write the broken config");

    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("expected a config event after the debounced edit");
    match event {
        ConfigEvent::Rejected(errors) => {
            assert!(
                !errors.is_empty(),
                "rejection should carry at least one validation error"
            );
        }
        ConfigEvent::Changed(set) => {
            panic!("expected the invalid edit to be rejected, got: {set:?}")
        }
    }

    // A subsequent fix should recover and deliver a Changed event -- the
    // rejected edit above must not have wedged the watcher.
    fs::write(dir.join("config.toml"), VALID_EDIT).expect("failed to write the fixed config");
    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("expected a config event after the fix");
    match event {
        ConfigEvent::Changed(set) => {
            assert_eq!(
                set.base.hotkeys.get(&Command::SnapLeft),
                Some(&KeyCombo::parse("ctrl+shift+left").unwrap())
            );
        }
        ConfigEvent::Rejected(errors) => panic!("expected the fix to be accepted, got: {errors:?}"),
    }

    watcher.stop();
    cleanup(&dir);
}
