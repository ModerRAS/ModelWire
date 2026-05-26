//! Slice tests for ModelWire adapters.
//!
//! These tests verify the adapter behavior by:
//! 1. Creating canonical requests
//! 2. Building upstream requests
//! 3. Mocking the upstream HTTP server with wiremock
//! 4. Verifying what was sent upstream
//! 5. Verifying response parsing

use bytes::BytesMut;
use modelwire_adapters::{
    anthropic::AnthropicAdapter,
    openai_chat::OpenAiChatAdapter,
    responses::ResponsesAdapter,
    sse::{extract_sse_frames, SseEventType},
    UpstreamAdapter,
};
use modelwire_core::{
    CanonicalInputItem, CanonicalOutputItem, CanonicalResponseRequest, CanonicalTool,
    CanonicalToolChoice, ContentBlock, WireApi,
};
use serde_json::json;

/// Test helper: create a basic canonical request.
fn basic_canonical_request() -> CanonicalResponseRequest {
    CanonicalResponseRequest {
        request_id: "req_test_001".to_string(),
        downstream_model: "test-model".to_string(),
        upstream_model: "test-model".to_string(),
        instructions: Some(modelwire_core::CanonicalInstructions {
            role: "system".to_string(),
            content: "You are a helpful assistant.".to_string(),
        }),
        input: vec![CanonicalInputItem::Text {
            content: "Hello, world!".to_string(),
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
    }
}

// ============================================================================
// Responses Adapter Tests
// ============================================================================

mod responses_adapter_tests {
    use super::*;

    #[test]
    fn test_responses_adapter_wire_api() {
        let adapter = ResponsesAdapter::new();
        assert_eq!(adapter.wire_api(), WireApi::Responses);
    }

    #[test]
    fn test_responses_build_request_basic() {
        let adapter = ResponsesAdapter::new();
        let canonical = basic_canonical_request();

        let request =
            adapter.build_request(&canonical, "https://api.openai.com/v1", Some("sk-test-key"));

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/responses");

        // Verify headers
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v.contains("sk-test-key")));

        // Verify body structure
        let body = &request.body;
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], false);
        assert!(body.get("instructions").is_some());
        assert!(body.get("input").is_some());
    }

    #[test]
    fn test_responses_build_request_with_tools() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tools = vec![CanonicalTool {
            name: "get_weather".to_string(),
            description: "Get weather for a location".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "The city name"
                    }
                },
                "required": ["location"]
            }),
        }];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("tools").is_some());
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["type"], "function");
    }

    #[test]
    fn test_responses_build_request_with_previous_response() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.previous_response_id = Some("prev_resp_123".to_string());

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("previous_response_id").is_some());
        assert_eq!(body["previous_response_id"], "prev_resp_123");
    }

    #[test]
    fn test_responses_build_request_streaming() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.stream = true;

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn test_responses_parse_response_basic() {
        let adapter = ResponsesAdapter::new();
        let body = r#"{
            "id": "resp_test_001",
            "model": "gpt-4",
            "created_at": 1234567890,
            "output": [
                {
                    "type": "message",
                    "id": "msg_001",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Hello! How can I help you?"}
                    ]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "total_tokens": 30
            }
        }"#;

        let events = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body.as_bytes()));

        let events = events.unwrap();
        assert!(events.len() >= 3); // ResponseCreated + OutputItemAdded + OutputItemDone + ResponseCompleted

        // Find ResponseCompleted event
        let completed = events
            .iter()
            .find(|e| matches!(e, modelwire_core::CanonicalEvent::ResponseCompleted { .. }))
            .unwrap();

        if let modelwire_core::CanonicalEvent::ResponseCompleted {
            response_id,
            output,
            usage,
        } = completed
        {
            assert_eq!(response_id, "resp_test_001");
            assert!(!output.is_empty());
            assert!(usage.is_some());
            let u = usage.as_ref().unwrap();
            assert_eq!(u.input_tokens, 10);
            assert_eq!(u.output_tokens, 20);
        }
    }

    #[test]
    fn test_responses_parse_response_with_function_call() {
        let adapter = ResponsesAdapter::new();
        let body = r#"{
            "id": "resp_test_002",
            "model": "gpt-4",
            "created_at": 1234567890,
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_001",
                    "call_id": "call_abc123",
                    "name": "get_weather",
                    "arguments": "{\"location\":\"Boston\"}"
                }
            ],
            "usage": {
                "input_tokens": 15,
                "output_tokens": 25,
                "total_tokens": 40
            }
        }"#;

        let events = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body.as_bytes()));

        let events = events.unwrap();

        // Find the function call output item
        let fc_item = events.iter().find(|e| {
            matches!(
                e,
                modelwire_core::CanonicalEvent::OutputItemAdded {
                    item: modelwire_core::CanonicalOutputItem::FunctionCall { .. },
                    ..
                }
            )
        });

        assert!(fc_item.is_some(), "Should have a function call item");
    }

    #[test]
    fn test_responses_parse_sse_event_created() {
        let adapter = ResponsesAdapter::new();
        let data = r#"{"response":{"id":"resp_001","model":"test","created_at":123}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("response.created", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::ResponseCreated {
                response_id,
                model,
                created_at,
            } => {
                assert_eq!(response_id, "resp_001");
                assert_eq!(model, "test");
                assert_eq!(created_at, 123);
            }
            _ => panic!("Expected ResponseCreated event"),
        }
    }

    #[test]
    fn test_responses_parse_sse_text_delta() {
        let adapter = ResponsesAdapter::new();
        let data = r#"{"item_id":"msg_001","delta":{"text":"Hello"}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("response.text.delta", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::OutputTextDelta { item_id, delta } => {
                assert_eq!(item_id, "msg_001");
                assert_eq!(delta, "Hello");
            }
            _ => panic!("Expected OutputTextDelta event"),
        }
    }

    #[test]
    fn test_responses_parse_sse_function_call_delta() {
        let adapter = ResponsesAdapter::new();
        let data = r#"{"item_id":"fc_001","delta":{"arguments":"{\"loc"}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("response.function_call_arguments.delta", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::FunctionCallArgumentsDelta { item_id, delta } => {
                assert_eq!(item_id, "fc_001");
                assert_eq!(delta, "{\"loc");
            }
            _ => panic!("Expected FunctionCallArgumentsDelta event"),
        }
    }

    #[test]
    fn test_responses_parse_sse_output_item_added() {
        let adapter = ResponsesAdapter::new();
        let json_str = json!({
            "response_id": "resp_001",
            "item": {
                "type": "message",
                "id": "msg_001",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hi"}]
            }
        })
        .to_string();
        let data = json_str.as_bytes();

        let event = adapter
            .parse_sse_event("response.output_item.added", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::OutputItemAdded { response_id, item } => {
                assert_eq!(response_id, "resp_001");
                match item {
                    CanonicalOutputItem::Message { id, .. } => {
                        assert_eq!(id, "msg_001");
                    }
                    _ => panic!("Expected Message item"),
                }
            }
            _ => panic!("Expected OutputItemAdded event"),
        }
    }

    #[test]
    fn test_responses_build_request_with_reasoning() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.reasoning = Some(modelwire_core::CanonicalReasoningOptions {
            include_summary: Some(true),
            include_encrypted_content: Some(false),
            effort: Some("medium".to_string()),
        });

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("reasoning").is_some());
        let reasoning = &body["reasoning"];
        assert_eq!(reasoning["include_summary"], true);
        assert_eq!(reasoning["effort"], "medium");
    }

    #[test]
    fn test_responses_build_request_multiple_inputs() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![
            CanonicalInputItem::Text {
                content: "First message".to_string(),
            },
            CanonicalInputItem::Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Second message".to_string(),
                }],
            },
        ];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
    }

    #[test]
    fn test_responses_build_request_with_assistant_function_call_replay_item() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![
            CanonicalInputItem::AssistantFunctionCall {
                call_id: "call_replay_1".to_string(),
                name: "lookup_weather".to_string(),
                arguments: "{\"city\":\"Boston\"}".to_string(),
            },
            CanonicalInputItem::FunctionCallOutput {
                call_id: "call_replay_1".to_string(),
                output: "{\"temperature\":72}".to_string(),
            },
        ];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);
        let input = request
            .body
            .get("input")
            .and_then(serde_json::Value::as_array)
            .expect("responses input should be an array");
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "call_replay_1");
        assert_eq!(input[0]["name"], "lookup_weather");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], "call_replay_1");
    }

    #[test]
    fn test_responses_build_request_with_single_function_call_output() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![CanonicalInputItem::FunctionCallOutput {
            call_id: "call_replay_1".to_string(),
            output: "{\"temperature\":72}".to_string(),
        }];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);
        let input = request
            .body
            .get("input")
            .and_then(serde_json::Value::as_array)
            .expect("single non-text input should be serialized as an input array");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_replay_1");
    }

    #[test]
    fn test_responses_build_request_tool_choice_none() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tool_choice = CanonicalToolChoice::None;

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert_eq!(body["tool_choice"], "none");
    }

    #[test]
    fn test_responses_build_request_tool_choice_specific() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tool_choice = CanonicalToolChoice::Specific("get_weather".to_string());

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["name"], "get_weather");
    }

    #[test]
    fn test_responses_parse_sse_unknown_event_type() {
        let adapter = ResponsesAdapter::new();
        let data = r#"{"some":"data"}"#.as_bytes();

        let result = adapter.parse_sse_event("unknown.event.type", data);

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}

