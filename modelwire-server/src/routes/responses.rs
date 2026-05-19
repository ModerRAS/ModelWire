//! POST /v1/responses endpoint.

use axum::{
    body::Body,
    extract::{Request, State},
    http::header::AUTHORIZATION,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use http_body_util::LengthLimitError;
use std::error::Error as StdError;
use std::sync::Arc;
use tracing::{error, info};

use crate::{
    error::error_response_to_response,
    middleware::auth::DownstreamAuthContext,
    relay::{
        relay_compact_response_scoped, relay_non_streaming_response_scoped,
        relay_streaming_response_scoped,
    },
    ServerState,
};
use modelwire_core::error::{Error, ErrorKind};

/// POST /v1/responses - Create a model response.
pub async fn create_response(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Response {
    let auth_context = request.extensions().get::<DownstreamAuthContext>().cloned();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let downstream_authorization = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    let body = match axum::body::to_bytes(request.into_body(), state.config.server.max_body_size)
        .await
    {
        Ok(body) => body,
        Err(error) => {
            error!(request_id = %request_id, error = %error, "Failed to read request body");
            return error_response_to_response(body_read_error_to_response(&error).to_response());
        }
    };

    let raw_json = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            error!(request_id = %request_id, error = %error, "Failed to parse request JSON");
            return error_response_to_response(
                Error::new(ErrorKind::RequestInvalid, "Invalid JSON in request body").to_response(),
            );
        }
    };

    if let Some(forbidden) = deny_if_route_not_allowed(&raw_json, auth_context.as_ref()) {
        return forbidden;
    }
    if let Some(forbidden) =
        deny_if_provider_scope_not_satisfiable(&state, &raw_json, auth_context.as_ref())
    {
        return forbidden;
    }

    info!(
        request_id = %request_id,
        model = raw_json.get("model").and_then(|value| value.as_str()).unwrap_or(""),
        stream = raw_json.get("stream").and_then(|value| value.as_bool()).unwrap_or(false),
        "Processing response request"
    );

    let stream = raw_json
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let allowed_providers = auth_context
        .as_ref()
        .and_then(|ctx| ctx.allowed_providers.clone());
    let archive_capture_mode_override = auth_context
        .as_ref()
        .and_then(|ctx| ctx.archive_capture_mode.clone());
    let downstream_key_hash = auth_context.as_ref().and_then(|ctx| ctx.key_hash.clone());

    if stream {
        match relay_streaming_response_scoped(
            Arc::clone(&state),
            request_id,
            raw_json,
            downstream_authorization,
            allowed_providers,
        )
        .await
        {
            Ok(result) => {
                let mut all = Vec::new();
                for frame in result.sse_frames {
                    all.extend_from_slice(&frame);
                }
                (
                    axum::http::StatusCode::OK,
                    [
                        ("content-type", "text/event-stream"),
                        ("cache-control", "no-cache"),
                        ("connection", "keep-alive"),
                    ],
                    all,
                )
                    .into_response()
            }
            Err(error) => error_response_to_response(error.to_response()),
        }
    } else {
        match relay_non_streaming_response_scoped(
            Arc::clone(&state),
            request_id,
            raw_json,
            downstream_authorization,
            downstream_key_hash,
            allowed_providers,
            archive_capture_mode_override,
        )
        .await
        {
            Ok(response) => Json(response).into_response(),
            Err(error) => error_response_to_response(error.to_response()),
        }
    }
}

/// Compact endpoint - capability dependent.
pub async fn compact_response(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Response {
    let auth_context = request.extensions().get::<DownstreamAuthContext>().cloned();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let downstream_authorization = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    let body = match axum::body::to_bytes(request.into_body(), state.config.server.max_body_size)
        .await
    {
        Ok(body) => body,
        Err(error) => {
            error!(request_id = %request_id, error = %error, "Failed to read compact request body");
            return error_response_to_response(body_read_error_to_response(&error).to_response());
        }
    };

    let raw_json = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            error!(request_id = %request_id, error = %error, "Failed to parse compact request JSON");
            return error_response_to_response(
                Error::new(ErrorKind::RequestInvalid, "Invalid JSON in request body").to_response(),
            );
        }
    };

    if let Some(forbidden) = deny_if_route_not_allowed(&raw_json, auth_context.as_ref()) {
        return forbidden;
    }
    if let Some(forbidden) =
        deny_if_provider_scope_not_satisfiable(&state, &raw_json, auth_context.as_ref())
    {
        return forbidden;
    }

    let allowed_providers = auth_context
        .as_ref()
        .and_then(|ctx| ctx.allowed_providers.clone());
    let archive_capture_mode_override = auth_context
        .as_ref()
        .and_then(|ctx| ctx.archive_capture_mode.clone());

    match relay_compact_response_scoped(
        Arc::clone(&state),
        request_id,
        raw_json,
        downstream_authorization,
        allowed_providers,
        archive_capture_mode_override,
    )
    .await
    {
        Ok(response) => Json(response).into_response(),
        Err(error) => error_response_to_response(error.to_response()),
    }
}

