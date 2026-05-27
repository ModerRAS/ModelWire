//! OpenAI Chat Completions adapter.

use super::{UpstreamAdapter, UpstreamError, UpstreamRequest};
use async_trait::async_trait;
use modelwire_core::{CanonicalEvent, CanonicalResponseRequest, WireApi};
use tracing::{debug, instrument};

/// OpenAI Chat Completions adapter.
pub struct OpenAiChatAdapter;

impl OpenAiChatAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenAiChatAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UpstreamAdapter for OpenAiChatAdapter {
    fn wire_api(&self) -> WireApi {
        WireApi::OpenAiChat
    }

    #[instrument(skip(self, canonical), fields(upstream_model = %canonical.upstream_model))]
    fn build_request(
        &self,
        canonical: &CanonicalResponseRequest,
        base_url: &str,
        api_key: Option<&str>,
    ) -> UpstreamRequest {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];

        if let Some(key) = api_key {
            headers.push(("Authorization".to_string(), format!("Bearer {}", key)));
        }

        let mut messages = Vec::new();

        // Add instructions as system message
        if let Some(ref instructions) = canonical.instructions {
            messages.push(serde_json::json!({
                "role": instructions.role.as_str().trim_end_matches(" ").trim_end_matches("_").to_lowercase(),
                "content": instructions.content,
            }));
        }

