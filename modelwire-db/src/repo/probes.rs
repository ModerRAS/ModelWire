//! Probes repository.

use crate::DbPool;
use modelwire_core::ProbeResult;

/// Store probe result.
pub async fn store_probe_result(
    db: &DbPool,
    provider_id: &str,
    credential_hash: &str,
    upstream_model: &str,
    wire_api: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    let now_ts = chrono::Utc::now().timestamp();
    let probe = ProbeResult {
        provider_id: provider_id.to_string(),
        credential_hash: credential_hash.to_string(),
        upstream_model: upstream_model.to_string(),
        wire_api: modelwire_core::WireApi::parse(wire_api).unwrap_or(modelwire_core::WireApi::Auto),
        supports_streaming: false,
        supports_tools: false,
        supports_parallel_tool_calls: false,
        tool_support_known: false,
        supports_previous_response_id: false,
        supports_reasoning_encrypted_content: false,
        supports_reasoning_summary: false,
        last_success_at: if status == "success" {
            Some(now_ts)
        } else {
            None
        },
        last_failure_at: if status == "success" {
            None
        } else {
            Some(now_ts)
        },
        failure_kind: None,
        failure_message_redacted: None,
        expires_at: now_ts + 3600,
    };
    store_probe_result_detailed(db, &probe, status).await
}

