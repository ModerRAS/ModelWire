//! Codex-style end-to-end slice tests.
//!
//! These tests verify the complete relay pipeline through actual HTTP endpoints.
//! Each test:
//! 1. Starts a mock upstream server using wiremock
//! 2. Sends a real HTTP request to ModelWire's /v1/responses
//! 3. Verifies the downstream response matches expected Responses API shape
//! 4. Verifies the upstream request was correctly transformed

use axum::{body::Body, http::Request};
use modelwire_core::{
    hash_key_for_logging, ArchiveConfig, Config, ProviderConfig, RelayKeyConfig, RouteConfig,
    SecurityConfig, ServerConfig, TargetConfig,
};
use modelwire_db::Database;
use modelwire_server::{server::build_router, ServerState};
use serde_json::json;
use std::sync::Arc;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

// ============================================================================
// Test 1: codex_simple_text_nonstream
// ============================================================================

mod codex_simple_text_nonstream_tests {
    use super::*;

    /// Codex-style non-streaming text relay.
    ///
    /// Downstream request: POST /v1/responses with text input
    /// Upstream receives: Responses-shaped JSON request
    /// Downstream response: Responses-shaped JSON with message output
    #[tokio::test]
    async fn codex_simple_text_nonstream() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        // Configure mock to capture the upstream request shape
        let upstream_request_body = Arc::new(std::sync::Mutex::new(None));
        let upstream_request_body_clone = Arc::clone(&upstream_request_body);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                // Capture the upstream request body
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *upstream_request_body_clone.lock().unwrap() = Some(body);
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_upstream_001",
                    "model": "gpt-upstream",
                    "created_at": 1234567890,
                    "output": [{
                        "type": "message",
                        "id": "msg_upstream_001",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "Hello! How can I help you today?"
                        }]
                    }],
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 15,
                        "total_tokens": 25
                    }
                }))
            })
            .expect(1)
            .mount(&upstream)
            .await;

        // Build ModelWire test state
        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send Codex-style request to ModelWire
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "Hello, how are you?"
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
        assert!(
            response_json.get("created_at").is_some(),
            "Response must have 'created_at' field"
        );
        assert_eq!(
            response_json.get("model").and_then(|v| v.as_str()),
            Some("codex-main"),
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

        let msg_item = output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
            .expect("Output should contain a message item");
        assert_eq!(
            msg_item.get("role").and_then(|v| v.as_str()),
            Some("assistant"),
            "Message role should be 'assistant'"
        );

        let content = msg_item
            .get("content")
            .and_then(|v| v.as_array())
            .expect("Message should have content array");
        assert!(!content.is_empty(), "Content array should not be empty");

        let text_block = content
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("output_text"))
            .expect("Content should have output_text block");
        assert!(
            text_block.get("text").and_then(|v| v.as_str()).is_some(),
            "output_text should have text field"
        );

        // Assert usage is present
        let usage = response_json
            .get("usage")
            .expect("Response should have usage");
        assert_eq!(
            usage.get("input_tokens").and_then(|v| v.as_i64()),
            Some(10),
            "Usage should report input_tokens"
        );
        assert_eq!(
            usage.get("output_tokens").and_then(|v| v.as_i64()),
            Some(15),
            "Usage should report output_tokens"
        );

        // Assert upstream received correctly transformed request
        let captured_body = upstream_request_body.lock().unwrap();
        assert!(
            captured_body.is_some(),
            "Upstream request body should be captured"
        );
        let upstream_req = captured_body.as_ref().unwrap();
        assert_eq!(
            upstream_req.get("model").and_then(|v| v.as_str()),
            Some("gpt-upstream"),
            "Upstream request should use upstream model"
        );
        assert!(
            upstream_req.get("input").is_some(),
            "Upstream request should have input field"
        );
    }

    /// Verifies that ModelWire response ID is different from upstream ID
    #[tokio::test]
    async fn modelwire_response_id_is_owned() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_upstream_secret",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_upstream",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Response text"}]
                }]
            })))
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
            .body(Body::from(r#"{"model": "codex-main", "input": "test"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // ModelWire should own the response ID
        let downstream_id = response_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("Response should have id");
        assert!(
            !downstream_id.starts_with("resp_upstream"),
            "Downstream response ID should be ModelWire-owned, not upstream ID"
        );
    }
}

// ============================================================================
// Test 2: codex_simple_text_stream
// ============================================================================

mod codex_simple_text_stream_tests {
    use super::*;

