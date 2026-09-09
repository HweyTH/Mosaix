//! TOML schema types for base config (`config.toml`) and profile overlays
//! (`profiles/*.toml`), plus the merged [`ResolvedConfig`] shape they
//! produce.
//!
//! Base config is the only place `version` is stamped -- a profile has no
//! version of its own, it's an overlay validated against whatever base
//! config it's paired with.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use mosaix_rules::RuleConfig;
use mosaix_domain::{Gaps, NormalizedRect, WorkspaceName};

/// The base config file's name, and the name validation errors and
/// provenance use to refer to it. Base config is one fixed file, unlike a
/// profile, whose filename is whatever the user called it.
pub const BASE_CONFIG_FILE_NAME: &str = "config.toml";

/// The `version` value this build of `mosaix-config` understands. Any
/// other value (including a missing field, which fails to parse rather
/// than defaulting) is a validation error -- no lenient guessing.
pub const CURRENT_VERSION: u32 = 1;

/// A command a hotkey can be bound to. Defined here rather than in
/// `mosaix-engine`, which `mosaix-config` deliberately does not depend on,
/// so the four zone-snap variants mirror
/// [`mosaix_engine::ZoneSnapDirection`] rather than reusing it.
///
/// Twenty-three unit verbs, plus two that carry a payload. A saved layout is
/// named by the user, so a binding to one has to name a string the schema
/// cannot know in advance, and [`Command::ApplyLayout`] is where that
/// string lives. Bindings stay a single keyspace: `ApplyLayout { name:
/// "writing" }` and `ApplyLayout { name: "code" }` are two distinct keys
/// of one `BTreeMap`, so merge, diff, and duplicate-binding detection keep
/// operating on one set.
///
/// Deliberately not `Copy`: the payload owns a `String`.
///
/// Serde derives nothing here. A unit verb is written flat
/// (`snap-left = "ctrl+alt+left"`) and a parameterized one nested under
/// its verb (`[hotkeys.apply-layout]`), which is one map with two value
/// shapes; the crate-private `hotkey_bindings` module is where that map is
/// read and written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Command {
    SnapLeft,
    SnapRight,
    SnapTop,
    SnapBottom,
    Rearrange,
    ToggleAutomaticTiling,
    ToggleFloating,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    SwapLeft,
    SwapRight,
    SwapUp,
    SwapDown,
    /// Move the nearest container-tree divider facing that way by five
    /// percentage points.
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    TogglePause,
    /// Return the focused window to the display and bounds it had before
    /// Mosaix last placed it. One step, not the persistent undo history.
    RestorePlacement,
    /// Move the focused window to the next display, keeping its position
    /// and size as a fraction of that display's work area.
    ThrowNext,
    /// The same, to the previous display.
    ThrowPrev,
    /// Apply the saved layout called `name` to the focused window's
    /// display. Ordered last so every layout binding sorts after every
    /// unit verb, which is what lets the serializer emit the nested
    /// `apply-layout` table after the flat entries -- TOML requires every
    /// table to follow the scalars of the table containing it.
    ApplyLayout {
        name: String,
    },
    /// Display the logical workspace called `name` on the focused display,
    /// or focus it where it is already displayed. The second parameterized
    /// command, written the same way as `apply-layout`:
    /// `[hotkeys.focus-workspace]` with one entry per workspace name.
    FocusWorkspace {
        name: String,
    },
}

/// The TOML verb naming a parameterized command's table.
const APPLY_LAYOUT_VERB: &str = "apply-layout";
const FOCUS_WORKSPACE_VERB: &str = "focus-workspace";

