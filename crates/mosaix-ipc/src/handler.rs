use std::collections::BTreeMap;

use mosaix_config::{
    BindingEdit, BindingWrite, Command, ConfigLayer, KeyCombo, LayoutEdit, LayoutWrite,
    ResolvedConfig, SavedLayout,
};
use mosaix_domain::commands::TreeResizeResult;
use mosaix_domain::undo::UndoResult;
use mosaix_engine::{
    EligibilityReason, EngineState, Event, EventSender, StateReader, ZoneSnapDirection,
};
use mosaix_persistence::PersistenceHealth;
use mosaix_rules::ManageAction;
use serde::{Deserialize, Serialize};

use crate::protocol::{IpcRequest, IpcResponse};

/// One display's container tree, as its windows in visual order.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ContainerTreeSnapshot {
    pub display_id: isize,
    /// Every window holding a leaf, in visual order, whether or not it is
    /// currently arranged.
    pub windows: Vec<isize>,
    /// The windows the display cannot fit at their minimum size, newest
    /// insertion first (CONTEXT.md "Constraint-overflow window"). They
    /// appear in `windows` too, because they keep their leaves.
    #[serde(default)]
    pub constraint_overflow: Vec<isize>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StateSnapshot {
    pub revision: u64,
    pub display_count: usize,
    pub topology_fingerprint: String,
    pub window_count: usize,
    pub focused_window: Option<isize>,
    pub focused_display: Option<isize>,
    pub persistence_status: String,
    pub last_durable_revision: u64,
    pub persistence_reason: Option<String>,
    /// Whether undoing right now would actually move windows. Computed by
    /// running the same preflight undo itself runs, so this never claims an
    /// availability that a request would then refuse.
    pub undo_available: bool,
    /// What the next undo would reverse, whether or not it currently can.
    pub undo_command: Option<String>,
    pub undo_transaction_id: Option<i64>,
    /// Why undo is unavailable, when a transaction exists but cannot run.
    pub undo_blocked_reason: Option<String>,
    /// Which arrangement automatic tiling is producing: `balanced` or
    /// `tree`. Reported whether or not tiling is currently active, so a
    /// caller can tell "tree mode, suspended" from "balanced mode".
    pub tiling_mode: String,
    /// Each display's container tree, as the windows it holds in visual
    /// order. Empty under the balanced grid, which keeps no structure.
    pub container_trees: Vec<ContainerTreeSnapshot>,
    pub paused: bool,
    pub automatic_tiling_active: bool,
    pub automatic_tiling_suspended: bool,
    /// One precedence-resolved status for human-facing clients.
    pub mode: String,
    /// Number of windows whose circuit breaker is currently open (Feature 31).
    /// These windows are excluded from automatic placement until the user
    /// explicitly resets them with a zone-snap command.
    pub circuit_breaker_count: usize,
    /// Each placement-circuit exclusion with a stable machine-readable
    /// reason. This is intentionally a list rather than a count so clients
    /// can surface an actionable per-window diagnostic.
    pub degraded_windows: Vec<DegradedWindow>,
    /// Sanitized authoritative inventory. Titles and executable paths never
    /// cross this diagnostics boundary.
    pub managed_windows: Vec<ManagedWindowSnapshot>,
    /// The saved layouts the resolved config currently offers, by name
    /// (CONTEXT.md "Saved layout"). Shape only -- a layout never names a
    /// window, so there is nothing here to sanitize (ADR 0018).
    pub saved_layouts: BTreeMap<String, SavedLayout>,
    /// The saved layout most recently applied to each display, keyed by
    /// display id.
    ///
    /// Alongside `saved_layouts`, this is what lets tooling built on top
    /// of Mosaix tell what a display is currently arranged as, not just
    /// what it could be arranged as. A display absent from this map has
    /// had no saved layout applied since the agent started.
    pub last_applied_layouts: BTreeMap<isize, String>,
    /// Which layer supplies each entry of `saved_layouts`, keyed
    /// identically. This is what lets the settings application show a
    /// layout write's destination *before* the save rather than only in
    /// the receipt afterwards (ADR 0022).
    pub layout_sources: BTreeMap<String, LayoutSourceSnapshot>,
    /// Every hotkey binding in effect for the current topology, each
    /// naming the configuration file that supplies it. This is what lets
    /// the settings application list bindings without reading a TOML file,
    /// and what makes the write destination visible before a save
    /// (ADR 0022).
    pub hotkeys: Vec<HotkeyBindingSnapshot>,
    /// Whether a hotkey editor currently holds registration suspended
    /// (ADR 0021). The editor states this rather than leaving the user to
    /// infer it from shortcuts that have stopped working.
    pub hotkey_capture_suspended: bool,
    /// The commands whose bindings the last registration pass could not
    /// register, in the same TOML-path spelling
    /// [`HotkeyBindingSnapshot::command`] uses.
    ///
    /// Normally empty. A combination another application took while
    /// capture held registration suspended appears here when the editor
    /// closes, which is how the user finds out a shortcut is dead rather
    /// than by pressing it.
    pub unregistered_bindings: Vec<String>,
}

/// Which layer supplies one saved layout, and so which file a save of it
/// would be written to (ADR 0022).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LayoutSourceSnapshot {
    /// `base`, `profile`, or `unknown` for a resolved config that recorded
    /// no source.
    pub source: String,
    /// The file that supplies it. `None` when this build cannot name it.
    pub file: Option<String>,
}

/// One resolved hotkey binding, flattened for a client that has no
/// `mosaix-config` types to deserialize into.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HotkeyBindingSnapshot {
    /// The command's TOML path -- `snap-left`, or `apply-layout.writing`
    /// for a layout binding. The same spelling a validation error uses, so
    /// what the interface shows and what an error names are one string.
    pub command: String,
    /// The saved layout a parameterized binding applies, `None` for the
    /// unit verbs. Broken out so a client can label the binding without
    /// re-parsing `command`.
    pub layout: Option<String>,
    /// The combination, in the spelling config files use
    /// (`ctrl+alt+left`), or `None` for a command nothing is bound to.
    ///
    /// The list covers every command Mosaix has rather than only the
    /// bound ones, so a command can be *given* a combination in the
    /// interface and not only rebound -- and so a binding reset out of
    /// existence leaves a row to bind again rather than vanishing.
    pub combo: Option<String>,
    /// Which layer supplies this binding: `base`, `profile`, `unbound`
    /// for a command nothing binds, or `unknown` for a resolved config
    /// that recorded no source.
    pub source: String,
    /// The file that supplies it, and so the file a GUI edit of it would
    /// be written to (ADR 0022).
    ///
    /// `None` when this build cannot name it. A destination shown to a
    /// user before a write has to be the real one or absent -- a
    /// plausible-looking guess is the worst of the three.
    pub file: Option<String>,
}

/// Every binding in `config`, paired with where it came from.
///
/// A binding with no recorded source is listed anyway rather than hidden:
/// `merge` records one for every binding it resolves, so the only way to
/// reach that is a `ResolvedConfig` built by hand, and dropping a real
/// binding from the list a user reads instead of the TOML file would be
/// worse than admitting where it came from is unknown.
/// The layer a resolved value came from, as the interface labels it.
fn layer_name(layer: Option<ConfigLayer>) -> String {
    match layer {
        Some(ConfigLayer::Base) => "base",
        Some(ConfigLayer::Profile) => "profile",
        None => "unknown",
    }
    .to_owned()
}

/// The file `layer` means, given the profile (if any) this config was
/// merged from -- and so the file an edit of that value would be written
/// to (ADR 0022).
///
/// `None` when this build cannot name it. A destination shown to a user
/// before a write has to be the real one or absent: a plausible-looking
/// guess is the worst of the three.
fn supplying_file(layer: Option<ConfigLayer>, profile_file: &Option<String>) -> Option<String> {
    let base_file = || Some(mosaix_config::BASE_CONFIG_FILE_NAME.to_owned());
    match (layer, profile_file) {
        (Some(ConfigLayer::Base), _) => base_file(),
        // `validate` attaches the filename to every resolved profile, so
        // this is `Some` for any config that came through it.
        (Some(ConfigLayer::Profile), file) => file.clone(),
        // No recorded source. With no profile in play there is only one
        // file it could be; with one, naming either would be a guess.
        (None, None) => base_file(),
        (None, Some(_)) => None,
    }
}

/// Every command the interface can bind, in a stable order: the unit
/// verbs as they are declared, then one entry per saved layout.
///
/// Every command, not every *binding*, because a command nothing binds is
/// exactly the one a user most wants to reach -- a saved layout that is
/// not yet one keystroke away, or a binding they just reset out of
/// existence.
fn bindable_commands(config: &ResolvedConfig) -> Vec<Command> {
    let mut commands: Vec<Command> = Command::unit_verbs()
        .into_iter()
        .chain(
            config
                .layouts
                .keys()
                .map(|name| Command::ApplyLayout { name: name.clone() }),
        )
        .collect();
    // Whatever is actually bound is listed too, even a layout binding
    // whose layout is not declared -- which validation rejects, so it
    // should never arrive, and dropping it silently is exactly how a
    // binding the user can see in their file would go missing here.
    for command in config.hotkeys.keys() {
        if !commands.contains(command) {
            commands.push(command.clone());
        }
    }
    commands
}

