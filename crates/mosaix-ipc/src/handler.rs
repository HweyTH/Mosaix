use std::collections::BTreeMap;

use mosaix_config::SavedLayout;
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
        }
    }
}

pub fn handle_request(
    request: &IpcRequest,
    events: &EventSender,
    state_reader: &StateReader,
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
        );

        let IpcResponse::Error { message } = response else {
            panic!("no focused managed window must be rejected, got {response:?}");
        };
        assert!(
            message.contains("focused"),
            "the rejection must say what was missing, got {message:?}"
        );
    }
}