impl Command {
    /// The verb this command is written as in a `[hotkeys]` table. For
    /// [`Command::ApplyLayout`] that is the *table* name, not the whole
    /// binding -- the layout name is the key inside it.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::SnapLeft => "snap-left",
            Self::SnapRight => "snap-right",
            Self::SnapTop => "snap-top",
            Self::SnapBottom => "snap-bottom",
            Self::Rearrange => "rearrange",
            Self::ToggleAutomaticTiling => "toggle-automatic-tiling",
            Self::ToggleFloating => "toggle-floating",
            Self::FocusLeft => "focus-left",
            Self::FocusRight => "focus-right",
            Self::FocusUp => "focus-up",
            Self::FocusDown => "focus-down",
            Self::SwapLeft => "swap-left",
            Self::SwapRight => "swap-right",
            Self::SwapUp => "swap-up",
            Self::SwapDown => "swap-down",
            Self::ResizeLeft => "resize-left",
            Self::ResizeRight => "resize-right",
            Self::ResizeUp => "resize-up",
            Self::ResizeDown => "resize-down",
            Self::TogglePause => "toggle-pause",
            Self::RestorePlacement => "restore-placement",
            Self::ThrowNext => "throw-next",
            Self::ThrowPrev => "throw-prev",
            Self::ApplyLayout { .. } => APPLY_LAYOUT_VERB,
            Self::FocusWorkspace { .. } => FOCUS_WORKSPACE_VERB,
        }
    }

    /// Every unit verb, in declaration order.
    ///
    /// [`Command::unit_from_verb`] reads names off this list through
    /// [`Command::verb`] rather than repeating them, so the spelling of a
    /// verb lives in exactly one place and the two directions cannot drift
    /// apart -- which is what the serde derive used to guarantee for free.
    pub fn unit_verbs() -> [Self; 23] {
        [
            Self::SnapLeft,
            Self::SnapRight,
            Self::SnapTop,
            Self::SnapBottom,
            Self::Rearrange,
            Self::ToggleAutomaticTiling,
            Self::ToggleFloating,
            Self::FocusLeft,
            Self::FocusRight,
            Self::FocusUp,
            Self::FocusDown,
            Self::SwapLeft,
            Self::SwapRight,
            Self::SwapUp,
            Self::SwapDown,
            Self::ResizeLeft,
            Self::ResizeRight,
            Self::ResizeUp,
            Self::ResizeDown,
            Self::TogglePause,
            Self::RestorePlacement,
            Self::ThrowNext,
            Self::ThrowPrev,
        ]
    }

    /// The unit-verb command `verb` names.
    ///
    /// `None` for anything else, including [`APPLY_LAYOUT_VERB`]: that
    /// verb is a table, and a command cannot be built from it without the
    /// layout name inside. Callers turn a `None` into the load-time
    /// "unknown command" rejection that names the file and line.
    ///
    /// A linear scan of twenty-three, run once per binding at config load.
    fn unit_from_verb(verb: &str) -> Option<Self> {
        Self::unit_verbs()
            .into_iter()
            .find(|command| command.verb() == verb)
    }

    /// The command a TOML path names -- the inverse of the [`fmt::Display`]
    /// spelling, so a client that has only the string a validation error
    /// or the state snapshot gave it can name the same command back.
    ///
    /// `None` for a path no verb matches, which keeps an unknown command
    /// a refusal rather than something silently misread. A layout name may
    /// itself contain dots (`apply-layout.deep.work`), so only the first
    /// separator divides verb from payload.
    pub fn parse(path: &str) -> Option<Self> {
        match path.split_once('.') {
            Some((APPLY_LAYOUT_VERB, name)) if !name.is_empty() => Some(Self::ApplyLayout {
                name: name.to_owned(),
            }),
            Some((FOCUS_WORKSPACE_VERB, name)) if !name.is_empty() => Some(Self::FocusWorkspace {
                name: name.to_owned(),
            }),
            Some(_) => None,
            None => Self::unit_from_verb(path),
        }
    }
}

impl fmt::Display for Command {
    /// The command's TOML path, so a validation error points at what the
    /// user actually typed: `snap-left`, or `apply-layout.writing`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApplyLayout { name } => write!(f, "{APPLY_LAYOUT_VERB}.{name}"),
            Self::FocusWorkspace { name } => write!(f, "{FOCUS_WORKSPACE_VERB}.{name}"),
            unit => f.write_str(unit.verb()),
        }
    }
}

/// Reads and writes a `[hotkeys]` table: one map holding both flat unit
/// bindings and the nested `apply-layout` table.
///
/// ```toml
/// [hotkeys]
/// snap-left = "ctrl+alt+left"
///
/// [hotkeys.apply-layout]
/// writing = "ctrl+alt+1"
/// ```
///
/// Written by hand rather than derived because the two value shapes -- a
/// combo string for a unit verb, a table of layout-name-to-combo for the
/// parameterized one -- cannot both come out of one derived map. The
/// payoff is that an unrecognized verb is still a deserialization error,
/// so TOML attaches the file and line to it.
pub(crate) mod hotkey_bindings {
    use std::collections::BTreeMap;
    use std::fmt;

    use serde::de::{Error as _, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};

    use super::{Command, KeyCombo, APPLY_LAYOUT_VERB, FOCUS_WORKSPACE_VERB};

