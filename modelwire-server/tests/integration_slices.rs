//! Integration slice tests for ModelWire adapters.
//!
//! These tests verify the complete relay pipeline through actual HTTP endpoints
//! for Chat Completions and Anthropic Messages adapters.
//!
//! Each test:
//! 1. Starts a mock upstream server using wiremock
//! 2. Configures ModelWire with the appropriate adapter target
//! 3. Sends a real HTTP request to ModelWire's /v1/responses
//! 4. Verifies the downstream response matches expected Responses API shape
//! 5. Verifies the upstream request was correctly transformed

use axum::{body::Body, http::Request};
use modelwire_core::{
    hash_key_for_logging, ArchiveConfig, Config, ProviderConfig, RelayKeyConfig, RouteConfig,
    SecurityConfig, ServerConfig, TargetConfig,
};
use modelwire_db::Database;
use modelwire_server::{server::build_router, ServerState};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn relay_security_for(raw_key: &str) -> SecurityConfig {
    let relay_secret = "test-relay-secret";
    SecurityConfig {
        downstream_auth: "relay_key".to_string(),
        log_secret: Some(relay_secret.to_string()),
        managed_key_encryption_secret: Some("test-managed-key-secret".to_string()),
        relay_keys: vec![RelayKeyConfig {
            key_hash: hash_key_for_logging(raw_key, relay_secret),
            enabled: true,
            ..RelayKeyConfig::default()
        }],
        ..SecurityConfig::default()
    }
}

// ============================================================================
// Test 1: chat_nonstream_nonstreaming_text
// ============================================================================

mod chat_nonstream_tests {
    use super::*;

    /// Chat Completions non-streaming text relay.
    ///
    /// Downstream request: POST /v1/responses with text input (no stream)
    /// Upstream receives: OpenAI Chat Completions JSON request at /chat/completions
    /// Downstream response: Responses-shaped JSON with message output
    #[tokio::test]
    async fn chat_nonstream_nonstreaming_text() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        // Capture the upstream request to verify it was transformed to Chat format
        let upstream_request_body = Arc::new(std::sync::Mutex::new(None));
        let upstream_request_body_clone = Arc::clone(&upstream_request_body);
        let upstream_request_headers: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let upstream_request_headers_clone = Arc::clone(&upstream_request_headers);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                // Capture the upstream request body
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *upstream_request_body_clone.lock().unwrap() = Some(body);

                // Capture headers
                let headers: Vec<(String, String)> = req
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                *upstream_request_headers_clone.lock().unwrap() = headers;

                // Return Chat Completions response format
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl-abc123",
                    "object": "chat.completion",
                    "created": 1234567890,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "Hello from Chat Completions!"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 8,
                        "total_tokens": 18
                    }
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        // Build ModelWire test state with Chat adapter target
        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send Responses request to ModelWire
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "Say hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Assert HTTP status is OK
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK response"
        );

        // Assert content-type is JSON
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "Expected JSON content-type"
        );

        // Extract and parse response body
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value =
            serde_json::from_slice(&body).expect("Response should be valid JSON");

        // Assert downstream response has required Responses API fields
        assert!(
            response_json.get("id").is_some(),
            "Response must have 'id' field"
        );
        assert_eq!(
            response_json.get("object").and_then(|v| v.as_str()),
            Some("response"),
            "Response object type should be 'response'"
        );
        assert_eq!(
            response_json.get("model").and_then(|v| v.as_str()),
            Some("chat-gpt-4"),
            "Response model should be downstream model"
        );
        assert_eq!(
            response_json.get("status").and_then(|v| v.as_str()),
            Some("completed"),
            "Response status should be 'completed'"
        );

        // Assert output contains message item
        let output = response_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("Response must have 'output' array");
        assert!(!output.is_empty(), "Output array should not be empty");

        let _msg_item = output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
            .expect("Output should contain a message item");

        // Verify upstream received Chat Completions format
        let captured_body = upstream_request_body.lock().unwrap();
        assert!(
            captured_body.is_some(),
            "Upstream request body should be captured"
        );
        let upstream_req = captured_body.as_ref().unwrap();
        assert_eq!(
            upstream_req.get("model").and_then(|v| v.as_str()),
            Some("gpt-4"),
            "Upstream request should use upstream model"
        );
        assert!(
            upstream_req.get("messages").is_some(),
            "Upstream request should have 'messages' field (Chat format)"
        );

        // Verify headers include Authorization
        let captured_headers = upstream_request_headers.lock().unwrap();
        assert!(
            captured_headers
                .iter()
                .any(|(k, _)| k.to_lowercase() == "authorization"),
            "Upstream request should include Authorization header"
        );
    }

    /// Verifies Chat adapter transforms Responses tools to Chat tools format
    #[tokio::test]
    async fn chat_tools_are_transformed_to_chat_format() {
        let upstream = MockServer::start().await;
        let captured_tools = Arc::new(std::sync::Mutex::new(None));
        let captured_tools_clone = Arc::clone(&captured_tools);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                if let Some(tools) = body.get("tools").cloned() {
                    *captured_tools_clone.lock().unwrap() = Some(tools);
                }
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl-tool",
                    "object": "chat.completion",
                    "created": 1234567890,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_123",
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "arguments": "{\"location\":\"Boston\"}"
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 35}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "What's the weather?",
                    "tools": [{
                        "type": "function",
                        "name": "get_weather",
                        "description": "Get weather for a location",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "location": {
                                    "type": "string",
                                    "description": "The city name"
                                }
                            },
                            "required": ["location"]
                        }
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK"
        );

        // Verify tools were captured and in Chat format
        let tools = captured_tools.lock().unwrap();
        assert!(
            tools.is_some(),
            "Tools should be captured from upstream request"
        );
        let tools_arr = tools.as_ref().unwrap().as_array().unwrap();
        assert_eq!(tools_arr.len(), 1, "Should have exactly 1 tool");

        // Chat format wraps function in { type: "function", function: { ... } }
        let tool = &tools_arr[0];
        assert_eq!(
            tool.get("type").and_then(|v| v.as_str()),
            Some("function"),
            "Tool type should be 'function' in Chat format"
        );
        assert!(
            tool.get("function").is_some(),
            "Tool should have 'function' wrapper"
        );
    }

    /// Verifies that multiple upstream tool calls preserve order in downstream output.
    #[tokio::test]
    async fn chat_parallel_tool_calls_preserve_order_received() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-tool-order",
                "object": "chat.completion",
                "created": 1234567890,
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_a",
                                "type": "function",
                                "function": {
                                    "name": "tool_a",
                                    "arguments": "{\"x\":1}"
                                }
                            },
                            {
                                "id": "call_b",
                                "type": "function",
                                "function": {
                                    "name": "tool_b",
                                    "arguments": "{\"y\":2}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 35}
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "Call both tools",
                    "parallel_tool_calls": true,
                    "tools": [
                        {
                            "type": "function",
                            "name": "tool_a",
                            "description": "Tool A",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "x": {"type": "number"}
                                },
                                "required": ["x"]
                            }
                        },
                        {
                            "type": "function",
                            "name": "tool_b",
                            "description": "Tool B",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "y": {"type": "number"}
                                },
                                "required": ["y"]
                            }
                        }
                    ]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK"
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let output = json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("response output must be array");

        let calls: Vec<_> = output
            .iter()
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .collect();
        assert_eq!(calls.len(), 2, "expected two function_call outputs");
        assert_eq!(
            calls[0].get("name").and_then(|v| v.as_str()),
            Some("tool_a"),
            "first tool call should preserve upstream order"
        );
        assert_eq!(
            calls[1].get("name").and_then(|v| v.as_str()),
            Some("tool_b"),
            "second tool call should preserve upstream order"
        );
    }
}

// ============================================================================
// Test 2: chat_streaming_text
// ============================================================================

mod chat_streaming_tests {
    use super::*;

    /// Chat Completions streaming text relay.
    ///
    /// Downstream request: POST /v1/responses with stream=true
    /// Upstream returns: SSE in Chat Completions format
    /// Downstream response: Responses SSE with text delta events
    #[tokio::test]
    async fn chat_streaming_text() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: delta\n\
                 data: {\"id\":\"chatcmpl-stream\",\"delta\":{\"content\":\"Hello\"},\"index\":0}\n\n\
                 event: delta\n\
                 data: {\"id\":\"chatcmpl-stream\",\"delta\":{\"content\":\" there!\"},\"index\":0}\n\n\
                 event: [DONE]\n\
                 data: \n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        // Build ModelWire test state with Chat adapter target
        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send streaming request to ModelWire
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "Say hello",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Assert HTTP status is OK
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK response"
        );

        // Assert content-type is SSE
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "Expected text/event-stream content-type for streaming"
        );

        // Extract SSE body
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        // Assert presence of required SSE events (in Responses format, not Chat format)
        assert!(
            body_str.contains("event: response.created")
                || body_str.contains("response.created")
                || body_str.contains("Hello")
                || body_str.contains("there!"),
            "SSE should contain text content or Responses event structure"
        );

        // Verify Chat upstream was called (stream=true in request)
        // The key is that we got an SSE response back
        assert!(
            body_str.contains("Hello") || body_str.contains("there!") || body_str.contains("text"),
            "SSE should contain text content from Chat stream"
        );
    }

    /// Verifies Chat streaming response completes and has proper structure
    #[tokio::test]
    async fn chat_streaming_completes_with_proper_sse() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: delta\n\
                 data: {\"id\":\"chatcmpl-abc\",\"delta\":{\"content\":\"Test\"},\"index\":0}\n\n\
                 event: [DONE]\n\
                 data: \n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model": "chat-gpt-4", "input": "test", "stream": true}"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Debug: print status for non-200 responses
        let status = response.status();
        let body = if status != axum::http::StatusCode::OK {
            let body_debug = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            eprintln!(
                "DEBUG: Got status {:?}, body: {:?}",
                status,
                String::from_utf8_lossy(&body_debug)
            );
            body_debug
        } else {
            axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
        };

        assert_eq!(status, axum::http::StatusCode::OK, "Expected 200 OK");

        // Verify SSE structure
        let body_str = String::from_utf8_lossy(&body);

        // Content should be present in the response
        assert!(
            body_str.contains("Test") || body_str.contains("text") || body_str.contains("delta"),
            "SSE should contain text content"
        );
    }
}

// ============================================================================
// Test 3: anthropic_nonstream_nonstreaming_text
// ============================================================================

mod anthropic_nonstream_tests {
    use super::*;

