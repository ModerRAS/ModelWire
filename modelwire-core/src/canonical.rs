//! Canonical types for ModelWire.
//!
//! Provider-neutral internal representation of requests, responses, and events.
//! Do not pass raw upstream/downstream JSON through the entire system.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Wire API protocol types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WireApi {
    /// Auto-detect protocol.
    #[default]
    Auto,
    /// Native OpenAI Responses.
    Responses,
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions.
    OpenAiChat,
}

impl WireApi {
    /// Parse from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "auto" => Some(WireApi::Auto),
            "responses" => Some(WireApi::Responses),
            "anthropic" => Some(WireApi::Anthropic),
            "openai_chat" | "openai-chat" | "chat" => Some(WireApi::OpenAiChat),
            _ => None,
        }
    }

    /// Convert to config string.
    pub fn as_str(&self) -> &'static str {
        match self {
            WireApi::Auto => "auto",
            WireApi::Responses => "responses",
            WireApi::Anthropic => "anthropic",
            WireApi::OpenAiChat => "openai_chat",
        }
    }
}

/// Canonical response request representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalResponseRequest {
    /// ModelWire request ID.
    pub request_id: String,

    /// Downstream model ID.
    pub downstream_model: String,

    /// Mapped upstream model ID.
    pub upstream_model: String,

    /// System/developer instructions.
    #[serde(default)]
    pub instructions: Option<CanonicalInstructions>,

    /// Input items.
    pub input: Vec<CanonicalInputItem>,

    /// Previous response ID for continuation.
    #[serde(default)]
    pub previous_response_id: Option<String>,

    /// Tool definitions.
    #[serde(default)]
    pub tools: Vec<CanonicalTool>,

    /// Tool choice policy.
    #[serde(default)]
    pub tool_choice: CanonicalToolChoice,

    /// Whether to allow parallel tool calls.
    #[serde(default)]
    pub parallel_tool_calls: bool,

    /// Maximum output tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,

    /// Sampling temperature.
    #[serde(default)]
    pub temperature: Option<f32>,

    /// Nucleus sampling parameter.
    #[serde(default)]
    pub top_p: Option<f32>,

    /// Whether to stream the response.
    #[serde(default)]
    pub stream: bool,

    /// Reasoning options.
    #[serde(default)]
    pub reasoning: Option<CanonicalReasoningOptions>,

    /// Include fields for response customization.
    #[serde(default)]
    pub include: Vec<String>,

    /// Request metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,

    /// Store for future reference.
    #[serde(default)]
    pub store: bool,

    /// Raw downstream JSON for diagnostics.
    pub raw_downstream: serde_json::Value,
}

/// System/developer instructions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalInstructions {
    /// Role (system or developer).
    pub role: String,

    /// Content.
    pub content: String,
}

/// Input item types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanonicalInputItem {
    /// Text input (string shortcut).
    Text { content: String },

    /// Message input.
    Message {
        role: String,
        content: Vec<ContentBlock>,
    },

    /// Assistant tool call replay item.
    ///
    /// Used when ModelWire materializes canonical history for cross-upstream
    /// replay and needs to preserve assistant function call lineage.
    AssistantFunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },

    /// Function call output (tool result).
    FunctionCallOutput { call_id: String, output: String },
}

/// Content block types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Text content.
    Text { text: String },

    /// Image content.
    Image {
        data: String,
        mime_type: Option<String>,
    },

    /// Input JSON for function calls.
    InputJson { json: String },

    /// Reasoning content.
    Reasoning {
        summary: Vec<ReasoningSummaryPart>,
        #[serde(default)]
        encrypted_content: Option<String>,
    },
}

/// Reasoning summary part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningSummaryPart {
    #[serde(rename = "type")]
    pub part_type: String,
    pub text: Option<String>,
}

