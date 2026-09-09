//! Default config generation and the startup-failure fallback.
//!
//! The bindings here (Win+Alt) are what a first-run generated
//! `config.toml` and the in-memory fallback both reproduce.
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

/// The default `config.toml` content, as a fully-populated [`BaseConfig`]
/// value rather than a hand-written string -- guarantees the generated
/// TOML and this crate's own schema/parsing never drift apart.
pub fn default_base_config() -> BaseConfig {
    let mut hotkeys = BTreeMap::new();
    hotkeys.insert(Command::SnapLeft, default_combo("LEFT"));
    hotkeys.insert(Command::SnapRight, default_combo("RIGHT"));
    hotkeys.insert(Command::SnapTop, default_combo("UP"));
    hotkeys.insert(Command::SnapBottom, default_combo("DOWN"));
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
    hotkeys.insert(Command::ResizeLeft, shifted_combo("LEFT"));
    hotkeys.insert(Command::ResizeRight, shifted_combo("RIGHT"));
    hotkeys.insert(Command::ResizeUp, shifted_combo("UP"));
    hotkeys.insert(Command::ResizeDown, shifted_combo("DOWN"));
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
        assert_eq!(
            base.hotkeys.get(&Command::SnapLeft),
            Some(&default_combo("LEFT"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapRight),
            Some(&default_combo("RIGHT"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapTop),
            Some(&default_combo("UP"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapBottom),
            Some(&default_combo("DOWN"))
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
            Some(&KeyCombo::parse("win+alt+left").unwrap())
        );
        assert_eq!(fallback.gaps, Gaps::default());
    }

    #[test]
    fn default_config_content_is_parseable_toml_containing_all_four_commands() {
        let content = default_config_content();
        let parsed: BaseConfig = toml::from_str(&content).expect("generated content must parse");

        assert_eq!(parsed, default_base_config());
    }
}
