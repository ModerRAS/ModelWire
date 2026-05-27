//! Server application builder and router.

use crate::middleware::{admin_auth, admin_origin, auth, logging, request_id};
use crate::routes::{health, models, response_state, responses};
use crate::{admin, ServerState};
use axum::{
    body::Body,
    extract::Path,
    http::{Response, StatusCode},
    middleware as axum_middleware,
    response::IntoResponse,
    routing::{any, get, post},
    Json, Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use modelwire_core::error::{ErrorDetail, ErrorResponse};

fn mime_guess(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Handler for serving WebUI static files with admin prefix
async fn serve_webui_with_prefix(Path(path): Path<String>) -> Response<Body> {
    // Determine the WebUI dist path - relative to working directory
    let base_path = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("modelwire-webui")
        .join("dist");

    let index_path = base_path.join("index.html");
    let requested_path = base_path.join(&path);

    // Try to serve the requested file if it exists
    if requested_path.is_file() {
        let mime = mime_guess(&requested_path);
        if let Ok(body) = tokio::fs::read(&requested_path).await {
            return Response::builder()
                .header("Content-Type", mime)
                .body(Body::from(body))
                .unwrap_or_else(|_| not_found());
        }
    }
    if index_path.is_file() {
        // Fallback to index.html for SPA routing
        if let Ok(body) = tokio::fs::read(&index_path).await {
            return Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .body(Body::from(body))
                .unwrap_or_else(|_| not_found());
        }
    }
    not_found()
}

fn not_found() -> Response<Body> {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: ErrorDetail {
                message: "Route not found".to_string(),
                error_type: Some("not_found".to_string()),
                param: None,
                code: Some("not_found".to_string()),
            },
        }),
    )
        .into_response()
}

async fn normalized_api_not_found() -> impl axum::response::IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: ErrorDetail {
                message: "Route not found".to_string(),
                error_type: Some("not_found".to_string()),
                param: None,
                code: Some("not_found".to_string()),
            },
        }),
    )
}

async fn normalized_api_method_not_allowed() -> impl axum::response::IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(ErrorResponse {
            error: ErrorDetail {
                message: "Method not allowed for this route".to_string(),
                error_type: Some("method_not_allowed".to_string()),
                param: None,
                code: Some("method_not_allowed".to_string()),
            },
        }),
    )
}

/// Redirect root to admin login
async fn redirect_to_admin() -> Response<Body> {
    Response::builder()
        .status(StatusCode::PERMANENT_REDIRECT)
        .header("Location", "/admin/login")
        .body(Body::empty())
        .unwrap()
}

/// Build the Axum router.
pub fn build_router(state: Arc<ServerState>) -> Router {
    let admin_api_routes = admin::routes()
        .route_layer(axum_middleware::from_fn_with_state(
            Arc::clone(&state),
            admin_auth::admin_auth,
        ))
        .route_layer(axum_middleware::from_fn_with_state(
            Arc::clone(&state),
            admin_origin::admin_origin,
        ));

    // API routes (requires downstream auth)
    let api_routes = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/v1", post(responses::create_response))
        .route("/v1/responses", post(responses::create_response))
        .route(
            "/v1/responses/:response_id",
            get(response_state::get_response_by_id),
        )
        .route(
            "/v1/responses/:response_id/input_items",
            get(response_state::get_response_input_items),
        )
        .route("/v1/models", get(models::list_models))
        .route("/v1/responses/compact", post(responses::compact_response))
        .route("/v1/*path", any(normalized_api_not_found))
        // Admin API routes
        .nest("/admin/api", admin_api_routes);

    // WebUI routes - serve static files from modelwire-webui/dist/
    let webui_routes = Router::new()
        .route("/", get(redirect_to_admin))
        .route("/admin", get(redirect_to_admin))
        .route(
            "/login",
            get(|| async { axum::response::Redirect::to("/admin/login") }),
        )
        .route(
            "/dashboard",
            get(|| async { axum::response::Redirect::to("/admin/dashboard") }),
        )
        .route(
            "/providers",
            get(|| async { axum::response::Redirect::to("/admin/providers") }),
        )
        .route(
            "/routes",
            get(|| async { axum::response::Redirect::to("/admin/routes") }),
        )
        .route(
            "/probes",
            get(|| async { axum::response::Redirect::to("/admin/probes") }),
        )
        .route(
            "/logs",
            get(|| async { axum::response::Redirect::to("/admin/logs") }),
        )
        .route(
            "/settings",
            get(|| async { axum::response::Redirect::to("/admin/settings") }),
        )
        .route("/admin/*path", get(serve_webui_with_prefix));

    // Merge into single router with middleware
    let app = Router::new()
        .merge(api_routes)
        .merge(webui_routes)
        .fallback(normalized_api_not_found)
        .method_not_allowed_fallback(normalized_api_method_not_allowed)
        // Add body limit middleware
        .layer(RequestBodyLimitLayer::new(state.config.server.max_body_size))
        // Add request ID middleware
        .layer(axum_middleware::from_fn_with_state(
            Arc::clone(&state),
            request_id::request_id,
        ))
        // Add logging middleware
        .layer(axum_middleware::from_fn_with_state(
            Arc::clone(&state),
            logging::logging,
        ))
        // Add downstream auth middleware (for /v1/ routes)
        .layer(axum_middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::auth,
        ))
        // Add trace middleware
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::extract::Request| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .map(|v| v.to_str().unwrap_or("unknown"))
                        .unwrap_or("unknown");
                    tracing::info_span!(
                        "request",
                        request_id = %request_id,
                        method = %request.method(),
                        uri = %request.uri(),
                    )
                })
                .on_response(|response: &axum::response::Response, latency: std::time::Duration, span: &tracing::Span| {
                    let status = response.status().as_u16();
                    span.record("http.status_code", status as i64);
                    if status >= 400 {
                        span.record("error", true);
                    }
                    tracing::info!(status = %status, latency_ms = ?latency, "request completed");
                }),
        )
        .with_state(state);

    app
}