// ============================================================================
// Anthropic Adapter Tests
// ============================================================================

mod anthropic_adapter_tests {
    use super::*;

    #[test]
    fn test_anthropic_adapter_wire_api() {
        let adapter = AnthropicAdapter::new();
        assert_eq!(adapter.wire_api(), WireApi::Anthropic);
    }

    #[test]
    fn test_anthropic_build_request_basic() {
        let adapter = AnthropicAdapter::new();
        let canonical = basic_canonical_request();

        let request = adapter.build_request(
            &canonical,
            "https://api.anthropic.com/v1",
            Some("sk-ant-key"),
        );

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/messages");

        // Verify headers
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
        assert!(request.headers.iter().any(|(k, _)| k == "x-api-key"));
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));

        // Verify body structure
        let body = &request.body;
        assert_eq!(body["model"], "test-model");
        assert!(body.get("messages").is_some());
        assert!(body.get("system").is_some());
    }

    #[test]
    fn test_anthropic_build_request_with_tools() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tools = vec![CanonicalTool {
            name: "get_weather".to_string(),
            description: "Get weather for a location".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string"
                    }
                }
            }),
        }];

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);

        let body = &request.body;
        assert!(body.get("tools").is_some());
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        // Anthropic uses input_schema instead of parameters
        assert!(tools[0].get("input_schema").is_some());
    }

    #[test]
    fn test_anthropic_build_request_message_input() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![
            CanonicalInputItem::Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Hello from message".to_string(),
                }],
            },
            CanonicalInputItem::Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "I can help".to_string(),
                }],
            },
        ];

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);

        let body = &request.body;
        let messages = body["messages"].as_array().unwrap();
        assert!(messages.len() >= 2);
    }

    #[test]
    fn test_anthropic_build_request_function_call_output() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![CanonicalInputItem::FunctionCallOutput {
            call_id: "call_123".to_string(),
            output: "Sunny, 72 degrees".to_string(),
        }];

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);

        let body = &request.body;
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "call_123");
    }

    #[test]
    fn test_anthropic_build_request_assistant_tool_use_before_tool_result() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![
            CanonicalInputItem::AssistantFunctionCall {
                call_id: "call_123".to_string(),
                name: "lookup_weather".to_string(),
                arguments: "{\"city\":\"Boston\"}".to_string(),
            },
            CanonicalInputItem::FunctionCallOutput {
                call_id: "call_123".to_string(),
                output: "{\"forecast\":\"sunny\"}".to_string(),
            },
        ];

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);
        let messages = request
            .body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .expect("anthropic messages should be an array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"][0]["type"], "tool_use");
        assert_eq!(messages[0]["content"][0]["id"], "call_123");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "call_123");
    }

    #[test]
    fn test_anthropic_build_request_streaming() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.stream = true;

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);

        let body = &request.body;
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn test_anthropic_parse_response_basic() {
        let adapter = AnthropicAdapter::new();
        let body = r#"{
            "id": "msg_test_001",
            "model": "claude-3",
            "type": "message",
            "content": [
                {"type": "text", "text": "Hello! I'm Claude."}
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 15,
                "total_tokens": 25
            }
        }"#;

        let events = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body.as_bytes()));

        let events = events.unwrap();
        assert!(events.len() >= 4); // ResponseCreated + OutputItemAdded + OutputTextDelta + OutputItemDone + ResponseCompleted

        let completed = events
            .iter()
            .find(|e| matches!(e, modelwire_core::CanonicalEvent::ResponseCompleted { .. }))
            .unwrap();

        if let modelwire_core::CanonicalEvent::ResponseCompleted {
            response_id, usage, ..
        } = completed
        {
            assert_eq!(response_id, "msg_test_001");
            assert!(usage.is_some());
        }
    }

    #[test]
    fn test_anthropic_parse_response_with_tool_use() {
        let adapter = AnthropicAdapter::new();
        let body = r#"{
            "id": "msg_test_002",
            "model": "claude-3",
            "type": "message",
            "content": [
                {"type": "tool_use", "id": "toolu_001", "name": "get_weather", "input": {"location": "Boston"}}
            ],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 8,
                "total_tokens": 20
            }
        }"#;

        let events = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body.as_bytes()));

        let events = events.unwrap();

        // Find function call event
        let fc_event = events.iter().find(|e| {
            matches!(
                e,
                modelwire_core::CanonicalEvent::OutputItemAdded {
                    item: modelwire_core::CanonicalOutputItem::FunctionCall { .. },
                    ..
                }
            )
        });

        assert!(fc_event.is_some(), "Should have a function call item");
    }

    #[test]
    fn test_anthropic_parse_sse_message_start() {
        let adapter = AnthropicAdapter::new();
        let data = r#"{"message":{"id":"msg_001","type":"message","model":"claude-3"}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("message_start", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::ResponseCreated {
                response_id, model, ..
            } => {
                assert_eq!(response_id, "msg_001");
                assert_eq!(model, "claude-3");
            }
            _ => panic!("Expected ResponseCreated event"),
        }
    }

    #[test]
    fn test_anthropic_parse_sse_content_block_delta_text() {
        let adapter = AnthropicAdapter::new();
        let data = r#"{"index":0,"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("content_block_delta", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::OutputTextDelta { item_id, delta } => {
                assert_eq!(item_id, "0");
                assert_eq!(delta, "Hello");
            }
            _ => panic!("Expected OutputTextDelta event"),
        }
    }

    #[test]
    fn test_openai_chat_parse_real_sse_choices_delta_without_event_name() {
        let adapter = OpenAiChatAdapter::new();
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#.as_bytes();

        let event = adapter.parse_sse_event("", data).unwrap().unwrap();

        match event {
            modelwire_core::CanonicalEvent::OutputTextDelta { item_id, delta } => {
                assert_eq!(item_id, "chat_stream_item_0");
                assert_eq!(delta, "Hello");
            }
            _ => panic!("Expected OutputTextDelta event"),
        }
    }

    #[test]
    fn test_openai_chat_parse_real_sse_tool_call_arguments_delta() {
        let adapter = OpenAiChatAdapter::new();
        let data = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "{\"city\"" }
                    }]
                }
            }]
        })
        .to_string();

        let event = adapter
            .parse_sse_event("", data.as_bytes())
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::FunctionCallArgumentsDelta { item_id, delta } => {
                assert_eq!(item_id, "chat_stream_tool_0");
                assert_eq!(delta, "{\"city\"");
            }
            _ => panic!("Expected FunctionCallArgumentsDelta event"),
        }
    }

    #[test]
    fn test_anthropic_parse_sse_content_block_delta_tool() {
        let adapter = AnthropicAdapter::new();
        let data = r#"{"index":0,"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"location\":"}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("content_block_delta", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::FunctionCallArgumentsDelta { item_id, delta } => {
                assert_eq!(item_id, "0");
                assert_eq!(delta, "{\"location\":");
            }
            _ => panic!("Expected FunctionCallArgumentsDelta event"),
        }
    }

    #[test]
    fn test_anthropic_parse_sse_content_block_start() {
        let adapter = AnthropicAdapter::new();
        let data = r#"{"content_block":{"type":"text"}}"#.as_bytes();

        let event = adapter
            .parse_sse_event("content_block_start", data)
            .unwrap();

        // Should return an OutputItemAdded for text blocks
        assert!(event.is_some());
    }

    #[test]
    fn test_anthropic_build_request_max_tokens() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.max_output_tokens = Some(4096);

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);

        let body = &request.body;
        assert!(body.get("max_tokens").is_some());
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn test_anthropic_build_request_temperature() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.temperature = Some(0.5);

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);

        let body = &request.body;
        assert!(body.get("temperature").is_some());
        assert_eq!(body["temperature"], 0.5);
    }
}

