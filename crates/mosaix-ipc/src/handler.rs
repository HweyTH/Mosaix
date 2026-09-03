use mosaix_engine::{EngineState, Event, EventSender, StateReader, ZoneSnapDirection};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::protocol::{IpcRequest, IpcResponse};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StateSnapshot {
    pub revision: u64,
    pub display_count: usize,
    pub window_count: usize,
    pub focused_window: Option<isize>,
    pub paused: bool,
    /// Number of windows whose circuit breaker is currently open (Feature 31).
    /// These windows are excluded from automatic placement until the user
    /// explicitly resets them with a zone-snap command.
    pub circuit_breaker_count: usize,
}

impl From<EngineState> for StateSnapshot {
    fn from(state: EngineState) -> Self {
        let circuit_breaker_count = state.circuit_breaker_count();
        Self {
            revision: state.revision,
            display_count: state.displays.len(),
            window_count: state.windows.len(),
            focused_window: state.focused_window.map(|id| id.0),
            paused: state.paused,
            circuit_breaker_count,
        }
    }
}

pub fn handle_request(
    request: &IpcRequest,
    events: &EventSender,
    state_reader: &StateReader,
    config_dir: &Path,
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
        // Focus navigation is intentionally a protocol-level placeholder
        // until the engine grows focus commands.
        IpcRequest::FocusLeft | IpcRequest::FocusRight => IpcResponse::Ok { data: None },
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
        IpcRequest::GetPauseState => {
            let state = state_reader.snapshot();
            let value = serde_json::json!({ "paused": state.paused });
            IpcResponse::Ok { data: Some(value) }
        }
        IpcRequest::ApplyLayoutByName { name } => {
            if !can_apply(state_reader) {
                return unavailable_apply_response();
            }
            send_event(events, Event::LayoutApplyRequested { name: name.clone() })
        }
        IpcRequest::ApplyLayoutDraft { cells } => {
            if !can_apply(state_reader) {
                return unavailable_apply_response();
            }
            send_event(
                events,
                Event::LayoutApplyDraftRequested {
                    cells: cells
                        .iter()
                        .map(|cell| (cell.x, cell.y, cell.width, cell.height))
                        .collect(),
                },
            )
        }
        IpcRequest::SaveLayout { layout } => match mosaix_config::save_layout(
            config_dir,
            mosaix_config::SavedLayout {
                name: layout.name.clone(),
                cells: layout
                    .cells
                    .iter()
                    .map(|cell| mosaix_config::LayoutCell {
                        x: cell.x,
                        y: cell.y,
                        width: cell.width,
                        height: cell.height,
                    })
                    .collect(),
            },
        ) {
            Ok(()) if can_apply(state_reader) => send_event(
                events,
                Event::LayoutApplyDraftRequested {
                    cells: layout
                        .cells
                        .iter()
                        .map(|cell| (cell.x, cell.y, cell.width, cell.height))
                        .collect(),
                },
            ),
            Ok(()) => unavailable_apply_response(),
            Err(error) => IpcResponse::Error {
                message: error.to_string(),
            },
        },
    }
}

fn can_apply(state_reader: &StateReader) -> bool {
    let state = state_reader.snapshot();
    !state.paused
        && state
            .focused_window
            .is_some_and(|window| state.windows.contains_key(&window))
}
fn unavailable_apply_response() -> IpcResponse {
    IpcResponse::Error {
        message: "no focused managed window is available to apply this layout".into(),
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