/// Start the server.
pub async fn serve(state: Arc<ServerState>) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = state
        .config
        .server
        .bind
        .parse()
        .map_err(|e| format!("Invalid bind address: {}", e))?;

    validate_startup_security(&state.config.server.bind, &state.config.security)?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("ModelWire listening on {}", addr);

    let app = build_router(state);

    axum::serve(listener, app).await?;

    Ok(())
}

fn validate_startup_security(
    bind: &str,
    security: &modelwire_core::SecurityConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind_is_public = is_public_bind_address(bind);
    let auth_mode = security.downstream_auth.trim();
    let relay_key_entries_enabled = security.relay_keys.iter().any(|entry| entry.enabled);

    if security.public_deployment || bind_is_public {
        if auth_mode.is_empty() || auth_mode == "none" {
            return Err(
                "Public bind/deployment requires downstream auth; auth mode 'none' is not allowed."
                    .into(),
            );
        }
        match auth_mode {
            "relay_key" if !relay_key_entries_enabled => {
                return Err(
                    "Public bind/deployment with relay_key auth requires at least one enabled security.relay_keys entry."
                        .into(),
                );
            }
            "managed" => {
                return Err(
                    "Public bind/deployment with downstream_auth='managed' is not supported until managed downstream auth validation is implemented."
                        .into(),
                );
            }
            "passthrough" | "trusted_passthrough" if !security.allow_passthrough_keys => {
                return Err(
                    "Public bind/deployment rejects passthrough auth unless allow_passthrough_keys=true."
                        .into(),
                );
            }
            _ => {}
        }
    }

    if bind_is_public
        && (auth_mode == "passthrough" || auth_mode == "trusted_passthrough")
        && !security.allow_passthrough_keys
    {
        return Err(
            "Public bind rejects passthrough auth unless allow_passthrough_keys=true.".into(),
        );
    }

    Ok(())
}