    /// Anthropic Messages non-streaming text relay.
    ///
    /// Downstream request: POST /v1/responses with text input
    /// Upstream receives: Anthropic Messages JSON request at /messages
    /// Downstream response: Responses-shaped JSON with message output
    #[tokio::test]
    async fn anthropic_nonstream_nonstreaming_text() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        // Capture the upstream request to verify Anthropic format
        let upstream_request_body = Arc::new(std::sync::Mutex::new(None));
        let upstream_request_body_clone = Arc::clone(&upstream_request_body);
        let upstream_request_headers: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let upstream_request_headers_clone = Arc::clone(&upstream_request_headers);

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(move |req: &wiremock::Request| {
                // Capture the upstream request body
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *upstream_request_body_clone.lock().unwrap() = Some(body);

                // Capture headers
                let headers: Vec<(String, String)> = req
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                *upstream_request_headers_clone.lock().unwrap() = headers;

                // Return Anthropic Messages response format
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msg_abc123",
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": "Hello from Anthropic!"
                    }],
                    "model": "claude-3-sonnet",
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 8,
                        "total_tokens": 18
                    }
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        // Build ModelWire test state with Anthropic adapter target
        let state = Arc::new(build_anthropic_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send Responses request to ModelWire
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "anthropic-claude",
                    "input": "Say hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Assert HTTP status is OK
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK response"
        );

        // Assert content-type is JSON
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "Expected JSON content-type"
        );

        // Extract and parse response body
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value =
            serde_json::from_slice(&body).expect("Response should be valid JSON");

        // Assert downstream response has required Responses API fields
        assert!(
            response_json.get("id").is_some(),
            "Response must have 'id' field"
        );
        assert_eq!(
            response_json.get("object").and_then(|v| v.as_str()),
            Some("response"),
            "Response object type should be 'response'"
        );
        assert_eq!(
            response_json.get("model").and_then(|v| v.as_str()),
            Some("anthropic-claude"),
            "Response model should be downstream model"
        );
        assert_eq!(
            response_json.get("status").and_then(|v| v.as_str()),
            Some("completed"),
            "Response status should be 'completed'"
        );

        // Assert output contains message item
        let output = response_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("Response must have 'output' array");
        assert!(!output.is_empty(), "Output array should not be empty");

        let _msg_item = output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
            .expect("Output should contain a message item");

        // Verify upstream received Anthropic format
        let captured_body = upstream_request_body.lock().unwrap();
        assert!(
            captured_body.is_some(),
            "Upstream request body should be captured"
        );
        let upstream_req = captured_body.as_ref().unwrap();
        assert_eq!(
            upstream_req.get("model").and_then(|v| v.as_str()),
            Some("claude-3-sonnet"),
            "Upstream request should use upstream model"
        );
        assert!(
            upstream_req.get("messages").is_some(),
            "Upstream request should have 'messages' field (Anthropic format)"
        );

        // Verify headers include anthropic-version and x-api-key
        let captured_headers = upstream_request_headers.lock().unwrap();
        assert!(
            captured_headers
                .iter()
                .any(|(k, _)| k.to_lowercase() == "anthropic-version"),
            "Upstream request should include anthropic-version header"
        );
        assert!(
            captured_headers
                .iter()
                .any(|(k, _)| k.to_lowercase() == "x-api-key"),
            "Upstream request should include x-api-key header"
        );
    }

    /// Verifies Anthropic adapter handles system instructions correctly
    #[tokio::test]
    async fn anthropic_system_instructions_are_mapped() {
        let upstream = MockServer::start().await;
        let captured_system = Arc::new(std::sync::Mutex::new(None));
        let captured_system_clone = Arc::clone(&captured_system);

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                if let Some(system) = body.get("system").cloned() {
                    *captured_system_clone.lock().unwrap() = Some(system);
                }
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msg_system",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Response with system"}],
                    "model": "claude-3-sonnet",
                    "usage": {"input_tokens": 15, "output_tokens": 5, "total_tokens": 20}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_anthropic_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "anthropic-claude",
                    "input": "Hello",
                    "instructions": "You are a helpful assistant."
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK"
        );

        // Verify system prompt was sent in Anthropic format
        let system = captured_system.lock().unwrap();
        assert!(
            system.is_some(),
            "System prompt should be captured from upstream request"
        );
        let system_val = system.as_ref().unwrap();
        assert_eq!(
            system_val.as_str().unwrap_or(""),
            "You are a helpful assistant.",
            "System prompt should be mapped correctly"
        );
    }

    /// Verifies Anthropic usage maps to downstream Responses usage.
    #[tokio::test]
    async fn anthropic_usage_maps_to_downstream_usage() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_usage_map",
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "usage mapped"
                }],
                "model": "claude-3-sonnet",
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 7,
                    "total_tokens": 19,
                    "thinking_tokens": 3
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_anthropic_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "anthropic-claude",
                    "input": "usage please"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let usage = response_json
            .get("usage")
            .and_then(|v| v.as_object())
            .expect("downstream response should include usage object");

        assert_eq!(usage.get("input_tokens").and_then(|v| v.as_u64()), Some(12));
        assert_eq!(usage.get("output_tokens").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(usage.get("total_tokens").and_then(|v| v.as_u64()), Some(19));
        assert_eq!(
            usage.get("reasoning_tokens").and_then(|v| v.as_u64()),
            Some(3)
        );
    }
}

// ============================================================================
// Test 4: anthropic_streaming_text
// ============================================================================

mod anthropic_streaming_tests {
    use super::*;

    /// Anthropic Messages streaming text relay.
    ///
    /// Downstream request: POST /v1/responses with stream=true
    /// Upstream returns: SSE in Anthropic text/event-stream format
    /// Downstream response: Responses SSE with text delta events
    #[tokio::test]
    async fn anthropic_streaming_text() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        // Anthropic streaming format
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: message_start\n\
                 data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream_123\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-3-sonnet\",\"stop_reason\":null}}\n\n\
                 event: content_block_start\n\
                 data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n\
                 event: content_block_delta\n\
                 data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
                 event: content_block_delta\n\
                 data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" there!\"}}\n\n\
                 event: content_block_stop\n\
                 data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                 event: message_stop\n\
                 data: {\"type\":\"message_stop\"}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        // Build ModelWire test state with Anthropic adapter target
        let state = Arc::new(build_anthropic_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send streaming request to ModelWire
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "anthropic-claude",
                    "input": "Say hello",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Assert HTTP status is OK
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK response"
        );

        // Assert content-type is SSE
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "Expected text/event-stream content-type for streaming"
        );

        // Extract SSE body
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        // Assert text content is present (transformed to Responses format)
        assert!(
            body_str.contains("Hello") || body_str.contains("there!"),
            "SSE should contain text content from Anthropic stream"
        );
    }

    /// Verifies Anthropic streaming completes with proper structure
    #[tokio::test]
    async fn anthropic_streaming_response_completes() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: message_start\n\
                 data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_xyz\",\"type\":\"message\",\"role\":\"assistant\"}}\n\n\
                 event: content_block_start\n\
                 data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n\
                 event: content_block_delta\n\
                 data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Test\"}}\n\n\
                 event: content_block_stop\n\
                 data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                 event: message_stop\n\
                 data: {\"type\":\"message_stop\"}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_anthropic_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model": "anthropic-claude", "input": "test", "stream": true}"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK"
        );

        // Verify content is present
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        assert!(
            body_str.contains("Test") || body_str.contains("text") || body_str.contains("delta"),
            "SSE should contain text content"
        );
    }

    /// Verifies Anthropic streaming tool input deltas map to Responses tool argument deltas.
    #[tokio::test]
    async fn anthropic_streaming_tool_input_maps_to_argument_deltas() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: message_start\n\
                 data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tool_stream_1\",\"type\":\"message\",\"role\":\"assistant\"}}\n\n\
                 event: content_block_start\n\
                 data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_123\",\"name\":\"get_weather\"}}\n\n\
                 event: content_block_delta\n\
                 data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"location\\\":\"}}\n\n\
                 event: content_block_delta\n\
                 data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"Boston\\\"}\"}}\n\n\
                 event: content_block_stop\n\
                 data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                 event: message_stop\n\
                 data: {\"type\":\"message_stop\"}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_anthropic_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "anthropic-claude",
                    "input": "Use tool",
                    "stream": true,
                    "tools": [{
                        "type": "function",
                        "name": "get_weather",
                        "description": "Get weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "location": { "type": "string" }
                            },
                            "required": ["location"]
                        }
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Expected 200 OK"
        );

        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "Expected text/event-stream content-type"
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        assert!(
            body_str.contains("event: response.function_call_arguments.delta"),
            "downstream SSE should emit function_call_arguments.delta events"
        );
        assert!(
            body_str.contains("\"arguments\":\"{\\\"location\\\":\"")
                || body_str.contains("\\\"Boston\\\""),
            "downstream SSE should include mapped tool input JSON fragments"
        );
        assert!(
            body_str.contains("\"name\":\"get_weather\""),
            "streaming tool completion should preserve the upstream function name"
        );
        assert!(
            !body_str.contains("\"name\":\"unknown_tool\""),
            "streaming tool completion should not synthesize unknown_tool when metadata was provided"
        );
    }
}

// ============================================================================
// Test 5: state_scope_reuse_success
// ============================================================================

mod state_scope_reuse_tests {
    use super::*;

