//! Default config generation and the startup-failure fallback (ADR 0007).
//!
//! The bindings here (Ctrl+Alt+Arrow) are what a first-run generated
//! `config.toml` and the in-memory fallback both reproduce -- the same
//! defaults `mosaix-agent`'s now-deleted hardcoded `keybindings.rs` table
//! used to own directly, before config-driven hotkeys replaced it (ADR
//! 0003, superseded by ADR 0005).

use std::collections::BTreeMap;

use mosaix_domain::Gaps;

use crate::schema::{
    BaseConfig, BehaviorSection, Command, FocusBorderSection, KeyCombo, ResolvedConfig,
    CURRENT_VERSION,
};
use crate::validate::merge;

fn arrow_combo(key: &str) -> KeyCombo {
    KeyCombo {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        key: key.to_string(),
    }
}

fn shifted_combo(key: &str) -> KeyCombo {
    KeyCombo {
        shift: true,
        ..arrow_combo(key)
    }
}

/// The default `config.toml` content, as a fully-populated [`BaseConfig`]
/// value rather than a hand-written string -- guarantees the generated
/// TOML and this crate's own schema/parsing never drift apart.
pub fn default_base_config() -> BaseConfig {
    let mut hotkeys = BTreeMap::new();
    hotkeys.insert(Command::SnapLeft, arrow_combo("LEFT"));
    hotkeys.insert(Command::SnapRight, arrow_combo("RIGHT"));
    hotkeys.insert(Command::SnapTop, arrow_combo("UP"));
    hotkeys.insert(Command::SnapBottom, arrow_combo("DOWN"));
    hotkeys.insert(Command::Rearrange, arrow_combo("R"));
    hotkeys.insert(Command::ToggleAutomaticTiling, arrow_combo("T"));
    hotkeys.insert(Command::ToggleFloating, arrow_combo("SPACE"));
    hotkeys.insert(Command::FocusLeft, arrow_combo("H"));
    hotkeys.insert(Command::FocusDown, arrow_combo("J"));
    hotkeys.insert(Command::FocusUp, arrow_combo("K"));
    hotkeys.insert(Command::FocusRight, arrow_combo("L"));
    hotkeys.insert(Command::SwapLeft, shifted_combo("H"));
    hotkeys.insert(Command::SwapDown, shifted_combo("J"));
    hotkeys.insert(Command::SwapUp, shifted_combo("K"));
    hotkeys.insert(Command::SwapRight, shifted_combo("L"));
    hotkeys.insert(Command::TogglePause, arrow_combo("P"));

    BaseConfig {
        version: CURRENT_VERSION,
        hotkeys,
        gaps: Gaps::default(),
        behavior: BehaviorSection::default(),
        focus_border: FocusBorderSection::default(),
    }
}

/// The default `config.toml` file content, written on first run (ticket
/// 03) and used as the pre-config-existing default (ticket 06). Its own
/// output round-trips through [`crate::validate`] successfully.
pub fn default_config_content() -> String {
    toml::to_string_pretty(&default_base_config())
        .expect("default base config is always representable as TOML")
}

/// The in-memory fallback resolved config, used only when the config
/// directory can't be read or created at all on startup and there is no
/// last-known-good config to fall back to (ADR 0007). Same bindings as
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
    fn default_bindings_mirror_the_current_hardcoded_ctrl_alt_arrow_table() {
        let base = default_base_config();

        assert_eq!(base.version, CURRENT_VERSION);
        assert_eq!(
            base.hotkeys.get(&Command::SnapLeft),
            Some(&arrow_combo("LEFT"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapRight),
            Some(&arrow_combo("RIGHT"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapTop),
            Some(&arrow_combo("UP"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SnapBottom),
            Some(&arrow_combo("DOWN"))
        );
        assert_eq!(base.gaps, Gaps::default());
        assert_eq!(
            base.hotkeys.get(&Command::Rearrange),
            Some(&arrow_combo("R"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::FocusLeft),
            Some(&arrow_combo("H"))
        );
        assert_eq!(
            base.hotkeys.get(&Command::SwapRight),
            Some(&KeyCombo::parse("ctrl+alt+shift+l").unwrap())
        );
        assert_eq!(base.behavior, BehaviorSection::default());
    }

    #[test]
    fn fallback_config_needs_no_toml_parsing_and_matches_the_defaults() {
        let fallback = fallback_config();

        assert_eq!(
            fallback.hotkeys.get(&Command::SnapLeft),
            Some(&KeyCombo::parse("ctrl+alt+left").unwrap())
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
