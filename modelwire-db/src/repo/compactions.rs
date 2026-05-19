//! Compaction lineage repository.
//!
//! Stores lineage for native/local compaction operations.

use crate::DbPool;

#[derive(Debug, Clone)]
pub struct CompactionLineageInsert<'a> {
    pub id: &'a str,
    pub request_id: &'a str,
    pub route_id: Option<&'a str>,
    pub downstream_model: &'a str,
    pub source_response_ids_json: &'a str,
    pub provider_id: Option<&'a str>,
    pub upstream_model: Option<&'a str>,
    pub state_scope: Option<&'a str>,
    pub method: &'a str,
    pub provider_native: bool,
    pub summarizer_model: Option<&'a str>,
    pub prompt_version: Option<&'a str>,
    pub source_tokens: Option<i64>,
    pub summary_tokens: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CompactionLineageRecord {
    pub id: String,
    pub request_id: String,
    pub route_id: Option<String>,
    pub downstream_model: String,
    pub source_response_ids_json: String,
    pub provider_id: Option<String>,
    pub upstream_model: Option<String>,
    pub state_scope: Option<String>,
    pub method: String,
    pub provider_native: i32,
    pub summarizer_model: Option<String>,
    pub prompt_version: Option<String>,
    pub source_tokens: Option<i64>,
    pub summary_tokens: Option<i64>,
    pub created_at: String,
}

pub async fn store_compaction_lineage(
    db: &DbPool,
    lineage: &CompactionLineageInsert<'_>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO compaction_lineage (
                    id, request_id, route_id, downstream_model, source_response_ids_json,
                    provider_id, upstream_model, state_scope, method, provider_native,
                    summarizer_model, prompt_version, source_tokens, summary_tokens, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(lineage.id)
            .bind(lineage.request_id)
            .bind(lineage.route_id)
            .bind(lineage.downstream_model)
            .bind(lineage.source_response_ids_json)
            .bind(lineage.provider_id)
            .bind(lineage.upstream_model)
            .bind(lineage.state_scope)
            .bind(lineage.method)
            .bind(if lineage.provider_native { 1 } else { 0 })
            .bind(lineage.summarizer_model)
            .bind(lineage.prompt_version)
            .bind(lineage.source_tokens)
            .bind(lineage.summary_tokens)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO compaction_lineage (
                    id, request_id, route_id, downstream_model, source_response_ids_json,
                    provider_id, upstream_model, state_scope, method, provider_native,
                    summarizer_model, prompt_version, source_tokens, summary_tokens, created_at
                )
                VALUES (
                    $1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW()
                )
                "#,
            )
            .bind(lineage.id)
            .bind(lineage.request_id)
            .bind(lineage.route_id)
            .bind(lineage.downstream_model)
            .bind(lineage.source_response_ids_json)
            .bind(lineage.provider_id)
            .bind(lineage.upstream_model)
            .bind(lineage.state_scope)
            .bind(lineage.method)
            .bind(lineage.provider_native)
            .bind(lineage.summarizer_model)
            .bind(lineage.prompt_version)
            .bind(lineage.source_tokens)
            .bind(lineage.summary_tokens)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

pub async fn get_latest_compaction_lineage(
    db: &DbPool,
) -> Result<Option<CompactionLineageRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, CompactionLineageRecord>(
                r#"
                SELECT id, request_id, route_id, downstream_model, source_response_ids_json,
                       provider_id, upstream_model, state_scope, method, provider_native,
                       summarizer_model, prompt_version, source_tokens, summary_tokens, created_at
                FROM compaction_lineage
                ORDER BY created_at DESC
                LIMIT 1
                "#,
            )
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, CompactionLineageRecord>(
                r#"
                SELECT id, request_id, route_id, downstream_model,
                       source_response_ids_json::text AS source_response_ids_json,
                       provider_id, upstream_model, state_scope, method,
                       CASE WHEN provider_native THEN 1 ELSE 0 END AS provider_native,
                       summarizer_model, prompt_version, source_tokens, summary_tokens,
                       created_at::text AS created_at
                FROM compaction_lineage
                ORDER BY created_at DESC
                LIMIT 1
                "#,
            )
            .fetch_optional(pool)
            .await
        }
    }
}