fn deny_if_route_not_allowed(
    raw_json: &serde_json::Value,
    auth_context: Option<&DownstreamAuthContext>,
) -> Option<Response> {
    let allowed_models = auth_context.and_then(|ctx| ctx.allowed_models.as_ref())?;
    let model = raw_json
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)?;

    if allowed_models.iter().any(|allowed| allowed == &model) {
        return None;
    }

    Some(
        (
            StatusCode::FORBIDDEN,
            Json(
                Error::new(
                    ErrorKind::AuthFailed,
                    format!("Relay key is not allowed to access model '{model}'"),
                )
                .to_response(),
            ),
        )
            .into_response(),
    )
}

fn body_read_error_to_response(error: &axum::Error) -> Error {
    if is_length_limit_error(error) {
        return Error::new(
            ErrorKind::RequestTooLarge,
            "Request body exceeds configured max body size",
        );
    }
    Error::new(ErrorKind::RequestInvalid, "Failed to read request body")
}

fn is_length_limit_error(error: &axum::Error) -> bool {
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(source) = current {
        if source.is::<LengthLimitError>() {
            return true;
        }
        current = source.source();
    }
    false
}

fn deny_if_provider_scope_not_satisfiable(
    state: &ServerState,
    raw_json: &serde_json::Value,
    auth_context: Option<&DownstreamAuthContext>,
) -> Option<Response> {
    let allowed_providers = auth_context.and_then(|ctx| ctx.allowed_providers.as_ref())?;
    let model = raw_json
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)?;
    let route = state.config.get_route(&model)?;
    let route_has_allowed_provider = route.targets.iter().any(|target| {
        allowed_providers
            .iter()
            .any(|provider| provider == &target.provider)
    });

    if route_has_allowed_provider {
        return None;
    }

    Some(
        (
            StatusCode::FORBIDDEN,
            Json(
                Error::new(
                    ErrorKind::AuthFailed,
                    format!("Relay key is not allowed to access any provider for model '{model}'"),
                )
                .to_response(),
            ),
        )
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use crate::{server::build_router, ServerState};
    use axum::{body::Body, http::Request};
    use modelwire_core::error::ErrorResponse;
    use modelwire_core::{
        ArchiveConfig, Config, ProviderConfig, RouteConfig, SecurityConfig, ServerConfig,
        TargetConfig,
    };
    use modelwire_db::repo::responses::{
        get_latest_upstream_handle, get_response, store_response_metadata, ResponseInsert,
    };
    use modelwire_db::Database;
    use std::sync::Arc;
    use tower::util::ServiceExt;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn create_response_rejects_oversized_body_with_413() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state = build_state(&upstream.uri()).await;
        state.config.server.max_body_size = 64;
        let app = build_router(Arc::new(state));

        let oversized_body = format!(
            "{{\"model\":\"codex-main\",\"input\":\"{}\"}}",
            "x".repeat(4096)
        );

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(oversized_body))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("request_too_large"));
    }

    #[tokio::test]
    async fn create_response_missing_model_returns_400_request_invalid() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "input": "hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("request_invalid"));
    }

    #[tokio::test]
    async fn create_response_unknown_model_returns_404_model_not_found() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "does-not-exist",
                    "input": "hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("model_not_found"));
    }

    #[tokio::test]
    async fn create_response_upstream_401_returns_normalized_auth_error() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "invalid api key"}
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "input": "hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("auth_failed"));
    }

    #[tokio::test]
    async fn create_response_rejects_unsupported_image_input_with_clear_400() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_image",
                            "image_url": "https://example.test/a.png"
                        }]
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("request_invalid"));
        assert!(
            payload
                .error
                .message
                .contains("Unsupported content block type 'input_image'"),
            "Expected clear unsupported-image error, got: {}",
            payload.error.message
        );
    }

    #[tokio::test]
    async fn create_response_rejects_unsupported_file_input_with_clear_400() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_file",
                            "file_id": "file_abc123"
                        }]
                    }]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("request_invalid"));
        assert!(
            payload
                .error
                .message
                .contains("Unsupported content block type 'input_file'"),
            "Expected clear unsupported-file error, got: {}",
            payload.error.message
        );
    }

    #[tokio::test]
    async fn create_response_keeps_upstream_id_private_but_persists_operational_metadata() {
        let upstream = MockServer::start().await;
        let upstream_private_id = "resp_upstream_private_001";
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": upstream_private_id,
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_up_1",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "hello"}]
                }],
                "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7}
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "input": "hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let downstream_id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .expect("response should include downstream id");

        assert!(
            !body
                .windows(upstream_private_id.len())
                .any(|w| w == upstream_private_id.as_bytes()),
            "Downstream payload must not leak upstream private response id"
        );
        assert_ne!(
            downstream_id, upstream_private_id,
            "Downstream response id must be ModelWire-owned, not upstream-owned"
        );

        let persisted = get_response(&state.db, downstream_id)
            .await
            .unwrap()
            .expect("response shell should be persisted");
        assert_eq!(persisted.downstream_model, "codex-main");
        assert_eq!(persisted.provider_id.as_deref(), Some("provider-a"));
        assert_eq!(persisted.upstream_model.as_deref(), Some("gpt-upstream"));
        assert_eq!(persisted.wire_api.as_deref(), Some("responses"));
        assert_eq!(
            persisted.upstream_response_id.as_deref(),
            Some(upstream_private_id)
        );

        let handle = get_latest_upstream_handle(&state.db, downstream_id)
            .await
            .unwrap()
            .expect("upstream handle should be persisted");
        assert_eq!(handle.provider_id, "provider-a");
        assert_eq!(handle.upstream_model, "gpt-upstream");
        assert_eq!(handle.wire_api, "responses");
        assert_eq!(
            handle.upstream_response_id.as_deref(),
            Some(upstream_private_id)
        );
    }

    #[tokio::test]
    async fn create_response_stream_returns_sse_payload() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_upstream\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_upstream\",\"output\":[]}}\n\n",
            ))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "input": "hello",
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
    }

    #[tokio::test]
    async fn compact_response_forwards_to_native_responses_target() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "cmp_1",
                "object": "response.compaction",
                "status": "completed"
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses/compact")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn compact_response_rejects_when_only_chat_target_exists() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_state_with_wire_api(&upstream.uri(), "openai_chat").await);
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses/compact")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn compact_response_rejects_cross_state_scope_source() {
        let upstream_a = MockServer::start().await;
        let upstream_b = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream_a)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream_b)
            .await;

        let state =
            Arc::new(build_state_two_targets_for_scope(&upstream_a.uri(), &upstream_b.uri()).await);
        store_response_metadata(
            &state.db,
            &ResponseInsert {
                id: "resp_mw_prev_scope_a",
                request_id: "req_prev_scope",
                downstream_model: "codex-main",
                route_id: Some("route-a"),
                target_id: Some("route-a:provider-a:10"),
                provider_id: Some("provider-a"),
                upstream_model: Some("gpt-upstream"),
                wire_api: Some("responses"),
                upstream_response_id: Some("resp_up_prev"),
                state_scope: Some("scope-a"),
                previous_response_id: None,
                status: "completed",
                usage_json: None,
                error_json: None,
            },
        )
        .await
        .unwrap();

        let app = build_router(Arc::clone(&state));
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses/compact")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "response_id": "resp_mw_prev_scope_a"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    async fn build_state(base_url: &str) -> ServerState {
        build_state_with_wire_api(base_url, "responses").await
    }

    async fn build_state_with_wire_api(base_url: &str, wire_api: &str) -> ServerState {
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "provider-a".to_string(),
                name: "Provider A".to_string(),
                base_url: base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-a".to_string()),
                api_key: Some("k1".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true, // Allow localhost URLs in tests
                config_json: None,
            }],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![TargetConfig {
                    provider: "provider-a".to_string(),
                    upstream_model: "gpt-upstream".to_string(),
                    wire_api: wire_api.to_string(),
                    priority: 10,
                    enabled: true,
                    context_window_tokens: None,
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: None,
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
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }

    async fn build_state_two_targets_for_scope(
        first_base_url: &str,
        second_base_url: &str,
    ) -> ServerState {
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: first_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true, // Allow localhost URLs in tests
                    config_json: None,
                },
                ProviderConfig {
                    id: "provider-b".to_string(),
                    name: "Provider B".to_string(),
                    base_url: second_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-b".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true, // Allow localhost URLs in tests
                    config_json: None,
                },
            ],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![TargetConfig {
                    provider: "provider-b".to_string(),
                    upstream_model: "gpt-upstream".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 10,
                    enabled: true,
                    context_window_tokens: None,
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: None,
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
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }

    /// Verifies that Codex-style base URL `/v1` routes to `/v1/responses`.
    /// This is the codex_config_base_url_v1 slice test.
    #[tokio::test]
    async fn codex_config_base_url_v1() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_mw_test_v1",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Hello from /v1"}]
                }],
                "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let state = Arc::new(build_test_state_for_v1_redirect(&upstream.uri()).await);
        let app = build_router(Arc::clone(&state));

        // Codex-style request to /v1 (not /v1/responses)
        // Both /v1 and /v1/responses should work for POST
        let request = Request::builder()
            .method("POST")
            .uri("/v1")
            .header("authorization", "Bearer mw_key")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "codex-main",
                    "input": "hello"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should return 200 OK (both /v1 and /v1/responses route to create_response)
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "POST /v1 should work like /v1/responses, got {}",
            response.status()
        );
    }

    async fn build_test_state_for_v1_redirect(upstream_url: &str) -> ServerState {
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "test-provider".to_string(),
                name: "Test Provider".to_string(),
                base_url: upstream_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: None,
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
                    context_window_tokens: None,
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: None,
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
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }
}