// ============================================================================
// OpenAI Chat Adapter Tests
// ============================================================================

mod openai_chat_adapter_tests {
    use super::*;

    #[test]
    fn test_openai_chat_adapter_wire_api() {
        let adapter = OpenAiChatAdapter::new();
        assert_eq!(adapter.wire_api(), WireApi::OpenAiChat);
    }

    #[test]
    fn test_openai_chat_build_request_basic() {
        let adapter = OpenAiChatAdapter::new();
        let canonical = basic_canonical_request();

        let request =
            adapter.build_request(&canonical, "https://api.openai.com/v1", Some("sk-test-key"));

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/chat/completions");

        // Verify headers
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v.contains("sk-test-key")));

        // Verify body structure
        let body = &request.body;
        assert_eq!(body["model"], "test-model");
        assert!(body.get("messages").is_some());
        let messages = body["messages"].as_array().unwrap();
        assert!(messages.len() >= 2); // system + user
    }

    #[test]
    fn test_openai_chat_build_request_with_tools() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tools = vec![CanonicalTool {
            name: "get_weather".to_string(),
            description: "Get weather for a location".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string"
                    }
                }
            }),
        }];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("tools").is_some());
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert!(tools[0].get("function").is_some());
    }

    #[test]
    fn test_openai_chat_build_request_message_input() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![
            CanonicalInputItem::Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            },
            CanonicalInputItem::Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Hi there!".to_string(),
                }],
            },
        ];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        let messages = body["messages"].as_array().unwrap();
        assert!(messages.len() >= 3);
    }

    #[test]
    fn test_openai_chat_build_request_function_call_output() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![CanonicalInputItem::FunctionCallOutput {
            call_id: "call_123".to_string(),
            output: "Sunny, 72F".to_string(),
        }];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        let messages = body["messages"].as_array().unwrap();
        let msg = messages.last().unwrap();
        assert_eq!(msg["role"], "tool");
        assert_eq!(msg["tool_call_id"], "call_123");
    }

    #[test]
    fn test_openai_chat_build_request_assistant_tool_call_before_tool_message() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.input = vec![
            CanonicalInputItem::AssistantFunctionCall {
                call_id: "call_123".to_string(),
                name: "lookup_weather".to_string(),
                arguments: "{\"city\":\"Boston\"}".to_string(),
            },
            CanonicalInputItem::FunctionCallOutput {
                call_id: "call_123".to_string(),
                output: "{\"forecast\":\"sunny\"}".to_string(),
            },
        ];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);
        let messages = request
            .body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .expect("chat messages should be an array");
        assert!(
            messages.len() >= 2,
            "chat request should include assistant tool call plus tool result"
        );
        let assistant_tool_call = &messages[messages.len() - 2];
        let tool_result = &messages[messages.len() - 1];
        assert_eq!(assistant_tool_call["role"], "assistant");
        assert_eq!(assistant_tool_call["tool_calls"][0]["id"], "call_123");
        assert_eq!(
            assistant_tool_call["tool_calls"][0]["function"]["name"],
            "lookup_weather"
        );
        assert_eq!(tool_result["role"], "tool");
        assert_eq!(tool_result["tool_call_id"], "call_123");
    }

    #[test]
    fn test_openai_chat_build_request_streaming() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.stream = true;

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn test_openai_chat_build_request_tool_choice_none() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tool_choice = CanonicalToolChoice::None;

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert_eq!(body["tool_choice"], "none");
    }

    #[test]
    fn test_openai_chat_build_request_tool_choice_specific() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tool_choice = CanonicalToolChoice::Specific("get_weather".to_string());

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_openai_chat_build_request_max_tokens() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.max_output_tokens = Some(2048);

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("max_tokens").is_some());
        assert_eq!(body["max_tokens"], 2048);
    }

    #[test]
    fn test_openai_chat_build_request_temperature() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.temperature = Some(0.8);

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("temperature").is_some());
        let temp = body["temperature"].as_f64().unwrap();
        assert!(
            (temp - 0.8).abs() < 0.001,
            "temperature should be approximately 0.8"
        );
    }

    #[test]
    fn test_openai_chat_build_request_top_p() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.top_p = Some(0.9);

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);

        let body = &request.body;
        assert!(body.get("top_p").is_some());
        let top_p = body["top_p"].as_f64().unwrap();
        assert!(
            (top_p - 0.9).abs() < 0.001,
            "top_p should be approximately 0.9"
        );
    }

    #[test]
    fn test_openai_chat_parse_response_basic() {
        let adapter = OpenAiChatAdapter::new();
        let body = r#"{
            "id": "chatcmpl_001",
            "model": "gpt-4",
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I assist you today?"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 15,
                "total_tokens": 25
            }
        }"#;

        let events = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body.as_bytes()));

        let events = events.unwrap();
        assert!(events.len() >= 4); // ResponseCreated + OutputItemAdded + OutputTextDelta + OutputItemDone + ResponseCompleted

        let completed = events
            .iter()
            .find(|e| matches!(e, modelwire_core::CanonicalEvent::ResponseCompleted { .. }))
            .unwrap();

        if let modelwire_core::CanonicalEvent::ResponseCompleted {
            response_id, usage, ..
        } = completed
        {
            assert_eq!(response_id, "chatcmpl_001");
            assert!(usage.is_some());
            let u = usage.as_ref().unwrap();
            assert_eq!(u.input_tokens, 10);
            assert_eq!(u.output_tokens, 15);
        }
    }

    #[test]
    fn test_openai_chat_parse_response_with_tool_calls() {
        let adapter = OpenAiChatAdapter::new();
        let body = r#"{
            "id": "chatcmpl_002",
            "model": "gpt-4",
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_abc123",
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "arguments": "{\"location\":\"Boston\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 10,
                "total_tokens": 22
            }
        }"#;

        let events = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body.as_bytes()));

        let events = events.unwrap();

        // Find function call event
        let fc_event = events.iter().find(|e| {
            matches!(
                e,
                modelwire_core::CanonicalEvent::OutputItemAdded {
                    item: modelwire_core::CanonicalOutputItem::FunctionCall { .. },
                    ..
                }
            )
        });

        assert!(fc_event.is_some(), "Should have a function call item");
    }

    #[test]
    fn test_openai_chat_parse_sse_delta() {
        let adapter = OpenAiChatAdapter::new();
        let data = r#"{"index":0,"delta":{"content":"Hello"},"finish_reason":null}"#.as_bytes();

        let event = adapter
            .parse_sse_event("chat_chunk", data)
            .unwrap()
            .unwrap();

        match event {
            modelwire_core::CanonicalEvent::OutputTextDelta { item_id, delta } => {
                assert_eq!(item_id, "chat_stream_item_0");
                assert_eq!(delta, "Hello");
            }
            _ => panic!("Expected OutputTextDelta event"),
        }
    }

    #[test]
    fn test_openai_chat_parse_sse_done() {
        let adapter = OpenAiChatAdapter::new();
        let data = r#"{}"#.as_bytes();

        let result = adapter.parse_sse_event("[DONE]", data);

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_openai_chat_parse_sse_unknown_event() {
        let adapter = OpenAiChatAdapter::new();
        let data = r#"{"some":"data"}"#.as_bytes();

        let result = adapter.parse_sse_event("unknown.event", data);

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}

// ============================================================================
// SSE Utilities Tests
// ============================================================================

mod sse_utilities_tests {
    use super::*;

    #[test]
    fn test_sse_event_type_parse() {
        assert_eq!(
            SseEventType::parse("response.created"),
            SseEventType::ResponseCreated
        );
        assert_eq!(
            SseEventType::parse("response.text.delta"),
            SseEventType::ResponseTextDelta
        );
        assert_eq!(
            SseEventType::parse("response.completed"),
            SseEventType::ResponseCompleted
        );
        assert_eq!(
            SseEventType::parse("response.function_call_arguments.delta"),
            SseEventType::ResponseFunctionCallArgumentsDelta
        );
        assert_eq!(SseEventType::parse("random.event"), SseEventType::Unknown);
    }

    #[test]
    fn test_sse_event_type_as_str() {
        assert_eq!(SseEventType::ResponseCreated.as_str(), "response.created");
        assert_eq!(
            SseEventType::ResponseTextDelta.as_str(),
            "response.text.delta"
        );
        assert_eq!(
            SseEventType::ResponseCompleted.as_str(),
            "response.completed"
        );
        assert_eq!(
            SseEventType::ResponseFunctionCallArgumentsDelta.as_str(),
            "response.function_call_arguments.delta"
        );
    }

    #[test]
    fn test_extract_sse_frames_basic() {
        let mut buffer = BytesMut::new();
        let data = b"event: response.created\ndata: {\"id\":\"test\"}\n\n";
        let frames = extract_sse_frames(&mut buffer, data);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("response.created"));
    }

    #[test]
    fn test_extract_sse_frames_multiple() {
        let mut buffer = BytesMut::new();
        let data = b"event: response.created\ndata: {\"id\":\"1\"}\n\nevent: response.text.delta\ndata: {\"delta\":\"hi\"}\n\n";
        let frames = extract_sse_frames(&mut buffer, data);

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("response.created"));
        assert_eq!(frames[1].event.as_deref(), Some("response.text.delta"));
    }

    #[test]
    fn test_extract_sse_frames_no_event() {
        let mut buffer = BytesMut::new();
        let data = b"data: {\"some\":\"data\"}\n\n";
        let frames = extract_sse_frames(&mut buffer, data);

        assert_eq!(frames.len(), 1);
        assert!(frames[0].event.is_none());
    }

    #[test]
    fn test_extract_sse_frames_crlf() {
        let mut buffer = BytesMut::new();
        let data = b"event: test\r\ndata: {\"x\":1}\r\n\r\n";
        let frames = extract_sse_frames(&mut buffer, data);

        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_extract_sse_frames_incremental() {
        let mut buffer = BytesMut::new();

        // First chunk - incomplete frame
        let chunk1 = b"event: test\ndata: {\"part";
        let frames1 = extract_sse_frames(&mut buffer, chunk1);
        assert!(frames1.is_empty());

        // Second chunk - completes the frame
        let chunk2 = b"ial\":true}\n\n";
        let frames2 = extract_sse_frames(&mut buffer, chunk2);
        assert_eq!(frames2.len(), 1);
    }

    #[test]
    fn test_extract_sse_frames_multiline_data() {
        let mut buffer = BytesMut::new();
        let data = b"event: test\ndata: line1\ndata: line2\ndata: line3\n\n";
        let frames = extract_sse_frames(&mut buffer, data);

        assert_eq!(frames.len(), 1);
        let data_str = String::from_utf8_lossy(&frames[0].data);
        assert!(data_str.contains("line1"));
        assert!(data_str.contains("line2"));
        assert!(data_str.contains("line3"));
    }
}

