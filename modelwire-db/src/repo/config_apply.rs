//! Admin configuration apply repository.
//!
//! Applies validated provider/route/target config to operational DB tables in
//! a single transaction.

use crate::DbPool;

/// Summary of rows applied during config import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedConfigCounts {
    pub providers: u64,
    pub routes: u64,
    pub targets: u64,
}

/// Replace provider/route/target operational config tables in one transaction.
pub async fn replace_admin_config(
    db: &DbPool,
    config: &modelwire_core::Config,
) -> Result<AppliedConfigCounts, sqlx::Error> {
    replace_admin_config_with_options(
        db,
        config,
        ApplyConfigOptions {
            include_managed_api_keys: true,
        },
    )
    .await
}

/// Options controlling config import persistence behavior.
#[derive(Debug, Clone, Copy)]
pub struct ApplyConfigOptions {
    /// Whether to persist managed provider API key material in provider config_json.
    /// Set to `false` for admin/runtime import paths that must avoid at-rest plaintext.
    pub include_managed_api_keys: bool,
}

/// Replace provider/route/target tables with configurable secret persistence behavior.
pub async fn replace_admin_config_with_options(
    db: &DbPool,
    config: &modelwire_core::Config,
    options: ApplyConfigOptions,
) -> Result<AppliedConfigCounts, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let mut tx = pool.begin().await?;
            sqlx::query("DELETE FROM route_targets")
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM routes").execute(&mut *tx).await?;
            sqlx::query("DELETE FROM providers")
                .execute(&mut *tx)
                .await?;

            let now = chrono::Utc::now().to_rfc3339();
            let mut providers_count = 0_u64;
            let mut routes_count = 0_u64;
            let mut targets_count = 0_u64;

            for provider in &config.providers {
                let provider_cfg = serde_json::json!({
                    "allow_private_ips": provider.allow_private_ips,
                    "skip_ssrf_validation": provider.skip_ssrf_validation,
                    "api_key_set": provider.api_key.is_some(),
                    "managed_api_key": if options.include_managed_api_keys {
                        provider.api_key.clone()
                    } else {
                        None
                    },
                    "config_json": provider.config_json,
                })
                .to_string();

                sqlx::query(
                    r#"
                    INSERT INTO providers
                    (id, name, base_url, auth_mode, default_wire_api, state_scope, config_json, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(&provider.id)
                .bind(&provider.name)
                .bind(&provider.base_url)
                .bind(&provider.auth_mode)
                .bind(&provider.default_wire_api)
                .bind(provider.state_scope.as_deref())
                .bind(provider_cfg)
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                providers_count += 1;
            }

            for route in &config.routes {
                let route_id = route
                    .id
                    .clone()
                    .unwrap_or_else(|| route.downstream_model.clone());
                sqlx::query(
                    r#"
                    INSERT INTO routes
                    (id, downstream_model, description, enabled, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(&route_id)
                .bind(&route.downstream_model)
                .bind(route.description.as_deref())
                .bind(if route.enabled { 1_i32 } else { 0_i32 })
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                routes_count += 1;

                for target in &route.targets {
                    let target_id = format!("{route_id}:{}:{}", target.provider, target.priority);
                    let target_cfg = serde_json::json!({
                        "context_window_tokens": target.context_window_tokens,
                        "max_output_tokens": target.max_output_tokens,
                        "auto_compact_recommended_tokens": target.auto_compact_recommended_tokens,
                        "context_safety_margin_tokens": target.context_safety_margin_tokens,
                        "token_estimator": target.token_estimator,
                        "context_overflow_policy": target.context_overflow_policy,
                        "config_json": target.config_json,
                    })
                    .to_string();

                    sqlx::query(
                        r#"
                        INSERT INTO route_targets
                        (id, route_id, provider_id, upstream_model, wire_api, priority, enabled, config_json, created_at, updated_at)
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        "#,
                    )
                    .bind(&target_id)
                    .bind(&route_id)
                    .bind(&target.provider)
                    .bind(&target.upstream_model)
                    .bind(&target.wire_api)
                    .bind(target.priority)
                    .bind(if target.enabled { 1_i32 } else { 0_i32 })
                    .bind(target_cfg)
                    .bind(&now)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await?;
                    targets_count += 1;
                }
            }

            tx.commit().await?;
            Ok(AppliedConfigCounts {
                providers: providers_count,
                routes: routes_count,
                targets: targets_count,
            })
        }
        DbPool::Postgres(pool) => {
            let mut tx = pool.begin().await?;
            sqlx::query("DELETE FROM route_targets")
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM routes").execute(&mut *tx).await?;
            sqlx::query("DELETE FROM providers")
                .execute(&mut *tx)
                .await?;

            let now = chrono::Utc::now();
            let mut providers_count = 0_u64;
            let mut routes_count = 0_u64;
            let mut targets_count = 0_u64;

            for provider in &config.providers {
                let provider_cfg = serde_json::json!({
                    "allow_private_ips": provider.allow_private_ips,
                    "skip_ssrf_validation": provider.skip_ssrf_validation,
                    "api_key_set": provider.api_key.is_some(),
                    "managed_api_key": if options.include_managed_api_keys {
                        provider.api_key.clone()
                    } else {
                        None
                    },
                    "config_json": provider.config_json,
                })
                .to_string();

                sqlx::query(
                    r#"
                    INSERT INTO providers
                    (id, name, base_url, auth_mode, default_wire_api, state_scope, config_json, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    "#,
                )
                .bind(&provider.id)
                .bind(&provider.name)
                .bind(&provider.base_url)
                .bind(&provider.auth_mode)
                .bind(&provider.default_wire_api)
                .bind(provider.state_scope.as_deref())
                .bind(provider_cfg)
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                providers_count += 1;
            }

            for route in &config.routes {
                let route_id = route
                    .id
                    .clone()
                    .unwrap_or_else(|| route.downstream_model.clone());
                sqlx::query(
                    r#"
                    INSERT INTO routes
                    (id, downstream_model, description, enabled, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6)
                    "#,
                )
                .bind(&route_id)
                .bind(&route.downstream_model)
                .bind(route.description.as_deref())
                .bind(route.enabled)
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                routes_count += 1;

                for target in &route.targets {
                    let target_id = format!("{route_id}:{}:{}", target.provider, target.priority);
                    let target_cfg = serde_json::json!({
                        "context_window_tokens": target.context_window_tokens,
                        "max_output_tokens": target.max_output_tokens,
                        "auto_compact_recommended_tokens": target.auto_compact_recommended_tokens,
                        "context_safety_margin_tokens": target.context_safety_margin_tokens,
                        "token_estimator": target.token_estimator,
                        "context_overflow_policy": target.context_overflow_policy,
                        "config_json": target.config_json,
                    })
                    .to_string();

                    sqlx::query(
                        r#"
                        INSERT INTO route_targets
                        (id, route_id, provider_id, upstream_model, wire_api, priority, enabled, config_json, created_at, updated_at)
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                        "#,
                    )
                    .bind(&target_id)
                    .bind(&route_id)
                    .bind(&target.provider)
                    .bind(&target.upstream_model)
                    .bind(&target.wire_api)
                    .bind(target.priority)
                    .bind(target.enabled)
                    .bind(target_cfg)
                    .bind(now)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
                    targets_count += 1;
                }
            }

            tx.commit().await?;
            Ok(AppliedConfigCounts {
                providers: providers_count,
                routes: routes_count,
                targets: targets_count,
            })
        }
    }
}
