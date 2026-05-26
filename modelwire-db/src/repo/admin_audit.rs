//! Admin configuration audit events repository.

use crate::DbPool;

#[derive(Debug, Clone)]
pub struct AdminAuditInsert<'a> {
    pub id: &'a str,
    pub request_id: &'a str,
    pub actor_key_hash: &'a str,
    pub action: &'a str,
    pub resource_type: &'a str,
    pub resource_id: &'a str,
    pub diff_json: &'a str,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminAuditRecord {
    pub id: String,
    pub request_id: String,
    pub actor_key_hash: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub diff_json: String,
    pub created_at: String,
}

pub async fn store_admin_audit_event(
    db: &DbPool,
    event: &AdminAuditInsert<'_>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO admin_audit_events (
                    id, request_id, actor_key_hash, action,
                    resource_type, resource_id, diff_json, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(event.id)
            .bind(event.request_id)
            .bind(event.actor_key_hash)
            .bind(event.action)
            .bind(event.resource_type)
            .bind(event.resource_id)
            .bind(event.diff_json)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO admin_audit_events (
                    id, request_id, actor_key_hash, action,
                    resource_type, resource_id, diff_json, created_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, NOW())
                "#,
            )
            .bind(event.id)
            .bind(event.request_id)
            .bind(event.actor_key_hash)
            .bind(event.action)
            .bind(event.resource_type)
            .bind(event.resource_id)
            .bind(event.diff_json)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

pub async fn list_admin_audit_events(
    db: &DbPool,
    limit: i64,
) -> Result<Vec<AdminAuditRecord>, sqlx::Error> {
    let normalized_limit = limit.clamp(1, 500);
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, AdminAuditRecord>(
                r#"
                SELECT id, request_id, actor_key_hash, action,
                       resource_type, resource_id, diff_json, created_at
                FROM admin_audit_events
                ORDER BY created_at DESC
                LIMIT ?
                "#,
            )
            .bind(normalized_limit)
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, AdminAuditRecord>(
                r#"
                SELECT id, request_id, actor_key_hash, action,
                       resource_type, resource_id, diff_json::text AS diff_json,
                       created_at::text AS created_at
                FROM admin_audit_events
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
