//! Runtime operational config bootstrap helpers.
//!
//! Transitional rule:
//! If admin config tables are empty but file config contains providers/routes,
//! seed the operational DB once from file config so data plane can run.

use std::sync::OnceLock;

use modelwire_core::{Error, ErrorKind};
use modelwire_db::{
    repo::config_apply::{replace_admin_config_with_options, ApplyConfigOptions},
    DbPool,
};

use crate::secrets::encrypt_managed_key;
use crate::ServerState;

fn seed_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn has_any_route(db: &modelwire_db::Database) -> Result<bool, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let count: (i64,) = sqlx::query_as("SELECT COUNT(1) FROM routes")
                .fetch_one(pool)
                .await?;
            Ok(count.0 > 0)
        }
        DbPool::Postgres(pool) => {
            let count: (i64,) = sqlx::query_as("SELECT COUNT(1) FROM routes")
                .fetch_one(pool)
                .await?;
            Ok(count.0 > 0)
        }
    }
}

/// Ensure operational provider/route/target tables are seeded from file config.
pub async fn ensure_operational_config_seeded(state: &ServerState) -> Result<(), Error> {
    if state.config.providers.is_empty() && state.config.routes.is_empty() {
        return Ok(());
    }

    if has_any_route(&state.db).await.map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to read runtime config state: {error}"),
        )
    })? {
        return Ok(());
    }

    let _guard = seed_lock().lock().await;

    if has_any_route(&state.db).await.map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to read runtime config state: {error}"),
        )
    })? {
        return Ok(());
    }

    let seed_config = config_with_encrypted_managed_keys(state)?;

    replace_admin_config_with_options(
        &state.db,
        &seed_config,
        ApplyConfigOptions {
            include_managed_api_keys: true,
        },
    )
    .await
    .map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to seed runtime config from file config: {error}"),
        )
    })?;

    Ok(())
}

fn config_with_encrypted_managed_keys(
    state: &ServerState,
) -> Result<modelwire_core::Config, Error> {
    let mut config = state.config.clone();
    let Some(secret) = state
        .config
        .security
        .managed_key_encryption_secret
        .as_deref()
    else {
        return Ok(config);
    };

    for provider in &mut config.providers {
        if provider.auth_mode != "managed" {
            continue;
        }
        let Some(api_key) = provider.api_key.as_deref() else {
            continue;
        };
        if api_key.starts_with("mwenc:v1:") {
            continue;
        }
        provider.api_key = Some(encrypt_managed_key(api_key, secret)?);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use modelwire_core::{
        ArchiveConfig, Config, ProviderConfig, RouteConfig, SecurityConfig, ServerConfig,
        TargetConfig,
    };
    use modelwire_db::repo::providers::get_provider as get_provider_row;

    #[tokio::test]
    async fn file_seed_encrypts_managed_upstream_key_at_rest_when_secret_configured() {
        let secret = "seed-managed-key-secret";
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig {
                managed_key_encryption_secret: Some(secret.to_string()),
                ..SecurityConfig::default()
            },
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "managed-provider".to_string(),
                name: "Managed Provider".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                auth_mode: "managed".to_string(),
                api_key: Some("sk-seed-secret".to_string()),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-a".to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: false,
                config_json: None,
            }],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![TargetConfig {
                    provider: "managed-provider".to_string(),
                    upstream_model: "gpt-upstream".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 10,
                    enabled: true,
                    context_window_tokens: Some(128_000),
                    max_output_tokens: Some(4096),
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
            .unwrap();
        db.run_migrations().await.unwrap();
        let state = ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        };

        ensure_operational_config_seeded(&state).await.unwrap();

        let row = get_provider_row(&state.db, "managed-provider")
            .await
            .unwrap()
            .expect("seeded provider should be persisted");
        assert!(
            !row.config_json.contains("sk-seed-secret"),
            "seeded provider config must not store plaintext managed keys"
        );
        let persisted: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
        let encrypted = persisted
            .get("managed_api_key")
            .and_then(serde_json::Value::as_str)
            .expect("managed key ciphertext should be stored for runtime use");
        assert!(encrypted.starts_with("mwenc:v1:"));
        let decrypted = crate::secrets::decrypt_managed_key(encrypted, secret).unwrap();
        assert_eq!(decrypted, "sk-seed-secret");
    }
}