    pub fn serialize<S>(
        bindings: &BTreeMap<Command, KeyCombo>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut units: Vec<(&'static str, &KeyCombo)> = Vec::new();
        let mut layouts: BTreeMap<&str, &KeyCombo> = BTreeMap::new();
        let mut workspaces: BTreeMap<&str, &KeyCombo> = BTreeMap::new();
        for (command, combo) in bindings {
            match command {
                Command::ApplyLayout { name } => {
                    layouts.insert(name.as_str(), combo);
                }
                Command::FocusWorkspace { name } => {
                    workspaces.insert(name.as_str(), combo);
                }
                unit => units.push((unit.verb(), combo)),
            }
        }

        // Every flat entry first, the nested tables last: TOML cannot emit
        // a scalar after a table in the same parent.
        let tables = usize::from(!layouts.is_empty()) + usize::from(!workspaces.is_empty());
        let mut map = serializer.serialize_map(Some(units.len() + tables))?;
        for (verb, combo) in units {
            map.serialize_entry(verb, combo)?;
        }
        if !layouts.is_empty() {
            map.serialize_entry(APPLY_LAYOUT_VERB, &layouts)?;
        }
        if !workspaces.is_empty() {
            map.serialize_entry(FOCUS_WORKSPACE_VERB, &workspaces)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<Command, KeyCombo>, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// A seed that always fails, naming the verb it was handed.
        ///
        /// Rejecting the *key* would make TOML blame the `[hotkeys]`
        /// header; consuming the value and failing inside the value's own
        /// deserializer puts the reported line on what the user actually
        /// mistyped.
        struct UnknownVerb<'a>(&'a str);

        impl<'de> serde::de::DeserializeSeed<'de> for UnknownVerb<'_> {
            type Value = std::convert::Infallible;

            fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                // Every `visit_*` is left at its default, which errors,
                // so any value shape lands in the `map_err` below with
                // TOML's span for this line already attached.
                struct Fail;
                impl<'de> Visitor<'de> for Fail {
                    type Value = std::convert::Infallible;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a hotkey binding for a known command")
                    }
                }
                deserializer
                    .deserialize_any(Fail)
                    .map_err(|_| D::Error::custom(format!("unknown hotkey command {:?}", self.0)))
            }
        }

        struct Bindings;

        impl<'de> Visitor<'de> for Bindings {
            type Value = BTreeMap<Command, KeyCombo>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of hotkey bindings")
            }

            fn visit_map<M>(self, mut entries: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut bindings = BTreeMap::new();
                while let Some(verb) = entries.next_key::<String>()? {
                    if verb == APPLY_LAYOUT_VERB {
                        for (name, combo) in entries.next_value::<BTreeMap<String, KeyCombo>>()? {
                            bindings.insert(Command::ApplyLayout { name }, combo);
                        }
                        continue;
                    }
                    if verb == FOCUS_WORKSPACE_VERB {
                        for (name, combo) in entries.next_value::<BTreeMap<String, KeyCombo>>()? {
                            bindings.insert(Command::FocusWorkspace { name }, combo);
                        }
                        continue;
                    }
                    let Some(command) = Command::unit_from_verb(&verb) else {
                        return Err(entries
                            .next_value_seed(UnknownVerb(&verb))
                            .expect_err("UnknownVerb never deserializes"));
                    };
                    bindings.insert(command, entries.next_value()?);
                }
                Ok(bindings)
            }
        }

        deserializer.deserialize_map(Bindings)
    }
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
/// implemented yet, since adding one means adding the engine concept it
/// controls first. `deny_unknown_fields` so a typo'd or speculative flag
/// name is rejected rather than silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorSection {}

/// Automatic-tiling activation is intentionally available only to a
/// topology profile, never base config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticTilingSection {
    pub enabled: bool,
    /// Which arrangement automatic tiling produces. Omitted means
    /// [`TilingMode::Balanced`], so an existing profile keeps the layout
    /// it has always produced.
    #[serde(default)]
    pub mode: TilingMode,
}

/// A profile's request for experimental workspace switching. Only a
/// topology profile may carry it: base config applies to every unmatched
/// topology, and parking must never activate merely because a user docked.
///
/// ```toml
/// [workspace_switching]
/// experimental = true
///
/// [workspace_switching.displayed]
/// "\\.\DISPLAY1|1920x1080|scale=1" = "dev"
/// "\\.\DISPLAY2|2560x1440|scale=1" = "chat"
/// ```
///
/// `displayed` maps every display of the profile's topology, by the
/// stable fingerprint `mosaix state` reports, to a distinct declared
/// workspace. A mapping that misses a display, repeats a workspace,
/// names an unknown one, or names a display the topology lacks rejects
/// the whole directory; the engine never invents a name to complete it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSwitchingSection {
    /// Whether switching is requested. `false` keeps the mapping
    /// validated but inert.
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub displayed: BTreeMap<String, String>,
}

/// A profile's switching request as merged into a resolved config: the
/// same shape with every workspace name validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspaceSwitching {
    pub experimental: bool,
    pub displayed: BTreeMap<String, WorkspaceName>,
}

