use std::collections::BTreeMap;

use mosaix_config::{Command, ConfigLayer, LayoutEdit, LayoutWrite, ResolvedConfig, SavedLayout};
use mosaix_engine::{
    EligibilityReason, EngineState, Event, EventSender, StateReader, ZoneSnapDirection,
};
use mosaix_rules::ManageAction;
use serde::{Deserialize, Serialize};

use crate::protocol::{IpcRequest, IpcResponse};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StateSnapshot {
    pub revision: u64,
    pub display_count: usize,
    pub topology_fingerprint: String,
    pub window_count: usize,
    pub focused_window: Option<isize>,
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
    /// (`ctrl+alt+left`).
    pub combo: String,
    /// Which layer supplies this binding: `base`, `profile`, or
    /// `unknown` for a resolved config that recorded no source.
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
fn binding_snapshots(config: &ResolvedConfig) -> Vec<HotkeyBindingSnapshot> {
    let base_file = || Some(mosaix_config::BASE_CONFIG_FILE_NAME.to_owned());
    config
        .hotkeys
        .iter()
        .map(|(command, combo)| {
            let layer = config.binding_sources.get(command).copied();
            HotkeyBindingSnapshot {
                command: command.to_string(),
                layout: match command {
                    Command::ApplyLayout { name } => Some(name.clone()),
                    _ => None,
                },
                combo: combo.to_string(),
                source: match layer {
                    Some(ConfigLayer::Base) => "base",
                    Some(ConfigLayer::Profile) => "profile",
                    None => "unknown",
                }
                .to_owned(),
                file: match (layer, &config.profile_file) {
                    (Some(ConfigLayer::Base), _) => base_file(),
                    // `validate` attaches the filename to every resolved
                    // profile, so this is `Some` for any config that came
                    // through it.
                    (Some(ConfigLayer::Profile), file) => file.clone(),
                    // No recorded source. With no profile in play there is
                    // only one file it could be; with one, naming either
                    // would be a guess.
                    (None, None) => base_file(),
                    (None, Some(_)) => None,
                },
            }
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
            })
            .collect();
        managed_windows.sort_by_key(|window| window.window_id);
        let saved_layouts = state.resolved_config.layouts.clone();
        let hotkeys = binding_snapshots(&state.resolved_config);
        let unregistered_bindings = state
            .unregistered_bindings
            .iter()
            .map(|command| command.to_string())
            .collect();
        Self {
            revision: state.revision,
            display_count: state.displays.len(),
            topology_fingerprint: mosaix_domain::topology_fingerprint(&state.displays),
            window_count: state.windows.len(),
            focused_window: state.focused_window.map(|id| id.0),
            paused: state.paused,
            automatic_tiling_active: state.automatic_tiling_active,
            automatic_tiling_suspended: state.automatic_tiling_suspended,
            mode,
            circuit_breaker_count,
            degraded_windows,
            managed_windows,
            saved_layouts,
            hotkeys,
            hotkey_capture_suspended: state.hotkey_capture_suspended,
            unregistered_bindings,
        }
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
}

pub fn handle_request(
    request: &IpcRequest,
    events: &EventSender,
    state_reader: &StateReader,
    config: &dyn ConfigStore,
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
        IpcRequest::GetPauseState => {
            let state = state_reader.snapshot();
            let value = serde_json::json!({ "paused": state.paused });
            IpcResponse::Ok { data: Some(value) }
        }
        IpcRequest::ApplyLayout { name } => {
            // The reducer would reach the same verdict, but only a log
            // would come of it. Asking first is what lets the caller be
            // told *why* nothing happened (ADR 0020).
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
        IpcRequest::SaveLayout { name, cells } => edit_layouts(
            events,
            state_reader,
            config,
            LayoutEdit::Save {
                name: name.clone(),
                cells: cells.clone(),
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

        assert_eq!(json["hotkeys"][0]["command"], "apply-layout.writing");
        assert_eq!(
            json["hotkeys"][0]["layout"], "writing",
            "a client labels a layout binding without re-parsing the command path"
        );
    }

    #[test]
    fn state_snapshot_lists_every_resolved_binding() {
        let mut state = EngineState::default();
        state.resolved_config = mosaix_config::fallback_config();

        let snapshot = StateSnapshot::from(state.clone());

        assert_eq!(
            snapshot.hotkeys.len(),
            state.resolved_config.hotkeys.len(),
            "the list is what a user reads instead of the TOML file, so it cannot be partial"
        );
    }

    /// A configuration directory that records what it was asked to do and
    /// answers however the test scripted.
    #[derive(Debug, Default)]
    struct RecordingStore {
        edits: std::sync::Mutex<Vec<(String, LayoutEdit)>>,
        answer: Option<Result<LayoutWrite, ConfigError>>,
    }

    impl RecordingStore {
        fn answering(answer: Result<LayoutWrite, ConfigError>) -> Self {
            Self {
                edits: std::sync::Mutex::default(),
                answer: Some(answer),
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
            },
            &engine.events(),
            &engine.state_reader(),
            &store,
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
            },
            &engine.events(),
            &engine.state_reader(),
            &RecordingStore::wrote("config.toml", &[("writing", 0.5)]),
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

            handle_request(&request, &engine.events(), &engine.state_reader(), &store);

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
        );
        wait_for_revision(&engine, 1);
        assert!(engine.state_reader().snapshot().hotkey_capture_suspended);

        handle_request(
            &IpcRequest::EndHotkeyCapture,
            &engine.events(),
            &engine.state_reader(),
            &store,
        );
        wait_for_revision(&engine, 2);
        assert!(!engine.state_reader().snapshot().hotkey_capture_suspended);
    }

    #[test]
    fn a_connection_that_started_capture_owes_a_capture_end_when_it_ends() {
        let mut hold = CaptureHold::default();

        hold.observe(&IpcRequest::StartHotkeyCapture, &IpcResponse::Ok { data: None });

        assert!(matches!(hold.release(), Some(Event::HotkeyCaptureEnded)));
    }

    #[test]
    fn a_connection_that_ended_capture_cleanly_owes_nothing() {
        let mut hold = CaptureHold::default();
        hold.observe(&IpcRequest::StartHotkeyCapture, &IpcResponse::Ok { data: None });

        hold.observe(&IpcRequest::EndHotkeyCapture, &IpcResponse::Ok { data: None });

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
        hold.observe(&IpcRequest::StartHotkeyCapture, &IpcResponse::Ok { data: None });

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
}
