//! Default config generation and the startup-failure fallback.
//!
//! The bindings here are what a first-run generated `config.toml` and the
//! in-memory fallback both reproduce. Win+Alt carries the letters,
//! Win+Alt+Shift moves a window (arrows to a zone, home row to swap), and
//! Win+Ctrl+Alt resizes.
//!
//! The arrows do not sit on plain Win+Alt because Windows 11 reserves
//! Win+Alt+Arrow: `RegisterHotKey` refuses it with
//! ERROR_HOTKEY_ALREADY_REGISTERED, so the four snap commands -- the ones
//! the product is mostly used for -- silently did nothing.
//!
//! Win rather than Ctrl as the second modifier because Ctrl+Alt *is*
//! AltGr: on a European layout every Ctrl+Alt binding here would fire
//! while the user was typing an ordinary character. Ctrl+Alt also runs
//! straight into JetBrains' own table (Ctrl+Alt+L reformats code).
//!
//! Plain Alt, which GlazeWM and komorebi default to, is not open to us:
//! RegisterHotKey is OS-arbitrated and cannot swallow the keystroke, so
//! an Alt binding would eat the menu mnemonic underneath it.

use std::collections::BTreeMap;

use mosaix_domain::Gaps;

use crate::schema::{
    BaseConfig, BehaviorSection, Command, KeyCombo, ResolvedConfig, CURRENT_VERSION,
};
use crate::validate::merge;

fn default_combo(key: &str) -> KeyCombo {
    KeyCombo {
        ctrl: false,
        alt: true,
        shift: false,
        win: true,
        key: key.to_string(),
    }
}

fn shifted_combo(key: &str) -> KeyCombo {
    KeyCombo {
        shift: true,
        ..default_combo(key)
    }
}

/// Win+Ctrl+Alt: the only modifier set left for the arrow keys once the
/// others are ruled out.
///
/// Not AltGr, despite carrying Ctrl and Alt -- AltGr produces Ctrl+Alt,
/// never Ctrl+Alt+Win, so a European layout cannot reach this by typing.
fn tertiary_combo(key: &str) -> KeyCombo {
    KeyCombo {
        ctrl: true,
        ..default_combo(key)
    }
}

/// The default `config.toml` content, as a fully-populated [`BaseConfig`]
/// value rather than a hand-written string -- guarantees the generated
/// TOML and this crate's own schema/parsing never drift apart.
pub fn default_base_config() -> BaseConfig {
    let mut hotkeys = BTreeMap::new();
    hotkeys.insert(Command::SnapLeft, shifted_combo("LEFT"));
    hotkeys.insert(Command::SnapRight, shifted_combo("RIGHT"));
    hotkeys.insert(Command::SnapTop, shifted_combo("UP"));
    hotkeys.insert(Command::SnapBottom, shifted_combo("DOWN"));
    hotkeys.insert(Command::Rearrange, default_combo("E"));
    hotkeys.insert(Command::ToggleAutomaticTiling, default_combo("A"));
    hotkeys.insert(Command::ToggleFloating, default_combo("SPACE"));
    hotkeys.insert(Command::FocusLeft, default_combo("H"));
    hotkeys.insert(Command::FocusDown, default_combo("J"));
    hotkeys.insert(Command::FocusUp, default_combo("K"));
    hotkeys.insert(Command::FocusRight, default_combo("L"));
    hotkeys.insert(Command::SwapLeft, shifted_combo("H"));
    hotkeys.insert(Command::SwapDown, shifted_combo("J"));
    hotkeys.insert(Command::SwapUp, shifted_combo("K"));
    hotkeys.insert(Command::SwapRight, shifted_combo("L"));
    hotkeys.insert(Command::ResizeLeft, tertiary_combo("LEFT"));
    hotkeys.insert(Command::ResizeRight, tertiary_combo("RIGHT"));
    hotkeys.insert(Command::ResizeUp, tertiary_combo("UP"));
    hotkeys.insert(Command::ResizeDown, tertiary_combo("DOWN"));
    hotkeys.insert(Command::TogglePause, default_combo("P"));
    hotkeys.insert(Command::RestorePlacement, default_combo("Z"));
    hotkeys.insert(Command::ThrowNext, default_combo("N"));
    hotkeys.insert(Command::ThrowPrev, shifted_combo("N"));

    BaseConfig {
        version: CURRENT_VERSION,
        // One workspace, so a fresh install tiles exactly as it did before
        // workspaces existed. More are declared by the user, never
        // invented by the engine.
        workspaces: crate::schema::default_workspaces(),
        hotkeys,
        gaps: Gaps::default(),
        behavior: BehaviorSection::default(),
        focus_border: crate::schema::FocusBorderSection::default(),
        // No layouts ship as defaults: a saved layout describes a shape
        // one user chose, so Mosaix has nothing to guess at.
        layouts: BTreeMap::new(),
        // No rules ship as defaults either: the built-in rules in
        // `mosaix-rules` already cover what Mosaix should never manage,
        // and they are the low-priority fallback rather than config.
        rules: Vec::new(),
        workspace_switching: None,
    }
}

/// The default `config.toml` file content, written on first run and used
/// as the pre-config-existing default. Its own output round-trips through
/// [`crate::validate`] successfully.
pub fn default_config_content() -> String {
    toml::to_string_pretty(&default_base_config())
        .expect("default base config is always representable as TOML")
}