/// How automatic tiling arranges a display's windows.
///
/// The first tree release deliberately offers one tree policy. Stack,
/// monocle, and the rest stay out of this enum until they exist, so a
/// configuration file cannot name a mode that does nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TilingMode {
    /// The stateless aspect-aware grid. Recomputed from scratch each time,
    /// so it forgets any structure the user shaped.
    #[default]
    Balanced,
    /// A persistent tree of weighted horizontal and vertical splits, with
    /// deterministic BSP insertion.
    Tree,
}

impl TilingMode {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Tree => "tree",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Default for RgbaColor {
    fn default() -> Self {
        Self {
            red: 0,
            green: 120,
            blue: 215,
            alpha: 255,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FocusBorderSection {
    pub enabled: bool,
    pub color: RgbaColor,
    pub thickness: u16,
}

impl Default for FocusBorderSection {
    fn default() -> Self {
        Self {
            enabled: true,
            color: RgbaColor::default(),
            thickness: 2,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusBorderOverride {
    pub enabled: Option<bool>,
    pub color: Option<RgbaColor>,
    pub thickness: Option<u16>,
}

/// A saved layout: shape only, never window identity. Applying one lays
/// its `cells` over a display's work area and fills them with whichever
/// managed windows are there, in visual window order.
///
/// Cells are [`NormalizedRect`]s -- fractions of a display's work area,
/// the same form zones already use, and the form that type's own docs call
/// the persisted-zone format. Reusing it rather than declaring a
/// config-local twin follows [`Gaps`], which this schema also takes
/// straight from `mosaix-domain`.
///
/// A table rather than a bare cell list so the array-of-tables spelling
/// (`[[layouts.writing.cells]]`) is available to a hand-editing user, and
/// so per-layout settings have somewhere to go.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedLayout {
    #[serde(default)]
    pub cells: Vec<NormalizedRect>,
}

/// Base config: `config.toml`'s full schema. Applies whenever the current
/// display topology matches no saved profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseConfig {
    pub version: u32,
    /// The logical workspaces this configuration declares. Configuration
    /// is one of the two ways a workspace comes to exist, and this is the
    /// declaration. Omitted means one workspace called `main`, so a
    /// configuration file written before workspaces existed keeps one
    /// workspace per display it can fill.
    ///
    /// A scalar array, so it sits with `version` ahead of every table.
    #[serde(default = "default_workspaces")]
    pub workspaces: Vec<String>,
    #[serde(default, with = "hotkey_bindings")]
    pub hotkeys: BTreeMap<Command, KeyCombo>,
    #[serde(default)]
    pub gaps: Gaps,
    #[serde(default)]
    pub behavior: BehaviorSection,
    #[serde(default)]
    pub focus_border: FocusBorderSection,
    /// Named saved layouts, keyed by the name a binding or a CLI command
    /// refers to. Serialized last because TOML puts every table after the
    /// scalar fields of the table containing it.
    #[serde(default)]
    pub layouts: BTreeMap<String, SavedLayout>,
    /// Ordered window rules, each an `[[rules]]` entry. Precedence is by
    /// each rule's own `priority`, not by position, so the order here is
    /// only the order the file happens to list them in.
    ///
    /// Serialized after `layouts` because TOML puts every table -- and
    /// every array of tables -- after the scalar fields of the table
    /// containing it, and skipped when empty so a generated `config.toml`
    /// does not grow a stray `[[rules]]` header.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RuleConfig>,
    /// Present only so a switching request written into base config is
    /// refused with a reason that says where it belongs, rather than as
    /// an unknown field. Never written.
    #[serde(default, skip_serializing)]
    pub workspace_switching: Option<WorkspaceSwitchingSection>,
}

/// A sparse `outer`/`inner` override, letting a profile override just one
/// of `Gaps`'s two fields while inheriting the other from base config. The
/// field-level merge reaches `Gaps`'s own leaf fields, not just whole
/// sections.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapsOverride {
    pub outer: Option<i32>,
    pub inner: Option<i32>,
}

/// A profile overlay: a file under `profiles/`. All fields are optional
/// except `fingerprint`, the match key compared against
/// `mosaix_domain::topology_fingerprint()`'s current output -- never the
/// filename. Any field a profile doesn't set falls through to base config
/// when merged ([`crate::merge`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub fingerprint: String,
    /// Workspaces this profile declares in addition to base config's. A
    /// name base config already declares is the same workspace, not a
    /// second one. Skipped when empty for the same reason `layouts` is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    #[serde(default, with = "hotkey_bindings")]
    pub hotkeys: BTreeMap<Command, KeyCombo>,
    #[serde(default)]
    pub gaps: GapsOverride,
    #[serde(default)]
    pub behavior: BehaviorSection,
    pub automatic_tiling: Option<AutomaticTilingSection>,
    #[serde(default)]
    pub focus_border: FocusBorderOverride,
    /// A sparse saved-layout override. A layout the profile declares
    /// replaces base config's layout of that name; a layout it does not
    /// mention falls through to base config, the same field-level merge
    /// every other profile field gets. There is no way to *remove* a base
    /// layout from a profile, matching the rest of the overlay: a profile
    /// adds and overrides, it never subtracts.
    ///
    /// Serialized last, and skipped entirely when empty, for the reason
    /// base config's `layouts` is serialized last -- TOML puts every table
    /// after the scalars of the table containing it, and a profile that
    /// carries no layouts should not grow an empty `[layouts]` header when
    /// the settings application rewrites it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, SavedLayout>,
    /// The experimental switching request for this topology, if any.
    /// Serialized after `layouts`, and skipped when absent, for the same
    /// TOML ordering reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_switching: Option<WorkspaceSwitchingSection>,
    /// Present only so rules written into a profile are refused with a
    /// reason that says where they belong, rather than as an unknown
    /// field. Never written -- the exact mirror of base config's
    /// `workspace_switching`.
    ///
    /// Rules are global because a window's management decision must not
    /// change under it when a monitor is unplugged.
    #[serde(default, skip_serializing)]
    pub rules: Vec<RuleConfig>,
}

/// What `workspaces` means when a base config does not mention it.
pub fn default_workspaces() -> Vec<String> {
    vec![DEFAULT_WORKSPACE_NAME.to_owned()]
}

/// The one workspace a configuration that names none is taken to declare.
pub const DEFAULT_WORKSPACE_NAME: &str = "main";

/// Which configuration layer a resolved value came from.
///
/// A merge result reads the same whichever file supplied it, so this is
/// the only record of where a value originated -- and the settings
/// application needs it, because a binding edited there is written to the
/// layer that currently supplies it. Whenever a profile is matched, the
/// value on screen and the file that would receive a write are different
/// objects, and the user has to be told which.
///
/// Carries no filename: profiles are matched by fingerprint, never by
/// filename, so the file is a fact `validate` knows and `merge` does not.
/// It is recorded once per resolved config, in
/// [`ResolvedConfig::profile_file`], rather than repeated per binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigLayer {
    /// `config.toml`.
    Base,
    /// The matched topology profile, named by
    /// [`ResolvedConfig::profile_file`].
    Profile,
}

/// Whether two saved-layout names are the same layout to a user.
///
/// Case-insensitive, because whole-directory validation rejects two
/// layouts whose names differ only by case: `writing` and `Writing` are
/// two map entries but one layout, and a binding naming either would be
/// ambiguous. The single definition matters -- validation and the
/// pre-write check in [`crate::edit_layouts`] both ask this question, and
/// answering it two different ways would let a name through one and not
/// the other.
pub fn layout_names_collide(first: &str, second: &str) -> bool {
    first.to_lowercase() == second.to_lowercase()
}

/// The merged result of base config plus (optionally) one profile -- the
/// actual settings in effect for one topology. What [`crate::merge`]
/// produces and what `Event::ConfigChanged` will eventually carry into
/// `EngineState`.
///
/// Derives `Default` (empty hotkeys, default `Gaps`, empty `[behavior]`) so
/// `EngineState` can derive `Default` too, mirroring how an empty
/// `displays: Vec::new()` stands in for "nothing observed yet" -- not a
/// value [`crate::validate`] itself would ever produce.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedConfig {
    /// Every workspace configuration declares for this topology, base
    /// config's first and then the profile's additions, each name once.
    pub workspaces: Vec<WorkspaceName>,
    /// The matched profile's switching request. `None` for base config,
    /// which cannot make one, and for a profile that makes none.
    pub workspace_switching: Option<ResolvedWorkspaceSwitching>,
    pub hotkeys: BTreeMap<Command, KeyCombo>,
    /// Which layer supplied each binding in `hotkeys`, keyed identically.
    ///
    /// A parallel field rather than a change to `hotkeys`' value type
    /// deliberately: the bindings map has consumers in `mosaix-agent` and
    /// this crate's own diff that care only about what is bound to what,
    /// and pairing every value with its origin would fan a UI concern out
    /// into all of them for nothing.
    pub binding_sources: BTreeMap<Command, ConfigLayer>,
    pub gaps: Gaps,
    pub behavior: BehaviorSection,
    pub automatic_tiling_enabled: bool,
    /// Which arrangement automatic tiling produces when it is enabled.
    /// Meaningless while `automatic_tiling_enabled` is false, and left at
    /// its default there rather than being made an `Option`.
    pub tiling_mode: TilingMode,
    pub focus_border: FocusBorderSection,
    pub layouts: BTreeMap<String, SavedLayout>,
    /// Which layer supplied each saved layout in `layouts`, keyed
    /// identically.
    ///
    /// The same parallel-field shape `binding_sources` uses, and for the
    /// same reason: a layout's consumers care about its cells, and pairing
    /// every one with its origin would fan a UI concern out into all of
    /// them. Bindings got this first (#38) because that was all the hotkey
    /// list needed; showing a layout write's destination before the save
    /// is what needs it here.
    pub layout_sources: BTreeMap<String, ConfigLayer>,
    /// The ordered window rules in effect, still in their config form.
    ///
    /// Not compiled to `mosaix_rules::Rule` here because a compiled rule
    /// owns a `Regex`, which has no `PartialEq`, and this type's equality
    /// is what lets the reducer tell a real config change from a rewrite
    /// of the same content. `crate::validate` has already proved every
    /// pattern compiles by the time one of these is handed out.
    pub rules: Vec<RuleConfig>,
    /// The profile file this config was merged from, or `None` when base
    /// config alone supplies it. Set by [`crate::validate`], which is
    /// where a filename is known; [`crate::merge`] leaves it `None`
    /// because a profile is matched by fingerprint and does not carry the
    /// name of the file it was read from.
    pub profile_file: Option<String>,
}

/// One profile's resolved settings, paired with the `fingerprint` it's
/// matched against. Runtime profile selection, which happens outside this
/// crate, picks among these by comparing `topology_fingerprint()` to
/// `fingerprint`, falling back to [`ResolvedConfigSet::base`] when nothing
/// matches.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    pub fingerprint: String,
    pub config: ResolvedConfig,
}

