//! starcil-protocol — the wire contract shared by server, TUI client, CLI,
//! plugins and remote bridge. Newline-delimited JSON over a local socket
//! (named pipe on Windows, Unix domain socket elsewhere).
//!
//! Envelope:
//!   request  {"id":"req_1","method":"pane.split","params":{...}}
//!   success  {"id":"req_1","result":{"type":"pane_info", ...}}
//!   error    {"id":"req_1","error":{"code":"not_found","message":"..."}}
//! Subscriptions keep the connection open; later lines are pushed events.

pub mod attach;
pub mod error;
pub mod events;
pub mod methods;
pub mod types;

use serde::{Deserialize, Serialize};

/// Protocol version: major must match, minor negotiates capabilities.
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;

/// Hard cap on a single NDJSON frame (bytes). Oversized frames are rejected
/// with `frame_too_large`, never crash the peer.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessResponse {
    pub id: String,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub id: String,
    pub error: error::ApiError,
}

/// Any single line read from the socket, from the client's point of view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Incoming {
    Success(SuccessResponse),
    Error(ErrorResponse),
    Event(events::EventFrame),
}

impl Request {
    pub fn new(id: impl Into<String>, method: impl Into<String>, params: serde_json::Value) -> Self {
        Request { id: id.into(), method: method.into(), params }
    }
}

pub fn success(id: &str, result: serde_json::Value) -> String {
    serde_json::to_string(&SuccessResponse { id: id.to_string(), result })
        .expect("response serialization cannot fail")
}

pub fn failure(id: &str, err: error::ApiError) -> String {
    serde_json::to_string(&ErrorResponse { id: id.to_string(), error: err })
        .expect("response serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = Request::new("cli:pane:split", "pane.split", serde_json::json!({"direction":"right"}));
        let s = serde_json::to_string(&r).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(back.method, "pane.split");
        assert_eq!(back.id, "cli:pane:split");
    }

    #[test]
    fn incoming_disambiguates() {
        let ok: Incoming = serde_json::from_str(r#"{"id":"a","result":{"type":"pong"}}"#).unwrap();
        assert!(matches!(ok, Incoming::Success(_)));
        let err: Incoming =
            serde_json::from_str(r#"{"id":"a","error":{"code":"not_found","message":"pane not found"}}"#).unwrap();
        assert!(matches!(err, Incoming::Error(_)));
        let ev: Incoming =
            serde_json::from_str(r#"{"event":"pane.created","data":{"pane_id":"w1:p2"}}"#).unwrap();
        assert!(matches!(ev, Incoming::Event(_)));
    }

    #[test]
    fn missing_params_default_to_null() {
        let r: Request = serde_json::from_str(r#"{"id":"x","method":"ping"}"#).unwrap();
        assert!(r.params.is_null());
    }
}