fn is_public_bind_address(bind: &str) -> bool {
    let addr = bind.trim();
    if addr.is_empty() {
        return false;
    }

    if addr.starts_with('[') {
        if let Some(close_index) = addr.find(']') {
            return &addr[1..close_index] == "::";
        }
    }

    if let Some((host, _port)) = addr.rsplit_once(':') {
        return host == "0.0.0.0" || host == "::";
    }

    addr == "0.0.0.0" || addr == "::"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerState;
    use axum::body::Body;
    use axum::http::Request;
    use modelwire_core::error::ErrorResponse;
    use modelwire_core::SecurityConfig;
    use modelwire_core::{
        ArchiveConfig, Config, ProviderConfig, RouteConfig, ServerConfig, TargetConfig,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};
    use tower::util::ServiceExt;

    static CWD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    struct CurrentDirGuard {
        original: PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(path: &std::path::Path) -> Self {
            let original = std::env::current_dir().expect("current dir should be available");
            std::env::set_current_dir(path).expect("set current dir should succeed");
            Self { original }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn cwd_lock() -> &'static tokio::sync::Mutex<()> {
        CWD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn build_min_state() -> Arc<ServerState> {
        let relay_secret = "test-relay-secret";
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig {
                downstream_auth: "relay_key".to_string(),
                log_secret: Some(relay_secret.to_string()),
                relay_keys: vec![modelwire_core::RelayKeyConfig {
                    key_hash: modelwire_core::hash_key_for_logging("mw_test_key", relay_secret),
                    enabled: true,
                    ..modelwire_core::RelayKeyConfig::default()
                }],
                ..SecurityConfig::default()
            },
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "p1".to_string(),
                name: "Provider 1".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-1".to_string()),
                api_key: Some("k1".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true,
                config_json: None,
            }],
            routes: vec![RouteConfig {
                id: Some("r1".to_string()),
                downstream_model: "m1".to_string(),
                description: None,
                enabled: true,
                targets: vec![TargetConfig {
                    provider: "p1".to_string(),
                    upstream_model: "um1".to_string(),
                    wire_api: "responses".to_string(),
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
        let db = modelwire_db::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        db.run_migrations()
            .await
            .expect("migrations should succeed");
        Arc::new(ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    #[test]
    fn startup_validation_rejects_public_bind_without_auth() {
        let security = SecurityConfig {
            public_deployment: true,
            downstream_auth: "none".to_string(),
            ..SecurityConfig::default()
        };
        let result = validate_startup_security("0.0.0.0:8787", &security);
        assert!(result.is_err());
    }

    #[test]
    fn startup_validation_rejects_public_passthrough_when_disabled() {
        let security = SecurityConfig {
            public_deployment: true,
            downstream_auth: "passthrough".to_string(),
            allow_passthrough_keys: false,
            ..SecurityConfig::default()
        };
        let result = validate_startup_security("0.0.0.0:8787", &security);
        assert!(result.is_err());
    }

    #[test]
    fn startup_validation_allows_public_relay_key() {
        let security = SecurityConfig {
            public_deployment: true,
            downstream_auth: "relay_key".to_string(),
            allow_passthrough_keys: false,
            relay_keys: vec![modelwire_core::RelayKeyConfig {
                key_hash: "hash1".to_string(),
                enabled: true,
                ..modelwire_core::RelayKeyConfig::default()
            }],
            ..SecurityConfig::default()
        };
        let result = validate_startup_security("0.0.0.0:8787", &security);
        assert!(result.is_ok());
    }

    #[test]
    fn startup_validation_rejects_public_relay_key_without_enabled_keys() {
        let security = SecurityConfig {
            public_deployment: true,
            downstream_auth: "relay_key".to_string(),
            allow_passthrough_keys: false,
            relay_keys: vec![],
            ..SecurityConfig::default()
        };
        let result = validate_startup_security("0.0.0.0:8787", &security);
        assert!(result.is_err());
    }

    #[test]
    fn startup_validation_rejects_public_managed_until_implemented() {
        let security = SecurityConfig {
            public_deployment: true,
            downstream_auth: "managed".to_string(),
            ..SecurityConfig::default()
        };
        let result = validate_startup_security("0.0.0.0:8787", &security);
        assert!(result.is_err());
    }

    #[test]
    fn startup_validation_allows_local_bind_without_public_flag() {
        let security = SecurityConfig {
            public_deployment: false,
            downstream_auth: "none".to_string(),
            ..SecurityConfig::default()
        };
        let result = validate_startup_security("127.0.0.1:8787", &security);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn webui_root_redirects_to_admin_login() {
        let state = build_min_state().await;
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::PERMANENT_REDIRECT
        );
        assert_eq!(
            response
                .headers()
                .get("Location")
                .and_then(|v| v.to_str().ok()),
            Some("/admin/login")
        );
    }

    #[tokio::test]
    async fn webui_dist_index_served_by_backend() {
        let _cwd_lock = cwd_lock().lock().await;
        let temp = tempfile::tempdir().expect("temp dir should create");
        let dist_path = temp.path().join("modelwire-webui").join("dist");
        std::fs::create_dir_all(&dist_path).expect("webui dist path should create");
        std::fs::write(
            dist_path.join("index.html"),
            "<!doctype html><html><head><title>ModelWire Admin Test</title></head><body><div id=\"root\"></div></body></html>",
        )
        .expect("index.html fixture should write");
        let _cwd_guard = CurrentDirGuard::change_to(temp.path());

        let state = build_min_state().await;
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/admin/dashboard")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(
            html.contains("ModelWire Admin Test"),
            "backend should serve webui dist index html for admin route"
        );
    }

    #[tokio::test]
    async fn unknown_api_path_returns_normalized_json_404() {
        let state = build_min_state().await;
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/definitely-not-exist")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("not_found"));
    }

    #[tokio::test]
    async fn unsupported_method_returns_normalized_json_405() {
        let state = build_min_state().await;
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/v1/responses")
            .header("authorization", "Bearer mw_test_key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.error.code.as_deref(), Some("method_not_allowed"));
    }

    #[tokio::test]
    async fn request_id_header_is_reflected_in_response() {
        let state = build_min_state().await;
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/healthz")
            .header("authorization", "Bearer mw_test_key")
            .header("x-request-id", "req_mw_acceptance_001")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok()),
            Some("req_mw_acceptance_001")
        );
    }
}
