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
    ensure_default_config, fallback_config, load, save_profile_settings, watch, Command,
    ConfigEvent, FocusBorderOverride, GapsOverride, KeyCombo, ProfileSettingsUpdate, RgbaColor,
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

#[test]
fn settings_atomically_create_and_replace_the_matching_topology_profile() {
    let dir = temp_dir("settings-profile");
    ensure_default_config(&dir).unwrap();
    let update = |enabled, outer, thickness| ProfileSettingsUpdate {
        fingerprint: "DISPLAY-A@0,0 1920x1080 scale=1".to_owned(),
        automatic_tiling_enabled: enabled,
        gaps: GapsOverride {
            outer: Some(outer),
            inner: Some(6),
        },
        focus_border: FocusBorderOverride {
            enabled: Some(true),
            color: Some(RgbaColor {
                red: 20,
                green: 130,
                blue: 220,
                alpha: 240,
            }),
            thickness: Some(thickness),
        },
    };

    save_profile_settings(&dir, update(true, 12, 3)).unwrap();
    let replaced = save_profile_settings(&dir, update(false, 18, 5)).unwrap();

    assert_eq!(replaced.profiles.len(), 1);
    let profile = &replaced.profiles[0].config;
    assert!(!profile.automatic_tiling_enabled);
    assert_eq!(profile.gaps.outer, 18);
    assert_eq!(profile.gaps.inner, 6);
    assert_eq!(profile.focus_border.thickness, 5);
    assert_eq!(
        fs::read_dir(dir.join("profiles")).unwrap().count(),
        1,
        "updating the same fingerprint must replace, not duplicate, its profile"
    );

    cleanup(&dir);
}

#[test]
fn saving_tiling_settings_keeps_a_profiles_hand_written_layouts() {
    // The settings application rewrites a whole profile file to change one
    // setting, so anything it does not know about has to survive the round
    // trip. A layouts table a user hand-wrote is exactly that.
    let dir = temp_dir("profile-layouts");
    ensure_default_config(&dir).unwrap();
    let fingerprint = "DISPLAY-A@0,0 1920x1080 scale=1";
    fs::write(
        dir.join("profiles").join("desk.toml"),
        format!(
            "fingerprint = \"{fingerprint}\"\n\
             [layouts.docked]\n\
             cells = [{{ x = 0.0, y = 0.0, width = 0.5, height = 1.0 }}]\n"
        ),
    )
    .unwrap();

    let saved = save_profile_settings(
        &dir,
        ProfileSettingsUpdate {
            fingerprint: fingerprint.to_owned(),
            automatic_tiling_enabled: true,
            gaps: GapsOverride {
                outer: Some(8),
                inner: Some(4),
            },
            focus_border: FocusBorderOverride::default(),
        },
    )
    .unwrap();

    let profile = &saved.profiles[0].config;
    assert!(profile.automatic_tiling_enabled);
    assert_eq!(
        profile
            .layouts
            .get("docked")
            .map(|layout| layout.cells.len()),
        Some(1),
        "the profile's layouts must survive a settings write, got {:?}",
        profile.layouts.keys().collect::<Vec<_>>()
    );

    cleanup(&dir);
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

/// A file that parses cleanly and is still invalid: the layout `writing`
/// has a cell running off the right of the work area. Distinct from
/// `INVALID_EDIT`, which fails at the TOML layer -- this one only fails
/// once `validate` applies the saved-layout rules.
const INVALID_LAYOUT_EDIT: &str = r#"
version = 1

[hotkeys]
snap-left = "ctrl+alt+left"

[layouts.writing]
cells = [{ x = 0.6, y = 0.0, width = 0.9, height = 1.0 }]
"#;

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

#[test]
fn a_malformed_saved_layout_is_rejected_and_the_previous_config_keeps_running() {
    let dir = temp_dir("invalid-layout");
    ensure_default_config(&dir).expect("should create the default config");

    // The config in effect before the bad edit: the one the agent is
    // already running on.
    let before = load(&dir).unwrap().expect("the default config is valid");

    let (watcher, events) = watch(dir.clone()).expect("watcher should start");

    fs::write(dir.join("config.toml"), INVALID_LAYOUT_EDIT)
        .expect("failed to write the broken layout");

    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("expected a config event after the debounced edit");
    match event {
        ConfigEvent::Rejected(errors) => {
            assert!(
                errors
                    .iter()
                    .any(|error| error.to_string().contains("config.toml")),
                "the rejection must name the file, got: {errors:?}"
            );
            assert!(
                errors
                    .iter()
                    .any(|error| error.to_string().contains("writing")),
                "the rejection must name the layout, got: {errors:?}"
            );
        }
        ConfigEvent::Changed(set) => {
            panic!("a cell outside the work area must be rejected, got: {set:?}")
        }
    }

    // A rejection is not a new resolved config, so nothing supersedes what
    // the agent already holds -- and the watcher is still live enough to
    // deliver the fix.
    fs::write(dir.join("config.toml"), VALID_EDIT).expect("failed to write the fixed config");
    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("expected a config event after the fix");
    match event {
        ConfigEvent::Changed(set) => {
            assert_ne!(
                set.base.hotkeys, before.base.hotkeys,
                "the fix should be a genuinely different config from the one that was running"
            );
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
