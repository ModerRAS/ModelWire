//! Request logs repository.

use crate::DbPool;

/// Request log row model.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RequestLogRecord {
    pub id: String,
    pub request_id: String,
    pub downstream_key_hash: Option<String>,
    pub downstream_model: Option<String>,
    pub route_id: Option<String>,
    pub target_id: Option<String>,
    pub provider_id: Option<String>,
    pub upstream_model: Option<String>,
    pub wire_api: Option<String>,
    pub status_code: Option<i32>,
    pub error_kind: Option<String>,
    pub latency_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub created_at: String,
}

/// Store request log.
#[allow(clippy::too_many_arguments)]
pub async fn store_log(
    db: &DbPool,
    request_id: &str,
    downstream_key_hash: Option<&str>,
    downstream_model: Option<&str>,
    route_id: Option<&str>,
    target_id: Option<&str>,
    provider_id: Option<&str>,
    upstream_model: Option<&str>,
    wire_api: Option<&str>,
    status_code: Option<i32>,
    error_kind: Option<&str>,
    latency_ms: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO request_logs (id, request_id, downstream_key_hash, downstream_model, route_id, target_id, provider_id, upstream_model, wire_api, status_code, error_kind, latency_ms, input_tokens, output_tokens, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(format!("log_{}", uuid::Uuid::new_v4()))
            .bind(request_id)
            .bind(downstream_key_hash)
            .bind(downstream_model)
            .bind(route_id)
            .bind(target_id)
            .bind(provider_id)
            .bind(upstream_model)
            .bind(wire_api)
            .bind(status_code)
            .bind(error_kind)
            .bind(latency_ms)
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO request_logs (id, request_id, downstream_key_hash, downstream_model, route_id, target_id, provider_id, upstream_model, wire_api, status_code, error_kind, latency_ms, input_tokens, output_tokens, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW())
                "#,
            )
            .bind(format!("log_{}", uuid::Uuid::new_v4()))
            .bind(request_id)
            .bind(downstream_key_hash)
            .bind(downstream_model)
            .bind(route_id)
            .bind(target_id)
            .bind(provider_id)
            .bind(upstream_model)
            .bind(wire_api)
            .bind(status_code)
            .bind(error_kind)
            .bind(latency_ms)
            .bind(input_tokens)
            .bind(output_tokens)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// List request logs ordered by most recent first.
pub async fn list_logs(db: &DbPool, limit: i64) -> Result<Vec<RequestLogRecord>, sqlx::Error> {
    let normalized_limit = limit.clamp(1, 500);
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, RequestLogRecord>(
                r#"
                SELECT
                    id, request_id, downstream_key_hash, downstream_model,
                    route_id, target_id, provider_id, upstream_model, wire_api,
                    status_code, error_kind, latency_ms, input_tokens, output_tokens,
                    reasoning_tokens, created_at
                FROM request_logs
                ORDER BY created_at DESC
                LIMIT ?
                "#,
            )
            .bind(normalized_limit)
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, RequestLogRecord>(
                r#"
                SELECT
                    id, request_id, downstream_key_hash, downstream_model,
                    route_id, target_id, provider_id, upstream_model, wire_api,
                    status_code, error_kind, latency_ms, input_tokens, output_tokens,
                    reasoning_tokens, TO_CHAR(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at
                FROM request_logs
                ORDER BY created_at DESC
                LIMIT $1
                "#,
            )
            .bind(normalized_limit)
            .fetch_all(pool)
            .await
        }
    }
}

/// Count total request logs.
pub async fn count_logs(db: &DbPool) -> Result<i64, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct CountRow {
        count: i64,
    }

    match db {
        DbPool::Sqlite(pool) => {
            let row = sqlx::query_as::<_, CountRow>(
                r#"
                SELECT COUNT(*) AS count
                FROM request_logs
                "#,
            )
            .fetch_one(pool)
            .await?;
            Ok(row.count)
        }
        DbPool::Postgres(pool) => {
            let row = sqlx::query_as::<_, CountRow>(
                r#"
                SELECT COUNT(*)::BIGINT AS count
                FROM request_logs
                "#,
            )
            .fetch_one(pool)
            .await?;
            Ok(row.count)
        }
    }
}
