use mosaix_domain::NormalizedRect;
use serde::{Deserialize, Serialize};

/// The wire contract this build speaks.
///
/// Bumped to 2 when [`IpcRequest`] gained its first data-carrying variant
/// ([`IpcRequest::ApplyLayout`]), to 3 for the saved-layout editing
/// requests, to 4 for the hotkey-capture pair, and to 5 when
/// [`IpcRequest::SaveLayout`] gained its write-destination redirect. An
/// older agent has no tag for a request this build added -- and no field
/// for one an existing request grew -- so the version is what makes the
/// mismatch reportable instead of surfacing as a deserialization failure.
pub const PROTOCOL_VERSION: u32 = 5;

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
    GetPauseState,
    /// Apply the saved layout called `name` to the focused window's
    /// display (ADR 0020). The first request carrying a payload, hence
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
            IpcRequest::GetPauseState,
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
