//! Stable public error codes. Servers attach a human message and optional
//! JSON details; codes never change meaning once shipped.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError { code, message: message.into(), details: None }
    }

    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("{what} not found"))
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }
}

impl From<starcil_domain::ModelError> for ApiError {
    fn from(e: starcil_domain::ModelError) -> Self {
        match e {
            starcil_domain::ModelError::NotFound(what) => {
                ApiError::new(ErrorCode::NotFound, format!("{what} not found"))
            }
            starcil_domain::ModelError::InvalidState(msg) => ApiError::new(ErrorCode::InvalidState, msg),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", serde_json::to_string(&self.code).unwrap_or_default(), self.message)
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    AmbiguousTarget,
    InvalidParams,
    InvalidState,
    LeaseConflict,
    StreamConflict,
    Timeout,
    AgentPromptStalled,
    ProtocolMismatch,
    FrameTooLarge,
    UnknownMethod,
    FeatureDisabled,
    PlatformUnsupported,
    PluginDisabled,
    PopupNotOpen,
    RateLimited,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_snake_case() {
        assert_eq!(serde_json::to_string(&ErrorCode::NotFound).unwrap(), "\"not_found\"");
        assert_eq!(
            serde_json::to_string(&ErrorCode::AgentPromptStalled).unwrap(),
            "\"agent_prompt_stalled\""
        );
        assert_eq!(serde_json::to_string(&ErrorCode::FeatureDisabled).unwrap(), "\"feature_disabled\"");
    }

    #[test]
    fn error_shape_matches_contract() {
        let e = ApiError::not_found("pane w1:p9");
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"code":"not_found","message":"pane w1:p9 not found"}"#);
    }
}
