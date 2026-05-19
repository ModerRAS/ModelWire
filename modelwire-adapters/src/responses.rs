//! Native OpenAI Responses adapter.

use super::{UpstreamAdapter, UpstreamError, UpstreamRequest};
use async_trait::async_trait;
use modelwire_core::{CanonicalEvent, CanonicalResponseRequest, WireApi};
use tracing::{debug, instrument};

/// Native Responses adapter.
pub struct ResponsesAdapter;

impl ResponsesAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ResponsesAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UpstreamAdapter for ResponsesAdapter {
    fn wire_api(&self) -> WireApi {
        WireApi::Responses
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

        let mut body = serde_json::json!({
            "model": canonical.upstream_model,
            "stream": canonical.stream,
        });

        // Add instructions
        if let Some(ref instructions) = canonical.instructions {
            body["instructions"] = serde_json::json!({
                "role": instructions.role,
                "content": instructions.content,
            });
        }

        // Add input
        if canonical.input.len() == 1 {
            if let modelwire_core::CanonicalInputItem::Text { ref content } = canonical.input[0] {
                body["input"] = serde_json::json!(content);
            }
        } else {
            let input_items: Vec<_> = canonical
                .input
                .iter()
                .map(|item| match item {
                    modelwire_core::CanonicalInputItem::Text { content } => {
                        serde_json::json!({ "type": "text", "text": content })
                    }
                    modelwire_core::CanonicalInputItem::Message { role, content } => {
                        serde_json::json!({
                            "type": "message",
                            "role": role,
                            "content": content,
                        })
                    }
                    modelwire_core::CanonicalInputItem::FunctionCallOutput { call_id, output } => {
                        serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output,
                        })
                    }
                })
                .collect();
            body["input"] = serde_json::json!(input_items);
        }

        // Add previous_response_id if available
        if let Some(ref prev_id) = canonical.previous_response_id {
            body["previous_response_id"] = serde_json::json!(prev_id);
        }

        // Add tools
        if !canonical.tools.is_empty() {
            body["tools"] = serde_json::json!(canonical
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect::<Vec<_>>());
        }

        // Add tool choice
        match canonical.tool_choice {
            modelwire_core::CanonicalToolChoice::Auto => {
                body["tool_choice"] = serde_json::json!("auto");
            }
            modelwire_core::CanonicalToolChoice::None => {
                body["tool_choice"] = serde_json::json!("none");
            }
            modelwire_core::CanonicalToolChoice::Specific(ref name) => {
                body["tool_choice"] = serde_json::json!({
                    "type": "function",
                    "name": name,
                });
            }
        }

        // Add generation parameters
        if let Some(max_tokens) = canonical.max_output_tokens {
            body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = canonical.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(top_p) = canonical.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }

        // Add reasoning options if present
        if let Some(ref reasoning) = canonical.reasoning {
            let mut reasoning_obj = serde_json::json!({});
            if let Some(include_summary) = reasoning.include_summary {
                reasoning_obj["include_summary"] = serde_json::json!(include_summary);
            }
            if let Some(include_encrypted) = reasoning.include_encrypted_content {
                reasoning_obj["include_encrypted_content"] = serde_json::json!(include_encrypted);
            }
            if let Some(effort) = &reasoning.effort {
                reasoning_obj["effort"] = serde_json::json!(effort);
            }
            if !reasoning_obj.is_array() || !reasoning_obj.as_array().unwrap().is_empty() {
                body["reasoning"] = reasoning_obj;
            }
        }

        // Add include fields
        if !canonical.include.is_empty() {
            body["include"] = serde_json::json!(canonical.include);
        }

        debug!(body = %serde_json::to_string_pretty(&body).unwrap_or_default(), "Built Responses request");

        UpstreamRequest {
            method: "POST".to_string(),
            path: "/responses".to_string(),
            headers,
            body,
        }
    }

    async fn parse_response(&self, body: &[u8]) -> Result<Vec<CanonicalEvent>, UpstreamError> {
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| UpstreamError::ParseError(format!("Invalid JSON: {}", e)))?;

        // Parse Responses JSON into canonical events
        let mut events = Vec::new();

        // Response created event
        if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
            let model = json.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let created = json
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or_else(super::now_timestamp);

            events.push(CanonicalEvent::ResponseCreated {
                response_id: id.to_string(),
                model: model.to_string(),
                created_at: created,
            });
        }

        // Output items
        if let Some(output) = json.get("output").and_then(|v| v.as_array()) {
            for item in output {
                let item_type = item
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("message");

                match item_type {
                    "message" => {
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let role = item
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("assistant");
                        let content = item.get("content").and_then(|v| v.as_array());

                        let mut content_blocks = Vec::new();
                        if let Some(blocks) = content {
                            for block in blocks {
                                let block_type =
                                    block.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                                if block_type == "output_text" {
                                    let text =
                                        block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                    content_blocks.push(modelwire_core::ContentBlock::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                        }

                        events.push(CanonicalEvent::OutputItemAdded {
                            response_id: json
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            item: modelwire_core::CanonicalOutputItem::Message {
                                id: id.to_string(),
                                role: role.to_string(),
                                content: content_blocks.clone(),
                            },
                        });

                        events.push(CanonicalEvent::OutputItemDone {
                            response_id: json
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            item: modelwire_core::CanonicalOutputItem::Message {
                                id: id.to_string(),
                                role: role.to_string(),
                                content: content_blocks.clone(),
                            },
                        });
                    }
                    "function_call" => {
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let arguments = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");

                        events.push(CanonicalEvent::OutputItemAdded {
                            response_id: json
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                id: id.to_string(),
                                call_id: call_id.to_string(),
                                name: name.to_string(),
                                arguments: arguments.to_string(),
                            },
                        });

                        events.push(CanonicalEvent::OutputItemDone {
                            response_id: json
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            item: modelwire_core::CanonicalOutputItem::FunctionCall {
                                id: id.to_string(),
                                call_id: call_id.to_string(),
                                name: name.to_string(),
                                arguments: arguments.to_string(),
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
                    reasoning_tokens: u.get("reasoning_tokens").and_then(|v| v.as_u64()),
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
            response_id: json
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
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
            "response.created" => {
                let response_id = json
                    .get("response")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let model = json
                    .get("response")
                    .and_then(|v| v.get("model"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let created = json
                    .get("response")
                    .and_then(|v| v.get("created_at"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);

                Ok(Some(CanonicalEvent::ResponseCreated {
                    response_id: response_id.to_string(),
                    model: model.to_string(),
                    created_at: created,
                }))
            }
            "response.output_item.added" => {
                let item = json
                    .get("item")
                    .ok_or_else(|| UpstreamError::InvalidResponse("missing item".to_string()))?;
                let output_item = Self::parse_output_item(item)?;

                Ok(Some(CanonicalEvent::OutputItemAdded {
                    response_id: json
                        .get("response_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    item: output_item,
                }))
            }
            "response.text.delta" => {
                let item_id = json.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                let delta = json
                    .get("delta")
                    .and_then(|v| v.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                Ok(Some(CanonicalEvent::OutputTextDelta {
                    item_id: item_id.to_string(),
                    delta: delta.to_string(),
                }))
            }
            "response.function_call_arguments.delta" => {
                let item_id = json.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                let delta = json
                    .get("delta")
                    .and_then(|v| v.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                Ok(Some(CanonicalEvent::FunctionCallArgumentsDelta {
                    item_id: item_id.to_string(),
                    delta: delta.to_string(),
                }))
            }
            "response.output_item.done" => {
                let item = json
                    .get("item")
                    .ok_or_else(|| UpstreamError::InvalidResponse("missing item".to_string()))?;
                let output_item = Self::parse_output_item(item)?;

                Ok(Some(CanonicalEvent::OutputItemDone {
                    response_id: json
                        .get("response_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    item: output_item,
                }))
            }
            "response.completed" => {
                let response_id = json
                    .get("response")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output: Vec<_> = json
                    .get("response")
                    .and_then(|v| v.get("output"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| Self::parse_output_item(item).ok())
                            .collect()
                    })
                    .unwrap_or_default();

                let usage = json
                    .get("response")
                    .and_then(|v| v.get("usage"))
                    .and_then(|v| v.as_object())
                    .map(|u| modelwire_core::ResponseUsage {
                        input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        reasoning_tokens: u.get("reasoning_tokens").and_then(|v| v.as_u64()),
                    });

                Ok(Some(CanonicalEvent::ResponseCompleted {
                    response_id: response_id.to_string(),
                    output,
                    usage,
                }))
            }
            _ => Ok(None),
        }
    }
}

impl ResponsesAdapter {
    fn parse_output_item(
        item: &serde_json::Value,
    ) -> Result<modelwire_core::CanonicalOutputItem, UpstreamError> {
        let item_type = item
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("message");

        match item_type {
            "message" => {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let role = item
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant");
                let content = item.get("content").and_then(|v| v.as_array());

                let mut content_blocks = Vec::new();
                if let Some(blocks) = content {
                    for block in blocks {
                        let block_type =
                            block.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                        if block_type == "output_text" {
                            let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            content_blocks.push(modelwire_core::ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }

                Ok(modelwire_core::CanonicalOutputItem::Message {
                    id: id.to_string(),
                    role: role.to_string(),
                    content: content_blocks,
                })
            }
            "function_call" => {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");

                Ok(modelwire_core::CanonicalOutputItem::FunctionCall {
                    id: id.to_string(),
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                })
            }
            _ => Err(UpstreamError::InvalidResponse(format!(
                "Unknown item type: {}",
                item_type
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modelwire_core::CanonicalInputItem;

    #[test]
    fn test_build_request() {
        let adapter = ResponsesAdapter::new();
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
            temperature: Some(0.7),
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
        assert_eq!(request.path, "/responses");
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v.contains("sk-test")));
        assert_eq!(request.body["model"], "gpt-4");
    }

    #[test]
    fn test_wire_api() {
        let adapter = ResponsesAdapter::new();
        assert_eq!(adapter.wire_api(), WireApi::Responses);
    }
}