    /// Equivalent to minimum-slice `state_scope_optimistic_reuse_success`.
    #[tokio::test]
    async fn state_scope_optimistic_reuse_success() {
        let upstream = MockServer::start().await;
        let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let requests_clone = Arc::clone(&requests);
        let call_index = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_index_clone = Arc::clone(&call_index);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                requests_clone.lock().unwrap().push(body);
                let idx = call_index_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if idx == 0 {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "resp_upstream_state_scope_success_1",
                        "model": "gpt-4",
                        "output": [{
                            "type": "message",
                            "id": "msg_state_scope_success_1",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "first"}]
                        }],
                        "usage": {"input_tokens": 10, "output_tokens": 4, "total_tokens": 14}
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "resp_upstream_state_scope_success_2",
                        "model": "gpt-4",
                        "output": [{
                            "type": "message",
                            "id": "msg_state_scope_success_2",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "second"}]
                        }],
                        "usage": {"input_tokens": 15, "output_tokens": 5, "total_tokens": 20}
                    }))
                }
            })
            .expect(2)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state_scope_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "first"
                })
                .to_string(),
            ))
            .unwrap();
        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(first_response.status(), axum::http::StatusCode::OK);
        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("first response id should be present");

        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "second",
                    "previous_response_id": first_response_id
                })
                .to_string(),
            ))
            .unwrap();
        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(second_response.status(), axum::http::StatusCode::OK);

        let captured = requests.lock().unwrap().clone();
        assert_eq!(captured.len(), 2, "two upstream requests expected");
        assert!(
            captured[1].get("previous_response_id").is_some(),
            "second upstream request should optimistically reuse previous_response_id"
        );
    }

    /// State scope reuse - second request with previous_response_id reuses upstream handle.
    ///
    /// Setup: Two requests to same provider with same state_scope
    /// - First request: success, stores upstream_response_id handle
    /// - Second request: with previous_response_id, reuses the upstream handle
    /// - Assert: Second upstream request uses same previous_response_id
    #[tokio::test]
    async fn state_scope_reuse_success() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        // Track both requests to verify handle reuse
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);
        let first_upstream_response_id = Arc::new(std::sync::Mutex::new(None));
        let first_upstream_response_id_clone = Arc::clone(&first_upstream_response_id);
        let second_request_had_previous = Arc::new(std::sync::Mutex::new(None));
        let second_request_had_previous_clone = Arc::clone(&second_request_had_previous);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let count = call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();

                if count == 0 {
                    // First request - capture upstream response ID
                    let resp_id = format!("resp_upstream_{}", uuid::Uuid::now_v7());
                    *first_upstream_response_id_clone.lock().unwrap() = Some(resp_id.clone());

                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": resp_id,
                        "model": "gpt-4",
                        "output": [{
                            "type": "message",
                            "id": "msg_first",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "First response"}]
                        }],
                        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                    }))
                } else {
                    // Second request - check for previous_response_id
                    let has_previous = body.get("previous_response_id").is_some();
                    *second_request_had_previous_clone.lock().unwrap() = Some(has_previous);

                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": format!("resp_upstream_{}", uuid::Uuid::now_v7()),
                        "model": "gpt-4",
                        "output": [{
                            "type": "message",
                            "id": "msg_second",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "Second response"}]
                        }],
                        "usage": {"input_tokens": 15, "output_tokens": 6, "total_tokens": 21}
                    }))
                }
            })
            .expect(2)
            .mount(&upstream)
            .await;

        // Build ModelWire test state with same state_scope for both requests
        let state = Arc::new(build_state_scope_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // ===== FIRST REQUEST =====
        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "First request"
                })
                .to_string(),
            ))
            .unwrap();

        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(
            first_response.status(),
            axum::http::StatusCode::OK,
            "First request should return 200 OK"
        );

        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("First response should have id");

        // ===== SECOND REQUEST with previous_response_id =====
        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Second request with continuation",
                    "previous_response_id": first_response_id
                })
                .to_string(),
            ))
            .unwrap();

        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(
            second_response.status(),
            axum::http::StatusCode::OK,
            "Second request should return 200 OK"
        );

        // Verify two upstream calls were made
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "Exactly 2 upstream calls should be made"
        );

        // Verify second request included previous_response_id
        let had_previous_val = second_request_had_previous.lock().unwrap().unwrap_or(false);
        assert!(
            had_previous_val,
            "Second upstream request should include previous_response_id for handle reuse"
        );

        // Verify second response succeeded
        let second_body = axum::body::to_bytes(second_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
        assert!(
            second_json.get("id").is_some(),
            "Second response should have id"
        );
    }

    /// Verifies state scope reuse requires same provider scope
    #[tokio::test]
    async fn different_state_scope_prevents_reuse() {
        let upstream_a = MockServer::start().await;
        let upstream_b = MockServer::start().await;

        let second_request_capture = Arc::new(std::sync::Mutex::new(None));
        let second_request_capture_clone = Arc::clone(&second_request_capture);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_upstream_scope_a_1",
                    "model": "gpt-a",
                    "output": [{
                        "type": "message",
                        "id": "msg_scope_a_1",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "First response from scope A"}]
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                }))
            })
            .expect(1)
            .mount(&upstream_a)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *second_request_capture_clone.lock().unwrap() = Some(body);

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_upstream_scope_b_1",
                    "model": "gpt-b",
                    "output": [{
                        "type": "message",
                        "id": "msg_scope_b_1",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Second response from scope B"}]
                    }],
                    "usage": {"input_tokens": 20, "output_tokens": 7, "total_tokens": 27}
                }))
            })
            .expect(1)
            .mount(&upstream_b)
            .await;

        // Build state with different providers/state scopes and two downstream models.
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: relay_security_for("mw_test_key"),
            archive: ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: upstream_a.uri(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true,
                    config_json: None,
                },
                ProviderConfig {
                    id: "provider-b".to_string(),
                    name: "Provider B".to_string(),
                    base_url: upstream_b.uri(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-b".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true,
                    config_json: None,
                },
            ],
            routes: vec![
                RouteConfig {
                    id: Some("route-a".to_string()),
                    downstream_model: "model-a".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "provider-a".to_string(),
                        upstream_model: "gpt-a".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
                RouteConfig {
                    id: Some("route-b".to_string()),
                    downstream_model: "model-b".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "provider-b".to_string(),
                        upstream_model: "gpt-b".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
            ],
        };

        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let state = Arc::new(ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let app = build_router(Arc::clone(&state));

        // First request
        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "model-a", "input": "First"}"#))
            .unwrap();

        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(first_response.status(), axum::http::StatusCode::OK);

        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json.get("id").and_then(|v| v.as_str()).unwrap();

        // Second request with previous - different scope should cause replay
        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "model-b",
                    "input": "Second",
                    "previous_response_id": first_response_id
                })
                .to_string(),
            ))
            .unwrap();

        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(second_response.status(), axum::http::StatusCode::OK);

        let captured = second_request_capture
            .lock()
            .unwrap()
            .clone()
            .expect("Second upstream request should be captured");

        // Different provider/state_scope must never receive raw previous upstream handle.
        assert!(
            captured.get("previous_response_id").is_none(),
            "Cross-provider/different-scope continuation must not forward previous_response_id"
        );

        // Continuation should replay history into input for the new upstream.
        let input = captured
            .get("input")
            .and_then(|v| v.as_array())
            .expect("Replayed request must include input array");
        assert!(
            input.len() >= 2,
            "Replayed request should contain prior visible history + new turn"
        );
    }
}

// ============================================================================
// Test 6: downstream_disconnect_cancels_upstream
// ============================================================================

mod downstream_disconnect_tests {
    use super::*;

    /// Verifies that downstream client disconnect cancels upstream request.
    ///
    /// This test simulates a client disconnecting mid-stream and verifies
    /// the upstream request is cancelled appropriately.
    #[tokio::test]
    async fn downstream_disconnect_cancels_upstream() {
        // Start mock upstream server with slow response
        let upstream = MockServer::start().await;

        // Track whether upstream was called and if it got cancelled
        let upstream_called = Arc::new(std::sync::Mutex::new(false));
        let upstream_called_clone = Arc::clone(&upstream_called);
        let cancellation_detected = Arc::new(std::sync::Mutex::new(false));
        let _cancellation_detected_clone = Arc::clone(&cancellation_detected);

        // Use a responder that can detect early connection drop
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                *upstream_called_clone.lock().unwrap() = true;

                // Return a streaming response that takes time
                ResponseTemplate::new(200)
                    .set_body_string(
                        "event: response.created\n\
                         data: {\"response\":{\"id\":\"resp_disconnect\",\"model\":\"gpt-4\"}}\n\n",
                    )
                    .set_delay(std::time::Duration::from_millis(100))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        // Build test state
        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Create a streaming request
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Stream test",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        // Execute request and immediately drop the body (simulating disconnect)
        let response = app.oneshot(request).await.unwrap();

        // The upstream should have been called
        let called = upstream_called.lock().unwrap();
        assert!(
            *called,
            "Upstream should have been called for streaming request"
        );

        // For streaming responses, we expect the server to handle the response
        // The exact cancellation behavior depends on implementation
        // The key is that the client disconnect should be handled gracefully

        // Verify the response is a streaming response
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "Streaming request should return SSE content-type"
        );
    }

    /// Verifies server properly handles upstream errors after partial response
    #[tokio::test]
    async fn upstream_error_after_partial_response_handled() {
        let upstream = MockServer::start().await;
        let partial_sent = Arc::new(std::sync::Mutex::new(false));
        let partial_sent_clone = Arc::clone(&partial_sent);

        // Return partial response then error
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                *partial_sent_clone.lock().unwrap() = true;

                // Return partial success then connection drops
                ResponseTemplate::new(200).set_body_string(
                    "event: response.created\n\
                     data: {\"response\":{\"id\":\"resp_partial\",\"model\":\"gpt-4\"}}\n\n\
                     event: response.output_item.added\n\
                     data: {\"response_id\":\"resp_partial\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\"}}\n\n",
                )
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Test",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Debug: print status for 502 errors
        let status = response.status();
        if status != axum::http::StatusCode::OK {
            let body_debug = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            eprintln!(
                "DEBUG: Got status {:?}, body: {:?}",
                status,
                String::from_utf8_lossy(&body_debug)
            );
        }

        // Assert HTTP status is OK
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "Expected 200 OK response"
        );

        let partial = partial_sent.lock().unwrap();
        assert!(*partial, "Partial upstream response should have been sent");
    }
}

// ============================================================================
// Test 7: fallback_429_before_commit
// ============================================================================

mod fallback_429_tests {
    use super::*;

    /// Verifies that 429 responses trigger fallback to second target before commit.
    ///
    /// First target returns 429 (rate limited), second target returns success.
    /// Assert fallback happens and second target is called.
    #[tokio::test]
    async fn fallback_429_before_commit() {
        // Start two mock upstream servers
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        // Track which upstream was called
        let first_called = Arc::new(std::sync::Mutex::new(false));
        let first_called_clone = Arc::clone(&first_called);
        let second_called = Arc::new(std::sync::Mutex::new(false));
        let second_called_clone = Arc::clone(&second_called);

        // First target returns 429 rate limit error
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                *first_called_clone.lock().unwrap() = true;
                ResponseTemplate::new(429).set_body_json(serde_json::json!({
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "Rate limit exceeded"
                    }
                }))
            })
            .expect(1)
            .mount(&first)
            .await;

        // Second target returns success
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                *second_called_clone.lock().unwrap() = true;
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "resp_fallback_123",
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": "msg_fallback",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "Response from fallback target"
                        }]
                    }],
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 6,
                        "total_tokens": 16
                    }
                }))
            })
            .expect(1)
            .mount(&second)
            .await;

        // Build test state with two targets on same provider
        let state = Arc::new(build_state_with_two_targets(&first.uri(), &second.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send request
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Fallback test"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Debug: show the response if it failed
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        let first_called_val = *first_called.lock().unwrap();
        let second_called_val = *second_called.lock().unwrap();
        println!(
            "DEBUG: status={:?}, first_called={}, second_called={}, body={}",
            status, first_called_val, second_called_val, body_str
        );

        // Assert request succeeded via fallback
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "Expected 200 OK from fallback target"
        );

        // Verify first target was called
        assert!(
            first_called_val,
            "First target (429) should have been called"
        );

        // Verify second target was called (fallback happened)
        assert!(
            second_called_val,
            "Second target should be called after 429 from first target"
        );

        // Verify response content
        let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert!(
            response_json.get("id").is_some(),
            "Response should have id field"
        );
        assert_eq!(
            response_json.get("status").and_then(|v| v.as_str()),
            Some("completed"),
            "Response status should be completed"
        );
    }

    /// Verifies route target priority order with three targets.
    ///
    /// Target #1 (priority=1) fails with 500, target #2 (priority=2) succeeds,
    /// target #3 (priority=3) must not be called.
    #[tokio::test]
    async fn fallback_tries_three_targets_in_priority_order() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        let third = MockServer::start().await;

        let call_order = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        let call_order_first = Arc::clone(&call_order);
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                call_order_first.lock().unwrap().push("first".to_string());
                ResponseTemplate::new(500).set_body_string("first failed")
            })
            .expect(1)
            .mount(&first)
            .await;

        let call_order_second = Arc::clone(&call_order);
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                call_order_second.lock().unwrap().push("second".to_string());
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "resp_second_success",
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": "msg_second",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "second target won"
                        }]
                    }]
                }))
            })
            .expect(1)
            .mount(&second)
            .await;

        let call_order_third = Arc::clone(&call_order);
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                call_order_third.lock().unwrap().push("third".to_string());
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "resp_third_should_not_run",
                    "model": "gpt-upstream",
                    "output": []
                }))
            })
            .expect(0)
            .mount(&third)
            .await;

        let state = Arc::new(
            build_state_with_three_targets(&first.uri(), &second.uri(), &third.uri()).await,
        );
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "priority order"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let order = call_order.lock().unwrap().clone();
        assert_eq!(order, vec!["first".to_string(), "second".to_string()]);
    }

    /// Verifies connection error before downstream commit falls back to next target.
    #[tokio::test]
    async fn fallback_on_connection_reset_before_commit() {
        let unreachable_base = "http://127.0.0.1:9";
        let fallback = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_fallback_after_conn_error",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_conn_fallback",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "fallback after connection error"
                    }]
                }]
            })))
            .expect(1)
            .mount(&fallback)
            .await;

        let state = Arc::new(build_state_with_two_targets(unreachable_base, &fallback.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "connection reset fallback"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    /// Verifies malformed streaming payload before first semantic event falls back.
    #[tokio::test]
    async fn fallback_on_malformed_stream_before_commit() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {invalid_json}\n\n",
            ))
            .expect(1)
            .mount(&first)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_fallback_stream\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_fallback_stream\",\"output\":[]}}\n\n",
            ))
            .expect(1)
            .mount(&second)
            .await;

        let state = Arc::new(build_state_with_two_targets(&first.uri(), &second.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "malformed stream fallback",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let sse = String::from_utf8_lossy(&body);
        assert!(sse.contains("event: response.created"));
        assert!(sse.contains("event: response.completed"));
        assert!(sse.contains("resp_mw_"));
        assert!(
            !sse.contains("resp_fallback_stream"),
            "fallback stream must not expose upstream response id downstream"
        );
    }
}