/// The full result of validating one config directory: base config
/// resolved on its own, plus every profile resolved against that same
/// base. Each entry already passed its own duplicate-binding check
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

    #[test]
    fn the_existing_verbs_keep_their_flat_form() {
        let base: BaseConfig = toml::from_str(
            "version = 1\n[hotkeys]\nsnap-left = \"ctrl+alt+left\"\ntoggle-pause = \"ctrl+alt+p\"\n",
        )
        .expect("flat bindings parse");

        assert_eq!(
            base.hotkeys.get(&Command::SnapLeft),
            Some(&KeyCombo::parse("ctrl+alt+left").unwrap())
        );
        assert_eq!(
            base.hotkeys.get(&Command::TogglePause),
            Some(&KeyCombo::parse("ctrl+alt+p").unwrap())
        );
    }

    #[test]
    fn a_nested_apply_layout_table_binds_one_command_per_layout() {
        let base: BaseConfig = toml::from_str(
            "version = 1\n\
             [hotkeys]\n\
             snap-left = \"ctrl+alt+left\"\n\
             [hotkeys.apply-layout]\n\
             writing = \"ctrl+alt+1\"\n\
             coding = \"ctrl+alt+2\"\n",
        )
        .expect("a nested apply-layout table parses");

        assert_eq!(base.hotkeys.len(), 3, "one keyspace holds all three");
        assert_eq!(
            base.hotkeys.get(&Command::ApplyLayout {
                name: "writing".to_owned()
            }),
            Some(&KeyCombo::parse("ctrl+alt+1").unwrap())
        );
        assert_eq!(
            base.hotkeys.get(&Command::ApplyLayout {
                name: "coding".to_owned()
            }),
            Some(&KeyCombo::parse("ctrl+alt+2").unwrap())
        );
    }

    #[test]
    fn a_mixed_hotkeys_table_round_trips_through_toml() {
        let base: BaseConfig = toml::from_str(
            "version = 1\n\
             [hotkeys]\n\
             snap-left = \"ctrl+alt+left\"\n\
             toggle-pause = \"ctrl+alt+p\"\n\
             [hotkeys.apply-layout]\n\
             writing = \"ctrl+alt+1\"\n",
        )
        .unwrap();

        let rendered = toml::to_string_pretty(&base).expect("mixed bindings serialize");
        let reparsed: BaseConfig = toml::from_str(&rendered).expect("and parse back");

        assert_eq!(reparsed, base);
        assert!(
            rendered.contains("[hotkeys.apply-layout]"),
            "the nested table is written as a table, got:\n{rendered}"
        );
    }

    #[test]
    fn an_unknown_verb_is_rejected_naming_the_line_it_is_written_on() {
        let error = toml::from_str::<BaseConfig>(
            "version = 1\n\
             \n\
             [hotkeys]\n\
             snap-left = \"ctrl+alt+left\"\n\
             snap-lft = \"ctrl+alt+x\"\n",
        )
        .unwrap_err();
        let message = error.to_string();

        assert!(
            message.contains("snap-lft"),
            "the rejection must name the verb, got: {message}"
        );
        assert!(
            message.contains("line 5"),
            "the rejection must name the line the typo is on, got: {message}"
        );
    }

    #[test]
    fn apply_layout_written_flat_is_rejected_rather_than_read_as_a_layout_named_nothing() {
        // `apply-layout` is a table, and there is no layout name in the
        // flat spelling -- deserialization has to say so rather than
        // inventing one.
        assert!(toml::from_str::<BaseConfig>(
            "version = 1\n[hotkeys]\napply-layout = \"ctrl+alt+1\"\n"
        )
        .is_err());
    }

    #[test]
    fn a_layout_name_with_a_space_survives_the_quoted_key_spelling() {
        let base: BaseConfig =
            toml::from_str("version = 1\n[hotkeys.apply-layout]\n\"deep work\" = \"ctrl+alt+1\"\n")
                .unwrap();

        assert!(base.hotkeys.contains_key(&Command::ApplyLayout {
            name: "deep work".to_owned()
        }));

        let reparsed: BaseConfig = toml::from_str(&toml::to_string_pretty(&base).unwrap()).unwrap();
        assert_eq!(reparsed, base);
    }

    #[test]
    fn a_profile_can_bind_a_layout_too() {
        let profile: ProfileConfig = toml::from_str(
            "fingerprint = \"MON-A\"\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+1\"\n",
        )
        .expect("a profile's hotkeys table takes the same two shapes");

        assert!(profile.hotkeys.contains_key(&Command::ApplyLayout {
            name: "writing".to_owned()
        }));
    }

    #[test]
    fn every_unit_verb_round_trips_between_its_name_and_its_command() {
        // The one guard against `verb` and `unit_from_verb` drifting
        // apart, which the removed serde derive used to give for free.
        for command in Command::unit_verbs() {
            assert_eq!(
                Command::unit_from_verb(command.verb()),
                Some(command.clone()),
                "{command} does not survive its own spelling"
            );
        }
    }

    #[test]
    fn apply_layout_is_not_reachable_as_a_unit_verb() {
        assert_eq!(Command::unit_from_verb(APPLY_LAYOUT_VERB), None);
        assert!(!Command::unit_verbs()
            .iter()
            .any(|command| matches!(command, Command::ApplyLayout { .. })));
    }

    #[test]
    fn a_command_is_displayed_as_the_toml_path_a_user_wrote() {
        assert_eq!(Command::SnapLeft.to_string(), "snap-left");
        assert_eq!(
            Command::ApplyLayout {
                name: "writing".to_owned()
            }
            .to_string(),
            "apply-layout.writing"
        );
    }

    #[test]
    fn two_layout_bindings_are_two_distinct_keys_in_one_map() {
        let writing = Command::ApplyLayout {
            name: "writing".to_owned(),
        };
        let coding = Command::ApplyLayout {
            name: "coding".to_owned(),
        };

        assert_ne!(writing, coding);
        // And every unit verb sorts ahead of them, which is what lets the
        // nested table be emitted last.
        assert!(Command::TogglePause < writing);
    }

    #[test]
    fn a_profile_omitting_a_tiling_mode_keeps_the_balanced_grid() {
        let profile: ProfileConfig =
            toml::from_str("fingerprint = \"display\"\n[automatic_tiling]\nenabled = true\n")
                .expect("a profile without a mode is valid");

        assert_eq!(
            profile.automatic_tiling.unwrap().mode,
            TilingMode::Balanced,
            "an existing profile must keep the arrangement it has always produced"
        );
    }

    #[test]
    fn a_profile_can_select_the_container_tree() {
        let profile: ProfileConfig = toml::from_str(
            "fingerprint = \"display\"\n[automatic_tiling]\nenabled = true\nmode = \"tree\"\n",
        )
        .expect("a tree profile is valid");

        assert_eq!(profile.automatic_tiling.unwrap().mode, TilingMode::Tree);
    }

    #[test]
    fn an_unknown_tiling_mode_is_rejected_rather_than_silently_ignored() {
        let error = toml::from_str::<ProfileConfig>(
            "fingerprint = \"display\"\n[automatic_tiling]\nenabled = true\nmode = \"monocle\"\n",
        )
        .expect_err("a mode Mosaix cannot honour must not be accepted");

        assert!(
            error.to_string().contains("monocle"),
            "the error should name the mode that was refused: {error}"
        );
    }

    #[test]
    fn a_profile_can_enable_automatic_tiling_but_base_config_cannot() {
        let profile: ProfileConfig =
            toml::from_str("fingerprint = \"display\"\n[automatic_tiling]\nenabled = true\n")
                .unwrap();
        assert!(profile.automatic_tiling.unwrap().enabled);

        assert!(
            toml::from_str::<BaseConfig>("version = 1\n[automatic_tiling]\nenabled = true\n")
                .is_err()
        );
    }

    #[test]
    fn a_commands_toml_path_parses_back_into_the_command_it_names() {
        for command in Command::unit_verbs() {
            assert_eq!(Command::parse(&command.to_string()), Some(command.clone()));
        }
        assert_eq!(
            Command::parse("apply-layout.writing"),
            Some(Command::ApplyLayout {
                name: "writing".to_owned()
            })
        );
        assert_eq!(
            Command::parse("apply-layout.deep.work"),
            Some(Command::ApplyLayout {
                name: "deep.work".to_owned()
            }),
            "a layout name may carry dots, so only the first separator divides the path"
        );
    }

    #[test]
    fn a_path_no_verb_names_does_not_parse() {
        assert_eq!(Command::parse("snap-diagonal"), None);
        assert_eq!(
            Command::parse("apply-layout"),
            None,
            "the verb alone is a table, and a command cannot be built without the layout name"
        );
        assert_eq!(Command::parse("apply-layout."), None);
    }

    #[test]
    fn a_workspace_binding_is_written_as_its_own_nested_table_and_round_trips() {
        let mut base = BaseConfig {
            version: CURRENT_VERSION,
            workspaces: vec!["dev".to_owned(), "chat".to_owned()],
            hotkeys: BTreeMap::new(),
            rules: Vec::new(),
            gaps: Gaps::default(),
            behavior: BehaviorSection::default(),
            focus_border: FocusBorderSection::default(),
            layouts: BTreeMap::new(),
            workspace_switching: None,
        };
        base.hotkeys
            .insert(Command::SnapLeft, KeyCombo::parse("ctrl+alt+left").unwrap());
        base.hotkeys.insert(
            Command::ApplyLayout {
                name: "writing".to_owned(),
            },
            KeyCombo::parse("ctrl+alt+1").unwrap(),
        );
        base.hotkeys.insert(
            Command::FocusWorkspace {
                name: "chat".to_owned(),
            },
            KeyCombo::parse("ctrl+alt+2").unwrap(),
        );

        let rendered = toml::to_string_pretty(&base).unwrap();
        assert!(
            rendered.contains("[hotkeys.focus-workspace]\nchat = \"ctrl+alt+2\""),
            "got:\n{rendered}"
        );
        assert!(
            rendered.find("[hotkeys.apply-layout]") < rendered.find("[hotkeys.focus-workspace]"),
            "both nested tables follow the flat entries, got:\n{rendered}"
        );
        let reparsed: BaseConfig = toml::from_str(&rendered).unwrap();
        assert_eq!(reparsed, base);
    }

    #[test]
    fn a_workspace_binding_parses_from_its_toml_path_and_prints_back_the_same() {
        let command = Command::parse("focus-workspace.deep.work").unwrap();

        assert_eq!(
            command,
            Command::FocusWorkspace {
                name: "deep.work".to_owned()
            }
        );
        assert_eq!(command.to_string(), "focus-workspace.deep.work");
        assert_eq!(command.verb(), "focus-workspace");
        assert_eq!(Command::parse("focus-workspace"), None);
    }

    #[test]
    fn a_profiles_switching_request_round_trips_through_toml() {
        let mut displayed = BTreeMap::new();
        displayed.insert(
            r"\\.\DISPLAY1|1920x1080|scale=1".to_owned(),
            "dev".to_owned(),
        );
        let profile = ProfileConfig {
            fingerprint: "MON".to_owned(),
            workspaces: vec!["dev".to_owned()],
            hotkeys: BTreeMap::new(),
            rules: Vec::new(),
            gaps: GapsOverride::default(),
            behavior: BehaviorSection::default(),
            automatic_tiling: None,
            focus_border: FocusBorderOverride::default(),
            layouts: BTreeMap::new(),
            workspace_switching: Some(WorkspaceSwitchingSection {
                experimental: true,
                displayed,
            }),
        };

        let rendered = toml::to_string_pretty(&profile).unwrap();
        assert!(
            rendered.contains("[workspace_switching]\nexperimental = true"),
            "got:\n{rendered}"
        );
        assert!(
            rendered.contains("[workspace_switching.displayed]"),
            "got:\n{rendered}"
        );
        let reparsed: ProfileConfig = toml::from_str(&rendered).unwrap();
        assert_eq!(reparsed, profile);

        let without = ProfileConfig {
            workspace_switching: None,
            ..profile
        };
        assert!(
            !toml::to_string_pretty(&without)
                .unwrap()
                .contains("workspace_switching"),
            "a profile that makes no request writes no section"
        );
    }
}
