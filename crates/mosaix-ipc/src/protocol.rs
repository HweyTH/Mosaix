use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

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
        ];
        for request in requests {
            let decoded: IpcEnvelope = serde_json::from_str(
                &serde_json::to_string(&wrap_request(request.clone())).unwrap(),
            )
            .unwrap();
            assert_eq!(decoded, wrap_request(request));
        }
    }
}