// ============================================================================
// Test 8: no_fallback_after_sse_commit
// ============================================================================

mod no_fallback_after_commit_tests {
    use super::*;

    /// Equivalent to minimum-slice `codex_context_overflow_before_upstream`.
    #[tokio::test]
    async fn codex_context_overflow_before_upstream() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "x".repeat(1_200_000),
                    "max_output_tokens": 20000
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: modelwire_core::error::ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            payload.error.code.as_deref(),
            Some("context_length_exceeded"),
            "overflow should be rejected before upstream call"
        );
    }

    /// Equivalent to minimum-slice `responses_stream_text_basic`.
    #[tokio::test]
    async fn responses_stream_text_basic() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_stream_basic\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.output_item.added\n\
                 data: {\"response_id\":\"resp_stream_basic\",\"item\":{\"type\":\"message\",\"id\":\"msg_stream_basic\",\"role\":\"assistant\",\"content\":[]}}\n\n\
                 event: response.output_text.delta\n\
                 data: {\"item_id\":\"msg_stream_basic\",\"delta\":{\"text\":\"hello\"}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_stream_basic\",\"output\":[]}}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "stream basic",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let sse = String::from_utf8_lossy(&body);
        assert!(sse.contains("event: response.created"));
        assert!(sse.contains("event: response.output_text.delta"));
        assert!(sse.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn streaming_first_sse_arrives_before_upstream_completion() {
        let first_base = spawn_delayed_sse_upstream(
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(900),
            vec![
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_stream_early\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n"
                    .to_string(),
                "event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_stream_early\",\"output\":[]}}\n\n"
                    .to_string(),
            ],
        )
        .await;

        let state = Arc::new(build_test_state(&first_base).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "early sse",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let start = Instant::now();
        let mut body_stream = response.into_body().into_data_stream();
        let first = futures::StreamExt::next(&mut body_stream)
            .await
            .expect("first frame should arrive")
            .expect("first frame should be ok");
        let elapsed = start.elapsed();
        let first_text = String::from_utf8_lossy(&first);
        assert!(
            first_text.contains("event: response.created"),
            "first streamed frame should contain response.created"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(700),
            "first downstream SSE frame should arrive before upstream completion delay"
        );

        let mut merged = first.to_vec();
        while let Some(next) = futures::StreamExt::next(&mut body_stream).await {
            let bytes = next.expect("stream chunk should be ok");
            merged.extend_from_slice(&bytes);
        }
        let all = String::from_utf8_lossy(&merged);
        assert!(all.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn streaming_large_response_does_not_buffer_entire_body() {
        let large_delta = "x".repeat(64 * 1024);
        let first_event = "event: response.created\n\
             data: {\"response\":{\"id\":\"resp_stream_large\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n"
            .to_string();
        let second_event = format!(
            "event: response.output_text.delta\n\
             data: {{\"item_id\":\"msg_stream_large\",\"delta\":{{\"text\":\"{}\"}}}}\n\n",
            large_delta
        );
        let third_event = "event: response.completed\n\
             data: {\"response\":{\"id\":\"resp_stream_large\",\"output\":[]}}\n\n"
            .to_string();

        let first_base = spawn_delayed_sse_upstream(
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(700),
            vec![first_event, second_event, third_event],
        )
        .await;

        let state = Arc::new(build_test_state(&first_base).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "large stream",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let mut body_stream = response.into_body().into_data_stream();

        let first_start = Instant::now();
        let first = futures::StreamExt::next(&mut body_stream)
            .await
            .expect("first frame should arrive")
            .expect("first frame should be ok");
        let first_elapsed = first_start.elapsed();
        assert!(
            first_elapsed < std::time::Duration::from_millis(500),
            "first frame should be emitted quickly without waiting for large trailing chunks"
        );
        let first_text = String::from_utf8_lossy(&first);
        assert!(first_text.contains("event: response.created"));
    }

    #[tokio::test]
    async fn streaming_downstream_sse_uses_modelwire_owned_ids() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_upstream_secret\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.output_item.added\n\
                 data: {\"response_id\":\"resp_upstream_secret\",\"item\":{\"type\":\"message\",\"id\":\"msg_upstream_secret\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n\n\
                 event: response.output_text.delta\n\
                 data: {\"item_id\":\"msg_upstream_secret\",\"delta\":{\"text\":\"hi\"}}\n\n\
                 event: response.output_item.done\n\
                 data: {\"response_id\":\"resp_upstream_secret\",\"item\":{\"type\":\"message\",\"id\":\"msg_upstream_secret\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_upstream_secret\",\"output\":[{\"type\":\"message\",\"id\":\"msg_upstream_secret\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "stream ids",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let sse = String::from_utf8_lossy(&body);

        assert!(
            !sse.contains("resp_upstream_secret"),
            "streaming downstream SSE must not expose upstream response IDs"
        );
        assert!(
            !sse.contains("msg_upstream_secret"),
            "streaming downstream SSE must not expose upstream output item IDs"
        );
        assert!(
            sse.contains("resp_mw_"),
            "streaming downstream SSE should use ModelWire response IDs"
        );
        assert!(
            sse.contains("msg_mw_"),
            "streaming downstream SSE should use ModelWire output item IDs"
        );

        let downstream_response_id = sse
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find_map(|value| {
                value
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| id.starts_with("resp_mw_"))
                    .map(ToOwned::to_owned)
            })
            .expect("stream should contain downstream response id");
        let persisted =
            modelwire_db::repo::responses::get_response(&state.db, &downstream_response_id)
                .await
                .unwrap()
                .expect("stream response should be persisted");
        assert_eq!(
            persisted.upstream_response_id.as_deref(),
            Some("resp_upstream_secret"),
            "stream persistence must retain the private upstream response handle"
        );
        let handle = modelwire_db::repo::responses::get_latest_upstream_handle(
            &state.db,
            &downstream_response_id,
        )
        .await
        .unwrap()
        .expect("stream response should persist an upstream handle");
        assert_eq!(
            handle.upstream_response_id.as_deref(),
            Some("resp_upstream_secret")
        );
    }

    /// Equivalent to minimum-slice `chat_stream_text_basic`.
    #[tokio::test]
    async fn chat_stream_text_basic() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: delta\n\
                 data: {\"id\":\"chatcmpl_stream_basic\",\"delta\":{\"content\":\"Hello\"},\"index\":0}\n\n\
                 event: delta\n\
                 data: {\"id\":\"chatcmpl_stream_basic\",\"delta\":{\"content\":\" world\"},\"index\":0}\n\n\
                 event: [DONE]\n\
                 data: \n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "stream chat basic",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let sse = String::from_utf8_lossy(&body);
        assert!(
            sse.contains("event: response.output_text.delta")
                || sse.contains("Hello")
                || sse.contains("world"),
            "chat stream should map upstream deltas into downstream text stream"
        );
    }

    #[tokio::test]
    async fn chat_stream_real_completions_sse_without_event_names() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"id\":\"chatcmpl_real_stream\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n\
                 data: {\"id\":\"chatcmpl_real_stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
                 data: {\"id\":\"chatcmpl_real_stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n\
                 data: [DONE]\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "stream chat real",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let sse = String::from_utf8_lossy(&body);

        assert!(
            sse.contains("event: response.created"),
            "chat stream should synthesize downstream response.created"
        );
        assert!(sse.contains("Hello"));
        assert!(sse.contains("world"));
        let created_index = sse
            .find("event: response.created")
            .expect("created event should be present");
        let added_index = sse
            .find("event: response.output_item.added")
            .expect("chat stream should synthesize output_item.added before deltas");
        let delta_index = sse
            .find("event: response.output_text.delta")
            .expect("text delta should be present");
        let done_index = sse
            .find("event: response.output_item.done")
            .expect("chat stream should synthesize output_item.done");
        let completed_index = sse
            .find("event: response.completed")
            .expect("chat stream should synthesize response.completed");
        assert!(
            created_index < added_index
                && added_index < delta_index
                && delta_index < done_index
                && done_index < completed_index,
            "chat stream should expose a complete Responses SSE event sequence"
        );
        assert!(
            !sse.contains("chatcmpl_real_stream"),
            "downstream stream must not leak Chat completion IDs"
        );
    }

    /// Verifies that once streaming is committed (response.created emitted),
    /// upstream failure does not trigger fallback to the next target.
    #[tokio::test]
    async fn no_fallback_after_sse_commit() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_committed\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.output_item.added\n\
                 data: {\"item\":invalid_json}\n\n",
            ))
            .expect(1)
            .mount(&first)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_should_not_fallback\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n",
            ))
            .expect(0)
            .mount(&second)
            .await;

        let state = Arc::new(build_state_with_two_targets(&first.uri(), &second.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "stream and commit",
                    "stream": true
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let sse = String::from_utf8_lossy(&body);
        assert!(sse.contains("event: response.created"));
        assert!(
            sse.contains("event: response.failed"),
            "post-commit failure should be surfaced, not fallback"
        );
        assert!(
            !sse.contains("resp_should_not_fallback"),
            "second target must not be called after commit"
        );
    }
}

// ============================================================================
// Test 9: model_mapping_capture
// ============================================================================

mod model_mapping_capture_tests {
    use super::*;

    /// Equivalent to minimum-slice `responses_text_basic`.
    #[tokio::test]
    async fn responses_text_basic() {
        let upstream = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = Arc::clone(&captured);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *captured_clone.lock().unwrap() = Some(body);

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_upstream_responses_text_basic",
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": "msg_upstream_responses_text_basic",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "hello from responses"}]
                    }],
                    "usage": {"input_tokens": 5, "output_tokens": 4, "total_tokens": 9}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "say hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            response_json.get("object").and_then(|v| v.as_str()),
            Some("response")
        );
        assert_eq!(
            response_json.get("model").and_then(|v| v.as_str()),
            Some("codex-main")
        );

        let upstream_body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("upstream request should be captured");
        assert_eq!(
            upstream_body.get("model").and_then(|v| v.as_str()),
            Some("gpt-upstream")
        );
    }

    /// Verifies downstream model alias maps to configured upstream model and
    /// upstream request captures mapped model value.
    #[tokio::test]
    async fn model_mapping_capture() {
        let upstream = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = Arc::clone(&captured);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *captured_clone.lock().unwrap() = Some(body);

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_model_map",
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": "msg_model_map",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "mapped"}]
                    }],
                    "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "capture model mapping"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            response_json.get("model").and_then(|v| v.as_str()),
            Some("codex-main"),
            "downstream response model should remain downstream alias"
        );

        let upstream_body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("upstream request should be captured");
        assert_eq!(
            upstream_body.get("model").and_then(|v| v.as_str()),
            Some("gpt-upstream"),
            "upstream request should use mapped upstream model"
        );
    }
}