/// Output item types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanonicalOutputItem {
    /// Assistant message.
    Message {
        id: String,
        role: String,
        content: Vec<ContentBlock>,
    },

    /// Function call.
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
    },

    /// Reasoning summary.
    Reasoning {
        id: String,
        summary: Vec<ReasoningSummaryPart>,
    },
}

/// Tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalTool {
    /// Tool name.
    pub name: String,

    /// Tool description.
    pub description: String,

    /// JSON Schema for tool parameters.
    pub parameters: serde_json::Value,
}

/// Tool choice options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CanonicalToolChoice {
    /// Let the model decide.
    #[default]
    Auto,

    /// Don't use any tools.
    None,

    /// Use a specific tool.
    Specific(String),
}

/// Reasoning options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalReasoningOptions {
    /// Whether to include reasoning summary.
    #[serde(default)]
    pub include_summary: Option<bool>,

    /// Whether to include encrypted reasoning.
    #[serde(default)]
    pub include_encrypted_content: Option<bool>,

    /// Effort level (low, medium, high).
    #[serde(default)]
    pub effort: Option<String>,
}

/// Response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// Response in progress.
    #[default]
    InProgress,
    /// Response completed successfully.
    Completed,
    /// Response failed.
    Failed,
    /// Response cancelled.
    Cancelled,
    /// Response expired.
    Expired,
}

/// Canonical event types for streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CanonicalEvent {
    /// Response created.
    ResponseCreated {
        response_id: String,
        model: String,
        created_at: i64,
    },

    /// Output item added.
    OutputItemAdded {
        response_id: String,
        item: CanonicalOutputItem,
    },

    /// Text delta.
    OutputTextDelta { item_id: String, delta: String },

    /// Function call arguments delta.
    FunctionCallArgumentsDelta { item_id: String, delta: String },

    /// Output item done.
    OutputItemDone {
        response_id: String,
        item: CanonicalOutputItem,
    },

    /// Reasoning summary delta.
    ReasoningSummaryDelta { item_id: String, delta: String },

    /// Response completed.
    ResponseCompleted {
        response_id: String,
        output: Vec<CanonicalOutputItem>,
        usage: Option<ResponseUsage>,
    },

    /// Response failed.
    ResponseFailed {
        response_id: String,
        error: ResponseError,
    },
}

/// Response usage information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseUsage {
    /// Input tokens.
    #[serde(default)]
    pub input_tokens: u64,

    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,

    /// Total tokens.
    #[serde(default)]
    pub total_tokens: u64,

    /// Reasoning tokens (if available).
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
}

/// Response error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    /// Error code.
    pub code: String,

    /// Error message.
    pub message: String,

    /// Error type.
    #[serde(default)]
    pub error_type: Option<String>,

    /// Whether the error is retriable.
    #[serde(default)]
    pub retriable: bool,
}

/// Tool call ID mapping.
///
/// Maps: downstream call ID <-> canonical call ID <-> upstream call ID
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallIdMap {
    /// Downstream call ID -> canonical call ID.
    #[serde(default)]
    pub downstream_to_canonical: HashMap<String, String>,

    /// Canonical call ID -> upstream call ID.
    #[serde(default)]
    pub canonical_to_upstream: HashMap<String, String>,

    /// Downstream call ID -> upstream call ID (direct mapping).
    #[serde(default)]
    pub downstream_to_upstream: HashMap<String, String>,
}

impl ToolCallIdMap {
    /// Add a mapping.
    pub fn add(&mut self, downstream_id: &str, canonical_id: &str, upstream_id: &str) {
        self.downstream_to_canonical
            .insert(downstream_id.to_string(), canonical_id.to_string());
        self.canonical_to_upstream
            .insert(canonical_id.to_string(), upstream_id.to_string());
        self.downstream_to_upstream
            .insert(downstream_id.to_string(), upstream_id.to_string());
    }

    /// Get canonical ID from downstream.
    pub fn get_canonical(&self, downstream_id: &str) -> Option<&String> {
        self.downstream_to_canonical.get(downstream_id)
    }