// ============================================================================
// Adapter Request Shape Comparison Tests
// ============================================================================

mod request_shape_comparison_tests {
    use super::*;

    #[test]
    fn test_all_adapters_handle_basic_request() {
        let canonical = basic_canonical_request();
        let base_url = "https://api.example.com/v1";
        let api_key = Some("sk-test-key");

        let responses_request =
            ResponsesAdapter::new().build_request(&canonical, base_url, api_key);
        let anthropic_request =
            AnthropicAdapter::new().build_request(&canonical, base_url, api_key);
        let chat_request = OpenAiChatAdapter::new().build_request(&canonical, base_url, api_key);

        // All should have POST method
        assert_eq!(responses_request.method, "POST");
        assert_eq!(anthropic_request.method, "POST");
        assert_eq!(chat_request.method, "POST");

        // All should have Content-Type header
        assert!(responses_request
            .headers
            .iter()
            .any(|(k, _)| k == "Content-Type"));
        assert!(anthropic_request
            .headers
            .iter()
            .any(|(k, _)| k == "Content-Type"));
        assert!(chat_request
            .headers
            .iter()
            .any(|(k, _)| k == "Content-Type"));
    }

    #[test]
    fn test_all_adapters_include_model() {
        let canonical = basic_canonical_request();
        let base_url = "https://api.example.com/v1";

        let responses_request = ResponsesAdapter::new().build_request(&canonical, base_url, None);
        let anthropic_request = AnthropicAdapter::new().build_request(&canonical, base_url, None);
        let chat_request = OpenAiChatAdapter::new().build_request(&canonical, base_url, None);

        assert_eq!(responses_request.body["model"], "test-model");
        assert_eq!(anthropic_request.body["model"], "test-model");
        assert_eq!(chat_request.body["model"], "test-model");
    }