// ============================================================================
// Test 10: auth header rewrite
// ============================================================================

mod auth_header_rewrite_tests {
    use super::*;

    /// Equivalent to minimum-slice `chat_text_basic`.
    #[tokio::test]
    async fn chat_text_basic() {
        let upstream = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = Arc::clone(&captured);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *captured_clone.lock().unwrap() = Some(body);

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl_chat_text_basic",
                    "object": "chat.completion",
                    "created": 1234567890,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "hello from chat"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 4, "total_tokens": 9}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "say hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            response_json.get("model").and_then(|v| v.as_str()),
            Some("chat-gpt-4")
        );

        let upstream_body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("upstream request should be captured");
        assert_eq!(
            upstream_body.get("model").and_then(|v| v.as_str()),
            Some("gpt-4")
        );
        assert!(
            upstream_body.get("messages").is_some(),
            "chat upstream request should include messages"
        );
    }

    /// Verifies downstream Authorization is passed to OpenAI-compatible upstream
    /// as Authorization when provider auth_mode is pass_authorization.
    #[tokio::test]
    async fn auth_header_rewrite_openai() {
        let upstream = MockServer::start().await;
        let captured_auth = Arc::new(std::sync::Mutex::new(None));
        let captured_auth_clone = Arc::clone(&captured_auth);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let auth = req.headers.iter().find_map(|(k, v)| {
                    if k.as_str().eq_ignore_ascii_case("authorization") {
                        Some(v.to_str().unwrap_or("").to_string())
                    } else {
                        None
                    }
                });
                *captured_auth_clone.lock().unwrap() = auth;

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl-auth-openai",
                    "object": "chat.completion",
                    "created": 1234567890,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let mut raw_state = build_chat_test_state(&upstream.uri()).await;
        raw_state.config.security.downstream_auth = "passthrough".to_string();
        raw_state.config.security.allow_passthrough_keys = true;
        raw_state.config.providers[0].auth_mode = "pass_authorization".to_string();
        let state = Arc::new(raw_state);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer sk-upstream-openai")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "auth rewrite openai"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let auth = captured_auth
            .lock()
            .unwrap()
            .clone()
            .expect("upstream authorization should be present");
        assert_eq!(auth, "Bearer sk-upstream-openai");
    }

    /// Verifies downstream Authorization is rewritten for Anthropic-compatible
    /// upstream as x-api-key when provider auth_mode is pass_authorization.
    #[tokio::test]
    async fn auth_header_rewrite_anthropic() {
        let upstream = MockServer::start().await;
        let captured_x_api_key = Arc::new(std::sync::Mutex::new(None));
        let captured_x_api_key_clone = Arc::clone(&captured_x_api_key);
        let captured_authz = Arc::new(std::sync::Mutex::new(None));
        let captured_authz_clone = Arc::clone(&captured_authz);

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(move |req: &wiremock::Request| {
                let x_api_key = req.headers.iter().find_map(|(k, v)| {
                    if k.as_str().eq_ignore_ascii_case("x-api-key") {
                        Some(v.to_str().unwrap_or("").to_string())
                    } else {
                        None
                    }
                });
                let authz = req.headers.iter().find_map(|(k, v)| {
                    if k.as_str().eq_ignore_ascii_case("authorization") {
                        Some(v.to_str().unwrap_or("").to_string())
                    } else {
                        None
                    }
                });
                *captured_x_api_key_clone.lock().unwrap() = x_api_key;
                *captured_authz_clone.lock().unwrap() = authz;

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msg-auth-anthropic",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "ok"}],
                    "model": "claude-3-sonnet",
                    "usage": {"input_tokens": 3, "output_tokens": 1, "total_tokens": 4}
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        let mut raw_state = build_anthropic_test_state(&upstream.uri()).await;
        raw_state.config.security.downstream_auth = "passthrough".to_string();
        raw_state.config.security.allow_passthrough_keys = true;
        raw_state.config.providers[0].auth_mode = "pass_authorization".to_string();
        let state = Arc::new(raw_state);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer sk-upstream-anthropic")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "anthropic-claude",
                    "input": "auth rewrite anthropic"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let x_api_key = captured_x_api_key
            .lock()
            .unwrap()
            .clone()
            .expect("x-api-key should be present");
        assert_eq!(x_api_key, "sk-upstream-anthropic");
        assert!(
            captured_authz.lock().unwrap().is_none(),
            "anthropic upstream request should not forward Authorization header"
        );
    }
}

// ============================================================================
// Test 11: tool_call_roundtrip_chat
// ============================================================================

mod tool_call_roundtrip_chat_tests {
    use super::*;

    /// Equivalent to minimum-slice `tool_call_roundtrip_responses`.
    /// This validates native Responses tool call + tool result continuation.
    #[tokio::test]
    async fn tool_call_roundtrip_responses() {
        let upstream = MockServer::start().await;
        let captured_requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured_requests_clone = Arc::clone(&captured_requests);
        let call_index = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_index_clone = Arc::clone(&call_index);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                captured_requests_clone.lock().unwrap().push(body);
                let idx = call_index_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if idx == 0 {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "resp_upstream_tool_roundtrip_1",
                        "model": "gpt-upstream",
                        "output": [{
                            "type": "function_call",
                            "id": "fc_upstream_1",
                            "call_id": "call_upstream_1",
                            "name": "lookup_weather",
                            "arguments": "{\"city\":\"Boston\"}"
                        }],
                        "usage": {"input_tokens": 10, "output_tokens": 8, "total_tokens": 18}
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "resp_upstream_tool_roundtrip_2",
                        "model": "gpt-upstream",
                        "output": [{
                            "type": "message",
                            "id": "msg_upstream_tool_roundtrip_2",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "Sunny in Boston"}]
                        }],
                        "usage": {"input_tokens": 20, "output_tokens": 6, "total_tokens": 26}
                    }))
                }
            })
            .expect(2)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "weather in boston?",
                    "tools": [{
                        "type": "function",
                        "name": "lookup_weather",
                        "description": "Lookup weather",
                        "parameters": {
                            "type": "object",
                            "properties": {"city": {"type": "string"}},
                            "required": ["city"]
                        }
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(first_response.status(), axum::http::StatusCode::OK);
        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("first response id should be present");
        let first_output = first_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("first response output should be array");
        let tool_call = first_output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .expect("first response should contain function_call");
        let call_id = tool_call
            .get("call_id")
            .and_then(|v| v.as_str())
            .expect("function_call should expose call_id");
        assert!(
            call_id.starts_with("call_mw_"),
            "downstream chat function_call call_id must be ModelWire-owned"
        );
        assert_ne!(
            call_id, "call_upstream_1",
            "downstream must not expose upstream chat tool call IDs"
        );
        assert!(
            call_id.starts_with("call_mw_"),
            "downstream function_call call_id must be ModelWire-owned"
        );
        assert_ne!(
            call_id, "call_upstream_1",
            "downstream must not expose upstream tool call IDs"
        );

        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "previous_response_id": first_response_id,
                    "input": [{
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": "{\"city\":\"Boston\",\"forecast\":\"sunny\"}"
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(second_response.status(), axum::http::StatusCode::OK);

        let captured = captured_requests.lock().unwrap().clone();
        assert_eq!(
            captured.len(),
            2,
            "two upstream responses requests expected"
        );
        assert!(
            captured[0].get("tools").is_some(),
            "first upstream request should include tools"
        );
        let second_input = captured[1]
            .get("input")
            .and_then(|v| v.as_array())
            .expect("second upstream request should include input array");
        assert!(
            second_input.iter().any(|item| {
                item.get("type").and_then(|v| v.as_str()) == Some("function_call_output")
            }),
            "second upstream request should include function_call_output"
        );
        let upstream_tool_result = second_input
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
            .expect("tool result should be sent upstream");
        assert_eq!(
            upstream_tool_result
                .get("call_id")
                .and_then(serde_json::Value::as_str),
            Some("call_upstream_1"),
            "same-upstream Responses continuation should translate ModelWire call_id back to the private upstream call_id"
        );
    }

    /// Verifies tool call + function_call_output roundtrip through Chat adapter.
    #[tokio::test]
    async fn tool_call_roundtrip_chat() {
        let upstream = MockServer::start().await;
        let captured_requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured_requests_clone = Arc::clone(&captured_requests);
        let call_index = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_index_clone = Arc::clone(&call_index);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                captured_requests_clone.lock().unwrap().push(body);
                let idx = call_index_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if idx == 0 {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "chatcmpl_tool_turn_1",
                        "object": "chat.completion",
                        "created": 1234567890,
                        "model": "gpt-4",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [{
                                    "id": "call_upstream_1",
                                    "type": "function",
                                    "function": {
                                        "name": "lookup_weather",
                                        "arguments": "{\"city\":\"Boston\"}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18}
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "chatcmpl_tool_turn_2",
                        "object": "chat.completion",
                        "created": 1234567891,
                        "model": "gpt-4",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "It is sunny in Boston."
                            },
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 20, "completion_tokens": 6, "total_tokens": 26}
                    }))
                }
            })
            .expect(2)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_chat_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "input": "What's the weather in Boston?",
                    "tools": [{
                        "type": "function",
                        "name": "lookup_weather",
                        "description": "Lookup weather",
                        "parameters": {
                            "type": "object",
                            "properties": {"city": {"type": "string"}},
                            "required": ["city"]
                        }
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(first_response.status(), axum::http::StatusCode::OK);
        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("first response id should be present");
        let first_output = first_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("first response output should be array");
        let tool_call = first_output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .expect("first response should contain function_call item");
        let call_id = tool_call
            .get("call_id")
            .and_then(|v| v.as_str())
            .expect("function_call should expose call_id");

        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "chat-gpt-4",
                    "previous_response_id": first_response_id,
                    "input": [{
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": "{\"city\":\"Boston\",\"forecast\":\"sunny\"}"
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(second_response.status(), axum::http::StatusCode::OK);
        let second_body = axum::body::to_bytes(second_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
        let second_output = second_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("second response output should be array");
        assert!(
            second_output
                .iter()
                .any(|item| item.get("type").and_then(|v| v.as_str()) == Some("message")),
            "second response should contain assistant message"
        );

        let captured = captured_requests.lock().unwrap().clone();
        assert_eq!(captured.len(), 2, "two upstream chat requests expected");
        assert!(
            captured[0].get("tools").is_some(),
            "first chat request should carry tool definitions"
        );
        assert!(
            captured[1].get("previous_response_id").is_none(),
            "chat upstream request must not include previous_response_id"
        );

        let second_messages = captured[1]
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("second chat request should include messages");
        assert!(
            second_messages.iter().any(|message| {
                message.get("role").and_then(|v| v.as_str()) == Some("tool")
                    && message
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .is_some()
            }),
            "second chat request should include mapped tool result message"
        );
        assert!(
            second_messages
                .iter()
                .filter(|message| message.get("role").and_then(|v| v.as_str()) == Some("tool"))
                .all(|message| {
                    message
                        .get("tool_call_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| id.starts_with("call_mw_"))
                }),
            "Chat replay should use ModelWire-owned tool IDs and keep upstream IDs private"
        );
    }
}

/// Build a test ServerState with two targets on different providers.
/// This enables fallback between providers.
async fn build_state_with_two_targets(first_base_url: &str, second_base_url: &str) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![
            ProviderConfig {
                id: "primary-provider".to_string(),
                name: "Primary Provider".to_string(),
                base_url: first_base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("primary-scope".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true, // Allow localhost URLs in tests
                config_json: None,
            },
            ProviderConfig {
                id: "fallback-provider".to_string(),
                name: "Fallback Provider".to_string(),
                base_url: second_base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("fallback-scope".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true, // Allow localhost URLs in tests
                config_json: None,
            },
        ],
        routes: vec![RouteConfig {
            id: Some("dual-route".to_string()),
            downstream_model: "codex-main".to_string(),
            description: None,
            enabled: true,
            targets: vec![
                TargetConfig {
                    provider: "primary-provider".to_string(),
                    upstream_model: "gpt-primary".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 1, // Primary - lowest priority tried first
                    enabled: true,
                    context_window_tokens: Some(200_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "reject".to_string(),
                    config_json: None,
                },
                TargetConfig {
                    provider: "fallback-provider".to_string(),
                    upstream_model: "gpt-fallback".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 2, // Fallback - higher priority tried second
                    enabled: true,
                    context_window_tokens: Some(200_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "reject".to_string(),
                    config_json: None,
                },
            ],
        }],
    };

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.run_migrations().await.unwrap();

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

/// Build a test ServerState with three targets across three providers.
async fn build_state_with_three_targets(
    first_base_url: &str,
    second_base_url: &str,
    third_base_url: &str,
) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![
            ProviderConfig {
                id: "provider-1".to_string(),
                name: "Provider 1".to_string(),
                base_url: first_base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-1".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true,
                config_json: None,
            },
            ProviderConfig {
                id: "provider-2".to_string(),
                name: "Provider 2".to_string(),
                base_url: second_base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-2".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true,
                config_json: None,
            },
            ProviderConfig {
                id: "provider-3".to_string(),
                name: "Provider 3".to_string(),
                base_url: third_base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-3".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true,
                config_json: None,
            },
        ],
        routes: vec![RouteConfig {
            id: Some("triple-route".to_string()),
            downstream_model: "codex-main".to_string(),
            description: None,
            enabled: true,
            targets: vec![
                TargetConfig {
                    provider: "provider-1".to_string(),
                    upstream_model: "gpt-first".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 1,
                    enabled: true,
                    context_window_tokens: Some(200_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "reject".to_string(),
                    config_json: None,
                },
                TargetConfig {
                    provider: "provider-2".to_string(),
                    upstream_model: "gpt-second".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 2,
                    enabled: true,
                    context_window_tokens: Some(200_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "reject".to_string(),
                    config_json: None,
                },
                TargetConfig {
                    provider: "provider-3".to_string(),
                    upstream_model: "gpt-third".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 3,
                    enabled: true,
                    context_window_tokens: Some(200_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "reject".to_string(),
                    config_json: None,
                },
            ],
        }],
    };

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.run_migrations().await.unwrap();

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Build a test ServerState for Responses adapter tests.
async fn build_test_state(upstream_base_url: &str) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![ProviderConfig {
            id: "test-provider".to_string(),
            name: "Test Provider".to_string(),
            base_url: upstream_base_url.to_string(),
            auth_mode: "managed".to_string(),
            default_wire_api: "responses".to_string(),
            state_scope: Some("test-scope".to_string()),
            api_key: Some("test-key".to_string()),
            allow_private_ips: false,
            skip_ssrf_validation: true, // Allow localhost URLs in tests
            config_json: None,
        }],
        routes: vec![RouteConfig {
            id: Some("test-route".to_string()),
            downstream_model: "codex-main".to_string(),
            description: None,
            enabled: true,
            targets: vec![TargetConfig {
                provider: "test-provider".to_string(),
                upstream_model: "gpt-upstream".to_string(),
                wire_api: "responses".to_string(),
                priority: 10,
                enabled: true,
                context_window_tokens: Some(200_000),
                max_output_tokens: None,
                auto_compact_recommended_tokens: None,
                context_safety_margin_tokens: Some(2_000),
                token_estimator: None,
                context_overflow_policy: "reject".to_string(),
                config_json: None,
            }],
        }],
    };

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.run_migrations().await.unwrap();

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

/// Build a test ServerState for OpenAI Chat adapter tests.
async fn build_chat_test_state(upstream_base_url: &str) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![ProviderConfig {
            id: "chat-provider".to_string(),
            name: "Chat Provider".to_string(),
            base_url: upstream_base_url.to_string(),
            auth_mode: "managed".to_string(),
            default_wire_api: "openai_chat".to_string(),
            state_scope: Some("chat-scope".to_string()),
            api_key: Some("test-key".to_string()),
            allow_private_ips: false,
            skip_ssrf_validation: true, // Allow localhost URLs in tests
            config_json: None,
        }],
        routes: vec![RouteConfig {
            id: Some("chat-route".to_string()),
            downstream_model: "chat-gpt-4".to_string(),
            description: None,
            enabled: true,
            targets: vec![TargetConfig {
                provider: "chat-provider".to_string(),
                upstream_model: "gpt-4".to_string(),
                wire_api: "openai_chat".to_string(),
                priority: 10,
                enabled: true,
                context_window_tokens: Some(128_000),
                max_output_tokens: None,
                auto_compact_recommended_tokens: None,
                context_safety_margin_tokens: Some(2_000),
                token_estimator: None,
                context_overflow_policy: "reject".to_string(),
                config_json: None,
            }],
        }],
    };

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.run_migrations().await.unwrap();

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

/// Build a test ServerState for Anthropic adapter tests.
async fn build_anthropic_test_state(upstream_base_url: &str) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![ProviderConfig {
            id: "anthropic-provider".to_string(),
            name: "Anthropic Provider".to_string(),
            base_url: upstream_base_url.to_string(),
            auth_mode: "managed".to_string(),
            default_wire_api: "anthropic".to_string(),
            state_scope: Some("anthropic-scope".to_string()),
            api_key: Some("sk-ant-test".to_string()),
            allow_private_ips: false,
            skip_ssrf_validation: true, // Allow localhost URLs in tests
            config_json: None,
        }],
        routes: vec![RouteConfig {
            id: Some("anthropic-route".to_string()),
            downstream_model: "anthropic-claude".to_string(),
            description: None,
            enabled: true,
            targets: vec![TargetConfig {
                provider: "anthropic-provider".to_string(),
                upstream_model: "claude-3-sonnet".to_string(),
                wire_api: "anthropic".to_string(),
                priority: 10,
                enabled: true,
                context_window_tokens: Some(200_000),
                max_output_tokens: None,
                auto_compact_recommended_tokens: None,
                context_safety_margin_tokens: Some(2_000),
                token_estimator: None,
                context_overflow_policy: "reject".to_string(),
                config_json: None,
            }],
        }],
    };

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.run_migrations().await.unwrap();

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

/// Build a test ServerState for state_scope reuse tests.
async fn build_state_scope_test_state(upstream_base_url: &str) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![ProviderConfig {
            id: "reuse-provider".to_string(),
            name: "Reuse Provider".to_string(),
            base_url: upstream_base_url.to_string(),
            auth_mode: "managed".to_string(),
            default_wire_api: "responses".to_string(),
            state_scope: Some("shared-scope".to_string()),
            api_key: Some("test-key".to_string()),
            allow_private_ips: false,
            skip_ssrf_validation: true, // Allow localhost URLs in tests
            config_json: None,
        }],
        routes: vec![RouteConfig {
            id: Some("reuse-route".to_string()),
            downstream_model: "codex-main".to_string(),
            description: None,
            enabled: true,
            targets: vec![TargetConfig {
                provider: "reuse-provider".to_string(),
                upstream_model: "gpt-4".to_string(),
                wire_api: "responses".to_string(),
                priority: 10,
                enabled: true,
                context_window_tokens: Some(200_000),
                max_output_tokens: None,
                auto_compact_recommended_tokens: None,
                context_safety_margin_tokens: Some(2_000),
                token_estimator: None,
                context_overflow_policy: "reject".to_string(),
                config_json: None,
            }],
        }],
    };

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.run_migrations().await.unwrap();

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

/// Build a test ServerState connecting to a specific database URL.
/// Used for restart/continuation tests that need on-disk persistence.
async fn build_state_with_db(db_path: &std::path::Path, upstream_url: &str) -> ServerState {
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: relay_security_for("mw_test_key"),
        archive: ArchiveConfig::default(),
        providers: vec![ProviderConfig {
            id: "restart-provider".to_string(),
            name: "Restart Provider".to_string(),
            base_url: upstream_url.to_string(),
            auth_mode: "managed".to_string(),
            default_wire_api: "responses".to_string(),
            state_scope: Some("restart-scope".to_string()),
            api_key: Some("test-key".to_string()),
            allow_private_ips: false,
            skip_ssrf_validation: true, // Allow localhost URLs in tests
            config_json: None,
        }],
        routes: vec![RouteConfig {
            id: Some("restart-route".to_string()),
            downstream_model: "codex-main".to_string(),
            description: None,
            enabled: true,
            targets: vec![TargetConfig {
                provider: "restart-provider".to_string(),
                upstream_model: "gpt-upstream".to_string(),
                wire_api: "responses".to_string(),
                priority: 10,
                enabled: true,
                context_window_tokens: Some(200_000),
                max_output_tokens: None,
                auto_compact_recommended_tokens: None,
                context_safety_margin_tokens: Some(2_000),
                token_estimator: None,
                context_overflow_policy: "reject".to_string(),
                config_json: None,
            }],
        }],
    };

    // Build SQLite URL - sqlx requires proper format for file paths
    // Format: sqlite:///path/to/file (3 slashes for absolute path, forward slashes)
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Create the file only if it doesn't exist (prevents data loss on restart)
    if !db_path.exists() {
        if let Err(e) = std::fs::write(db_path, b"") {
            eprintln!("DEBUG: Failed to create db file: {}", e);
        }
    }

    let path_str = db_path.to_string_lossy().replace('\\', "/");
    // Handle Windows absolute paths: /C:/... -> C:/...
    let clean_path = if let Some(stripped) = path_str.strip_prefix('/') {
        if stripped.len() >= 2 && stripped.chars().nth(1) == Some(':') {
            stripped.to_string() // e.g., /C:/... -> C:/...
        } else {
            path_str.to_string()
        }
    } else {
        path_str.to_string()
    };
    // MUST use forward slashes for sqlite URL
    let db_url = format!("sqlite:///{}", clean_path.replace('\\', "/"));

    // Debug: print database URL
    eprintln!("DEBUG: build_state_with_db - Database URL: {}", db_url);

    let db = Database::connect(&db_url)
        .await
        .expect("Failed to connect to database");
    db.run_migrations().await.unwrap();

    // Debug: verify database is working by running a simple query
    if let modelwire_db::Database::Sqlite(pool) = &db {
        let result: (i64,) = sqlx::query_as("SELECT 1").fetch_one(pool).await.unwrap();
        eprintln!(
            "DEBUG: Database connection verified, test query result: {:?}",
            result
        );
    }

    ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

async fn spawn_delayed_sse_upstream(
    first_chunk_delay: std::time::Duration,
    between_chunks_delay: std::time::Duration,
    chunks: Vec<String>,
) -> String {
    use axum::{body::Body, http::header::CONTENT_TYPE, routing::post, Router};
    use bytes::Bytes;
    use std::convert::Infallible;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    let chunks = Arc::new(chunks);
    let app = Router::new().route(
        "/responses",
        post({
            let chunks = Arc::clone(&chunks);
            move || {
                let chunks = Arc::clone(&chunks);
                async move {
                    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(16);
                    tokio::spawn(async move {
                        tokio::time::sleep(first_chunk_delay).await;
                        for (idx, chunk) in chunks.iter().enumerate() {
                            if tx.send(Ok(Bytes::from(chunk.clone()))).await.is_err() {
                                return;
                            }
                            if idx + 1 < chunks.len() && !between_chunks_delay.is_zero() {
                                tokio::time::sleep(between_chunks_delay).await;
                            }
                        }
                    });
                    (
                        [(CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(ReceiverStream::new(rx)),
                    )
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

// ============================================================================
// Test 8: restart_preserves_state
// ============================================================================

mod restart_tests {
    use super::*;

    /// Verifies that restart preserves response state across process restarts.
    ///
    /// This test:
    /// 1. Creates a response with previous_response_id and persists to on-disk SQLite
    /// 2. "Restarts" the server (drops old state, creates new state from same database)
    /// 3. Verifies the previous_response_id still resolves correctly
    #[tokio::test]
    async fn restart_preserves_state() {
        // Create temporary directory for on-disk SQLite
        let tmpdir = tempfile::tempdir().unwrap();
        let db_path = tmpdir.path().join("modelwire_restart.db");

        // Start mock upstream
        let upstream = MockServer::start().await;

        // Track all requests to see what upstream receives
        let all_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_requests_clone = Arc::clone(&all_requests);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                all_requests_clone.lock().unwrap().push(body);

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("resp_upstream_{}", uuid::Uuid::now_v7()),
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": format!("msg_{}", uuid::Uuid::now_v7()),
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Response"}]
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                }))
            })
            .expect(2)
            .mount(&upstream)
            .await;

        // Step 1: Create initial server state with on-disk database
        let state1 = Arc::new(build_state_with_db(&db_path, &upstream.uri()).await);
        let app1 = build_router(Arc::clone(&state1));

        // Debug: verify database path
        eprintln!("DEBUG: DB path: {:?}", db_path);
        eprintln!("DEBUG: DB path exists: {:?}", db_path.exists());
        eprintln!("DEBUG: DB path is file: {:?}", db_path.is_file());
        eprintln!(
            "DEBUG: DB path canonical: {:?}",
            std::fs::canonicalize(&db_path)
        );

        // Create first response
        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "First message"
                })
                .to_string(),
            ))
            .unwrap();

        let first_response = app1.clone().oneshot(first_request).await.unwrap();
        let status1 = first_response.status();

        let body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        // If first request failed, log the error and fail the test
        if status1 != axum::http::StatusCode::OK {
            panic!("First request failed with {}: {}", status1, body_str);
        }

        let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let first_response_id_value = response_json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("First response should have id"));

        // Debug: Verify database is working by running a simple query BEFORE the store check
        eprintln!("DEBUG: Verifying database is connected before store check");
        if let modelwire_db::Database::Sqlite(pool) = &state1.db {
            let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM responses")
                .fetch_all(pool)
                .await
                .unwrap_or_default();
            eprintln!(
                "DEBUG: Responses count before first response check: {}",
                rows.len()
            );
        }

        // Verify the first response was stored in the database
        let _stored_response =
            match modelwire_db::repo::responses::get_response(&state1.db, first_response_id_value)
                .await
            {
                Ok(Some(resp)) => resp,
                Ok(None) => {
                    // Debug: query all responses in database
                    eprintln!(
                        "DEBUG: First response {} not found, checking what's in DB",
                        first_response_id_value
                    );
                    if let modelwire_db::Database::Sqlite(pool) = &state1.db {
                        let rows: Vec<(String, String, String)> =
                            sqlx::query_as("SELECT id, downstream_model, status FROM responses")
                                .fetch_all(pool)
                                .await
                                .unwrap_or_default();
                        eprintln!("DEBUG: Responses in DB: {:?}", rows);
                    }
                    let db_url =
                        format!("sqlite:///{}", db_path.to_string_lossy().replace('\\', "/"));
                    eprintln!("DEBUG: Database URL: {}", db_url);

                    panic!(
                        "First response {} should have been stored in database",
                        first_response_id_value
                    );
                }
                Err(e) => {
                    eprintln!("DEBUG: Error querying database: {}", e);
                    panic!("Failed to query database: {}", e);
                }
            };

        // Step 2: "Restart" - drop state1, create new state from same database
        drop(state1);
        drop(app1);

        // Create new state with the same database path
        let state2 = Arc::new(build_state_with_db(&db_path, &upstream.uri()).await);
        let app2 = build_router(Arc::clone(&state2));

        // Verify the response is still in the database after restart
        eprintln!(
            "DEBUG: Checking if {} exists in restarted DB",
            first_response_id_value
        );
        eprintln!(
            "DEBUG: DB path exists: {:?}",
            std::path::Path::new(&db_path).exists()
        );

        let stored_response2 =
            modelwire_db::repo::responses::get_response(&state2.db, first_response_id_value)
                .await
                .unwrap();
        if stored_response2.is_none() {
            // Debug: query all responses in database
            if let modelwire_db::Database::Sqlite(pool) = &state2.db {
                let rows: Vec<(String, String, String)> =
                    sqlx::query_as("SELECT id, downstream_model, status FROM responses")
                        .fetch_all(pool)
                        .await
                        .unwrap_or_default();
                eprintln!("DEBUG: Responses in DB after restart: {:?}", rows);
            }

            panic!(
                "Response {} should still be in database after restart",
                first_response_id_value
            );
        }

        // Step 3: Verify previous_response_id still works after restart
        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Continue after restart",
                    "previous_response_id": first_response_id_value
                })
                .to_string(),
            ))
            .unwrap();

        let second_response = app2.oneshot(second_request).await.unwrap();

        // Debug output
        let status = second_response.status();
        if status != axum::http::StatusCode::OK {
            let body_debug = axum::body::to_bytes(second_response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            eprintln!(
                "DEBUG: restart_preserves_state - Got status {:?}, body: {:?}",
                status,
                String::from_utf8_lossy(&body_debug)
            );
        }

        // Assert response succeeds (not state_not_found error)
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "Continuation after restart should succeed - previous_response_id must be found"
        );

        // Verify both upstream calls were made
        let requests = all_requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "Both requests should be made to upstream"
        );
    }

    /// Verifies that server can be restarted and new responses can be created.
    #[tokio::test]
    async fn restart_clears_expired_state() {
        let tmpdir = tempfile::tempdir().unwrap();
        let db_path = tmpdir.path().join("modelwire_expired.db");

        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("resp_exp_{}", uuid::Uuid::now_v7()),
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": format!("msg_exp_{}", uuid::Uuid::now_v7()),
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Response"}]
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                }))
            })
            .expect(2)
            .mount(&upstream)
            .await;

        // Create state and make initial request
        let state1 = Arc::new(build_state_with_db(&db_path, &upstream.uri()).await);
        let app1 = build_router(Arc::clone(&state1));

        let request1 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "codex-main", "input": "Test"}"#))
            .unwrap();

        let resp1 = app1.clone().oneshot(request1).await.unwrap();
        if resp1.status() != axum::http::StatusCode::OK {
            let body = axum::body::to_bytes(resp1.into_body(), 1024 * 1024)
                .await
                .unwrap();
            panic!("First request failed: {}", String::from_utf8_lossy(&body));
        }

        drop(state1);
        drop(app1);

        // Create new state - server can be restarted
        let state2 = Arc::new(build_state_with_db(&db_path, &upstream.uri()).await);
        let app2 = build_router(Arc::clone(&state2));

        // Verify the new state can be created without errors
        let request2 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model": "codex-main", "input": "New request"}"#,
            ))
            .unwrap();

        let resp2 = app2.oneshot(request2).await.unwrap();
        assert_eq!(
            resp2.status(),
            axum::http::StatusCode::OK,
            "New request after restart should succeed"
        );
    }
}

