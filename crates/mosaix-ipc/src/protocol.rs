use mosaix_domain::NormalizedRect;
use serde::{Deserialize, Serialize};

/// The wire contract this build speaks.
///
/// Bumped to 2 when [`IpcRequest`] gained its first data-carrying variant
/// ([`IpcRequest::ApplyLayout`]), to 3 for the saved-layout editing
/// requests, to 4 for the hotkey-capture pair, to 5 when
/// [`IpcRequest::SaveLayout`] gained its write-destination redirect, and
/// to 6 for the binding-editing requests, and to 7 when
/// [`IpcRequest::ProbeHotkey`] gained the command it is probing for, and
/// to 10 for the four tree-resize requests, to 11 for
/// [`IpcRequest::RemoveTreePosition`], to 12 for the four workspace
/// lifecycle requests and the workspace fields of the state snapshot, and
/// to 13 for the experimental parking pair ([`IpcRequest::ParkWindow`],
/// [`IpcRequest::RestoreParkedWindows`]) and the parking-failure field of
/// the recovery snapshot, and to 14 for
/// [`IpcRequest::RestoreWorkspaceSwitch`] and the switch fields of the
/// state snapshot. An
/// older agent has no tag for a request this build added -- and no field
/// for one an existing request grew -- so the version is what makes the
/// mismatch reportable instead of surfacing as a deserialization failure.
pub const PROTOCOL_VERSION: u32 = 14;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IpcEnvelope {
    pub version: u32,
    pub payload: IpcPayload,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum IpcPayload {
    Request(IpcRequest),
    Response(IpcResponse),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "command")]