/// Store probe result with full capability metadata.
pub async fn store_probe_result_detailed(
    db: &DbPool,
    probe: &ProbeResult,
    status: &str,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now();
    let now_rfc3339 = now.to_rfc3339();
    let expires_at = chrono::DateTime::<chrono::Utc>::from_timestamp(probe.expires_at, 0)
        .unwrap_or_else(|| now + chrono::Duration::hours(1))
        .to_rfc3339();
    let last_success_at = probe
        .last_success_at
        .and_then(|value| chrono::DateTime::<chrono::Utc>::from_timestamp(value, 0))
        .map(|ts| ts.to_rfc3339());
    let last_failure_at = probe
        .last_failure_at
        .and_then(|value| chrono::DateTime::<chrono::Utc>::from_timestamp(value, 0))
        .map(|ts| ts.to_rfc3339());

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO probe_results (
                    id, provider_id, credential_hash, upstream_model, wire_api,
                    supports_streaming, supports_tools, supports_parallel_tool_calls,
                    supports_previous_response_id, supports_reasoning_encrypted_content,
                    supports_reasoning_summary, status, failure_kind,
                    failure_message_redacted, last_success_at, last_failure_at,
                    expires_at, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(provider_id, credential_hash, upstream_model) DO UPDATE SET
                    wire_api = excluded.wire_api,
                    supports_streaming = excluded.supports_streaming,
                    supports_tools = excluded.supports_tools,
                    supports_parallel_tool_calls = excluded.supports_parallel_tool_calls,
                    supports_previous_response_id = excluded.supports_previous_response_id,
                    supports_reasoning_encrypted_content = excluded.supports_reasoning_encrypted_content,
                    supports_reasoning_summary = excluded.supports_reasoning_summary,
                    status = excluded.status,
                    failure_kind = excluded.failure_kind,
                    failure_message_redacted = excluded.failure_message_redacted,
                    last_success_at = CASE
                        WHEN excluded.status = 'success' THEN excluded.last_success_at
                        ELSE last_success_at
                    END,
                    last_failure_at = CASE
                        WHEN excluded.status != 'success' THEN excluded.last_failure_at
                        ELSE last_failure_at
                    END,
                    expires_at = excluded.expires_at
                "#,
            )
            .bind(format!("probe_{}", uuid::Uuid::new_v4()))
            .bind(&probe.provider_id)
            .bind(&probe.credential_hash)
            .bind(&probe.upstream_model)
            .bind(probe.wire_api.as_str())
            .bind(if probe.supports_streaming { 1 } else { 0 })
            .bind(if probe.supports_tools { 1 } else { 0 })
            .bind(if probe.supports_parallel_tool_calls {
                1
            } else {
                0
            })
            .bind(if probe.supports_previous_response_id {
                1
            } else {
                0
            })
            .bind(if probe.supports_reasoning_encrypted_content {
                1
            } else {
                0
            })
            .bind(if probe.supports_reasoning_summary { 1 } else { 0 })
            .bind(status)
            .bind(probe.failure_kind.as_deref())
            .bind(probe.failure_message_redacted.as_deref())
            .bind(last_success_at.as_deref())
            .bind(last_failure_at.as_deref())
            .bind(&expires_at)
            .bind(&now_rfc3339)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO probe_results (
                    id, provider_id, credential_hash, upstream_model, wire_api,
                    supports_streaming, supports_tools, supports_parallel_tool_calls,
                    supports_previous_response_id, supports_reasoning_encrypted_content,
                    supports_reasoning_summary, status, failure_kind,
                    failure_message_redacted, last_success_at, last_failure_at,
                    expires_at, created_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, NOW())
                ON CONFLICT(provider_id, credential_hash, upstream_model) DO UPDATE SET
                    wire_api = excluded.wire_api,
                    supports_streaming = excluded.supports_streaming,
                    supports_tools = excluded.supports_tools,
                    supports_parallel_tool_calls = excluded.supports_parallel_tool_calls,
                    supports_previous_response_id = excluded.supports_previous_response_id,
                    supports_reasoning_encrypted_content = excluded.supports_reasoning_encrypted_content,
                    supports_reasoning_summary = excluded.supports_reasoning_summary,
                    status = excluded.status,
                    failure_kind = excluded.failure_kind,
                    failure_message_redacted = excluded.failure_message_redacted,
                    last_success_at = CASE
                        WHEN excluded.status = 'success' THEN excluded.last_success_at
                        ELSE last_success_at
                    END,
                    last_failure_at = CASE
                        WHEN excluded.status != 'success' THEN excluded.last_failure_at
                        ELSE last_failure_at
                    END,
                    expires_at = excluded.expires_at
                "#,
            )
            .bind(format!("probe_{}", uuid::Uuid::new_v4()))
            .bind(&probe.provider_id)
            .bind(&probe.credential_hash)
            .bind(&probe.upstream_model)
            .bind(probe.wire_api.as_str())
            .bind(probe.supports_streaming)
            .bind(probe.supports_tools)
            .bind(probe.supports_parallel_tool_calls)
            .bind(probe.supports_previous_response_id)
            .bind(probe.supports_reasoning_encrypted_content)
            .bind(probe.supports_reasoning_summary)
            .bind(status)
            .bind(probe.failure_kind.as_deref())
            .bind(probe.failure_message_redacted.as_deref())
            .bind(last_success_at.as_deref())
            .bind(last_failure_at.as_deref())
            .bind(&expires_at)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// List non-expired probe results ordered by most recent activity first.
