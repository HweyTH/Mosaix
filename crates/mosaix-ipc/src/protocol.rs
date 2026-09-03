use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IpcLayoutCell {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IpcSavedLayout {
    pub name: String,
    pub cells: Vec<IpcLayoutCell>,
}

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
    GetPauseState,
    SaveLayout { layout: IpcSavedLayout },
    ApplyLayoutByName { name: String },
    ApplyLayoutDraft { cells: Vec<IpcLayoutCell> },
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
        let requests = vec![
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
            IpcRequest::GetPauseState,
            IpcRequest::SaveLayout {
                layout: IpcSavedLayout {
                    name: "Two columns".into(),
                    cells: vec![IpcLayoutCell {
                        x: 0.0,
                        y: 0.0,
                        width: 0.5,
                        height: 1.0,
                    }],
                },
            },
            IpcRequest::ApplyLayoutByName {
                name: "Two columns".into(),
            },
            IpcRequest::ApplyLayoutDraft {
                cells: vec![IpcLayoutCell {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }],
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
}
