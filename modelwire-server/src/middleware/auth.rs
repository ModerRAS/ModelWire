//! Downstream authentication middleware.

use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::error::{error_response_to_response, ErrorKind};
use crate::request_limiter::{decrement_in_flight, enforce_ip_rate_limit, enforce_key_limits};
use crate::ServerState;
use modelwire_core::{error::Error, hash_key_for_logging};

/// Per-request downstream auth context propagated from middleware.
#[derive(Debug, Clone, Default)]
pub struct DownstreamAuthContext {
    /// Stable hash for the authenticated key.
    pub key_hash: Option<String>,
    /// Allowed downstream models for this key.
    /// `None` means unrestricted (legacy/global key behavior).
    pub allowed_models: Option<Vec<String>>,
    /// Allowed upstream provider IDs for this key.
    /// `None` means unrestricted (legacy/global key behavior).
    pub allowed_providers: Option<Vec<String>>,
    /// Optional archive capture mode override for this key.
    pub archive_capture_mode: Option<String>,
    /// Whether in-flight quota tracking was reserved for this request.
    pub limiter_slot_reserved: bool,
}

/// Middleware that authenticates downstream requests.
pub async fn auth(
    State(state): State<Arc<ServerState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let mut limiter_release: Option<String> = None;

    // Skip downstream auth for health endpoints and admin surfaces.
    // Admin API has dedicated auth/CSRF middleware.
    let path = request.uri().path();
    if path == "/healthz" || path == "/readyz" || path == "/admin" || path.starts_with("/admin/") {
        return next.run(request).await;
    }

    if let Err(error) = enforce_ip_rate_limit(
        state.as_ref(),
        &extract_client_identity(&request),
        state.config.security.ip_requests_per_minute,
    ) {
        return error_response_to_response(error.to_response());
    }

    // Check downstream auth mode
    match state.config.security.downstream_auth.as_str() {
        "relay_key" => {
            // Expect ModelWire relay key in Authorization header
            let auth = request.headers().get(AUTHORIZATION);

            if auth.is_none() {
                return error_response_to_response(
                    Error::new(ErrorKind::AuthFailed, "Missing authorization header").to_response(),
                );
            }

            let auth_str = auth.unwrap().to_str().unwrap_or("");

            // Validate relay key (stub - would check against stored hashes)
            if !auth_str.starts_with("Bearer mw_") {
                return error_response_to_response(
                    Error::new(ErrorKind::AuthFailed, "Invalid relay key format").to_response(),
                );
            }

            let relay_key = strip_bearer(auth_str).unwrap_or_default();
            let configured_keys = &state.config.security.relay_keys;
            if configured_keys.is_empty() {
                let secret = state
                    .config
                    .security
                    .log_secret
                    .as_deref()
                    .unwrap_or("modelwire-default-relay-secret");
                let key_hash = hash_key_for_logging(relay_key, secret);
                request.extensions_mut().insert(DownstreamAuthContext {
                    key_hash: Some(key_hash),
                    allowed_models: None,
                    allowed_providers: None,
                    archive_capture_mode: None,
                    limiter_slot_reserved: false,
                });
            } else {
                let secret = state
                    .config
                    .security
                    .log_secret
                    .as_deref()
                    .unwrap_or("modelwire-default-relay-secret");
                let key_hash = hash_key_for_logging(relay_key, secret);
                let Some(matched) = configured_keys
                    .iter()
                    .find(|entry| entry.enabled && entry.key_hash == key_hash)
                else {
                    return error_response_to_response(
                        Error::new(ErrorKind::AuthFailed, "Invalid relay key").to_response(),
                    );
                };

                let limiter_enforcement = match enforce_key_limits(
                    state.as_ref(),
                    &key_hash,
                    matched.requests_per_minute,
                    matched.max_concurrency,
                ) {
                    Ok(value) => value,
                    Err(error) => return error_response_to_response(error.to_response()),
                };

                let allowed_models = if matched.allowed_models.is_empty() {
                    None
                } else {
                    Some(matched.allowed_models.clone())
                };
                let allowed_providers = if matched.allowed_providers.is_empty() {
                    None
                } else {
                    Some(matched.allowed_providers.clone())
                };

                request.extensions_mut().insert(DownstreamAuthContext {
                    key_hash: Some(key_hash.clone()),
                    allowed_models,
                    allowed_providers,
                    archive_capture_mode: matched.archive_capture_mode.clone(),
                    limiter_slot_reserved: limiter_enforcement.in_flight_reserved,
                });
                if limiter_enforcement.in_flight_reserved {
                    limiter_release = Some(key_hash);
                }
            }
        }
        "passthrough" | "trusted_passthrough" => {
            if state.config.security.public_deployment
                && !state.config.security.allow_passthrough_keys
            {
                return error_response_to_response(
                    Error::new(
                        ErrorKind::AuthFailed,
                        "Passthrough auth is disabled for this public deployment",
                    )
                    .to_response(),
                );
            }
            // Pass authorization through
            if state.config.security.downstream_auth == "trusted_passthrough" {
                let required_header = state.config.security.trusted_passthrough_header.as_deref();
                let required_value = state.config.security.trusted_passthrough_value.as_deref();

                match (required_header, required_value) {
                    (Some(header_name), Some(expected_value)) => {
                        let Ok(parsed_name) = header_name.parse::<axum::http::header::HeaderName>()
                        else {
                            return error_response_to_response(
                                Error::new(
                                    ErrorKind::InternalError,
                                    "Invalid trusted_passthrough_header configuration",
                                )
                                .to_response(),
                            );
                        };
                        let provided = request
                            .headers()
                            .get(&parsed_name)
                            .and_then(|value| value.to_str().ok());
                        if provided != Some(expected_value) {
                            return error_response_to_response(
                                Error::new(
                                    ErrorKind::AuthFailed,
                                    "trusted_passthrough requires additional gateway control",
                                )
                                .to_response(),
                            );
                        }
                    }
                    _ => {
                        return error_response_to_response(
                            Error::new(
                                ErrorKind::AuthFailed,
                                "trusted_passthrough requires configured gateway header and value",
                            )
                            .to_response(),
                        );
                    }
                }
            }
        }
        "managed" => {
            // API key managed by ModelWire
            // Check that request is authorized
        }
        "none" => {
            // No auth (only for development!)
            if state.config.security.public_deployment {
                return error_response_to_response(
                    Error::new(
                        ErrorKind::AuthFailed,
                        "Authentication required for public deployment",
                    )
                    .to_response(),
                );
            }
        }
        _ => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!(
                        "Unknown downstream auth mode: {}",
                        state.config.security.downstream_auth
                    ),
                )
                .to_response(),
            );
        }
    }

    let response = next.run(request).await;

    // Release in-flight limiter slot after response completes.
    if let Some(key_hash) = limiter_release.as_deref() {
        decrement_in_flight(state.as_ref(), key_hash);
    }

    response
}