pub async fn list_probe_results(db: &DbPool, limit: i64) -> Result<Vec<ProbeRecord>, sqlx::Error> {
    let normalized_limit = limit.clamp(1, 1000);
    let now = chrono::Utc::now().to_rfc3339();

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, ProbeRecord>(
                r#"
                SELECT
                    id, provider_id, credential_hash, upstream_model, wire_api, status,
                    supports_streaming, supports_tools, supports_parallel_tool_calls,
                    supports_previous_response_id, supports_reasoning_encrypted_content,
                    supports_reasoning_summary, failure_kind, failure_message_redacted,
                    last_success_at, last_failure_at, expires_at
                FROM probe_results
                WHERE expires_at > ?
                ORDER BY COALESCE(last_success_at, last_failure_at, expires_at) DESC
                LIMIT ?
                "#,
            )
            .bind(&now)
            .bind(normalized_limit)
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, ProbeRecord>(
                r#"
                SELECT
                    id, provider_id, credential_hash, upstream_model, wire_api, status,
                    CASE WHEN supports_streaming THEN 1 ELSE 0 END AS supports_streaming,
                    CASE WHEN supports_tools THEN 1 ELSE 0 END AS supports_tools,
                    CASE WHEN supports_parallel_tool_calls THEN 1 ELSE 0 END AS supports_parallel_tool_calls,
                    CASE WHEN supports_previous_response_id THEN 1 ELSE 0 END AS supports_previous_response_id,
                    CASE WHEN supports_reasoning_encrypted_content THEN 1 ELSE 0 END AS supports_reasoning_encrypted_content,
                    CASE WHEN supports_reasoning_summary THEN 1 ELSE 0 END AS supports_reasoning_summary,
                    failure_kind, failure_message_redacted,
                    TO_CHAR(last_success_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_success_at,
                    TO_CHAR(last_failure_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_failure_at,
                    TO_CHAR(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS expires_at
                FROM probe_results
                WHERE expires_at > NOW()
                ORDER BY COALESCE(last_success_at, last_failure_at, expires_at) DESC
                LIMIT $1
                "#,
            )
            .bind(normalized_limit)
            .fetch_all(pool)
            .await
        }
    }
}

/// Get probe result.
pub async fn get_probe_result(
    db: &DbPool,
    provider_id: &str,
    credential_hash: &str,
    upstream_model: &str,
) -> Result<Option<ProbeRecord>, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, ProbeRecord>(
                r#"
                SELECT id, provider_id, credential_hash, upstream_model, wire_api, status,
                       supports_streaming, supports_tools, supports_parallel_tool_calls,
                       supports_previous_response_id, supports_reasoning_encrypted_content,
                       supports_reasoning_summary, failure_kind, failure_message_redacted,
                       last_success_at, last_failure_at, expires_at
                FROM probe_results
                WHERE provider_id = ? AND credential_hash = ? AND upstream_model = ? AND expires_at > ?
                "#,
            )
            .bind(provider_id)
            .bind(credential_hash)
            .bind(upstream_model)
            .bind(&now)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, ProbeRecord>(
                r#"
                SELECT id, provider_id, credential_hash, upstream_model, wire_api, status,
                       CASE WHEN supports_streaming THEN 1 ELSE 0 END AS supports_streaming,
                       CASE WHEN supports_tools THEN 1 ELSE 0 END AS supports_tools,
                       CASE WHEN supports_parallel_tool_calls THEN 1 ELSE 0 END AS supports_parallel_tool_calls,
                       CASE WHEN supports_previous_response_id THEN 1 ELSE 0 END AS supports_previous_response_id,
                       CASE WHEN supports_reasoning_encrypted_content THEN 1 ELSE 0 END AS supports_reasoning_encrypted_content,
                       CASE WHEN supports_reasoning_summary THEN 1 ELSE 0 END AS supports_reasoning_summary,
                       failure_kind, failure_message_redacted,
                       TO_CHAR(last_success_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_success_at,
                       TO_CHAR(last_failure_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS last_failure_at,
                       TO_CHAR(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS expires_at
                FROM probe_results
                WHERE provider_id = $1 AND credential_hash = $2 AND upstream_model = $3 AND expires_at > NOW()
                "#,
            )
            .bind(provider_id)
            .bind(credential_hash)
            .bind(upstream_model)
            .fetch_optional(pool)
            .await
        }
    }
}

/// Clear all persisted probe results.
pub async fn clear_probe_results(db: &DbPool) -> Result<u64, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query("DELETE FROM probe_results")
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query("DELETE FROM probe_results")
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
    }
}

/// Probe record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProbeRecord {
    pub id: String,
    pub provider_id: String,
    pub credential_hash: String,
    pub upstream_model: String,
    pub wire_api: String,
    pub status: String,
    pub supports_streaming: Option<i32>,
    pub supports_tools: Option<i32>,
    pub supports_parallel_tool_calls: Option<i32>,
    pub supports_previous_response_id: Option<i32>,
    pub supports_reasoning_encrypted_content: Option<i32>,
    pub supports_reasoning_summary: Option<i32>,
    pub failure_kind: Option<String>,
    pub failure_message_redacted: Option<String>,
    pub last_success_at: Option<String>,
    pub last_failure_at: Option<String>,
    pub expires_at: String,
}