    #[test]
    fn test_all_adapters_streaming_flag() {
        let mut canonical = basic_canonical_request();
        canonical.stream = true;
        let base_url = "https://api.example.com/v1";

        let responses_request = ResponsesAdapter::new().build_request(&canonical, base_url, None);
        let anthropic_request = AnthropicAdapter::new().build_request(&canonical, base_url, None);
        let chat_request = OpenAiChatAdapter::new().build_request(&canonical, base_url, None);

        assert_eq!(responses_request.body["stream"], true);
        assert_eq!(anthropic_request.body["stream"], true);
        assert_eq!(chat_request.body["stream"], true);
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod error_handling_tests {
    use super::*;

    #[test]
    fn test_responses_parse_invalid_json() {
        let adapter = ResponsesAdapter::new();
        let body = b"not valid json";

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body));

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            modelwire_adapters::UpstreamError::ParseError(_)
        ));
    }

    #[test]
    fn test_anthropic_parse_invalid_json() {
        let adapter = AnthropicAdapter::new();
        let body = b"not valid json";

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body));

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            modelwire_adapters::UpstreamError::ParseError(_)
        ));
    }

    #[test]
    fn test_openai_chat_parse_invalid_json() {
        let adapter = OpenAiChatAdapter::new();
        let body = b"not valid json";

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(adapter.parse_response(body));

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            modelwire_adapters::UpstreamError::ParseError(_)
        ));
    }

    #[test]
    fn test_responses_parse_sse_invalid_json() {
        let adapter = ResponsesAdapter::new();
        let data = b"not valid json";

        let result = adapter.parse_sse_event("response.created", data);

        assert!(result.is_err());
    }
}

