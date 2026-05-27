//! Model catalog endpoint.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::runtime_config::ensure_operational_config_seeded;
use crate::ServerState;
use modelwire_db::repo::routes::{get_targets as get_targets_row, list_routes as list_route_rows};

/// Model information.
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// Models list response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

/// GET /v1/models - List available models.
pub async fn list_models(state: State<Arc<ServerState>>) -> Json<ModelsResponse> {
    let _ = ensure_operational_config_seeded(state.as_ref()).await;
    let route_rows = list_route_rows(&state.db).await.unwrap_or_default();
    let mut models = Vec::new();
    for route in route_rows {
        if route.enabled == 0 {
            continue;
        }
        let targets = get_targets_row(&state.db, &route.id)
            .await
            .unwrap_or_default();
        let mut context_window: Option<u64> = None;
        let mut max_output_tokens: Option<u64> = None;
        for target in targets.into_iter().filter(|target| target.enabled != 0) {
            let target_cfg = serde_json::from_str::<serde_json::Value>(&target.config_json)
                .unwrap_or_else(|_| serde_json::json!({}));
            let target_context_window = target_cfg
                .get("context_window_tokens")
                .and_then(serde_json::Value::as_u64);
            let target_max_output = target_cfg
                .get("max_output_tokens")
                .and_then(serde_json::Value::as_u64);
            context_window = match (context_window, target_context_window) {
                (Some(current), Some(candidate)) => Some(current.min(candidate)),
                (None, candidate) => candidate,
                (current, None) => current,
            };
            max_output_tokens = match (max_output_tokens, target_max_output) {
                (Some(current), Some(candidate)) => Some(current.min(candidate)),
                (None, candidate) => candidate,
                (current, None) => current,
            };
        }
        models.push(ModelInfo {
            id: route.downstream_model,
            object: "model".to_string(),
            created: 1700000000, // Placeholder, could use route creation time
            owned_by: "modelwire".to_string(),
            context_window,
            max_output_tokens,
        });
    }

    Json(ModelsResponse {
        object: "list".to_string(),
        data: models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerState;
    use modelwire_core::{
        hash_key_for_logging, Config, ProviderConfig, RelayKeyConfig, RouteConfig, SecurityConfig,
        ServerConfig, TargetConfig,
    };
    use modelwire_db::Database;
    use std::sync::Arc;

    #[test]
    fn test_model_info_serialization() {
        let info = ModelInfo {
            id: "test-model".to_string(),
            object: "model".to_string(),
            created: 1700000000,
            owned_by: "modelwire".to_string(),
            context_window: Some(200000),
            max_output_tokens: Some(32768),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("test-model"));
        assert!(json.contains("200000"));
    }

    #[tokio::test]
    async fn context_metadata_reports_conservative_window() {
        let state = Arc::new(build_state_with_context_targets().await);
        let Json(response) = list_models(State(Arc::clone(&state))).await;
        let model = response
            .data
            .into_iter()
            .find(|m| m.id == "codex-main")
            .unwrap();
        assert_eq!(model.context_window, Some(120_000));
        assert_eq!(model.max_output_tokens, Some(8_192));
    }

    async fn build_state_with_context_targets() -> ServerState {
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig {
                downstream_auth: "relay_key".to_string(),
                log_secret: Some("test-relay-secret".to_string()),
                managed_key_encryption_secret: Some("test-managed-key-secret".to_string()),
                relay_keys: vec![RelayKeyConfig {
                    key_hash: hash_key_for_logging("mw_test_key", "test-relay-secret"),
                    enabled: true,
                    ..RelayKeyConfig::default()
                }],
                ..SecurityConfig::default()
            },
            archive: modelwire_core::ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: "https://example-a.test/v1".to_string(),
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
                    base_url: "https://example-b.test/v1".to_string(),
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
                id: Some("route-main".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![
                    TargetConfig {
                        provider: "provider-a".to_string(),
                        upstream_model: "gpt-upstream-a".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(200_000),
                        max_output_tokens: Some(16_384),
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "fallback".to_string(),
                        config_json: None,
                    },
                    TargetConfig {
                        provider: "provider-b".to_string(),
                        upstream_model: "gpt-upstream-b".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 20,
                        enabled: true,
                        context_window_tokens: Some(120_000),
                        max_output_tokens: Some(8_192),
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "fallback".to_string(),
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
}