    /// Get upstream ID from canonical.
    pub fn get_upstream(&self, canonical_id: &str) -> Option<&String> {
        self.canonical_to_upstream.get(canonical_id)
    }

    /// Get upstream ID from downstream.
    pub fn get_upstream_direct(&self, downstream_id: &str) -> Option<&String> {
        self.downstream_to_upstream.get(downstream_id)
    }
}

/// Probe result from lazy protocol detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// Provider ID.
    pub provider_id: String,

    /// Credential hash.
    pub credential_hash: String,

    /// Upstream model.
    pub upstream_model: String,

    /// Detected wire API.
    pub wire_api: WireApi,

    /// Whether streaming is supported.
    pub supports_streaming: bool,

    /// Whether tools are supported.
    pub supports_tools: bool,

    /// Whether parallel tool calls are supported.
    pub supports_parallel_tool_calls: bool,

    /// Whether tool support was explicitly probed and confirmed.
    pub tool_support_known: bool,

    /// Whether previous_response_id is supported.
    pub supports_previous_response_id: bool,

    /// Whether encrypted reasoning content is supported.
    pub supports_reasoning_encrypted_content: bool,

    /// Whether reasoning summary is supported.
    pub supports_reasoning_summary: bool,

    /// Last success timestamp.
    pub last_success_at: Option<i64>,

    /// Last failure timestamp.
    pub last_failure_at: Option<i64>,

    /// Failure kind if any.
    pub failure_kind: Option<String>,

    /// Redacted failure message.
    pub failure_message_redacted: Option<String>,

    /// TTL expiration timestamp.
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wire_api_parse() {
        assert_eq!(WireApi::parse("auto"), Some(WireApi::Auto));
        assert_eq!(WireApi::parse("responses"), Some(WireApi::Responses));
        assert_eq!(WireApi::parse("anthropic"), Some(WireApi::Anthropic));
        assert_eq!(WireApi::parse("openai_chat"), Some(WireApi::OpenAiChat));
        assert_eq!(WireApi::parse("OPENAI_CHAT"), Some(WireApi::OpenAiChat));
        assert_eq!(WireApi::parse("unknown"), None);
    }

    #[test]
    fn test_wire_api_as_str() {
        assert_eq!(WireApi::Auto.as_str(), "auto");
        assert_eq!(WireApi::Responses.as_str(), "responses");
        assert_eq!(WireApi::Anthropic.as_str(), "anthropic");
        assert_eq!(WireApi::OpenAiChat.as_str(), "openai_chat");
    }

    #[test]
    fn test_tool_call_id_map() {
        let mut map = ToolCallIdMap::default();
        map.add("downstream-1", "canonical-1", "upstream-1");

        assert_eq!(
            map.get_canonical("downstream-1"),
            Some(&"canonical-1".to_string())
        );
        assert_eq!(
            map.get_upstream("canonical-1"),
            Some(&"upstream-1".to_string())
        );
        assert_eq!(
            map.get_upstream_direct("downstream-1"),
            Some(&"upstream-1".to_string())
        );
        assert_eq!(map.get_canonical("nonexistent"), None);
    }

    #[test]
    fn test_canonical_request_serialization() {
        let request = CanonicalResponseRequest {
            request_id: "req_mw_test".to_string(),
            downstream_model: "test-model".to_string(),
            upstream_model: "test-model".to_string(),
            instructions: None,
            input: vec![CanonicalInputItem::Text {
                content: "Hello".to_string(),
            }],
            previous_response_id: None,
            tools: vec![],
            tool_choice: CanonicalToolChoice::Auto,
            parallel_tool_calls: false,
            max_output_tokens: Some(1024),
            temperature: Some(0.7),
            top_p: None,
            stream: false,
            reasoning: None,
            include: vec![],
            metadata: serde_json::Value::Null,
            store: false,
            raw_downstream: serde_json::Value::Null,
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: CanonicalResponseRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.request_id, "req_mw_test");
        assert_eq!(parsed.downstream_model, "test-model");
    }
}