// ============================================================================
// Tool Definition Conversion Tests
// ============================================================================

mod tool_conversion_tests {
    use super::*;

    fn create_test_tool(name: &str) -> CanonicalTool {
        CanonicalTool {
            name: name.to_string(),
            description: format!("Tool to {}", name),
            parameters: json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Input parameter"
                    }
                },
                "required": ["input"]
            }),
        }
    }

    #[test]
    fn test_responses_tool_schema() {
        let adapter = ResponsesAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tools = vec![create_test_tool("tool_one"), create_test_tool("tool_two")];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);
        let tools = request.body["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "tool_one");
        assert!(tools[0].get("parameters").is_some());
    }

    #[test]
    fn test_anthropic_tool_schema() {
        let adapter = AnthropicAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tools = vec![create_test_tool("tool_one"), create_test_tool("tool_two")];

        let request = adapter.build_request(&canonical, "https://api.anthropic.com/v1", None);
        let tools = request.body["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 2);
        // Anthropic uses 'name' directly, not wrapped in 'type'
        assert_eq!(tools[0]["name"], "tool_one");
        // Anthropic uses 'input_schema' instead of 'parameters'
        assert!(tools[0].get("input_schema").is_some());
        assert!(tools[0].get("parameters").is_none());
    }

    #[test]
    fn test_openai_chat_tool_schema() {
        let adapter = OpenAiChatAdapter::new();
        let mut canonical = basic_canonical_request();
        canonical.tools = vec![create_test_tool("tool_one"), create_test_tool("tool_two")];

        let request = adapter.build_request(&canonical, "https://api.openai.com/v1", None);
        let tools = request.body["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "function");
        assert!(tools[0].get("function").is_some());
        assert_eq!(tools[0]["function"]["name"], "tool_one");
        assert!(tools[0]["function"].get("parameters").is_some());
    }
}