fn strip_bearer(value: &str) -> Option<&str> {
    value.strip_prefix("Bearer ")
}

fn extract_client_identity(request: &Request) -> String {
    if let Some(forwarded_for) = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        let first = forwarded_for.split(',').next().map(str::trim).unwrap_or("");
        if !first.is_empty() {
            return first.to_string();
        }
    }
    if let Some(real_ip) = request
        .headers()
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
    {
        if !real_ip.trim().is_empty() {
            return real_ip.trim().to_string();
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use crate::{server::build_router, ServerState};
    use axum::{body::Body, http::Request};
    use modelwire_core::{
        ArchiveConfig, Config, ProviderConfig, RelayKeyConfig, RouteConfig, SecurityConfig,
        ServerConfig, TargetConfig,
    };
    use modelwire_db::Database;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn relay_key_auth_context_includes_archive_capture_mode_override() {
        let upstream = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/responses"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "resp_upstream_auth_archive_mode",
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": "msg_auth_archive_mode",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok"}]
                    }],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                })),
            )
            .mount(&upstream)
            .await;

        let raw_key = "mw_test_archive_mode_key";
        let log_secret = "auth-log-secret";
        let key_hash = modelwire_core::hash_key_for_logging(raw_key, log_secret);

        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..ServerConfig::default()
            },
            security: SecurityConfig {
                downstream_auth: "relay_key".to_string(),
                log_secret: Some(log_secret.to_string()),
                relay_keys: vec![RelayKeyConfig {
                    key_hash,
                    enabled: true,
                    allowed_models: vec!["codex-main".to_string()],
                    allowed_providers: vec!["provider-a".to_string()],
                    requests_per_minute: None,
                    max_concurrency: None,
                    archive_capture_mode: Some("metadata_only".to_string()),
                }],
                ..SecurityConfig::default()
            },
            archive: ArchiveConfig {
                capture_mode: "off".to_string(),
                root: tempfile::tempdir()
                    .unwrap()
                    .path()
                    .to_string_lossy()
                    .to_string(),
                include_lineage: true,
            },
            providers: vec![ProviderConfig {
                id: "provider-a".to_string(),
                name: "Provider A".to_string(),
                base_url: upstream.uri(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-a".to_string()),
                api_key: Some("k1".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true,
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
                    wire_api: "responses".to_string(),
                    priority: 10,
                    enabled: true,
                    context_window_tokens: Some(100_000),
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
        let state = Arc::new(ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writer: tokio::sync::Mutex::new(None),
        });
        let app = build_router(Arc::clone(&state));

        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", format!("Bearer {raw_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model":"codex-main",
                    "input":"auth archive mode override"
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let archive_root = std::path::PathBuf::from(&state.config.archive.root);
        let archive_dir = std::fs::read_dir(&archive_root)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .find(|path| path.is_dir())
            .expect("archive directory should exist");
        let manifest_path = archive_dir.join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["capture_mode"], "metadata_only",
            "relay key archive_capture_mode should override global archive.capture_mode"
        );
    }
}