        // Map input items to chat messages
        for item in &canonical.input {
            match item {
                modelwire_core::CanonicalInputItem::Text { content } => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                modelwire_core::CanonicalInputItem::Message { role, content } => {
                    let chat_role = match role.as_str() {
                        "system" | "developer" => "system",
                        "user" => "user",
                        "assistant" => "assistant",
                        _ => "user",
                    };

                    let chat_content: Vec<_> = content
                        .iter()
                        .filter_map(|block| match block {
                            modelwire_core::ContentBlock::Text { text } => {
                                Some(serde_json::json!({ "type": "text", "text": text }))
                            }
                            _ => None,
                        })
                        .collect();

                    messages.push(serde_json::json!({
                        "role": chat_role,
                        "content": if chat_content.len() == 1 {
                            serde_json::Value::String(chat_content[0].get("text").and_then(|v| v.as_str()).unwrap_or("").to_string())
                        } else {
                            serde_json::json!(chat_content)
                        },
                    }));
                }
                modelwire_core::CanonicalInputItem::FunctionCallOutput { call_id, output } => {
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output,
                    }));
                }
                modelwire_core::CanonicalInputItem::AssistantFunctionCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": serde_json::Value::Null,
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments,
                            }
                        }]
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": canonical.upstream_model,
            "messages": messages,
            "stream": canonical.stream,
        });

        // Add tools if present
        if !canonical.tools.is_empty() {
            body["tools"] = serde_json::json!(canonical
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect::<Vec<_>>());
        }

        // Add tool_choice
        match canonical.tool_choice {
            modelwire_core::CanonicalToolChoice::Auto => {}
            modelwire_core::CanonicalToolChoice::None => {
                body["tool_choice"] = serde_json::json!("none");
            }
            modelwire_core::CanonicalToolChoice::Specific(ref name) => {
                body["tool_choice"] = serde_json::json!({
                    "type": "function",
                    "function": { "name": name },
                });
            }
        }

        // Add generation parameters
        if let Some(max_tokens) = canonical.max_output_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = canonical.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(top_p) = canonical.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }

        debug!(
            stream = canonical.stream,
            messages = body
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0),
            has_tools = !canonical.tools.is_empty(),
            "Built Chat request"
        );

        UpstreamRequest {
            method: "POST".to_string(),
            path: "/chat/completions".to_string(),
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

        // Parse choices
        if let Some(choices) = json.get("choices").and_then(|v| v.as_array()) {
            for choice in choices {
                let message = choice
                    .get("message")
                    .ok_or_else(|| UpstreamError::InvalidResponse("missing message".to_string()))?;
                let role = message
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant");

                if role == "assistant" {
                    // Text content
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        if !content.is_empty() {
                            let item_id = modelwire_core::generate_message_id();

                            events.push(CanonicalEvent::OutputItemAdded {
                                response_id: response_id.to_string(),
                                item: modelwire_core::CanonicalOutputItem::Message {
                                    id: item_id.clone(),
                                    role: "assistant".to_string(),
                                    content: vec![modelwire_core::ContentBlock::Text {
                                        text: content.to_string(),
                                    }],
                                },
                            });

                            events.push(CanonicalEvent::OutputTextDelta {
                                item_id: item_id.clone(),
                                delta: content.to_string(),
                            });

                            events.push(CanonicalEvent::OutputItemDone {
                                response_id: response_id.to_string(),
                                item: modelwire_core::CanonicalOutputItem::Message {
                                    id: item_id.clone(),
                                    role: "assistant".to_string(),
                                    content: vec![modelwire_core::ContentBlock::Text {
                                        text: content.to_string(),
                                    }],
                                },
                            });
                        }
                    }

                    // Tool calls
                    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                        for tool_call in tool_calls {
                            let id = tool_call.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = tool_call
                                .get("function")
                                .and_then(|v| v.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let arguments = tool_call
                                .get("function")
                                .and_then(|v| v.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");

                            let item_id = modelwire_core::generate_call_id();

                            events.push(CanonicalEvent::OutputItemAdded {
                                response_id: response_id.to_string(),
                                item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                    id: item_id.clone(),
                                    call_id: id.to_string(),
                                    name: name.to_string(),
                                    arguments: arguments.to_string(),
                                },
                            });

                            events.push(CanonicalEvent::FunctionCallArgumentsDelta {
                                item_id: item_id.clone(),
                                delta: arguments.to_string(),
                            });

                            events.push(CanonicalEvent::OutputItemDone {
                                response_id: response_id.to_string(),
                                item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                    id: item_id.clone(),
                                    call_id: id.to_string(),
                                    name: name.to_string(),
                                    arguments: arguments.to_string(),
                                },
                            });
                        }
                    }
                }
            }
        }

        // Usage
        let usage =
            json.get("usage")
                .and_then(|v| v.as_object())
                .map(|u| modelwire_core::ResponseUsage {
                    input_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    output_tokens: u
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    reasoning_tokens: None,
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
            "[DONE]" => Ok(None),
            "" | "delta" | "chat_chunk" => {
                // Chat completions streaming format
                let (delta, index) = if let Some(choice) = json
                    .get("choices")
                    .and_then(|value| value.as_array())
                    .and_then(|choices| choices.first())
                {
                    (
                        choice.get("delta").ok_or_else(|| {
                            UpstreamError::InvalidResponse("missing choices[0].delta".to_string())
                        })?,
                        choice
                            .get("index")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0),
                    )
                } else {
                    (
                        json.get("delta").ok_or_else(|| {
                            UpstreamError::InvalidResponse("missing delta".to_string())
                        })?,
                        json.get("index")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0),
                    )
                };

                // Content delta
                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                    let item_id = format!("chat_stream_item_{index}");
                    return Ok(Some(CanonicalEvent::OutputTextDelta {
                        item_id,
                        delta: content.to_string(),
                    }));
                }

                if let Some(tool_calls) = delta.get("tool_calls").and_then(|value| value.as_array())
                {
                    if let Some(tool_call) = tool_calls.first() {
                        let tool_index = tool_call
                            .get("index")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0);
                        let item_id = format!("chat_stream_tool_{tool_index}");
                        let call_id = tool_call
                            .get("id")
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        let function = tool_call.get("function");
                        let name = function
                            .and_then(|function| function.get("name"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        if !call_id.is_empty() || !name.is_empty() {
                            return Ok(Some(CanonicalEvent::OutputItemAdded {
                                response_id: "".to_string(),
                                item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                    id: item_id,
                                    call_id: call_id.to_string(),
                                    name: name.to_string(),
                                    arguments: String::new(),
                                },
                            }));
                        }
                        if let Some(arguments) = tool_call
                            .get("function")
                            .and_then(|function| function.get("arguments"))
                            .and_then(|value| value.as_str())
                        {
                            return Ok(Some(CanonicalEvent::FunctionCallArgumentsDelta {
                                item_id,
                                delta: arguments.to_string(),
                            }));
                        }
                    }
                }

                Ok(None)
            }
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
        let adapter = OpenAiChatAdapter::new();
        let canonical = CanonicalResponseRequest {
            request_id: "req_mw_test".to_string(),
            downstream_model: "gpt-4".to_string(),
            upstream_model: "gpt-4".to_string(),
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

        let request =
            adapter.build_request(&canonical, "https://api.openai.com/v1", Some("sk-test"));

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/chat/completions");
        assert!(request.body["messages"].is_array());
        assert_eq!(request.body["model"], "gpt-4");
    }

    #[test]
    fn test_wire_api() {
        let adapter = OpenAiChatAdapter::new();
        assert_eq!(adapter.wire_api(), WireApi::OpenAiChat);
    }
}