/// Every command the interface can bind, paired with what presses it and
/// where that came from -- the list a user reads instead of the TOML file.
fn binding_snapshots(config: &ResolvedConfig) -> Vec<HotkeyBindingSnapshot> {
    bindable_commands(config)
        .into_iter()
        .map(|command| {
            let combo = config.hotkeys.get(&command);
            // A command nothing binds has no supplying layer, and so no
            // file to name: a write would create it, and where it would
            // be created is the caller's rule to apply, not a fact about
            // the configuration as it stands.
            let layer = combo.and(config.binding_sources.get(&command).copied());
            HotkeyBindingSnapshot {
                layout: match &command {
                    Command::ApplyLayout { name } => Some(name.clone()),
                    _ => None,
                },
                combo: combo.map(|combo| combo.to_string()),
                source: match combo {
                    Some(_) => layer_name(layer),
                    None => "unbound".to_owned(),
                },
                file: combo.and_then(|_| supplying_file(layer, &config.profile_file)),
                command: command.to_string(),
            }
        })
        .collect()
}

/// Where each saved layout comes from, keyed the way
/// [`StateSnapshot::saved_layouts`] is.
///
/// A parallel map rather than a field on the layout itself, matching how
/// `mosaix-config` records it: the layout's cells and the file supplying
/// them answer two different questions, and only the interface asks the
/// second.
fn layout_source_snapshots(config: &ResolvedConfig) -> BTreeMap<String, LayoutSourceSnapshot> {
    config
        .layouts
        .keys()
        .map(|name| {
            let layer = config.layout_sources.get(name).copied();
            (
                name.clone(),
                LayoutSourceSnapshot {
                    source: layer_name(layer),
                    file: supplying_file(layer, &config.profile_file),
                },
            )
        })
        .collect()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DegradedWindow {
    pub window_id: isize,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ManagedWindowSnapshot {
    pub window_id: isize,
    pub display_id: isize,
    pub action: String,
    pub eligibility: String,
    /// Whether the container tree is currently leaving this window where
    /// it is because the display cannot fit it at its minimum size.
    /// Separate from `eligibility`, which stays `eligible`: overflow is a
    /// planner outcome, not a rule or a session-floating choice.
    #[serde(default)]
    pub constraint_overflow: bool,
}

fn action_name(action: ManageAction) -> &'static str {
    match action {
        ManageAction::Tile => "tile",
        ManageAction::Float => "float",
        ManageAction::Exclude => "exclude",
    }
}

fn eligibility_name(reason: EligibilityReason) -> &'static str {
    match reason {
        EligibilityReason::Eligible => "eligible",
        EligibilityReason::FloatingRule => "floating_rule",
        EligibilityReason::NotTileable => "not_tileable",
        EligibilityReason::Elevated => "elevated",
        EligibilityReason::Minimized => "minimized",
        EligibilityReason::Maximized => "maximized",
        EligibilityReason::Fullscreen => "fullscreen",
        EligibilityReason::Hidden => "hidden",
        EligibilityReason::Cloaked => "cloaked",
        EligibilityReason::CircuitOpen => "circuit_open",
        EligibilityReason::SessionFloating => "session_floating",
    }
}

impl From<EngineState> for StateSnapshot {
    fn from(state: EngineState) -> Self {
        let circuit_breaker_count = state.circuit_breaker_count();
        let mode = if state.paused {
            "paused"
        } else if state.automatic_tiling_suspended {
            "suspended"
        } else if state.automatic_tiling_active && circuit_breaker_count > 0 {
            "degraded"
        } else if state.automatic_tiling_active {
            "active"
        } else {
            "manual"
        }
        .to_owned();
        let degraded_windows = state
            .windows
            .iter()
            .filter(|(_, placement)| placement.circuit_open())
            .map(|(window_id, _)| DegradedWindow {
                window_id: window_id.0,
                reason: "circuit_open".to_owned(),
            })
            .collect();
        let mut managed_windows: Vec<_> = state
            .inventory
            .values()
            .map(|managed| ManagedWindowSnapshot {
                window_id: managed.window.id.0,
                display_id: managed.window.display_id.0,
                action: action_name(managed.action).to_owned(),
                eligibility: eligibility_name(managed.eligibility).to_owned(),
                constraint_overflow: state
                    .constraint_overflow
                    .get(&managed.window.display_id)
                    .is_some_and(|overflow| overflow.contains(&managed.window.id)),
            })
            .collect();
        managed_windows.sort_by_key(|window| window.window_id);
        let last_applied_layouts = state
            .last_applied_layouts
            .iter()
            .map(|(display_id, name)| (display_id.0, name.clone()))
            .collect();
        let saved_layouts = state.resolved_config.layouts.clone();
        let layout_sources = layout_source_snapshots(&state.resolved_config);
        let hotkeys = binding_snapshots(&state.resolved_config);
        let unregistered_bindings = state
            .unregistered_bindings
            .iter()
            .map(|command| command.to_string())
            .collect();
        let (persistence_status, last_durable_revision, persistence_reason) =
            match state.persistence_health {
                PersistenceHealth::Healthy {
                    last_durable_revision,
                } => ("healthy".to_owned(), last_durable_revision, None),
                PersistenceHealth::Degraded {
                    last_durable_revision,
                    reason,
                } => (
                    "degraded".to_owned(),
                    last_durable_revision,
                    Some(reason.code().to_owned()),
                ),
            };
        // Asking the planner rather than merely reporting that history is
        // non-empty: a stored transaction whose windows are gone, or whose
        // database is degraded, is not an available undo.
        let planned = mosaix_engine::plan_undo(&state);
        let undo_available = planned.is_applied();
        let undo_blocked_reason = match &planned {
            UndoResult::Applied(_) => None,
            UndoResult::Refused(refusal) => Some(refusal.code().to_owned()),
        };
        // Trees are published as their leaves in visual order rather than
        // as the nested structure. That is what a client can act on -- the
        // structure itself is the reducer's, and republishing it would
        // invite a client to reason about a shape it cannot change.
        let mut container_trees: Vec<ContainerTreeSnapshot> = state
            .trees
            .iter()
            .map(|(display_id, tree)| ContainerTreeSnapshot {
                display_id: display_id.0,
                windows: tree.windows().into_iter().map(|id| id.0).collect(),
                constraint_overflow: state
                    .constraint_overflow
                    .get(display_id)
                    .map(|overflow| overflow.iter().map(|id| id.0).collect())
                    .unwrap_or_default(),
            })
            .collect();
        container_trees.sort_by_key(|snapshot| snapshot.display_id);

        let (undo_command, undo_transaction_id) = match &state.newest_undo {
            Some(transaction) => (Some(transaction.command.clone()), Some(transaction.id.0)),
            None => (None, None),
        };
        Self {
            revision: state.revision,
            display_count: state.displays.len(),
            topology_fingerprint: mosaix_domain::topology_fingerprint(&state.displays),
            window_count: state.windows.len(),
            focused_window: state.focused_window.map(|id| id.0),
            focused_display: state.focused_display.map(|id| id.0),
            persistence_status,
            last_durable_revision,
            persistence_reason,
            undo_available,
            undo_command,
            undo_transaction_id,
            undo_blocked_reason,
            tiling_mode: state.resolved_config.tiling_mode.code().to_owned(),
            container_trees,
            paused: state.paused,
            automatic_tiling_active: state.automatic_tiling_active,
            automatic_tiling_suspended: state.automatic_tiling_suspended,
            mode,
            circuit_breaker_count,
            degraded_windows,
            managed_windows,
            saved_layouts,
            last_applied_layouts,
            layout_sources,
            hotkeys,
            hotkey_capture_suspended: state.hotkey_capture_suspended,
            unregistered_bindings,
        }
    }
}

/// What the operating system said about one combination.
///
/// A trait rather than a direct call, so the classification below --
/// which refusal is another Mosaix binding, which is the system, and
/// which combinations are refused without asking at all -- is testable
/// without touching `RegisterHotKey`. The agent's implementation is the
/// real `mosaix_platform_windows::probe_hotkey`.
pub trait HotkeyProbe: Send + Sync {
    fn probe(&self, combo: &KeyCombo) -> ProbeOutcome;
}

/// The platform's raw answer, before Mosaix's own bindings are consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Registration succeeded and was released again.
    Available,
    /// Registration was refused. Something owns it; who is not knowable
    /// from here.
    Taken,
    /// Mosaix cannot express this combination at all -- a key name with
    /// no virtual-key code behind it. Distinct from `Taken`, because
    /// "nothing owns it, Mosaix just cannot send it" is a different thing
    /// to tell the user.
    Unsupported { reason: String },
}

/// A probe for a build with no platform to ask. Reports every
/// combination unsupported rather than claiming one is free.
#[derive(Debug, Clone)]
pub struct UnavailableHotkeyProbe {
    pub reason: String,
}

impl HotkeyProbe for UnavailableHotkeyProbe {
    fn probe(&self, _combo: &KeyCombo) -> ProbeOutcome {
        ProbeOutcome::Unsupported {
            reason: self.reason.clone(),
        }
    }
}

/// Whether `combo` is one of the two combinations the probe cannot see.
///
/// `Win+L` and `Ctrl+Alt+Del` are handled by Windows itself and are never
/// registered hotkeys, so `RegisterHotKey` accepts them and the binding
/// then never fires. Hardcoding exactly these two is deliberate: every
/// other refusal is left to the probe, including Windows-key chords
/// (ADR 0021).
fn is_reserved(combo: &KeyCombo) -> bool {
    let key = combo.key.to_ascii_uppercase();
    let win_lock = combo.win && !combo.ctrl && !combo.alt && !combo.shift && key == "L";
    let secure_attention = combo.ctrl && combo.alt && !combo.win && key == "DELETE";
    win_lock || secure_attention
}

/// The advisory Microsoft's `RegisterHotKey` documentation earns for
/// `F12`, which it reserves for the debugger.
///
/// A warning rather than a block: the reservation is real but not
/// absolute, and refusing a combination the user may well be able to use
/// is a worse answer than saying so (ADR 0021).
fn advisory(combo: &KeyCombo) -> Option<String> {
    combo
        .key
        .eq_ignore_ascii_case("F12")
        .then(|| "F12 is reserved for the debugger".to_owned())
}