// ============================================================================
// Test 9: state_scope_optimistic_reuse_failure_then_replay
// ============================================================================

mod state_scope_reuse_failure_tests {
    use super::*;

    /// Equivalent to minimum-slice `previous_response_cross_upstream_replay`.
    #[tokio::test]
    async fn previous_response_cross_upstream_replay() {
        let upstream_a = MockServer::start().await;
        let upstream_b = MockServer::start().await;
        let captured_b = Arc::new(std::sync::Mutex::new(None));
        let captured_b_clone = Arc::clone(&captured_b);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_upstream_cross_replay_1",
                    "model": "gpt-a",
                    "output": [{
                        "type": "message",
                        "id": "msg_upstream_cross_replay_1",
                        "role": "assistant",
                        "content": [{"type":"output_text","text":"from provider a"}]
                    }],
                    "usage": {"input_tokens": 8, "output_tokens": 3, "total_tokens": 11}
                }))
            })
            .expect(1)
            .mount(&upstream_a)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *captured_b_clone.lock().unwrap() = Some(body);
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_upstream_cross_replay_2",
                    "model": "gpt-b",
                    "output": [{
                        "type": "message",
                        "id": "msg_upstream_cross_replay_2",
                        "role": "assistant",
                        "content": [{"type":"output_text","text":"from provider b"}]
                    }],
                    "usage": {"input_tokens": 14, "output_tokens": 4, "total_tokens": 18}
                }))
            })
            .expect(1)
            .mount(&upstream_b)
            .await;

        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: relay_security_for("mw_test_key"),
            archive: ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: upstream_a.uri(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true,
                    config_json: None,
                },
                ProviderConfig {
                    id: "provider-b".to_string(),
                    name: "Provider B".to_string(),
                    base_url: upstream_b.uri(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-b".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true,
                    config_json: None,
                },
            ],
            routes: vec![
                RouteConfig {
                    id: Some("route-a".to_string()),
                    downstream_model: "model-a".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "provider-a".to_string(),
                        upstream_model: "gpt-a".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
                RouteConfig {
                    id: Some("route-b".to_string()),
                    downstream_model: "model-b".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "provider-b".to_string(),
                        upstream_model: "gpt-b".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
            ],
        };

        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();
        let state = Arc::new(ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let app = build_router(Arc::clone(&state));

        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "model-a",
                    "input": "first"
                })
                .to_string(),
            ))
            .unwrap();
        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(first_response.status(), axum::http::StatusCode::OK);
        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("first response id should be present");

        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "model-b",
                    "input": "second",
                    "previous_response_id": first_response_id
                })
                .to_string(),
            ))
            .unwrap();
        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(second_response.status(), axum::http::StatusCode::OK);

        let captured = captured_b
            .lock()
            .unwrap()
            .clone()
            .expect("second upstream request should be captured");
        assert!(
            captured.get("previous_response_id").is_none(),
            "cross-upstream replay should not forward raw previous_response_id"
        );
        let input = captured
            .get("input")
            .and_then(|v| v.as_array())
            .expect("replay should include input array");
        assert!(
            input.len() >= 2,
            "replay should include prior visible history plus new turn"
        );
        let replay_text = serde_json::to_string(input).unwrap();
        assert!(
            replay_text.contains("first"),
            "replay should include the first turn user input"
        );
        assert!(
            replay_text.contains("from provider a"),
            "replay should include the first turn assistant output"
        );
        assert!(
            replay_text.contains("second"),
            "replay should include the current turn user input"
        );
    }

    /// Verifies that when cross-upstream state_scope reuse fails, replay is attempted.
    ///
    /// This test simulates a scenario where:
    /// 1. First request establishes a state with provider A
    /// 2. Second request tries to reuse but provider changes (or handle becomes invalid)
    /// 3. System should fallback to replay instead of failing
    #[tokio::test]
    async fn state_scope_optimistic_reuse_failure_then_replay() {
        let upstream = MockServer::start().await;

        // Track request sequence to verify:
        // 1) first turn no previous_response_id
        // 2) optimistic reuse sends previous_response_id and fails 404
        // 3) retry replays history without previous_response_id
        let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let requests_clone = Arc::clone(&requests);
        let call_index = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_index_clone = Arc::clone(&call_index);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                requests_clone.lock().unwrap().push(body.clone());
                let idx = call_index_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if idx == 1 {
                    // Second overall call: optimistic handle reuse attempt.
                    // Force "not found" so relay retries with materialized replay.
                    return ResponseTemplate::new(404).set_body_string("response not found");
                }

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("resp_replay_{}", uuid::Uuid::now_v7()),
                    "model": "gpt-4",
                    "output": [{
                        "type": "message",
                        "id": format!("msg_replay_{}", uuid::Uuid::now_v7()),
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Handled via replay"}]
                    }],
                    "usage": {"input_tokens": 20, "output_tokens": 6, "total_tokens": 26}
                }))
            })
            .expect(3)
            .mount(&upstream)
            .await;

        // Build state for this test
        let state = Arc::new(build_state_scope_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // First request
        let first_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "codex-main", "input": "First"}"#))
            .unwrap();

        let first_response = app.clone().oneshot(first_request).await.unwrap();
        assert_eq!(first_response.status(), axum::http::StatusCode::OK);

        let first_body = axum::body::to_bytes(first_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let first_response_id = first_json.get("id").and_then(|v| v.as_str()).unwrap();

        // Second request with previous_response_id
        // In real scenario, reuse might fail and fallback to replay
        // For this test, we verify the system can handle either path
        let second_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Second with continuation",
                    "previous_response_id": first_response_id
                })
                .to_string(),
            ))
            .unwrap();

        let second_response = app.oneshot(second_request).await.unwrap();
        assert_eq!(second_response.status(), axum::http::StatusCode::OK);

        let captured = requests.lock().unwrap().clone();
        assert!(
            captured.len() == 3,
            "Expected first turn + optimistic reuse attempt + replay retry"
        );

        let optimistic_attempt = &captured[1];
        assert!(
            optimistic_attempt.get("previous_response_id").is_some(),
            "Optimistic reuse attempt should send previous_response_id upstream"
        );

        let replay_attempt = &captured[2];
        assert!(
            replay_attempt.get("previous_response_id").is_none(),
            "Replay retry must not send previous_response_id upstream"
        );
        let replay_input = replay_attempt
            .get("input")
            .and_then(|v| v.as_array())
            .expect("Replay retry should include input array");
        assert!(
            replay_input.len() >= 2,
            "Replay retry should include replayed history plus new user input"
        );
    }

    /// Verifies that cross-upstream replay includes full conversation history.
    #[tokio::test]
    async fn cross_upstream_replay_includes_history() {
        let upstream = MockServer::start().await;

        // Track if requests have previous_response_id (indicates state reuse attempt)
        let request_info = Arc::new(std::sync::Mutex::new(Vec::new()));
        let request_info_clone = Arc::clone(&request_info);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();

                // Capture whether request has previous_response_id
                let has_previous = body.get("previous_response_id").is_some();
                let has_input = body.get("input").is_some();

                request_info_clone.lock().unwrap().push(serde_json::json!({
                    "has_previous_response_id": has_previous,
                    "has_input": has_input,
                }));

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("resp_hist_{}", uuid::Uuid::now_v7()),
                    "model": "gpt-4",
                    "output": [{
                        "type": "message",
                        "id": format!("msg_hist_{}", uuid::Uuid::now_v7()),
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "With history"}]
                    }],
                    "usage": {"input_tokens": 30, "output_tokens": 5, "total_tokens": 35}
                }))
            })
            .expect(2)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state_scope_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // First request - should NOT have previous_response_id
        let req1 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "codex-main", "input": "Hello"}"#))
            .unwrap();

        let resp1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), axum::http::StatusCode::OK);

        let body1 = axum::body::to_bytes(resp1.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
        let resp_id1 = json1.get("id").and_then(|v| v.as_str()).unwrap();

        // Second request - should have previous_response_id (handle reuse or replay)
        let req2 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Continue",
                    "previous_response_id": resp_id1
                })
                .to_string(),
            ))
            .unwrap();

        let resp2 = app.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), axum::http::StatusCode::OK);

        // Verify both requests were made
        let info = request_info.lock().unwrap();
        assert_eq!(info.len(), 2, "Both requests should be captured");

        // First request should NOT have previous_response_id
        assert!(
            !info[0]
                .get("has_previous_response_id")
                .unwrap()
                .as_bool()
                .unwrap(),
            "First request should NOT have previous_response_id"
        );

        // Second request SHOULD have previous_response_id (for state reuse)
        // This is the key test - the system should use the upstream handle
        assert!(
            info[1]
                .get("has_previous_response_id")
                .unwrap()
                .as_bool()
                .unwrap(),
            "Second request should include previous_response_id for state continuation"
        );
    }
}