    /// Codex-style streaming text relay.
    ///
    /// Downstream request: POST /v1/responses with stream=true
    /// Upstream returns: SSE events
    /// Downstream response: Responses SSE with response.created, text deltas, response.completed
    #[tokio::test]
    async fn codex_simple_text_stream() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_upstream_stream\",\"model\":\"gpt-upstream\",\"created_at\":1234567890}}\n\n\
                 event: response.output_item.added\n\
                 data: {\"response_id\":\"resp_upstream_stream\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[]}}\n\n\
                 event: response.output_text.delta\n\
                 data: {\"item_id\":\"msg_1\",\"delta\":{\"text\":\"Hello\"}}\n\n\
                 event: response.output_text.delta\n\
                 data: {\"item_id\":\"msg_1\",\"delta\":{\"text\":\" there!\"}}\n\n\
                 event: response.output_item.done\n\
                 data: {\"response_id\":\"resp_upstream_stream\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello there!\"}]}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_upstream_stream\",\"output\":[]}}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        // Build ModelWire test state
        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Send streaming request to ModelWire
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
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

        // Assert presence of required SSE events
        assert!(
            body_str.contains("event: response.created"),
            "SSE should contain response.created event"
        );
        assert!(
            body_str.contains("event: response.output_text.delta"),
            "SSE should contain text delta events"
        );
        assert!(
            body_str.contains("event: response.completed"),
            "SSE should contain response.completed event"
        );

        // Assert text delta content is present
        assert!(
            body_str.contains("Hello") || body_str.contains("Hello there!"),
            "SSE should contain text content"
        );
    }

    /// Verifies streaming completes successfully with proper SSE structure
    #[tokio::test]
    async fn streaming_response_completes_with_sse() {
        let upstream = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_stream_state\",\"model\":\"gpt-upstream\",\"created_at\":1234567890}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_stream_state\",\"output\":[]}}\n\n",
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
                r#"{"model": "codex-main", "input": "test", "stream": true}"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // Verify SSE structure
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        assert!(
            body_str.contains("event: response.created"),
            "SSE should contain response.created"
        );
        assert!(
            body_str.contains("event: response.completed"),
            "SSE should contain response.completed"
        );
    }
}

// ============================================================================
// Test 3: codex_tool_loop_shell_like
// ============================================================================

mod codex_tool_loop_shell_like_tests {
    use super::*;