/// The Mosaix binding already using `combo`, other than `for_command`
/// itself.
///
/// Asked before the probe's answer is interpreted, because during hotkey
/// capture the agent holds no registrations at all -- so a combination
/// Mosaix itself owns probes as free, and only the resolved config knows
/// otherwise. `for_command` is excluded because re-pressing a binding's
/// own combination is not a conflict with anything.
fn mosaix_owner(
    config: &ResolvedConfig,
    combo: &KeyCombo,
    for_command: Option<&Command>,
) -> Option<Command> {
    config
        .hotkeys
        .iter()
        .find(|(command, bound)| *bound == combo && Some(*command) != for_command)
        .map(|(command, _)| command.clone())
}

/// The answer to "can I use this combination", as the interface reports
/// it.
///
/// A typed answer rather than a hand-built object, so the field names the
/// settings application deserializes are the ones this file declares.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HotkeyVerdict {
    /// `available`, `mosaix_binding`, `system_or_other_application`,
    /// `reserved`, or `unsupported`.
    pub availability: String,
    /// The Mosaix binding already using it, for `mosaix_binding`.
    pub command: Option<String>,
    /// An advisory that does not block the binding, such as `F12`.
    pub warning: Option<String>,
    /// Why Mosaix cannot express the combination, for `unsupported`.
    pub reason: Option<String>,
}

impl HotkeyVerdict {
    fn new(availability: &str, warning: Option<String>) -> Self {
        Self {
            availability: availability.to_owned(),
            command: None,
            warning,
            reason: None,
        }
    }
}

fn availability(
    config: &ResolvedConfig,
    probe: &dyn HotkeyProbe,
    combo: &KeyCombo,
    for_command: Option<&Command>,
) -> HotkeyVerdict {
    let warning = advisory(combo);
    if is_reserved(combo) {
        return HotkeyVerdict::new("reserved", warning);
    }
    if let Some(command) = mosaix_owner(config, combo, for_command) {
        return HotkeyVerdict {
            command: Some(command.to_string()),
            ..HotkeyVerdict::new("mosaix_binding", warning)
        };
    }
    match probe.probe(combo) {
        ProbeOutcome::Available => HotkeyVerdict::new("available", warning),
        // An unexplained refusal is attributed to the system (ADR 0021):
        // no Mosaix binding claimed it above, so whatever owns it is not
        // something the user can resolve inside Mosaix.
        ProbeOutcome::Taken => HotkeyVerdict::new("system_or_other_application", warning),
        ProbeOutcome::Unsupported { reason } => HotkeyVerdict {
            reason: Some(reason),
            ..HotkeyVerdict::new("unsupported", warning)
        },
    }
}

/// The agent's configuration directory, as the request handler needs it.
///
/// A trait rather than a path, so what the handler does with a layout edit
/// -- which errors it reports, what it answers with, and that it makes the
/// change live rather than waiting out the reload debounce -- is testable
/// without a directory on disk. The agent's implementation is the real
/// `mosaix_config::edit_layouts`.
pub trait ConfigStore: Send + Sync {
    /// Applies `edit` against the topology `fingerprint` is for, returning
    /// the file it landed in and the configuration as it now stands, or a
    /// reason the caller can act on.
    fn edit_layouts(&self, fingerprint: &str, edit: LayoutEdit)
        -> Result<LayoutWrite, ConfigError>;

    /// Applies `edit` against the topology `fingerprint` is for, returning
    /// the file it landed in and the configuration as it now stands.
    ///
    /// The binding twin of [`ConfigStore::edit_layouts`], for the same
    /// reason: the settings application asks, the agent writes (ADR 0022).
    fn edit_bindings(
        &self,
        fingerprint: &str,
        edit: BindingEdit,
    ) -> Result<BindingWrite, ConfigError>;
}

/// A configuration change that could not be made, in words meant for the
/// person who asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

/// A store for an agent with nowhere to write: the configuration directory
/// could not be located at startup.
///
/// Refusing every edit with that reason is the honest answer. Reporting
/// success for a write that went nowhere is the failure this whole path
/// exists to avoid.
#[derive(Debug, Clone)]
pub struct UnavailableConfigStore {
    pub reason: String,
}

impl ConfigStore for UnavailableConfigStore {
    fn edit_layouts(
        &self,
        _fingerprint: &str,
        _edit: LayoutEdit,
    ) -> Result<LayoutWrite, ConfigError> {
        Err(ConfigError(self.reason.clone()))
    }

    fn edit_bindings(
        &self,
        _fingerprint: &str,
        _edit: BindingEdit,
    ) -> Result<BindingWrite, ConfigError> {
        Err(ConfigError(self.reason.clone()))
    }
}

pub fn handle_request(
    request: &IpcRequest,
    events: &EventSender,
    state_reader: &StateReader,
    config: &dyn ConfigStore,
    hotkeys: &dyn HotkeyProbe,
) -> IpcResponse {
    match request {
        IpcRequest::Ping => IpcResponse::Ok { data: None },
        IpcRequest::GetState => {
            let state = state_reader.snapshot();
            let snapshot = StateSnapshot::from(state);
            let value = serde_json::to_value(snapshot).unwrap();
            IpcResponse::Ok { data: Some(value) }
        }
        IpcRequest::SnapLeft => send_event(
            events,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        ),
        IpcRequest::SnapRight => send_event(
            events,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Right,
            },
        ),
        IpcRequest::SnapTop => send_event(
            events,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Top,
            },
        ),
        IpcRequest::SnapBottom => send_event(
            events,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Bottom,
            },
        ),
        IpcRequest::Pause => send_event(events, Event::PauseRequested),
        IpcRequest::Resume => send_event(events, Event::ResumeRequested),
        IpcRequest::TogglePause => {
            let state = state_reader.snapshot();
            if state.paused {
                send_event(events, Event::ResumeRequested)
            } else {
                send_event(events, Event::PauseRequested)
            }
        }
        IpcRequest::Rearrange => send_event(events, Event::RearrangeRequested),
        IpcRequest::ToggleAutomaticTiling => {
            send_event(events, Event::ToggleAutomaticTilingRequested)
        }
        IpcRequest::ToggleFloating => send_event(events, Event::ToggleFloatingRequested),
        IpcRequest::FocusLeft => send_event(
            events,
            Event::DirectionalFocusRequested {
                direction: mosaix_engine::CardinalDirection::Left,
            },
        ),
        IpcRequest::FocusRight => send_event(
            events,
            Event::DirectionalFocusRequested {
                direction: mosaix_engine::CardinalDirection::Right,
            },
        ),
        IpcRequest::FocusUp => send_event(
            events,
            Event::DirectionalFocusRequested {
                direction: mosaix_engine::CardinalDirection::Up,
            },
        ),
        IpcRequest::FocusDown => send_event(
            events,
            Event::DirectionalFocusRequested {
                direction: mosaix_engine::CardinalDirection::Down,
            },
        ),
        IpcRequest::SwapLeft => send_event(
            events,
            Event::DirectionalSwapRequested {
                direction: mosaix_engine::CardinalDirection::Left,
            },
        ),
        IpcRequest::SwapRight => send_event(
            events,
            Event::DirectionalSwapRequested {
                direction: mosaix_engine::CardinalDirection::Right,
            },
        ),
        IpcRequest::SwapUp => send_event(
            events,
            Event::DirectionalSwapRequested {
                direction: mosaix_engine::CardinalDirection::Up,
            },
        ),
        IpcRequest::SwapDown => send_event(
            events,
            Event::DirectionalSwapRequested {
                direction: mosaix_engine::CardinalDirection::Down,
            },
        ),
        IpcRequest::ResizeLeft => {
            resize_tree(events, state_reader, mosaix_engine::CardinalDirection::Left)
        }
        IpcRequest::ResizeRight => resize_tree(
            events,
            state_reader,
            mosaix_engine::CardinalDirection::Right,
        ),
        IpcRequest::ResizeUp => {
            resize_tree(events, state_reader, mosaix_engine::CardinalDirection::Up)
        }
        IpcRequest::ResizeDown => {
            resize_tree(events, state_reader, mosaix_engine::CardinalDirection::Down)
        }
        IpcRequest::GetPauseState => {
            let state = state_reader.snapshot();
            let value = serde_json::json!({ "paused": state.paused });
            IpcResponse::Ok { data: Some(value) }
        }
        IpcRequest::FocusDisplay { display_id } => send_event(
            events,
            Event::FocusDisplayRequested {
                display_id: mosaix_domain::DisplayId(*display_id),
            },
        ),
        IpcRequest::Undo => {
            // Preflighting here is what lets a refusal carry its evidence.
            // The reducer re-reaches the verdict against its own state, so
            // this answer describes the plan, not a completed movement.
            let planned = mosaix_engine::plan_undo(&state_reader.snapshot());
            let payload = serde_json::to_value(&planned).expect("undo results serialize");
            match planned {
                UndoResult::Applied(_) => match send_event(events, Event::UndoRequested) {
                    IpcResponse::Ok { .. } => IpcResponse::Ok {
                        data: Some(payload),
                    },
                    other => other,
                },
                // A refusal is an answer, not a transport failure, so it
                // comes back as data the caller can inspect rather than as
                // a string it would have to parse.
                UndoResult::Refused(_) => IpcResponse::Ok {
                    data: Some(payload),
                },
            }
        }
        IpcRequest::ApplyLayout { name } => {
            // The reducer would reach the same verdict, but only a log
            // would come of it. Asking first is what lets the caller be
            // told *why* nothing happened.
            match mosaix_engine::plan_saved_layout(&state_reader.snapshot(), name) {
                Ok(plan) => match send_event(
                    events,
                    Event::SavedLayoutApplyRequested { name: name.clone() },
                ) {
                    // A layout with fewer cells than the display has
                    // windows still applies; what it could not place is
                    // counted back rather than dropped silently.
                    //
                    // These describe the *plan*, not the outcome. The
                    // reducer re-reaches the verdict against its own
                    // state, and a window whose circuit breaker is open
                    // will not move even though a cell was assigned to it
                    // -- so `cells_filled` counts cells that got a window,
                    // and deliberately does not claim they all moved.
                    IpcResponse::Ok { .. } => IpcResponse::Ok {
                        data: Some(serde_json::json!({
                            "layout": name,
                            "cells_filled": plan.placements.len(),
                            "unplaced": plan.unplaced,
                        })),
                    },
                    other => other,
                },
                Err(rejection) => IpcResponse::Error {
                    message: rejection.to_string(),
                },
            }
        }
        IpcRequest::SaveLayout {
            name,
            cells,
            to_base,
        } => edit_layouts(
            events,
            state_reader,
            config,
            LayoutEdit::Save {
                name: name.clone(),
                cells: cells.clone(),
                to_base: *to_base,
            },
        ),
        IpcRequest::RenameLayout { from, to } => edit_layouts(
            events,
            state_reader,
            config,
            LayoutEdit::Rename {
                from: from.clone(),
                to: to.clone(),
            },
        ),
        IpcRequest::DuplicateLayout { from, to } => edit_layouts(
            events,
            state_reader,
            config,
            LayoutEdit::Duplicate {
                from: from.clone(),
                to: to.clone(),
            },
        ),
        IpcRequest::DeleteLayout { name } => edit_layouts(
            events,
            state_reader,
            config,
            LayoutEdit::Delete { name: name.clone() },
        ),
        IpcRequest::StartHotkeyCapture => send_event(events, Event::HotkeyCaptureStarted),
        IpcRequest::EndHotkeyCapture => send_event(events, Event::HotkeyCaptureEnded),
        IpcRequest::ProbeHotkey { combo, for_command } => {
            let for_command = match for_command {
                Some(path) => match Command::parse(path) {
                    Some(command) => Some(command),
                    None => return unknown_command(path),
                },
                None => None,
            };
            match KeyCombo::parse(combo) {
                Ok(combo) => {
                    let verdict = availability(
                        &state_reader.snapshot().resolved_config,
                        hotkeys,
                        &combo,
                        for_command.as_ref(),
                    );
                    IpcResponse::Ok {
                        data: Some(serde_json::to_value(verdict).expect("a verdict serializes")),
                    }
                }
                Err(reason) => IpcResponse::Error { message: reason },
            }
        }
        IpcRequest::SetBinding {
            command_path,
            combo,
            to_base,
        } => {
            let Some(command) = Command::parse(command_path) else {
                return unknown_command(command_path);
            };
            match KeyCombo::parse(combo) {
                Ok(combo) => edit_bindings(
                    events,
                    state_reader,
                    config,
                    BindingEdit::Set {
                        command,
                        combo,
                        to_base: *to_base,
                    },
                ),
                Err(reason) => IpcResponse::Error { message: reason },
            }
        }
        IpcRequest::ResetBinding { command_path } => match Command::parse(command_path) {
            Some(command) => {
                edit_bindings(events, state_reader, config, BindingEdit::Reset { command })
            }
            None => unknown_command(command_path),
        },
    }
}

