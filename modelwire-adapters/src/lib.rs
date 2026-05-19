//! ModelWire Adapters
//!
//! Upstream protocol adapters for Responses, Anthropic Messages, and OpenAI Chat.

pub mod anthropic;
pub mod openai_chat;
pub mod responses;
pub mod sse;

use async_trait::async_trait;
use modelwire_core::{CanonicalEvent, CanonicalResponseRequest, WireApi};

/// Get current Unix timestamp.
pub fn now_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Adapter trait for upstream protocol translation.
#[async_trait]
pub trait UpstreamAdapter: Send + Sync {
    /// Get the wire API type.
    fn wire_api(&self) -> WireApi;

    /// Build upstream request from canonical request.
    fn build_request(
        &self,
        canonical: &CanonicalResponseRequest,
        base_url: &str,
        api_key: Option<&str>,
    ) -> UpstreamRequest;

    /// Parse upstream response into canonical events.
    async fn parse_response(&self, body: &[u8]) -> Result<Vec<CanonicalEvent>, UpstreamError>;

    /// Parse upstream SSE stream into canonical events.
    fn parse_sse_event(
        &self,
        event_type: &str,
        data: &[u8],
    ) -> Result<Option<CanonicalEvent>, UpstreamError>;
}

/// Upstream request builder result.
pub struct UpstreamRequest {
    /// HTTP method.
    pub method: String,
    /// Request path.
    pub path: String,
    /// Headers.
    pub headers: Vec<(String, String)>,
    /// Request body JSON.
    pub body: serde_json::Value,
}

/// Upstream error type.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("HTTP error: {status} - {message}")]
    HttpError { status: u16, message: String },

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Timeout")]
    Timeout,

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Protocol not supported")]
    ProtocolNotSupported,

    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}

/// Result type for upstream operations.
pub type UpstreamResult<T> = Result<T, UpstreamError>;