    /// Tool call + function_call_output roundtrip across two turns.
    ///
    /// Turn 1: Request with tools -> Response with function_call
    /// Turn 2: Request with function_call_output -> Response with text
    #[tokio::test]
    async fn codex_tool_loop_shell_like() {
        // Start mock upstream server
        let upstream = MockServer::start().await;

        // Track which turn we're on
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let turn = call_count_clone.load(std::sync::atomic::Ordering::SeqCst);
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();

                // Turn 1: Initial request with tools -> function call response
                if turn == 1 {
                    // Verify tools were sent upstream
                    assert!(body.get("tools").is_some(), "Turn 1 should include tools");
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "resp_tool_turn1",
                        "model": "gpt-upstream",
                        "output": [{
                            "type": "function_call",
                            "id": "fc_001",
                            "call_id": "call_abc123",
                            "name": "get_weather",
                            "arguments": "{\"location\":\"Boston\"}"
                        }],
                        "usage": {"input_tokens": 20, "output_tokens": 30, "total_tokens": 50}
                    }))
                } else {
                    // Turn 2: function_call_output response -> text response
                    // Verify function_call_output was sent upstream
                    let input = body.get("input").and_then(|v| v.as_array());
                    assert!(input.is_some(), "Turn 2 should include input");
                    let has_function_output = input.unwrap().iter().any(|item| {
                        item.get("type").and_then(|v| v.as_str()) == Some("function_call_output")
                    });
                    assert!(
                        has_function_output,
                        "Turn 2 should include function_call_output"
                    );

                    ResponseTemplate::new(200).set_body_json(json!({
                        "id": "resp_tool_turn2",
                        "model": "gpt-upstream",
                        "output": [{
                            "type": "message",
                            "id": "msg_turn2",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": "The weather in Boston is sunny and 72 degrees."
                            }]
                        }],
                        "usage": {"input_tokens": 25, "output_tokens": 20, "total_tokens": 45}
                    }))
                }
            })
            .expect(2)
            .mount(&upstream)
            .await;

        // Build ModelWire test state
        let state = Arc::new(build_test_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // ===== TURN 1: Initial request with tools =====
        let turn1_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "input": "What's the weather in Boston?",
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

        let turn1_response = app.clone().oneshot(turn1_request).await.unwrap();

        // Assert Turn 1 response
        assert_eq!(
            turn1_response.status(),
            axum::http::StatusCode::OK,
            "Turn 1 should return 200 OK"
        );

        let turn1_body = axum::body::to_bytes(turn1_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let turn1_json: serde_json::Value = serde_json::from_slice(&turn1_body).unwrap();

        // Verify Turn 1 response contains function call
        let turn1_output = turn1_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("Turn 1 response should have output");
        let fc_item = turn1_output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .expect("Turn 1 output should contain function_call");
        assert_eq!(
            fc_item.get("name").and_then(|v| v.as_str()),
            Some("get_weather"),
            "Function call should request get_weather"
        );
        let call_id = fc_item
            .get("call_id")
            .and_then(|v| v.as_str())
            .expect("Function call should have call_id");

        // Store the ModelWire response_id for Turn 2
        let turn1_response_id = turn1_json
            .get("id")
            .and_then(|v| v.as_str())
            .expect("Turn 1 response should have id");

        // ===== TURN 2: Function call output as input =====
        let turn2_request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "codex-main",
                    "previous_response_id": turn1_response_id,
                    "input": [{
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": "{\"location\":\"Boston\",\"weather\":\"sunny\",\"temperature\":72}"
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let turn2_response = app.oneshot(turn2_request).await.unwrap();

        // Assert Turn 2 response
        assert_eq!(
            turn2_response.status(),
            axum::http::StatusCode::OK,
            "Turn 2 should return 200 OK"
        );

        let turn2_body = axum::body::to_bytes(turn2_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let turn2_json: serde_json::Value = serde_json::from_slice(&turn2_body).unwrap();

        // Verify Turn 2 response contains text message (not another function call)
        let turn2_output = turn2_json
            .get("output")
            .and_then(|v| v.as_array())
            .expect("Turn 2 response should have output");
        let msg_item = turn2_output
            .iter()
            .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
            .expect("Turn 2 output should contain message");
        let content = msg_item
            .get("content")
            .and_then(|v| v.as_array())
            .expect("Message should have content");
        let text_block = content
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("output_text"))
            .expect("Content should have output_text");
        let text = text_block
            .get("text")
            .and_then(|v| v.as_str())
            .expect("output_text should have text");
        assert!(
            text.contains("Boston") || text.contains("weather") || text.contains("72"),
            "Turn 2 text should reference weather data: {}",
            text
        );

        // Verify exactly 2 upstream calls were made
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "Exactly 2 upstream calls should be made"
        );
    }

    /// Verifies tool schema is preserved correctly through the relay
    #[tokio::test]
    async fn tool_schema_preserved_in_upstream_request() {
        let upstream = MockServer::start().await;
        let captured_tools = Arc::new(std::sync::Mutex::new(None));
        let captured_tools_clone = Arc::clone(&captured_tools);

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                if let Some(tools) = body.get("tools").cloned() {
                    *captured_tools_clone.lock().unwrap() = Some(tools);
                }
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_tools",
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "function_call",
                        "id": "fc_test",
                        "call_id": "call_test",
                        "name": "test_tool",
                        "arguments": "{}"
                    }]
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
                    "input": "Use the test tool",
                    "tools": [{
                        "type": "function",
                        "name": "test_tool",
                        "description": "A test tool",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "arg1": {"type": "string"}
                            }
                        }
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // Verify tools were captured correctly
        let tools = captured_tools.lock().unwrap();
        assert!(tools.is_some(), "Tools should be captured");
        let tools_arr = tools.as_ref().unwrap().as_array().unwrap();
        assert_eq!(tools_arr.len(), 1, "Should have exactly 1 tool");
        assert_eq!(
            tools_arr[0].get("name").and_then(|v| v.as_str()),
            Some("test_tool"),
            "Tool name should be preserved"
        );
    }
}

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Build a test ServerState for slice tests.
async fn build_test_state(upstream_base_url: &str) -> ServerState {
    let relay_secret = "test-relay-secret";
    let config = Config {
        server: ServerConfig {
            upstream_timeout_secs: 5,
            ..ServerConfig::default()
        },
        security: SecurityConfig {
            downstream_auth: "relay_key".to_string(),
            log_secret: Some(relay_secret.to_string()),
            managed_key_encryption_secret: Some("test-managed-key-secret".to_string()),
            relay_keys: vec![RelayKeyConfig {
                key_hash: hash_key_for_logging("mw_test_key", relay_secret),
                enabled: true,
                ..RelayKeyConfig::default()
            }],
            ..SecurityConfig::default()
        },
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
