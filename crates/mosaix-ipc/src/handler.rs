use mosaix_engine::{EngineState, Event, EventSender, StateReader, ZoneSnapDirection};
use serde::{Deserialize, Serialize};

use crate::protocol::{IpcRequest, IpcResponse};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StateSnapshot {
    pub revision: u64,
    pub display_count: usize,
    pub window_count: usize,
    pub focused_window: Option<isize>,
    pub paused: bool,
    pub automatic_tiling_active: bool,
    pub automatic_tiling_suspended: bool,
    /// Number of windows whose circuit breaker is currently open (Feature 31).
    /// These windows are excluded from automatic placement until the user
    /// explicitly resets them with a zone-snap command.
    pub circuit_breaker_count: usize,
    /// Each placement-circuit exclusion with a stable machine-readable
    /// reason. This is intentionally a list rather than a count so clients
    /// can surface an actionable per-window diagnostic.
    pub degraded_windows: Vec<DegradedWindow>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DegradedWindow {
    pub window_id: isize,
    pub reason: String,
}

impl From<EngineState> for StateSnapshot {
    fn from(state: EngineState) -> Self {
        let circuit_breaker_count = state.circuit_breaker_count();
        let degraded_windows = state
            .windows
            .iter()
            .filter(|(_, placement)| placement.circuit_open())
            .map(|(window_id, _)| DegradedWindow {
                window_id: window_id.0,
                reason: "circuit_open".to_owned(),
            })
            .collect();
        Self {
            revision: state.revision,
            display_count: state.displays.len(),
            window_count: state.windows.len(),
            focused_window: state.focused_window.map(|id| id.0),
            paused: state.paused,
            automatic_tiling_active: state.automatic_tiling_active,
            automatic_tiling_suspended: state.automatic_tiling_suspended,
            circuit_breaker_count,
            degraded_windows,
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
    use mosaix_domain::{DisplayId, Rect, WindowId};
    use mosaix_engine::{WindowPlacement, CIRCUIT_BREAKER_THRESHOLD};

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
}