/// A command path this build has no verb for. Refused by name rather than
/// silently misread, the same posture load-time validation takes.
fn unknown_command(path: &str) -> IpcResponse {
    IpcResponse::Error {
        message: format!("{path:?} is not a command this build knows"),
    }
}

/// Performs one binding edit and makes the result live.
///
/// The same shape [`edit_layouts`] has, and for the same reason:
/// delivering the resulting configuration straight into the reducer is
/// what makes a rebind take effect without restarting the agent, rather
/// than after the reload debounce (ADR 0008). The rebind poller sees the
/// new resolved bindings and re-registers -- unless hotkey capture still
/// holds registration suspended, in which case the change takes effect
/// when the editor closes.
fn edit_bindings(
    events: &EventSender,
    state_reader: &StateReader,
    config: &dyn ConfigStore,
    edit: BindingEdit,
) -> IpcResponse {
    let fingerprint = mosaix_domain::topology_fingerprint(&state_reader.snapshot().displays);
    match config.edit_bindings(&fingerprint, edit) {
        Ok(write) => {
            let combo = write.combo.as_ref().map(|combo| combo.to_string());
            match send_event(events, Event::ConfigChanged(Box::new(write.config))) {
                IpcResponse::Ok { .. } => IpcResponse::Ok {
                    data: Some(serde_json::json!({
                        "file": write.file,
                        "combo": combo,
                    })),
                },
                other => other,
            }
        }
        Err(ConfigError(reason)) => IpcResponse::Error { message: reason },
    }
}

/// Performs one saved-layout edit and makes the result live.
///
/// The write is validated and persisted by `config`; delivering the
/// resulting configuration straight into the reducer is what makes a layout
/// applicable the moment the save is confirmed, rather than after the
/// reload debounce (ADR 0008). The watcher's echo arrives shortly after
/// carrying the identical set, and the reducer already discards a config
/// change that changes nothing.
fn edit_layouts(
    events: &EventSender,
    state_reader: &StateReader,
    config: &dyn ConfigStore,
    edit: LayoutEdit,
) -> IpcResponse {
    let fingerprint = mosaix_domain::topology_fingerprint(&state_reader.snapshot().displays);
    match config.edit_layouts(&fingerprint, edit) {
        Ok(write) => match send_event(events, Event::ConfigChanged(Box::new(write.config))) {
            // The file is what the caller cannot work out for itself: with
            // a profile matched, the layer that received the write and the
            // one the user was looking at are different objects (ADR 0022).
            IpcResponse::Ok { .. } => IpcResponse::Ok {
                data: Some(serde_json::json!({ "file": write.file })),
            },
            other => other,
        },
        Err(ConfigError(reason)) => IpcResponse::Error { message: reason },
    }
}

/// One connection's hold on hotkey-capture suspension.
///
/// Suspension is bounded by the connection that asked for it, not by a
/// message (ADR 0021), and the transport owns that lifetime -- so the
/// bookkeeping lives here, next to the request mapping, where it can be
/// tested without a pipe: what a connection asked for, and what its
/// ending therefore owes the engine.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CaptureHold {
    held: bool,
}

impl CaptureHold {
    /// Records what `request` did, given the `response` it produced.
    ///
    /// Only a request the agent accepted counts. A capture-start the
    /// engine never received suspended nothing, so its connection owes no
    /// capture-end when it ends.
    pub fn observe(&mut self, request: &IpcRequest, response: &IpcResponse) {
        if !matches!(response, IpcResponse::Ok { .. }) {
            return;
        }
        match request {
            IpcRequest::StartHotkeyCapture => self.held = true,
            IpcRequest::EndHotkeyCapture => self.held = false,
            _ => {}
        }
    }

    /// The event this connection's ending owes the engine, if any.
    ///
    /// `Some` exactly when the connection still holds suspension -- the
    /// case a crash or a kill produces, where the editor never sent
    /// capture-end and the closed pipe handle is the only signal that it
    /// is gone.
    pub fn release(&mut self) -> Option<Event> {
        std::mem::take(&mut self.held).then_some(Event::HotkeyCaptureEnded)
    }
}

/// Asks for a tree resize and answers with the typed outcome.
///
/// Preflighted the way undo is: the reducer reaches the same verdict
/// against its own state, so the answer describes the plan rather than a
/// completed movement, and a refusal comes back as data rather than as a
/// string the caller would have to parse.
fn resize_tree(
    events: &EventSender,
    state_reader: &StateReader,
    direction: mosaix_engine::CardinalDirection,
) -> IpcResponse {
    let result = match mosaix_engine::plan_tree_resize(&state_reader.snapshot(), direction) {
        Ok(plan) => {
            if let other @ IpcResponse::Error { .. } =
                send_event(events, Event::TreeResizeRequested { direction })
            {
                return other;
            }
            TreeResizeResult::Applied(plan.applied)
        }
        Err(refusal) => TreeResizeResult::Refused(refusal),
    };
    IpcResponse::Ok {
        data: Some(serde_json::to_value(result).expect("tree results serialize")),
    }
}