pub enum IpcRequest {
    Ping,
    GetState,
    SnapLeft,
    SnapRight,
    SnapTop,
    SnapBottom,
    FocusLeft,
    FocusRight,
    Pause,
    Resume,
    TogglePause,
    Rearrange,
    ToggleAutomaticTiling,
    ToggleFloating,
    FocusUp,
    FocusDown,
    SwapLeft,
    SwapRight,
    SwapUp,
    SwapDown,
    /// Move the nearest container-tree divider facing that way by five
    /// percentage points (CONTEXT.md "Tree resize"). Answered with a typed
    /// [`mosaix_domain::TreeResizeResult`] either way: a refusal is data
    /// the caller can inspect, not a transport error. Hence
    /// [`PROTOCOL_VERSION`] 10.
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    /// Delete the dormant slot numbered `position` from `display_id`'s
    /// container tree (CONTEXT.md "Dormant tree leaf"). Positions are the
    /// numbers the state snapshot reports for each tree. Answered with a
    /// typed [`mosaix_domain::RemovePositionResult`] either way, hence
    /// [`PROTOCOL_VERSION`] 11.
    RemoveTreePosition {
        display_id: isize,
        position: u64,
    },
    GetPauseState,
    /// Select a display for display-scoped commands, including an empty one.
    FocusDisplay {
        display_id: isize,
    },
    /// Create a hidden, empty logical workspace (CONTEXT.md "Logical
    /// workspace", ADR 0028). This and the three below are answered with
    /// a typed [`mosaix_domain::WorkspaceCommandResult`] either way, so a
    /// refusal is data the caller can inspect. Hence
    /// [`PROTOCOL_VERSION`] 12.
    CreateWorkspace {
        name: String,
    },
    /// Delete a hidden, empty, command-created workspace.
    DeleteWorkspace {
        name: String,
    },
    /// Display a hidden workspace on the focused display, or focus the
    /// last-focused window of one already displayed elsewhere (CONTEXT.md
    /// "Workspace focus"). Never moves a displayed workspace.
    FocusWorkspace {
        name: String,
    },
    /// Move a displayed workspace to `display_id`, exchanging it with
    /// whatever that display showed.
    MoveWorkspace {
        name: String,
        display_id: isize,
    },
    /// Ask to park one managed window through the experimental public-API
    /// parking path (CONTEXT.md "Window parking", ADR 0023): recovery data
    /// is recorded first, and the window leaves visible geometry only once
    /// that is durable. Answered with a typed
    /// [`mosaix_domain::ParkWindowResult`]. Hence [`PROTOCOL_VERSION`] 13.
    ParkWindow {
        window_id: isize,
    },
    /// Put back every window this session parked, through the verified
    /// restore path. Answered with the windows a restore was asked for.
    RestoreParkedWindows,
    /// Reconcile the windows a failed switch compensation left
    /// unaccounted for, which is the only way out of the
    /// workspace-switch-degraded condition (CONTEXT.md
    /// "Workspace-switch degraded"). Answered with a typed
    /// [`mosaix_domain::WorkspaceSwitchRestoreResult`], hence
    /// [`PROTOCOL_VERSION`] 14.
    RestoreWorkspaceSwitch,
    /// Reverse the newest undo transaction, or answer with the typed reason
    /// it was refused (ADR 0024). Carries no options: there is deliberately
    /// no force or best-guess variant, hence [`PROTOCOL_VERSION`] 9.
    Undo,
    /// Apply the saved layout called `name` to the focused display. The
    /// first request carrying a payload, hence
    /// [`PROTOCOL_VERSION`] 2.
    ApplyLayout {
        name: String,
    },
    /// Create the saved layout `name`, or replace the cells of the one
    /// that already exists.
    ///
    /// This and the three below are how the settings application changes
    /// configuration: it asks, the agent writes (ADR 0022). Each lands in
    /// the layer that supplies the layout being edited, and each is
    /// answered with the file it went to.
    SaveLayout {
        name: String,
        cells: Vec<NormalizedRect>,
        /// Redirect this write to base config instead of the layer that
        /// currently supplies the layout (ADR 0022). A layout the matched
        /// profile declares is *moved*: base config gains it and the
        /// profile gives it up, so the merge resolves to the copy the
        /// user asked for.
        to_base: bool,
    },
    RenameLayout {
        from: String,
        to: String,
    },
    DuplicateLayout {
        from: String,
        to: String,
    },
    DeleteLayout {
        name: String,
    },
    /// The hotkey editor is open on this connection: unregister every
    /// binding until it closes (ADR 0021).
    ///
    /// Suspension is bounded by the connection that asked for it, not by
    /// the matching request below. The agent emits capture-end when this
    /// connection ends for any reason -- close, crash, or kill -- because
    /// Windows closes the pipe handle either way, so there is no exit
    /// message that can be lost.
    StartHotkeyCapture,
    /// The hotkey editor closed cleanly. Registration resumes, exactly as
    /// it would have when this connection ended.
    EndHotkeyCapture,
    /// Ask whether `combo` is free, before the user commits to it.
    ///
    /// Answered by attempting registration and releasing it again, then
    /// naming who owns a refusal: another Mosaix binding, or the system
    /// or another application (ADR 0021). The combination is not bound by
    /// asking.
    ProbeHotkey {
        combo: String,
        /// The command being rebound, so its own binding does not count
        /// as a conflict with itself. `None` when the question is asked
        /// about no command in particular.
        for_command: Option<String>,
    },
    /// Bind `command` to `combo`.
    ///
    /// `command_path` is the TOML path the state snapshot reports --
    /// `snap-left`, or `apply-layout.writing` -- so what the interface
    /// shows and what it sends back are one string. It is spelled out
    /// rather than called `command` because that name is this
    /// enumeration's own serde tag. `to_base` is ADR 0022's redirect.
    SetBinding {
        command_path: String,
        combo: String,
        to_base: bool,
    },
    /// Step a binding back toward its default: a profile override is
    /// dropped, and a base binding returns to what a fresh install gives.
    ResetBinding {
        command_path: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "status")]
pub enum IpcResponse {
    Ok { data: Option<serde_json::Value> },
    Error { message: String },
    VersionMismatch { server_version: u32 },
}

pub fn wrap_request(request: IpcRequest) -> IpcEnvelope {
    IpcEnvelope {
        version: PROTOCOL_VERSION,
        payload: IpcPayload::Request(request),
    }
}

pub fn wrap_response(response: IpcResponse) -> IpcEnvelope {
    IpcEnvelope {
        version: PROTOCOL_VERSION,
        payload: IpcPayload::Response(response),
    }
}

