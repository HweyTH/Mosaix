//! TOML schema types for base config (`config.toml`) and profile overlays
//! (`profiles/*.toml`), plus the merged [`ResolvedConfig`] shape they
//! produce (CONTEXT.md "Base config"/"Profile"/"Resolved config").
//!
//! Base config is the only place `version` is stamped -- a profile has no
//! version of its own, it's an overlay validated against whatever base
//! config it's paired with (ADR 0004).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use mosaix_domain::Gaps;

/// The `version` value this build of `mosaix-config` understands. Any other
/// value (including a missing field, which fails to parse rather than
/// defaulting) is a validation error -- no lenient guessing (ADR 0007).
pub const CURRENT_VERSION: u32 = 1;

/// A zone-snap command a hotkey can be bound to. Mirrors
/// [`mosaix_engine::ZoneSnapDirection`]'s four variants, but is defined
/// here rather than depending on `mosaix-engine` (ADR 0005: `mosaix-config`
/// has no dependency back on `mosaix-engine`, only the reverse).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Command {
    SnapLeft,
    SnapRight,
    SnapTop,
    SnapBottom,
}

/// A parsed hotkey combination: a set of modifiers plus a key name.
///
/// Serializes to and deserializes from a single `"ctrl+alt+left"`-style
/// string (see [`KeyCombo::parse`]) rather than a nested table, so a
/// `[hotkeys]` section reads as `snap-left = "ctrl+alt+left"`. `key` is
/// stored canonicalized to uppercase so two differently-cased spellings of
/// the same combo compare equal (duplicate-binding detection depends on
/// this).
///
/// Deliberately platform-neutral: unlike
/// `mosaix_platform_windows::HotkeyBinding`, there is no virtual-key code
/// here. Translating a key name to a platform key code is a platform
/// adapter's job, not this crate's (`mosaix-config` is meant to be shared
/// across platforms the way `mosaix-platform-windows`/`mosaix-platform-macos`
/// aren't).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: String,
}

impl KeyCombo {
    /// Parses a `+`-separated combo string (e.g. `"ctrl+alt+left"`,
    /// case-insensitive, whitespace around `+` ignored). The last segment
    /// is the key; every other segment must be a recognized modifier name
    /// (`ctrl`/`control`, `alt`, `shift`, `win`/`super`/`meta`/`windows`).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let segments: Vec<&str> = raw.split('+').map(str::trim).collect();
        if segments.iter().any(|segment| segment.is_empty()) {
            return Err(format!("invalid key combo {raw:?}: empty segment"));
        }
        let Some((key, modifiers)) = segments.split_last() else {
            return Err(format!("invalid key combo {raw:?}: no key given"));
        };

        let mut combo = KeyCombo {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: key.to_ascii_uppercase(),
        };
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => combo.ctrl = true,
                "alt" => combo.alt = true,
                "shift" => combo.shift = true,
                "win" | "super" | "meta" | "windows" => combo.win = true,
                other => {
                    return Err(format!(
                        "invalid key combo {raw:?}: unrecognized modifier {other:?}"
                    ))
                }
            }
        }
        Ok(combo)
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("ctrl".to_string());
        }
        if self.alt {
            parts.push("alt".to_string());
        }
        if self.shift {
            parts.push("shift".to_string());
        }
        if self.win {
            parts.push("win".to_string());
        }
        parts.push(self.key.to_ascii_lowercase());
        write!(f, "{}", parts.join("+"))
    }
}

impl Serialize for KeyCombo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for KeyCombo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        KeyCombo::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// The `[behavior]` table. Empty-but-valid today -- no behavior flags are
/// implemented yet (adding one means adding the engine concept it controls
/// first, out of this ticket's scope). `deny_unknown_fields` so a typo'd or
/// speculative flag name is rejected rather than silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorSection {}

/// One normalized cell in a named saved layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutCell {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A reusable shape whose cells are filled in visual window order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedLayout {
    pub name: String,
    pub cells: Vec<LayoutCell>,
}

/// Base config: `config.toml`'s full schema (CONTEXT.md "Base config").
/// Applies whenever the current display topology matches no saved profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseConfig {
    pub version: u32,
    #[serde(default)]
    pub hotkeys: BTreeMap<Command, KeyCombo>,
    #[serde(default)]
    pub gaps: Gaps,
    #[serde(default)]
    pub behavior: BehaviorSection,
    #[serde(default)]
    pub layouts: Vec<SavedLayout>,
}