fn send_event(events: &EventSender, event: Event) -> IpcResponse {
    if events.send(event).is_err() {
        IpcResponse::Error {
            message: "Engine stopped".to_string(),
        }
    } else {
        IpcResponse::Ok { data: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::{ApplicationId, Window, WindowCapabilities, WindowLifecycle, WindowRole};
    use mosaix_domain::{DisplayId, Rect, WindowId};
    use mosaix_engine::{ManagedWindow, WindowPlacement, CIRCUIT_BREAKER_THRESHOLD};

    fn sample_window(id: isize) -> Window {
        Window {
            id: WindowId(id),
            process_id: 1,
            application_id: ApplicationId("test.exe".to_owned()),
            executable_path: None,
            title: "non-sensitive".to_owned(),
            native_class: None,
            role: WindowRole::Normal,
            bounds: Rect::new(0, 0, 100, 100),
            display_id: DisplayId(1),
            capabilities: WindowCapabilities {
                can_move: true,
                can_resize: true,
                can_minimize: true,
                can_maximize: true,
            },
            elevated: false,
            lifecycle: WindowLifecycle::Active,
            minimum_size: None,
        }
    }

    #[test]
    fn state_snapshot_exposes_each_degraded_window_with_a_stable_reason() {
        let mut state = EngineState::default();
        state.windows.insert(
            WindowId(41),
            WindowPlacement {
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 100, 100),
                observed_bounds: Rect::new(0, 0, 100, 100),
                previous_placement: None,
                cycle_step: None,
                rejection_count: CIRCUIT_BREAKER_THRESHOLD,
            },
        );

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["degraded_windows"][0]["window_id"], 41);
        assert_eq!(json["degraded_windows"][0]["reason"], "circuit_open");
    }

    #[test]
    fn an_undo_with_no_history_answers_with_a_typed_refusal_not_an_error() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());

        let response = handle_request(
            &IpcRequest::Undo,
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::wrote("config.toml", &[]),
            &no_probe(),
        );

        // A refusal is an answer about windows, not a transport failure.
        // Returning it as data is what lets a caller read the reason code
        // and evidence instead of parsing a sentence.
        let IpcResponse::Ok { data: Some(data) } = response else {
            panic!("expected a typed answer, got {response:?}");
        };
        assert_eq!(data["refused"], "nothing_to_undo");
        let parsed: UndoResult =
            serde_json::from_value(data).expect("the CLI can read what the agent sent");
        assert!(!parsed.is_applied());
    }

    #[test]
    fn a_resize_outside_tree_mode_answers_with_a_typed_refusal_not_an_error() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let response = handle_request(
            &IpcRequest::ResizeLeft,
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::wrote("config.toml", &[]),
            &no_probe(),
        );
        engine.stop();

        let IpcResponse::Ok { data: Some(data) } = response else {
            panic!("a refusal is an answer, got {response:?}");
        };
        assert_eq!(data["refused"], "not_tree_mode");
        let parsed: TreeResizeResult =
            serde_json::from_value(data).expect("the payload is the typed result");
        assert!(!parsed.is_applied());
    }

    #[test]
    fn state_snapshot_reports_the_balanced_grid_and_no_trees_by_default() {
        let json = serde_json::to_value(StateSnapshot::from(EngineState::default())).unwrap();

        assert_eq!(json["tiling_mode"], "balanced");
        assert_eq!(json["container_trees"], serde_json::json!([]));
    }

    #[test]
    fn state_snapshot_publishes_each_displays_tree_in_visual_order() {
        use mosaix_domain::tree::{ContainerTree, SplitAxis};

        let mut state = EngineState::default();
        state.resolved_config.tiling_mode = mosaix_config::TilingMode::Tree;
        let mut tree = ContainerTree::new();
        tree.insert_first(mosaix_domain::WindowId(11));
        tree.split_leaf(
            &mosaix_domain::WindowId(11),
            SplitAxis::Horizontal,
            mosaix_domain::WindowId(12),
        );
        state.trees.insert(DisplayId(2), tree);

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["tiling_mode"], "tree");
        assert_eq!(
            json["container_trees"],
            serde_json::json!([{ "display_id": 2, "windows": [11, 12], "constraint_overflow": [] }])
        );
    }

    #[test]
    fn state_snapshot_reports_constraint_overflow_apart_from_floating() {
        use mosaix_domain::tree::{ContainerTree, SplitAxis};

        let mut state = EngineState::default();
        state.resolved_config.tiling_mode = mosaix_config::TilingMode::Tree;
        let mut tree = ContainerTree::new();
        tree.insert_first(mosaix_domain::WindowId(11));
        tree.split_leaf(
            &mosaix_domain::WindowId(11),
            SplitAxis::Horizontal,
            mosaix_domain::WindowId(12),
        );
        state.trees.insert(DisplayId(2), tree);
        state
            .constraint_overflow
            .insert(DisplayId(2), vec![mosaix_domain::WindowId(12)]);
        for id in [11, 12] {
            let mut window = sample_window(id);
            window.display_id = DisplayId(2);
            state.inventory.insert(
                WindowId(id),
                ManagedWindow {
                    window,
                    action: ManageAction::Tile,
                    eligibility: EligibilityReason::Eligible,
                },
            );
        }

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(
            json["container_trees"][0]["constraint_overflow"],
            serde_json::json!([12]),
            "the tree names what it could not fit"
        );
        let managed = json["managed_windows"].as_array().unwrap();
        let overflowed = managed.iter().find(|w| w["window_id"] == 12).unwrap();
        assert_eq!(overflowed["constraint_overflow"], true);
        assert_eq!(
            overflowed["eligibility"], "eligible",
            "overflow is reported beside eligibility, not as a kind of floating"
        );
        let arranged = managed.iter().find(|w| w["window_id"] == 11).unwrap();
        assert_eq!(arranged["constraint_overflow"], false);
    }

    #[test]
    fn state_snapshot_reports_undo_as_unavailable_when_history_is_empty() {
        let json = serde_json::to_value(StateSnapshot::from(EngineState::default())).unwrap();

        assert_eq!(json["undo_available"], false);
        assert_eq!(json["undo_command"], serde_json::Value::Null);
        assert_eq!(json["undo_blocked_reason"], "nothing_to_undo");
    }

    #[test]
    fn state_snapshot_does_not_advertise_undo_it_would_refuse() {
        // The honest answer for a stored transaction whose database is
        // degraded is "not available", with the reason -- not "available"
        // followed by a refusal when the user acts on it.
        let mut state = EngineState::default();
        state.newest_undo = Some(mosaix_domain::UndoTransaction {
            id: mosaix_domain::UndoTransactionId(9),
            command: "snap-left".to_owned(),
            recorded_at_unix: 1_756_000_000,
            topology_fingerprint: String::new(),
            durable_revision: 3,
            members: Vec::new(),
            prior_trees: Vec::new(),
        });
        state.persistence_health = PersistenceHealth::Degraded {
            last_durable_revision: 3,
            reason: mosaix_persistence::PersistenceFailure::WriteFailed,
        };

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["undo_available"], false);
        assert_eq!(json["undo_blocked_reason"], "persistence_degraded");
        assert_eq!(
            json["undo_command"], "snap-left",
            "what undo would reverse is still worth reporting while it cannot"
        );
        assert_eq!(json["undo_transaction_id"], 9);
    }

    #[test]
    fn state_snapshot_exposes_focused_display_and_persistence_health() {
        let mut state = EngineState::default();
        state.focused_display = Some(DisplayId(-7));
        state.persistence_health = PersistenceHealth::Degraded {
            last_durable_revision: 41,
            reason: mosaix_persistence::PersistenceFailure::MigrationFailed,
        };

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["focused_display"], -7);
        assert_eq!(json["persistence_status"], "degraded");
        assert_eq!(json["last_durable_revision"], 41);
        assert_eq!(json["persistence_reason"], "migration_failed");
    }

    #[test]
    fn state_snapshot_publishes_sanitized_managed_inventory() {
        let mut state = EngineState::default();
        state.inventory.insert(
            WindowId(7),
            ManagedWindow {
                window: Window {
                    id: WindowId(7),
                    process_id: 99,
                    application_id: ApplicationId("private.exe".to_owned()),
                    executable_path: Some("C:/secret/private.exe".into()),
                    title: "Sensitive document title".to_owned(),
                    native_class: Some("Example".to_owned()),
                    role: WindowRole::Normal,
                    bounds: Rect::new(10, 20, 300, 200),
                    display_id: DisplayId(4),
                    capabilities: WindowCapabilities {
                        can_move: true,
                        can_resize: true,
                        can_minimize: true,
                        can_maximize: true,
                    },
                    elevated: true,
                    lifecycle: WindowLifecycle::Active,
                    minimum_size: None,
                },
                action: ManageAction::Tile,
                eligibility: EligibilityReason::Elevated,
            },
        );

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["managed_windows"][0]["window_id"], 7);
        assert_eq!(json["managed_windows"][0]["display_id"], 4);
        assert_eq!(json["managed_windows"][0]["action"], "tile");
        assert_eq!(json["managed_windows"][0]["eligibility"], "elevated");
        let encoded = json.to_string();
        assert!(!encoded.contains("Sensitive document title"));
        assert!(!encoded.contains("private.exe"));
    }

    #[test]
    fn state_snapshot_publishes_the_saved_layouts_by_name() {
        let mut layouts = BTreeMap::new();
        layouts.insert(
            "writing".to_owned(),
            SavedLayout {
                cells: vec![mosaix_domain::NormalizedRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.6,
                    height: 1.0,
                }],
            },
        );
        let mut state = EngineState::default();
        state.resolved_config.layouts = layouts;

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["saved_layouts"]["writing"]["cells"][0]["width"], 0.6);
    }

    /// A resolved config binding `snap-left` from base config and
    /// `snap-right` from the profile `desk.toml`, which is the split every
    /// provenance assertion needs.
    fn config_with_mixed_provenance() -> ResolvedConfig {
        let mut config = ResolvedConfig {
            profile_file: Some("desk.toml".to_owned()),
            ..ResolvedConfig::default()
        };
        for (command, combo, layer) in [
            (Command::SnapLeft, "ctrl+alt+left", ConfigLayer::Base),
            (Command::SnapRight, "ctrl+shift+right", ConfigLayer::Profile),
        ] {
            config.hotkeys.insert(
                command.clone(),
                mosaix_config::KeyCombo::parse(combo).unwrap(),
            );
            config.binding_sources.insert(command, layer);
        }
        config
    }

    #[test]
    fn state_snapshot_names_the_file_supplying_each_binding() {
        let mut state = EngineState::default();
        state.resolved_config = config_with_mixed_provenance();

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        let bindings = json["hotkeys"].as_array().unwrap();
        let left = bindings
            .iter()
            .find(|binding| binding["command"] == "snap-left")
            .unwrap();
        assert_eq!(left["combo"], "ctrl+alt+left");
        assert_eq!(left["source"], "base");
        assert_eq!(left["file"], "config.toml");

        let right = bindings
            .iter()
            .find(|binding| binding["command"] == "snap-right")
            .unwrap();
        assert_eq!(right["source"], "profile");
        assert_eq!(
            right["file"], "desk.toml",
            "an overridden binding must name the profile that would receive a write"
        );
    }

    #[test]
    fn state_snapshot_lists_a_layout_binding_with_the_layout_it_applies() {
        let mut state = EngineState::default();
        let command = Command::ApplyLayout {
            name: "writing".to_owned(),
        };
        state.resolved_config.hotkeys.insert(
            command.clone(),
            mosaix_config::KeyCombo::parse("ctrl+alt+1").unwrap(),
        );
        state
            .resolved_config
            .binding_sources
            .insert(command, ConfigLayer::Base);

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        let layout_binding = json["hotkeys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["command"] == "apply-layout.writing")
            .expect("a bound layout command is listed");
        assert_eq!(
            layout_binding["layout"], "writing",
            "a client labels a layout binding without re-parsing the command path"
        );
        assert_eq!(layout_binding["combo"], "ctrl+alt+1");
    }

    #[test]
    fn state_snapshot_lists_every_resolved_binding() {
        let mut state = EngineState::default();
        state.resolved_config = mosaix_config::fallback_config();

        let snapshot = StateSnapshot::from(state.clone());

        for command in state.resolved_config.hotkeys.keys() {
            assert!(
                snapshot
                    .hotkeys
                    .iter()
                    .any(|binding| binding.command == command.to_string()),
                "the list is what a user reads instead of the TOML file, so it cannot be \
                 partial; {command} is missing"
            );
        }
    }

    #[test]
    fn state_snapshot_lists_a_command_nothing_binds_so_it_can_be_given_a_combination() {
        let mut state = EngineState::default();
        state
            .resolved_config
            .layouts
            .insert("writing".to_owned(), SavedLayout { cells: Vec::new() });

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        let unbound = json["hotkeys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["command"] == "apply-layout.writing")
            .expect("a saved layout is bindable whether or not it is bound");
        assert!(unbound["combo"].is_null());
        assert_eq!(unbound["source"], "unbound");
        assert!(
            unbound["file"].is_null(),
            "nothing supplies it yet, so there is no file to name"
        );
    }

    #[test]
    fn every_unit_verb_is_listed_even_with_nothing_bound_at_all() {
        let json = serde_json::to_value(StateSnapshot::from(EngineState::default())).unwrap();

        assert_eq!(
            json["hotkeys"].as_array().unwrap().len(),
            Command::unit_verbs().len(),
            "a command reset out of existence has to leave a row to bind again"
        );
    }

    /// A configuration directory that records what it was asked to do and
    /// answers however the test scripted.
    #[derive(Debug, Default)]
    struct RecordingStore {
        edits: std::sync::Mutex<Vec<(String, LayoutEdit)>>,
        answer: Option<Result<LayoutWrite, ConfigError>>,
        binding_edits: std::sync::Mutex<Vec<(String, BindingEdit)>>,
        binding_answer: Option<Result<BindingWrite, ConfigError>>,
    }

    impl RecordingStore {
        fn answering(answer: Result<LayoutWrite, ConfigError>) -> Self {
            Self {
                answer: Some(answer),
                ..Self::default()
            }
        }

        /// A store that confirms a binding write, reporting `file` and the
        /// combination the binding now resolves to.
        fn bound(file: &str, combo: Option<&str>) -> Self {
            Self {
                binding_answer: Some(Ok(BindingWrite {
                    file: file.to_owned(),
                    combo: combo.map(|combo| KeyCombo::parse(combo).unwrap()),
                    config: mosaix_config::ResolvedConfigSet::default(),
                })),
                ..Self::default()
            }
        }

        fn wrote(file: &str, layouts: &[(&str, f64)]) -> Self {
            let mut base = ResolvedConfig::default();
            for (name, width) in layouts {
                base.layouts.insert(
                    (*name).to_owned(),
                    SavedLayout {
                        cells: vec![mosaix_domain::NormalizedRect {
                            x: 0.0,
                            y: 0.0,
                            width: *width,
                            height: 1.0,
                        }],
                    },
                );
            }
            Self::answering(Ok(LayoutWrite {
                file: file.to_owned(),
                config: mosaix_config::ResolvedConfigSet {
                    base,
                    profiles: Vec::new(),
                },
            }))
        }
    }

    impl ConfigStore for RecordingStore {
        fn edit_layouts(
            &self,
            fingerprint: &str,
            edit: LayoutEdit,
        ) -> Result<LayoutWrite, ConfigError> {
            self.edits
                .lock()
                .unwrap()
                .push((fingerprint.to_owned(), edit));
            self.answer
                .clone()
                .expect("the test scripted no answer for this edit")
        }

        fn edit_bindings(
            &self,
            fingerprint: &str,
            edit: BindingEdit,
        ) -> Result<BindingWrite, ConfigError> {
            self.binding_edits
                .lock()
                .unwrap()
                .push((fingerprint.to_owned(), edit));
            self.binding_answer
                .clone()
                .expect("the test scripted no answer for this binding edit")
        }
    }

    /// A probe answering whatever the test scripted, so classification is
    /// exercised without touching `RegisterHotKey`.
    #[derive(Debug)]
    struct ScriptedProbe(ProbeOutcome);

    impl HotkeyProbe for ScriptedProbe {
        fn probe(&self, _combo: &KeyCombo) -> ProbeOutcome {
            self.0.clone()
        }
    }

    /// The probe for a test whose request never reaches one.
    fn no_probe() -> ScriptedProbe {
        ScriptedProbe(ProbeOutcome::Unsupported {
            reason: "the test scripted no probe answer".to_owned(),
        })
    }

    fn one_cell() -> Vec<mosaix_domain::NormalizedRect> {
        vec![mosaix_domain::NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }]
    }

    #[test]
    fn saving_a_layout_answers_with_the_file_the_write_landed_in() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::wrote("desk.toml", &[("writing", 0.5)]);

        let response = handle_request(
            &IpcRequest::SaveLayout {
                name: "writing".to_owned(),
                cells: one_cell(),
                to_base: false,
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        match response {
            IpcResponse::Ok { data } => assert_eq!(data.unwrap()["file"], "desk.toml"),
            other => panic!("expected a confirmed save, got {other:?}"),
        }
        assert_eq!(
            store.edits.lock().unwrap()[0].1,
            LayoutEdit::Save {
                name: "writing".to_owned(),
                cells: one_cell(),
                to_base: false,
            }
        );
    }

    #[test]
    fn a_saved_layout_is_applicable_without_waiting_for_the_reload() {
        // The write is on disk either way; what this asserts is that the
        // agent does not make the user wait out the debounce before the
        // layout can be applied.
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());

        handle_request(
            &IpcRequest::SaveLayout {
                name: "writing".to_owned(),
                cells: one_cell(),
                to_base: false,
            },
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::wrote("config.toml", &[("writing", 0.5)]),
            &no_probe(),
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline
            && !engine
                .state_reader()
                .snapshot()
                .resolved_config
                .layouts
                .contains_key("writing")
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert!(
            engine
                .state_reader()
                .snapshot()
                .resolved_config
                .layouts
                .contains_key("writing"),
            "the saved layout should be in effect as soon as the save is confirmed"
        );
    }

    #[test]
    fn a_refused_layout_edit_is_reported_with_the_reason() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::answering(Err(ConfigError(
            "a saved layout named \"writing\" already exists".to_owned(),
        )));

        let response = handle_request(
            &IpcRequest::DuplicateLayout {
                from: "draft".to_owned(),
                to: "writing".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        assert_eq!(
            response,
            IpcResponse::Error {
                message: "a saved layout named \"writing\" already exists".to_owned(),
            },
            "the caller needs the reason, not a generic failure"
        );
    }

    #[test]
    fn every_layout_edit_reaches_the_store_as_itself() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        for (request, expected) in [
            (
                IpcRequest::RenameLayout {
                    from: "draft".to_owned(),
                    to: "writing".to_owned(),
                },
                LayoutEdit::Rename {
                    from: "draft".to_owned(),
                    to: "writing".to_owned(),
                },
            ),
            (
                IpcRequest::DuplicateLayout {
                    from: "writing".to_owned(),
                    to: "writing wide".to_owned(),
                },
                LayoutEdit::Duplicate {
                    from: "writing".to_owned(),
                    to: "writing wide".to_owned(),
                },
            ),
            (
                IpcRequest::DeleteLayout {
                    name: "writing".to_owned(),
                },
                LayoutEdit::Delete {
                    name: "writing".to_owned(),
                },
            ),
        ] {
            let store = RecordingStore::wrote("config.toml", &[]);

            handle_request(
                &request,
                &engine.events(),
                &engine.state_reader(),
                &store,
                &no_probe(),
            );

            assert_eq!(store.edits.lock().unwrap()[0].1, expected);
        }
    }

    #[test]
    fn an_agent_with_no_configuration_directory_refuses_rather_than_claiming_a_save() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());

        let response = handle_request(
            &IpcRequest::DeleteLayout {
                name: "writing".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &UnavailableConfigStore {
                reason: "no configuration directory".to_owned(),
            },
            &no_probe(),
        );

        assert_eq!(
            response,
            IpcResponse::Error {
                message: "no configuration directory".to_owned(),
            }
        );
    }

    /// An ordinary tileable window on display 1, the shape every layout
    /// test's inventory is built from.
    fn managed_window(id: isize, bounds: Rect) -> Window {
        Window {
            id: WindowId(id),
            process_id: 4,
            application_id: ApplicationId("test".to_owned()),
            executable_path: None,
            title: "non-sensitive-test-title".to_owned(),
            native_class: None,
            role: WindowRole::Normal,
            bounds,
            display_id: DisplayId(1),
            capabilities: WindowCapabilities {
                can_move: true,
                can_resize: true,
                can_minimize: true,
                can_maximize: true,
            },
            elevated: false,
            lifecycle: WindowLifecycle::Active,
            minimum_size: None,
        }
    }

    /// An engine with one display, one managed window focused on it, and
    /// whatever saved layouts `layouts` declares.
    fn engine_with_layouts(
        layouts: BTreeMap<String, SavedLayout>,
    ) -> (mosaix_engine::EngineHandle, WindowId) {
        let display = mosaix_domain::Display {
            id: DisplayId(1),
            stable_fingerprint: "MON-A".to_owned(),
            full_bounds: Rect::new(0, 0, 1920, 1080),
            work_area: Rect::new(0, 0, 1920, 1080),
            scale_factor: 1.0,
            rotation: mosaix_domain::Rotation::Landscape,
            is_primary: true,
        };
        let config_set = mosaix_config::ResolvedConfigSet {
            base: mosaix_config::ResolvedConfig {
                layouts,
                ..mosaix_config::ResolvedConfig::default()
            },
            profiles: Vec::new(),
        };
        let engine = mosaix_engine::spawn_engine(vec![display], config_set);
        engine
            .events()
            .send(Event::WindowsObserved {
                windows: vec![managed_window(11, Rect::new(0, 0, 400, 300))],
            })
            .unwrap();
        engine
            .events()
            .send(Event::WindowFocused {
                window_id: WindowId(11),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 400, 300),
            })
            .unwrap();
        wait_for_revision(&engine, 2);
        (engine, WindowId(11))
    }

    fn wait_for_revision(engine: &mosaix_engine::EngineHandle, at_least: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while engine.state_reader().revision() < at_least && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn applying_a_declared_layout_is_accepted_and_reaches_the_reducer() {
        let mut layouts = BTreeMap::new();
        layouts.insert(
            "half".to_owned(),
            SavedLayout {
                cells: vec![mosaix_domain::NormalizedRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.5,
                    height: 1.0,
                }],
            },
        );
        let (engine, window_id) = engine_with_layouts(layouts);
        let before = engine.state_reader().revision();

        let response = handle_request(
            &IpcRequest::ApplyLayout {
                name: "half".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::default(),
            &no_probe(),
        );

        assert_eq!(
            response,
            IpcResponse::Ok {
                data: Some(serde_json::json!({
                    "layout": "half",
                    "cells_filled": 1,
                    "unplaced": 0,
                })),
            }
        );
        wait_for_revision(&engine, before + 1);
        assert_eq!(
            engine.snapshot().windows[&window_id].bounds,
            Rect::new(0, 0, 960, 1080)
        );
    }

    #[test]
    fn a_layout_with_fewer_cells_than_windows_answers_with_the_unplaced_count() {
        let mut layouts = BTreeMap::new();
        layouts.insert(
            "solo".to_owned(),
            SavedLayout {
                cells: vec![mosaix_domain::NormalizedRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }],
            },
        );
        let (engine, _) = engine_with_layouts(layouts);
        // A second managed window on the same display, which the
        // single-cell layout has nowhere to put.
        let before = engine.state_reader().revision();
        engine
            .events()
            .send(Event::WindowsObserved {
                windows: vec![
                    managed_window(11, Rect::new(0, 0, 400, 300)),
                    managed_window(12, Rect::new(0, 400, 400, 300)),
                ],
            })
            .unwrap();
        wait_for_revision(&engine, before + 1);

        let response = handle_request(
            &IpcRequest::ApplyLayout {
                name: "solo".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::default(),
            &no_probe(),
        );

        let IpcResponse::Ok { data: Some(data) } = response else {
            panic!("a window surplus is not a failure, got {response:?}");
        };
        assert_eq!(data["cells_filled"], 1);
        assert_eq!(data["unplaced"], 1);
    }

    #[test]
    fn applying_an_undeclared_layout_answers_with_the_reason_naming_it() {
        let (engine, _) = engine_with_layouts(BTreeMap::new());

        let response = handle_request(
            &IpcRequest::ApplyLayout {
                name: "writing".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::default(),
            &no_probe(),
        );

        let IpcResponse::Error { message } = response else {
            panic!("an undeclared layout must be rejected, got {response:?}");
        };
        assert!(
            message.contains("writing"),
            "the rejection must name the layout, got {message:?}"
        );
    }

    #[test]
    fn applying_a_layout_with_nothing_focused_answers_with_that_reason() {
        let mut layouts = BTreeMap::new();
        layouts.insert("half".to_owned(), SavedLayout::default());
        let config_set = mosaix_config::ResolvedConfigSet {
            base: mosaix_config::ResolvedConfig {
                layouts,
                ..mosaix_config::ResolvedConfig::default()
            },
            profiles: Vec::new(),
        };
        let engine = mosaix_engine::spawn_engine(Vec::new(), config_set);

        let response = handle_request(
            &IpcRequest::ApplyLayout {
                name: "half".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::default(),
            &no_probe(),
        );

        let IpcResponse::Error { message } = response else {
            panic!("no focused managed window must be rejected, got {response:?}");
        };
        assert!(
            message.contains("focused"),
            "the rejection must say what was missing, got {message:?}"
        );
    }

    #[test]
    fn the_capture_pair_suspends_and_restores_registration_through_the_reducer() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = UnavailableConfigStore {
            reason: "no configuration directory".to_owned(),
        };

        handle_request(
            &IpcRequest::StartHotkeyCapture,
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );
        wait_for_revision(&engine, 1);
        assert!(engine.state_reader().snapshot().hotkey_capture_suspended);

        handle_request(
            &IpcRequest::EndHotkeyCapture,
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );
        wait_for_revision(&engine, 2);
        assert!(!engine.state_reader().snapshot().hotkey_capture_suspended);
    }

    #[test]
    fn a_connection_that_started_capture_owes_a_capture_end_when_it_ends() {
        let mut hold = CaptureHold::default();

        hold.observe(
            &IpcRequest::StartHotkeyCapture,
            &IpcResponse::Ok { data: None },
        );

        assert!(matches!(hold.release(), Some(Event::HotkeyCaptureEnded)));
    }

    #[test]
    fn a_connection_that_ended_capture_cleanly_owes_nothing() {
        let mut hold = CaptureHold::default();
        hold.observe(
            &IpcRequest::StartHotkeyCapture,
            &IpcResponse::Ok { data: None },
        );

        hold.observe(
            &IpcRequest::EndHotkeyCapture,
            &IpcResponse::Ok { data: None },
        );

        assert!(hold.release().is_none());
    }

    #[test]
    fn a_capture_start_the_agent_refused_leaves_nothing_to_release() {
        let mut hold = CaptureHold::default();

        hold.observe(
            &IpcRequest::StartHotkeyCapture,
            &IpcResponse::Error {
                message: "Engine stopped".to_owned(),
            },
        );

        assert!(hold.release().is_none());
    }

    #[test]
    fn releasing_twice_reports_the_debt_once() {
        let mut hold = CaptureHold::default();
        hold.observe(
            &IpcRequest::StartHotkeyCapture,
            &IpcResponse::Ok { data: None },
        );

        assert!(matches!(hold.release(), Some(Event::HotkeyCaptureEnded)));
        assert!(hold.release().is_none());
    }

    #[test]
    fn the_state_snapshot_names_the_bindings_that_did_not_come_back() {
        let mut state = EngineState::default();
        state.hotkey_capture_suspended = true;
        state.unregistered_bindings = vec![mosaix_config::Command::SnapLeft];

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["hotkey_capture_suspended"], true);
        assert_eq!(json["unregistered_bindings"][0], "snap-left");
    }

    #[test]
    fn the_state_snapshot_names_the_file_supplying_each_saved_layout() {
        let mut state = EngineState::default();
        state.resolved_config.profile_file = Some("desk.toml".to_owned());
        state.resolved_config.layouts.insert(
            "writing".to_owned(),
            mosaix_config::SavedLayout { cells: Vec::new() },
        );
        state.resolved_config.layouts.insert(
            "docked".to_owned(),
            mosaix_config::SavedLayout { cells: Vec::new() },
        );
        state
            .resolved_config
            .layout_sources
            .insert("writing".to_owned(), mosaix_config::ConfigLayer::Base);
        state
            .resolved_config
            .layout_sources
            .insert("docked".to_owned(), mosaix_config::ConfigLayer::Profile);

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["layout_sources"]["writing"]["source"], "base");
        assert_eq!(json["layout_sources"]["writing"]["file"], "config.toml");
        assert_eq!(json["layout_sources"]["docked"]["source"], "profile");
        assert_eq!(
            json["layout_sources"]["docked"]["file"], "desk.toml",
            "the profile is where a save of it would land, so it has to be nameable"
        );
    }

    #[test]
    fn a_layout_with_no_recorded_source_names_no_file_while_a_profile_is_matched() {
        let mut state = EngineState::default();
        state.resolved_config.profile_file = Some("desk.toml".to_owned());
        state.resolved_config.layouts.insert(
            "writing".to_owned(),
            mosaix_config::SavedLayout { cells: Vec::new() },
        );

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["layout_sources"]["writing"]["source"], "unknown");
        assert!(
            json["layout_sources"]["writing"]["file"].is_null(),
            "naming either layer would be a guess, and a guessed destination is the worst answer"
        );
    }

    #[test]
    fn a_redirected_save_reaches_the_config_store_as_a_redirect() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::wrote("config.toml", &[("docked", 0.5)]);

        handle_request(
            &IpcRequest::SaveLayout {
                name: "docked".to_owned(),
                cells: one_cell(),
                to_base: true,
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        assert_eq!(
            store.edits.lock().unwrap()[0].1,
            LayoutEdit::Save {
                name: "docked".to_owned(),
                cells: one_cell(),
                to_base: true,
            }
        );
    }

    /// An engine holding `bindings` as its resolved hotkeys, so the
    /// classification below has Mosaix's own combinations to compare
    /// against.
    fn engine_bound(bindings: &[(Command, &str)]) -> mosaix_engine::EngineHandle {
        let mut base = ResolvedConfig::default();
        for (command, combo) in bindings {
            base.hotkeys
                .insert(command.clone(), KeyCombo::parse(combo).unwrap());
        }
        mosaix_engine::spawn_engine(
            Vec::new(),
            mosaix_config::ResolvedConfigSet {
                base,
                profiles: Vec::new(),
            },
        )
    }

    fn probe_response(
        engine: &mosaix_engine::EngineHandle,
        probe: ProbeOutcome,
        combo: &str,
    ) -> serde_json::Value {
        let response = handle_request(
            &IpcRequest::ProbeHotkey {
                combo: combo.to_owned(),
                for_command: None,
            },
            &engine.events(),
            &engine.state_reader(),
            &UnavailableConfigStore {
                reason: "not asked".to_owned(),
            },
            &ScriptedProbe(probe),
        );
        match response {
            IpcResponse::Ok { data } => data.expect("a probe always answers with a verdict"),
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    #[test]
    fn a_free_combination_no_mosaix_binding_holds_is_reported_available() {
        let engine = engine_bound(&[(Command::SnapLeft, "ctrl+alt+left")]);

        let verdict = probe_response(&engine, ProbeOutcome::Available, "ctrl+alt+right");

        assert_eq!(verdict["availability"], "available");
    }

    #[test]
    fn a_combination_another_mosaix_binding_owns_names_that_binding() {
        // The probe says free on purpose: during capture the agent holds
        // no registrations, so a combination Mosaix owns really does read
        // as available and only the resolved config knows otherwise.
        let engine = engine_bound(&[(Command::SnapLeft, "ctrl+alt+left")]);

        let verdict = probe_response(&engine, ProbeOutcome::Available, "ctrl+alt+left");

        assert_eq!(verdict["availability"], "mosaix_binding");
        assert_eq!(
            verdict["command"], "snap-left",
            "a conflict the user can resolve inside Mosaix has to name what to go and change"
        );
    }

    #[test]
    fn an_unexplained_refusal_is_attributed_to_the_system() {
        let engine = engine_bound(&[]);

        let verdict = probe_response(&engine, ProbeOutcome::Taken, "ctrl+alt+right");

        assert_eq!(verdict["availability"], "system_or_other_application");
    }

    #[test]
    fn the_two_combinations_the_probe_cannot_see_are_refused_without_asking() {
        // The probe would accept both -- neither is a registered hotkey --
        // and the binding would then never fire (ADR 0021).
        let engine = engine_bound(&[]);

        for combo in ["win+l", "ctrl+alt+delete"] {
            assert_eq!(
                probe_response(&engine, ProbeOutcome::Available, combo)["availability"],
                "reserved",
                "{combo} is handled by Windows itself and can never be bound"
            );
        }
    }

    #[test]
    fn a_windows_key_chord_that_is_not_win_l_is_left_to_the_probe() {
        let engine = engine_bound(&[]);

        assert_eq!(
            probe_response(&engine, ProbeOutcome::Available, "win+j")["availability"],
            "available",
            "the reserved list is exactly two combinations; everything else is probed"
        );
    }

    #[test]
    fn the_debugger_reserved_function_key_warns_without_blocking() {
        let engine = engine_bound(&[]);

        let verdict = probe_response(&engine, ProbeOutcome::Available, "ctrl+alt+f12");

        assert_eq!(
            verdict["availability"], "available",
            "F12 is warned about, not blocked"
        );
        assert!(
            verdict["warning"]
                .as_str()
                .is_some_and(|warning| warning.contains("F12")),
            "the reservation is real enough to mention, got {verdict:?}"
        );
    }

    #[test]
    fn a_key_mosaix_cannot_express_is_reported_apart_from_a_taken_one() {
        let engine = engine_bound(&[]);

        let verdict = probe_response(
            &engine,
            ProbeOutcome::Unsupported {
                reason: "Mosaix has no key named \"BREAK\"".to_owned(),
            },
            "ctrl+alt+break",
        );

        assert_eq!(verdict["availability"], "unsupported");
        assert!(verdict["reason"].as_str().unwrap().contains("BREAK"));
    }

    #[test]
    fn a_combination_that_does_not_parse_is_refused_rather_than_probed() {
        let engine = engine_bound(&[]);

        let response = handle_request(
            &IpcRequest::ProbeHotkey {
                combo: "ctrl++".to_owned(),
                for_command: None,
            },
            &engine.events(),
            &engine.state_reader(),
            &UnavailableConfigStore {
                reason: "not asked".to_owned(),
            },
            &no_probe(),
        );

        assert!(matches!(response, IpcResponse::Error { .. }));
    }

    #[test]
    fn setting_a_binding_answers_with_the_file_and_the_combination_in_effect() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::bound("desk.toml", Some("ctrl+shift+left"));

        let response = handle_request(
            &IpcRequest::SetBinding {
                command_path: "snap-left".to_owned(),
                combo: "ctrl+shift+left".to_owned(),
                to_base: false,
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        match response {
            IpcResponse::Ok { data } => {
                let data = data.unwrap();
                assert_eq!(data["file"], "desk.toml");
                assert_eq!(data["combo"], "ctrl+shift+left");
            }
            other => panic!("expected a confirmed write, got {other:?}"),
        }
        assert_eq!(
            store.binding_edits.lock().unwrap()[0].1,
            BindingEdit::Set {
                command: Command::SnapLeft,
                combo: KeyCombo::parse("ctrl+shift+left").unwrap(),
                to_base: false,
            }
        );
    }

    #[test]
    fn a_redirected_binding_write_reaches_the_config_store_as_a_redirect() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::bound("config.toml", Some("ctrl+shift+left"));

        handle_request(
            &IpcRequest::SetBinding {
                command_path: "snap-left".to_owned(),
                combo: "ctrl+shift+left".to_owned(),
                to_base: true,
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        assert!(matches!(
            store.binding_edits.lock().unwrap()[0].1,
            BindingEdit::Set { to_base: true, .. }
        ));
    }

    #[test]
    fn resetting_a_binding_reports_what_it_now_resolves_to() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::bound("config.toml", Some("ctrl+alt+left"));

        let response = handle_request(
            &IpcRequest::ResetBinding {
                command_path: "snap-left".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        match response {
            IpcResponse::Ok { data } => assert_eq!(data.unwrap()["combo"], "ctrl+alt+left"),
            other => panic!("expected a confirmed write, got {other:?}"),
        }
        assert_eq!(
            store.binding_edits.lock().unwrap()[0].1,
            BindingEdit::Reset {
                command: Command::SnapLeft
            }
        );
    }

    #[test]
    fn a_reset_that_left_the_command_unbound_says_so_rather_than_naming_a_combination() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());
        let store = RecordingStore::bound("config.toml", None);

        let response = handle_request(
            &IpcRequest::ResetBinding {
                command_path: "apply-layout.writing".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
            &no_probe(),
        );

        match response {
            IpcResponse::Ok { data } => assert!(data.unwrap()["combo"].is_null()),
            other => panic!("expected a confirmed write, got {other:?}"),
        }
    }

    #[test]
    fn a_command_this_build_does_not_know_is_refused_by_name() {
        let engine = mosaix_engine::spawn_engine(Vec::new(), Default::default());

        let response = handle_request(
            &IpcRequest::ResetBinding {
                command_path: "snap-diagonal".to_owned(),
            },
            &engine.events(),
            &engine.state_reader(),
            &UnavailableConfigStore {
                reason: "not asked".to_owned(),
            },
            &no_probe(),
        );

        match response {
            IpcResponse::Error { message } => assert!(
                message.contains("snap-diagonal"),
                "the refusal names what was asked for, got {message}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_state_snapshot_says_what_each_display_is_currently_arranged_as() {
        let mut state = EngineState::default();
        state
            .last_applied_layouts
            .insert(DisplayId(3), "writing".to_owned());

        let json = serde_json::to_value(StateSnapshot::from(state)).unwrap();

        assert_eq!(json["last_applied_layouts"]["3"], "writing");
    }

    #[test]
    fn re_pressing_a_bindings_own_combination_is_not_a_conflict_with_itself() {
        let engine = engine_bound(&[(Command::SnapLeft, "ctrl+alt+left")]);

        let response = handle_request(
            &IpcRequest::ProbeHotkey {
                combo: "ctrl+alt+left".to_owned(),
                for_command: Some("snap-left".to_owned()),
            },
            &engine.events(),
            &engine.state_reader(),
            &UnavailableConfigStore {
                reason: "not asked".to_owned(),
            },
            &ScriptedProbe(ProbeOutcome::Available),
        );

        match response {
            IpcResponse::Ok { data } => assert_eq!(
                data.unwrap()["availability"],
                "available",
                "rebinding a command to what it is already bound to conflicts with nothing"
            ),
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    #[test]
    fn another_commands_binding_is_still_a_conflict_when_probing_for_one() {
        let engine = engine_bound(&[
            (Command::SnapLeft, "ctrl+alt+left"),
            (Command::SnapRight, "ctrl+alt+right"),
        ]);

        let response = handle_request(
            &IpcRequest::ProbeHotkey {
                combo: "ctrl+alt+left".to_owned(),
                for_command: Some("snap-right".to_owned()),
            },
            &engine.events(),
            &engine.state_reader(),
            &UnavailableConfigStore {
                reason: "not asked".to_owned(),
            },
            &ScriptedProbe(ProbeOutcome::Available),
        );

        match response {
            IpcResponse::Ok { data } => {
                let data = data.unwrap();
                assert_eq!(data["availability"], "mosaix_binding");
                assert_eq!(data["command"], "snap-left");
            }
            other => panic!("expected a verdict, got {other:?}"),
        }
    }
}