/// An envelope read only as far as its version, leaving the payload
/// uninterpreted.
///
/// This is what makes a version mismatch reportable. Deserializing
/// straight into [`IpcEnvelope`] fails on a variant tag this build doesn't
/// have -- exactly what a peer one version ahead sends -- and the version
/// field that explains the failure would never be reached.
#[derive(Deserialize)]
struct VersionedMessage {
    version: u32,
    payload: serde_json::Value,
}

fn read_version(message: &[u8]) -> Result<serde_json::Value, ProtocolError> {
    let message: VersionedMessage = serde_json::from_slice(message)
        .map_err(|error| ProtocolError::Malformed(format!("invalid IPC message: {error}")))?;
    if message.version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch {
            peer_version: message.version,
        });
    }
    Ok(message.payload)
}

/// Why a message could not be turned into the payload the reader wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// The peer speaks a different [`PROTOCOL_VERSION`]. Reported ahead of
    /// any payload complaint, since a version gap explains every other
    /// thing that would go wrong next.
    VersionMismatch { peer_version: u32 },
    /// Well-versioned but unreadable: bad JSON, or a payload of the wrong
    /// kind for this direction of the conversation.
    Malformed(String),
}

/// Decodes one newline-delimited message from a client into the request it
/// carries, or the response to send back instead of handling it.
pub fn decode_request(message: &[u8]) -> Result<IpcRequest, IpcResponse> {
    let payload = match read_version(message) {
        Ok(payload) => payload,
        Err(ProtocolError::VersionMismatch { .. }) => {
            return Err(IpcResponse::VersionMismatch {
                server_version: PROTOCOL_VERSION,
            })
        }
        Err(ProtocolError::Malformed(message)) => return Err(IpcResponse::Error { message }),
    };
    match serde_json::from_value::<IpcPayload>(payload) {
        Ok(IpcPayload::Request(request)) => Ok(request),
        Ok(IpcPayload::Response(_)) => Err(IpcResponse::Error {
            message: "expected an IPC request".to_owned(),
        }),
        Err(error) => Err(IpcResponse::Error {
            message: format!("invalid IPC message: {error}"),
        }),
    }
}