/// A sparse `outer`/`inner` override, letting a profile override just one
/// of `Gaps`'s two fields while inheriting the other from base config
/// (field-level merge, ADR 0004 -- applied down to `Gaps`'s own leaf
/// fields, not just whole sections).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapsOverride {
    pub outer: Option<i32>,
    pub inner: Option<i32>,
}

/// A profile overlay: a file under `profiles/` (CONTEXT.md "Profile"). All
/// fields are optional except `fingerprint`, the match key compared
/// against `mosaix_domain::topology_fingerprint()`'s current output (ADR
/// 0004) -- never the filename. Any field a profile doesn't set falls
/// through to base config when merged ([`crate::merge`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub fingerprint: String,
    #[serde(default)]
    pub hotkeys: BTreeMap<Command, KeyCombo>,
    #[serde(default)]
    pub gaps: GapsOverride,
    #[serde(default)]
    pub behavior: BehaviorSection,
    #[serde(default)]
    pub layouts: Option<Vec<SavedLayout>>,
}

/// The merged result of base config plus (optionally) one profile
/// (CONTEXT.md "Resolved config") -- the actual settings in effect for one
/// topology. What [`crate::merge`] produces and what
/// `Event::ConfigChanged` (ADR 0005) will eventually carry into
/// `EngineState`.
///
/// Derives `Default` (empty hotkeys, default `Gaps`, empty `[behavior]`) so
/// `EngineState` can derive `Default` too, mirroring how an empty
/// `displays: Vec::new()` stands in for "nothing observed yet" -- not a
/// value [`crate::validate`] itself would ever produce.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedConfig {
    pub hotkeys: BTreeMap<Command, KeyCombo>,
    pub gaps: Gaps,
    pub behavior: BehaviorSection,
    pub layouts: Vec<SavedLayout>,
}

/// One profile's resolved settings, paired with the `fingerprint` it's
/// matched against. Runtime profile selection (ticket 03/04's job, not
/// this crate's `validate`) picks among these by comparing
/// `topology_fingerprint()` to `fingerprint`, falling back to
/// [`ResolvedConfigSet::base`] when nothing matches.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    pub fingerprint: String,
    pub config: ResolvedConfig,
}

/// The full result of validating one config directory (ADR 0007): base
/// config resolved on its own, plus every profile resolved against that
/// same base. Each entry already passed its own duplicate-binding check
/// ([`crate::validate`]) -- selecting among them by topology is the only
/// step left to the caller.
///
/// Derives `Default` (base config's own default, no profiles) for the same
/// "nothing loaded yet" reason [`ResolvedConfig`] does -- not a value
/// [`crate::validate`] itself would ever produce.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedConfigSet {
    pub base: ResolvedConfig,
    pub profiles: Vec<ResolvedProfile>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_combo_parses_modifiers_and_key_case_insensitively() {
        let combo = KeyCombo::parse("Ctrl+Alt+Left").unwrap();

        assert_eq!(
            combo,
            KeyCombo {
                ctrl: true,
                alt: true,
                shift: false,
                win: false,
                key: "LEFT".to_string(),
            }
        );
    }

    #[test]
    fn key_combo_accepts_modifier_aliases() {
        let combo = KeyCombo::parse("control+super+shift+a").unwrap();

        assert_eq!(
            combo,
            KeyCombo {
                ctrl: true,
                alt: false,
                shift: true,
                win: true,
                key: "A".to_string(),
            }
        );
    }

    #[test]
    fn key_combo_with_no_modifiers_is_just_the_key() {
        assert_eq!(
            KeyCombo::parse("F5").unwrap(),
            KeyCombo {
                ctrl: false,
                alt: false,
                shift: false,
                win: false,
                key: "F5".to_string(),
            }
        );
    }

    #[test]
    fn key_combo_rejects_an_unrecognized_modifier() {
        assert!(KeyCombo::parse("hyper+left").is_err());
    }

    #[test]
    fn key_combo_rejects_empty_segments() {
        assert!(KeyCombo::parse("ctrl++left").is_err());
        assert!(KeyCombo::parse("").is_err());
    }

    #[test]
    fn key_combo_display_round_trips_through_parse() {
        let combo = KeyCombo::parse("Shift+Ctrl+Alt+Win+Left").unwrap();
        let rendered = combo.to_string();

        assert_eq!(rendered, "ctrl+alt+shift+win+left");
        assert_eq!(KeyCombo::parse(&rendered).unwrap(), combo);
    }

    #[test]
    fn differently_cased_combos_compare_equal() {
        assert_eq!(
            KeyCombo::parse("CTRL+ALT+LEFT").unwrap(),
            KeyCombo::parse("ctrl+alt+left").unwrap()
        );
    }
}
