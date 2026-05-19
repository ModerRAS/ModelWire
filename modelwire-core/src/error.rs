//! Error types for ModelWire.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Error categories for ModelWire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Authentication failed.
    AuthFailed,
    /// Rate limited.
    RateLimited,
    /// Protocol not supported.
    ProtocolNotSupported,
    /// Model not found.
    ModelNotFound,
    /// Invalid request.
    RequestInvalid,
    /// Context length exceeded.
    ContextLengthExceeded,
    /// Upstream timeout.
    UpstreamTimeout,
    /// Upstream unavailable.
    UpstreamUnavailable,
    /// Stream interrupted.
    StreamInterrupted,
    /// Tool mapping failed.
    ToolMappingFailed,
    /// State not found.
    StateNotFound,
    /// State not continuable.
    StateNotContinuable,
    /// State replay failed.
    StateReplayFailed,
    /// Request too large.
    RequestTooLarge,
    /// Internal error.
    InternalError,
}

impl ErrorKind {
    /// Get the HTTP status code for this error kind.
    pub fn status_code(&self) -> u16 {
        match self {
            ErrorKind::AuthFailed => 401,
            ErrorKind::RateLimited => 429,
            ErrorKind::ModelNotFound => 404,
            ErrorKind::RequestTooLarge => 413,
            ErrorKind::StateNotFound => 404,
            ErrorKind::StateNotContinuable => 409,
            ErrorKind::ContextLengthExceeded => 400,
            ErrorKind::RequestInvalid => 400,
            ErrorKind::ProtocolNotSupported => 400,
            ErrorKind::ToolMappingFailed => 400,
            ErrorKind::StreamInterrupted => 499,
            ErrorKind::StateReplayFailed => 500,
            ErrorKind::UpstreamTimeout => 504,
            ErrorKind::UpstreamUnavailable => 502,
            ErrorKind::InternalError => 500,
        }
    }

    /// Whether this error kind is fallback-eligible.
    pub fn is_fallback_eligible(&self) -> bool {
        matches!(
            self,
            ErrorKind::ProtocolNotSupported
                | ErrorKind::UpstreamUnavailable
                | ErrorKind::UpstreamTimeout
                | ErrorKind::RateLimited
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorKind::AuthFailed => "auth_failed",
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::ProtocolNotSupported => "protocol_not_supported",
            ErrorKind::ModelNotFound => "model_not_found",
            ErrorKind::RequestInvalid => "request_invalid",
            ErrorKind::ContextLengthExceeded => "context_length_exceeded",
            ErrorKind::UpstreamTimeout => "upstream_timeout",
            ErrorKind::UpstreamUnavailable => "upstream_unavailable",
            ErrorKind::StreamInterrupted => "stream_interrupted",
            ErrorKind::ToolMappingFailed => "tool_mapping_failed",
            ErrorKind::StateNotFound => "state_not_found",
            ErrorKind::StateNotContinuable => "state_not_continuable",
            ErrorKind::StateReplayFailed => "state_replay_failed",
            ErrorKind::RequestTooLarge => "request_too_large",
            ErrorKind::InternalError => "internal_error",
        };
        write!(f, "{}", s)
    }
}

/// ModelWire error type.
#[derive(Debug, thiserror::Error)]
pub struct Error {
    /// Error kind.
    pub kind: ErrorKind,

    /// Error message.
    pub message: String,

    /// Internal error details (not exposed to downstream).
    #[cfg(debug_assertions)]
    pub detail: Option<String>,
}

#[cfg(not(debug_assertions))]
impl Error {
    fn detail(&self) -> Option<&str> {
        None
    }
}

impl Error {
    /// Create a new error.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            #[cfg(debug_assertions)]
            detail: None,
        }
    }

    /// Add internal detail (only visible in debug builds).
    #[cfg(debug_assertions)]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Convert to downstream API error response.
    pub fn to_response(&self) -> ErrorResponse {
        ErrorResponse {
            error: ErrorDetail {
                message: self.message.clone(),
                error_type: Some(self.kind.to_string()),
                param: None,
                code: Some(self.kind.to_string()),
            },
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

/// Error response structure for downstream API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

/// Error detail structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Human-readable error message.
    pub message: String,

    /// Error type string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,

    /// Parameter that caused the error (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<serde_json::Value>,

    /// Error code string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Result type alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_kind_status_code() {
        assert_eq!(ErrorKind::AuthFailed.status_code(), 401);
        assert_eq!(ErrorKind::RateLimited.status_code(), 429);
        assert_eq!(ErrorKind::ModelNotFound.status_code(), 404);
        assert_eq!(ErrorKind::InternalError.status_code(), 500);
    }

    #[test]
    fn test_error_kind_fallback_eligible() {
        assert!(ErrorKind::ProtocolNotSupported.is_fallback_eligible());
        assert!(ErrorKind::UpstreamUnavailable.is_fallback_eligible());
        assert!(ErrorKind::RateLimited.is_fallback_eligible());
        assert!(!ErrorKind::AuthFailed.is_fallback_eligible());
        assert!(!ErrorKind::RequestInvalid.is_fallback_eligible());
        assert!(!ErrorKind::InternalError.is_fallback_eligible());
    }

    #[test]
    fn test_error_to_response() {
        let error = Error::new(ErrorKind::ModelNotFound, "Model not found: test-model");
        let response = error.to_response();
        assert_eq!(response.error.message, "Model not found: test-model");
        assert_eq!(response.error.code, Some("model_not_found".to_string()));
    }
}