/// Decodes one newline-delimited message from the agent into the response
/// it carries. The mirror of [`decode_request`], and the reason a client
/// talking to an agent one version behind is told about the version rather
/// than about a tag.
pub fn decode_response(message: &[u8]) -> Result<IpcResponse, ProtocolError> {
    let payload = read_version(message)?;
    match serde_json::from_value::<IpcPayload>(payload) {
        Ok(IpcPayload::Response(response)) => Ok(response),
        Ok(IpcPayload::Request(_)) => Err(ProtocolError::Malformed(
            "expected an IPC response".to_owned(),
        )),
        Err(error) => Err(ProtocolError::Malformed(format!(
            "invalid IPC message: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ping() {
        let req = wrap_request(IpcRequest::Ping);
        let serialized = serde_json::to_string(&req).unwrap();
        let deserialized: IpcEnvelope = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.version, PROTOCOL_VERSION);
    }

    #[test]
    fn request_envelopes_round_trip_for_every_command() {
        let requests = [
            IpcRequest::Ping,
            IpcRequest::GetState,
            IpcRequest::SnapLeft,
            IpcRequest::SnapRight,
            IpcRequest::SnapTop,
            IpcRequest::SnapBottom,
            IpcRequest::FocusLeft,
            IpcRequest::FocusRight,
            IpcRequest::Pause,
            IpcRequest::Resume,
            IpcRequest::TogglePause,
            IpcRequest::Rearrange,
            IpcRequest::ToggleAutomaticTiling,
            IpcRequest::ToggleFloating,
            IpcRequest::FocusUp,
            IpcRequest::FocusDown,
            IpcRequest::SwapLeft,
            IpcRequest::SwapRight,
            IpcRequest::SwapUp,
            IpcRequest::SwapDown,
            IpcRequest::ResizeLeft,
            IpcRequest::ResizeRight,
            IpcRequest::ResizeUp,
            IpcRequest::ResizeDown,
            IpcRequest::RemoveTreePosition {
                display_id: 1,
                position: 4,
            },
            IpcRequest::GetPauseState,
            IpcRequest::FocusDisplay { display_id: 7 },
            IpcRequest::CreateWorkspace {
                name: "dev".to_owned(),
            },
            IpcRequest::DeleteWorkspace {
                name: "dev".to_owned(),
            },
            IpcRequest::FocusWorkspace {
                name: "dev".to_owned(),
            },
            IpcRequest::MoveWorkspace {
                name: "dev".to_owned(),
                display_id: 7,
            },
            IpcRequest::ParkWindow { window_id: 41 },
            IpcRequest::RestoreParkedWindows,
            IpcRequest::RestoreWorkspaceSwitch,
            IpcRequest::Undo,
            IpcRequest::ApplyLayout {
                name: "writing".to_owned(),
            },
            IpcRequest::SaveLayout {
                name: "writing".to_owned(),
                cells: vec![NormalizedRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.5,
                    height: 1.0,
                }],
                to_base: false,
            },
            IpcRequest::RenameLayout {
                from: "writing".to_owned(),
                to: "drafting".to_owned(),
            },
            IpcRequest::DuplicateLayout {
                from: "writing".to_owned(),
                to: "writing wide".to_owned(),
            },
            IpcRequest::DeleteLayout {
                name: "writing".to_owned(),
            },
            IpcRequest::StartHotkeyCapture,
            IpcRequest::EndHotkeyCapture,
            IpcRequest::ProbeHotkey {
                combo: "ctrl+alt+left".to_owned(),
                for_command: Some("snap-left".to_owned()),
            },
            IpcRequest::SetBinding {
                command_path: "snap-left".to_owned(),
                combo: "ctrl+alt+left".to_owned(),
                to_base: true,
            },
            IpcRequest::ResetBinding {
                command_path: "apply-layout.writing".to_owned(),
            },
        ];
        for request in requests {
            let decoded: IpcEnvelope = serde_json::from_str(
                &serde_json::to_string(&wrap_request(request.clone())).unwrap(),
            )
            .unwrap();
            assert_eq!(decoded, wrap_request(request));
        }
    }

    #[test]
    fn a_layout_name_survives_the_round_trip_verbatim() {
        let request = IpcRequest::ApplyLayout {
            name: "writing / draft \"2\"".to_owned(),
        };

        let encoded = serde_json::to_string(&wrap_request(request.clone())).unwrap();

        assert_eq!(decode_request(encoded.as_bytes()), Ok(request));
    }

    #[test]
    fn a_message_from_another_protocol_version_is_a_version_mismatch_not_a_parse_failure() {
        // What an older agent's client sends: a valid envelope whose
        // version this build doesn't speak, carrying a request tag that
        // build never had.
        let older = serde_json::json!({
            "version": PROTOCOL_VERSION - 1,
            "payload": { "type": "Request", "command": "SomethingThisBuildLacks" },
        })
        .to_string();

        assert_eq!(
            decode_request(older.as_bytes()),
            Err(IpcResponse::VersionMismatch {
                server_version: PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn a_client_reading_an_older_agents_reply_reports_the_version_not_the_payload() {
        // An older agent answers in its own protocol, and may well answer
        // with a status this build has never heard of.
        let older = serde_json::json!({
            "version": PROTOCOL_VERSION - 1,
            "payload": { "type": "Response", "status": "SomethingThisBuildLacks" },
        })
        .to_string();

        assert_eq!(
            decode_response(older.as_bytes()),
            Err(ProtocolError::VersionMismatch {
                peer_version: PROTOCOL_VERSION - 1,
            })
        );
    }

    #[test]
    fn a_response_at_this_version_decodes_normally() {
        let encoded = serde_json::to_string(&wrap_response(IpcResponse::Error {
            message: "no saved layout named \"writing\"".to_owned(),
        }))
        .unwrap();

        assert_eq!(
            decode_response(encoded.as_bytes()),
            Ok(IpcResponse::Error {
                message: "no saved layout named \"writing\"".to_owned(),
            })
        );
    }

    #[test]
    fn a_response_arriving_where_a_request_belongs_is_rejected_as_such() {
        let encoded =
            serde_json::to_string(&wrap_response(IpcResponse::Ok { data: None })).unwrap();

        assert_eq!(
            decode_request(encoded.as_bytes()),
            Err(IpcResponse::Error {
                message: "expected an IPC request".to_owned(),
            })
        );
    }
}