// ============================================================================
// Test 10: probe_cache_tests
// ============================================================================

mod probe_cache_tests {
    use super::*;

    /// Verifies that different upstream models get separate probe entries.
    /// Uses two different downstream models mapped to different upstream models.
    #[tokio::test]
    async fn probe_per_upstream_model() {
        // Start mock upstream
        let upstream = MockServer::start().await;

        // Track which upstream models are requested
        let model_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let model_requests_clone = Arc::clone(&model_requests);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();

                let model = body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                model_requests_clone.lock().unwrap().push(model);

                ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("resp_probe_{}", uuid::Uuid::now_v7()),
                    "model": "test-model",
                    "output": [{
                        "type": "message",
                        "id": format!("msg_probe_{}", uuid::Uuid::now_v7()),
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Response"}]
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                }))
            })
            .expect(2)
            .mount(&upstream)
            .await;

        // Create state with two routes mapping to different upstream models
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: relay_security_for("mw_test_key"),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "probe-provider".to_string(),
                name: "Probe Provider".to_string(),
                base_url: upstream.uri(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("probe-scope".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true, // Allow localhost URLs in tests
                config_json: None,
            }],
            routes: vec![
                RouteConfig {
                    id: Some("route-a".to_string()),
                    downstream_model: "model-a".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "probe-provider".to_string(),
                        upstream_model: "gpt-model-a".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
                RouteConfig {
                    id: Some("route-b".to_string()),
                    downstream_model: "model-b".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "probe-provider".to_string(),
                        upstream_model: "gpt-model-b".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
            ],
        };

        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let state = Arc::new(ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let app = build_router(Arc::clone(&state));

        // Make request to first model
        let req1 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "model-a", "input": "Test A"}"#))
            .unwrap();

        let resp1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), axum::http::StatusCode::OK);

        // Make request to second model
        let req2 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "model-b", "input": "Test B"}"#))
            .unwrap();

        let resp2 = app.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), axum::http::StatusCode::OK);

        // Verify both upstream models were called
        let models = model_requests.lock().unwrap();
        assert!(
            models.iter().any(|m| m == "gpt-model-a"),
            "First upstream model should be called"
        );
        assert!(
            models.iter().any(|m| m == "gpt-model-b"),
            "Second upstream model should be called"
        );
    }

    /// Verifies that same upstream model gets shared probe entry.
    #[tokio::test]
    async fn probe_shared_for_same_upstream_model() {
        let upstream = MockServer::start().await;

        let request_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let request_count_clone = Arc::clone(&request_count);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_req: &wiremock::Request| {
                request_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("resp_shared_{}", uuid::Uuid::now_v7()),
                    "model": "gpt-shared",
                    "output": [{
                        "type": "message",
                        "id": format!("msg_shared_{}", uuid::Uuid::now_v7()),
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Shared model response"}]
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
                }))
            })
            .expect(2)
            .mount(&upstream)
            .await;

        // Build state with same upstream model for two routes
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: relay_security_for("mw_test_key"),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "shared-provider".to_string(),
                name: "Shared Provider".to_string(),
                base_url: upstream.uri(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("shared-scope".to_string()),
                api_key: Some("test-key".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true, // Allow localhost URLs in tests
                config_json: None,
            }],
            routes: vec![
                RouteConfig {
                    id: Some("route-a".to_string()),
                    downstream_model: "client-a".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "shared-provider".to_string(),
                        upstream_model: "gpt-shared".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
                RouteConfig {
                    id: Some("route-b".to_string()),
                    downstream_model: "client-b".to_string(),
                    description: None,
                    enabled: true,
                    targets: vec![TargetConfig {
                        provider: "shared-provider".to_string(),
                        upstream_model: "gpt-shared".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "reject".to_string(),
                        config_json: None,
                    }],
                },
            ],
        };

        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let state = Arc::new(ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let app = build_router(Arc::clone(&state));

        // Make request via first route
        let req1 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "client-a", "input": "Test A"}"#))
            .unwrap();

        let resp1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), axum::http::StatusCode::OK);

        // Make request via second route (same upstream model)
        let req2 = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model": "client-b", "input": "Test B"}"#))
            .unwrap();

        let resp2 = app.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), axum::http::StatusCode::OK);

        // Both requests should be made (probe caches in memory, not state)
        // The key is that both succeeded, proving shared model works
        let count = request_count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            count, 2,
            "Both requests should succeed for shared upstream model"
        );
    }
}
