//! Anthropic Messages adapter.

use super::{UpstreamAdapter, UpstreamError, UpstreamRequest};
use async_trait::async_trait;
use modelwire_core::{CanonicalEvent, CanonicalResponseRequest, WireApi};
use tracing::debug;

/// Anthropic Messages adapter.
pub struct AnthropicAdapter;

impl AnthropicAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AnthropicAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UpstreamAdapter for AnthropicAdapter {
    fn wire_api(&self) -> WireApi {
        WireApi::Anthropic
    }

    fn build_request(
        &self,
        canonical: &CanonicalResponseRequest,
        _base_url: &str,
        api_key: Option<&str>,
    ) -> UpstreamRequest {
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];

        if let Some(key) = api_key {
            headers.push(("x-api-key".to_string(), key.to_string()));
        }

        let mut messages: Vec<serde_json::Value> = Vec::new();

        // Add input items as messages
        for item in &canonical.input {
            match item {
                modelwire_core::CanonicalInputItem::Text { content } => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                modelwire_core::CanonicalInputItem::Message { role, content } => {
                    let anthropic_role = match role.as_str() {
                        "system" | "developer" => continue, // handled separately
                        "user" => "user",
                        "assistant" => "assistant",
                        _ => "user",
                    };

                    let anthropic_content: Vec<_> = content
                        .iter()
                        .filter_map(|block| match block {
                            modelwire_core::ContentBlock::Text { text } => {
                                Some(serde_json::json!({ "type": "text", "text": text }))
                            }
                            _ => None,
                        })
                        .collect();

                    if !anthropic_content.is_empty() {
                        messages.push(serde_json::json!({
                            "role": anthropic_role,
                            "content": if anthropic_content.len() == 1 {
                                anthropic_content[0].clone()
                            } else {
                                serde_json::json!(anthropic_content)
                            },
                        }));
                    }
                }
                modelwire_core::CanonicalInputItem::FunctionCallOutput { call_id, output } => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": output,
                        }],
                    }));
                }
                modelwire_core::CanonicalInputItem::AssistantFunctionCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let parsed_arguments = serde_json::from_str::<serde_json::Value>(arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": [{
                            "type": "tool_use",
                            "id": call_id,
                            "name": name,
                            "input": parsed_arguments,
                        }],
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": canonical.upstream_model,
            "messages": messages,
            "stream": canonical.stream,
        });

        // Add system prompt from instructions
        if let Some(ref instructions) = canonical.instructions {
            body["system"] = serde_json::json!(instructions.content);
        }

        // Add tools if present
        if !canonical.tools.is_empty() {
            body["tools"] = serde_json::json!(canonical
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect::<Vec<_>>());
        }

        // Add max_tokens
        if let Some(max_tokens) = canonical.max_output_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        // Add temperature
        if let Some(temp) = canonical.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        debug!(
            stream = canonical.stream,
            messages = body
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0),
            has_tools = !canonical.tools.is_empty(),
            "Built Anthropic request"
        );

        UpstreamRequest {
            method: "POST".to_string(),
            path: "/messages".to_string(),
            headers,
            body,
        }
    }

    async fn parse_response(&self, body: &[u8]) -> Result<Vec<CanonicalEvent>, UpstreamError> {
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| UpstreamError::ParseError(format!("Invalid JSON: {}", e)))?;

        let mut events = Vec::new();
        let response_id = json.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let model = json.get("model").and_then(|v| v.as_str()).unwrap_or("");

        // Response created
        events.push(CanonicalEvent::ResponseCreated {
            response_id: response_id.to_string(),
            model: model.to_string(),
            created_at: super::now_timestamp(),
        });

        // Parse content blocks
        if let Some(content) = json.get("content").and_then(|v| v.as_array()) {
            for block in content {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("text");

                match block_type {
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        let item_id = modelwire_core::generate_message_id();

                        events.push(CanonicalEvent::OutputItemAdded {
                            response_id: response_id.to_string(),
                            item: modelwire_core::CanonicalOutputItem::Message {
                                id: item_id.clone(),
                                role: "assistant".to_string(),
                                content: vec![modelwire_core::ContentBlock::Text {
                                    text: text.to_string(),
                                }],
                            },
                        });

                        events.push(CanonicalEvent::OutputTextDelta {
                            item_id: item_id.clone(),
                            delta: text.to_string(),
                        });

                        events.push(CanonicalEvent::OutputItemDone {
                            response_id: response_id.to_string(),
                            item: modelwire_core::CanonicalOutputItem::Message {
                                id: item_id.clone(),
                                role: "assistant".to_string(),
                                content: vec![modelwire_core::ContentBlock::Text {
                                    text: text.to_string(),
                                }],
                            },
                        });
                    }
                    "tool_use" => {
                        let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let input = block
                            .get("input")
                            .map(|v| serde_json::to_string(v).unwrap_or_default())
                            .unwrap_or_default();

                        let item_id = modelwire_core::generate_call_id();

                        events.push(CanonicalEvent::OutputItemAdded {
                            response_id: response_id.to_string(),
                            item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                id: item_id.clone(),
                                call_id: id.to_string(),
                                name: name.to_string(),
                                arguments: input.clone(),
                            },
                        });

                        events.push(CanonicalEvent::FunctionCallArgumentsDelta {
                            item_id: item_id.clone(),
                            delta: input.clone(),
                        });

                        events.push(CanonicalEvent::OutputItemDone {
                            response_id: response_id.to_string(),
                            item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                id: item_id.clone(),
                                call_id: id.to_string(),
                                name: name.to_string(),
                                arguments: input,
                            },
                        });
                    }
                    _ => {}
                }
            }
        }

        // Usage
        let usage =
            json.get("usage")
                .and_then(|v| v.as_object())
                .map(|u| modelwire_core::ResponseUsage {
                    input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    reasoning_tokens: u.get("thinking_tokens").and_then(|v| v.as_u64()),
                });

        // Response completed
        let output: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                CanonicalEvent::OutputItemDone { item, .. } => Some(item.clone()),
                _ => None,
            })
            .collect();

        events.push(CanonicalEvent::ResponseCompleted {
            response_id: response_id.to_string(),
            output,
            usage,
        });

        Ok(events)
    }

    fn parse_sse_event(
        &self,
        event_type: &str,
        data: &[u8],
    ) -> Result<Option<CanonicalEvent>, UpstreamError> {
        let json: serde_json::Value = serde_json::from_slice(data)
            .map_err(|e| UpstreamError::ParseError(format!("Invalid SSE data: {}", e)))?;

        match event_type {
            "message_start" => {
                let response_id = json
                    .get("message")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let model = json
                    .get("message")
                    .and_then(|v| v.get("model"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                Ok(Some(CanonicalEvent::ResponseCreated {
                    response_id: response_id.to_string(),
                    model: model.to_string(),
                    created_at: super::now_timestamp(),
                }))
            }
            "content_block_start" => {
                let item = json.get("content_block").ok_or_else(|| {
                    UpstreamError::InvalidResponse("missing content_block".to_string())
                })?;
                let block_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                if block_type == "text" {
                    let item_id = modelwire_core::generate_message_id();
                    Ok(Some(CanonicalEvent::OutputItemAdded {
                        response_id: "".to_string(),
                        item: modelwire_core::CanonicalOutputItem::Message {
                            id: item_id,
                            role: "assistant".to_string(),
                            content: vec![],
                        },
                    }))
                } else {
                    Ok(None)
                }
            }
            "content_block_delta" => {
                let delta_type = json
                    .get("delta")
                    .and_then(|v| v.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

                if delta_type == "text_delta" {
                    let text = json
                        .get("delta")
                        .and_then(|v| v.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    Ok(Some(CanonicalEvent::OutputTextDelta {
                        item_id: index.to_string(),
                        delta: text.to_string(),
                    }))
                } else if delta_type == "input_json_delta" {
                    let delta = json
                        .get("delta")
                        .and_then(|v| v.get("partial_json"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    Ok(Some(CanonicalEvent::FunctionCallArgumentsDelta {
                        item_id: index.to_string(),
                        delta: delta.to_string(),
                    }))
                } else {
                    Ok(None)
                }
            }
            "content_block_stop" => Ok(None),
            "message_stop" => Ok(None),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modelwire_core::CanonicalInputItem;

    #[test]
    fn test_build_request() {
        let adapter = AnthropicAdapter::new();
        let canonical = CanonicalResponseRequest {
            request_id: "req_mw_test".to_string(),
            downstream_model: "claude-3".to_string(),
            upstream_model: "claude-3-sonnet-20240229".to_string(),
            instructions: Some(modelwire_core::CanonicalInstructions {
                role: "system".to_string(),
                content: "You are helpful.".to_string(),
            }),
            input: vec![CanonicalInputItem::Text {
                content: "Hello".to_string(),
            }],
            previous_response_id: None,
            tools: vec![],
            tool_choice: modelwire_core::CanonicalToolChoice::Auto,
            parallel_tool_calls: false,
            max_output_tokens: Some(1024),
            temperature: None,
            top_p: None,
            stream: false,
            reasoning: None,
            include: vec![],
            metadata: serde_json::Value::Null,
            store: false,
            raw_downstream: serde_json::Value::Null,
        };

        let request = adapter.build_request(
            &canonical,
            "https://api.anthropic.com/v1",
            Some("sk-ant-api03"),
        );

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/messages");
        assert!(request.headers.iter().any(|(k, _)| k == "x-api-key"));
        assert!(request
            .headers
            .iter()
            .any(|(k, _)| k == "anthropic-version"));
        assert_eq!(request.body["model"], "claude-3-sonnet-20240229");
    }

    #[test]
    fn test_wire_api() {
        let adapter = AnthropicAdapter::new();
        assert_eq!(adapter.wire_api(), WireApi::Anthropic);
    }
}
