//! Model catalog endpoint.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::ServerState;

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
    let models: Vec<ModelInfo> = state
        .config
        .routes
        .iter()
        .filter(|r| r.enabled)
        .map(|route| {
            let targets = state.config.get_sorted_targets(route);
            let context_window = targets.iter().filter_map(|t| t.context_window_tokens).min();
            let max_output_tokens = targets.iter().filter_map(|t| t.max_output_tokens).min();

            ModelInfo {
                id: route.downstream_model.clone(),
                object: "model".to_string(),
                created: 1700000000, // Placeholder, could use route creation time
                owned_by: "modelwire".to_string(),
                context_window,
                max_output_tokens,
            }
        })
        .collect();

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
        Config, ProviderConfig, RouteConfig, SecurityConfig, ServerConfig, TargetConfig,
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
            security: SecurityConfig::default(),
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
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }
}