/// The in-memory fallback resolved config, used only when the config
/// directory can't be read or created at all on startup and there is no
/// last-known-good config to fall back to. Same bindings as
/// [`default_base_config`], but constructed directly with no TOML parsing
/// or disk access, so it stays available even if the TOML/filesystem layer
/// itself is what's broken.
pub fn fallback_config() -> ResolvedConfig {
    merge(&default_base_config(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::KeyCombo;

    #[test]
    fn default_bindings_use_win_alt_and_avoid_the_reserved_letters() {
        let base = default_base_config();

        assert_eq!(base.version, CURRENT_VERSION);
        // Win+Alt+Shift, not Win+Alt: Windows 11 reserves Win+Alt+Arrow.
        assert_eq!(
            base.hotkeys.get(&Command::SnapLeft),
            Some(&shifted_combo("LEFT"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapRight),
            Some(&shifted_combo("RIGHT"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapTop),
            Some(&shifted_combo("UP"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapBottom),
            Some(&shifted_combo("DOWN"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::ResizeLeft),
            Some(&tertiary_combo("LEFT"))
        );
        assert_eq!(base.gaps, Gaps::default());
        assert!(base.focus_border.enabled);
        assert_eq!(base.focus_border.color, crate::schema::RgbaColor::default());
        assert_eq!(base.focus_border.thickness, 2);
        // Not R: Xbox Game Bar holds Win+Alt+R system-wide, so
        // RegisterHotKey would simply fail for it on a stock Windows 11.
        assert_eq!(
            base.hotkeys.get(&Command::Rearrange),
            Some(&default_combo("E"))
        );
        // Not T, for the same reason -- Game Bar's recording timer.
        assert_eq!(
            base.hotkeys.get(&Command::ToggleAutomaticTiling),
            Some(&default_combo("A"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::FocusLeft),
            Some(&default_combo("H"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SwapRight),
            Some(&KeyCombo::parse("win+alt+shift+l").unwrap())
        );
        assert_eq!(base.behavior, BehaviorSection::default());
    }

    #[test]
    fn fallback_config_needs_no_toml_parsing_and_matches_the_defaults() {
        let fallback = fallback_config();

        assert_eq!(
            fallback.hotkeys.get(&Command::SnapLeft),
            Some(&KeyCombo::parse("win+alt+shift+left").unwrap())
        );
        assert_eq!(fallback.gaps, Gaps::default());
    }

    #[test]
    fn default_config_content_is_parseable_toml_containing_all_four_commands() {
        let content = default_config_content();
        let parsed: BaseConfig = toml::from_str(&content).expect("generated content must parse");

        assert_eq!(parsed, default_base_config());
    }

    /// Combinations a shipped default must never use, because the OS or a
    /// near-universal Microsoft component already owns them and
    /// `RegisterHotKey` refuses with ERROR_HOTKEY_ALREADY_REGISTERED. A
    /// binding that lands here does not warn at build time and does not
    /// warn at run time -- it is simply a key that does nothing.
    ///
    /// Kept as data rather than discovered by probing, so the check is
    /// deterministic and runs on any machine, including CI with no
    /// interactive desktop. Probing the live machine is a diagnostic, not
    /// a test: it answers a different question, and its answer changes
    /// with whatever the user happens to be running.
    const OS_RESERVED: &[&str] = &[
        // Windows 11 reserves the whole Win+Alt+Arrow set. This is what
        // shipped broken: all four snap commands silently did nothing.
        "win+alt+left",
        "win+alt+right",
        "win+alt+up",
        "win+alt+down",
        // Shell: snap, move-to-monitor, virtual desktop.
        "win+left",
        "win+right",
        "win+up",
        "win+down",
        "win+shift+left",
        "win+shift+right",
        "win+ctrl+left",
        "win+ctrl+right",
        "win+l",
        "win+d",
        "win+e",
        "win+r",
        "win+g",
        "win+tab",
        // Xbox Game Bar, present and enabled on a stock Windows 11.
        "win+alt+r",
        "win+alt+g",
        "win+alt+m",
        "win+alt+t",
        "win+alt+b",
        "win+alt+w",
    ];

    #[test]
    fn no_shipped_default_lands_on_a_combination_the_os_already_owns() {
        let base = default_base_config();
        let reserved: Vec<KeyCombo> = OS_RESERVED
            .iter()
            .map(|raw| KeyCombo::parse(raw).expect("the reserved table parses"))
            .collect();

        for (command, combo) in &base.hotkeys {
            assert!(
                !reserved.contains(combo),
                "default binding {command} is {combo}, which the OS already \
                 owns; RegisterHotKey refuses it and the command silently \
                 does nothing"
            );
        }
    }

    /// Ctrl+Alt is AltGr. A European layout reaches it by typing an
    /// ordinary character, so no default may sit there -- which is the
    /// whole reason the table moved off Ctrl+Alt in the first place.
    ///
    /// Win+Ctrl+Alt is exempt and is where the resize bindings live:
    /// AltGr produces Ctrl+Alt, never Ctrl+Alt+Win.
    #[test]
    fn no_shipped_default_is_reachable_as_altgr() {
        for (command, combo) in &default_base_config().hotkeys {
            assert!(
                !(combo.ctrl && combo.alt && !combo.win),
                "default binding {command} is {combo}, which is AltGr plus a key"
            );
        }
    }

    /// Two commands on one combination means one of them never fires:
    /// the second registration is refused. `validate` rejects this in a
    /// user's file, so the shipped table must not contain it either.
    #[test]
    fn no_two_shipped_defaults_share_a_combination() {
        let base = default_base_config();
        let mut seen: BTreeMap<String, &Command> = BTreeMap::new();

        for (command, combo) in &base.hotkeys {
            if let Some(first) = seen.insert(combo.to_string(), command) {
                panic!("{first} and {command} are both bound to {combo}");
            }
        }
        assert_eq!(seen.len(), base.hotkeys.len());
    }

}
