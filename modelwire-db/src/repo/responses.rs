//! Response repository.

use crate::DbPool;

/// Complete response metadata for the operational response table.
#[derive(Debug, Clone)]
pub struct ResponseInsert<'a> {
    pub id: &'a str,
    pub request_id: &'a str,
    pub downstream_model: &'a str,
    pub route_id: Option<&'a str>,
    pub target_id: Option<&'a str>,
    pub provider_id: Option<&'a str>,
    pub upstream_model: Option<&'a str>,
    pub wire_api: Option<&'a str>,
    pub upstream_response_id: Option<&'a str>,
    pub state_scope: Option<&'a str>,
    pub previous_response_id: Option<&'a str>,
    pub status: &'a str,
    pub usage_json: Option<&'a str>,
    pub error_json: Option<&'a str>,
}

/// Response item to store in the canonical transcript table.
#[derive(Debug, Clone)]
pub struct ResponseItemInsert<'a> {
    pub id: &'a str,
    pub response_id: &'a str,
    pub sequence: i64,
    pub item_type: &'a str,
    pub role: Option<&'a str>,
    pub call_id: Option<&'a str>,
    pub content_json: &'a str,
    pub visible: bool,
}

/// Private upstream handle for continuation and replay decisions.
#[derive(Debug, Clone)]
pub struct UpstreamHandleInsert<'a> {
    pub id: &'a str,
    pub modelwire_response_id: &'a str,
    pub provider_id: &'a str,
    pub credential_hash: &'a str,
    pub upstream_model: &'a str,
    pub wire_api: &'a str,
    pub state_scope: Option<&'a str>,
    pub upstream_response_id: Option<&'a str>,
    pub handle_json: &'a str,
}

/// Retrieved upstream handle row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UpstreamHandleRecord {
    pub id: String,
    pub modelwire_response_id: String,
    pub provider_id: String,
    pub credential_hash: String,
    pub upstream_model: String,
    pub wire_api: String,
    pub state_scope: Option<String>,
    pub upstream_response_id: Option<String>,
    pub handle_json: String,
    pub created_at: String,
}

/// Store a minimal response shell.
pub async fn store_response(
    db: &DbPool,
    id: &str,
    request_id: &str,
    downstream_model: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    store_response_metadata(
        db,
        &ResponseInsert {
            id,
            request_id,
            downstream_model,
            route_id: None,
            target_id: None,
            provider_id: None,
            upstream_model: None,
            wire_api: None,
            upstream_response_id: None,
            state_scope: None,
            previous_response_id: None,
            status,
            usage_json: None,
            error_json: None,
        },
    )
    .await
}

/// Store complete response metadata.
pub async fn store_response_metadata(
    db: &DbPool,
    response: &ResponseInsert<'_>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at = is_terminal_status(response.status).then_some(now.as_str());

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO responses (
                    id, request_id, downstream_model, route_id, target_id,
                    provider_id, upstream_model, wire_api, upstream_response_id,
                    state_scope, previous_response_id, status, usage_json,
                    error_json, created_at, completed_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    route_id = excluded.route_id,
                    target_id = excluded.target_id,
                    provider_id = excluded.provider_id,
                    upstream_model = excluded.upstream_model,
                    wire_api = excluded.wire_api,
                    upstream_response_id = excluded.upstream_response_id,
                    state_scope = excluded.state_scope,
                    previous_response_id = excluded.previous_response_id,
                    status = excluded.status,
                    usage_json = excluded.usage_json,
                    error_json = excluded.error_json,
                    completed_at = excluded.completed_at
                "#,
            )
            .bind(response.id)
            .bind(response.request_id)
            .bind(response.downstream_model)
            .bind(response.route_id)
            .bind(response.target_id)
            .bind(response.provider_id)
            .bind(response.upstream_model)
            .bind(response.wire_api)
            .bind(response.upstream_response_id)
            .bind(response.state_scope)
            .bind(response.previous_response_id)
            .bind(response.status)
            .bind(response.usage_json)
            .bind(response.error_json)
            .bind(&now)
            .bind(completed_at)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO responses (
                    id, request_id, downstream_model, route_id, target_id,
                    provider_id, upstream_model, wire_api, upstream_response_id,
                    state_scope, previous_response_id, status, usage_json,
                    error_json, created_at, completed_at
                )
                VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13::jsonb, $14::jsonb, NOW(),
                    CASE WHEN $12 IN ('completed', 'failed', 'cancelled') THEN NOW() ELSE NULL END
                )
                ON CONFLICT(id) DO UPDATE SET
                    route_id = excluded.route_id,
                    target_id = excluded.target_id,
                    provider_id = excluded.provider_id,
                    upstream_model = excluded.upstream_model,
                    wire_api = excluded.wire_api,
                    upstream_response_id = excluded.upstream_response_id,
                    state_scope = excluded.state_scope,
                    previous_response_id = excluded.previous_response_id,
                    status = excluded.status,
                    usage_json = excluded.usage_json,
                    error_json = excluded.error_json,
                    completed_at = excluded.completed_at
                "#,
            )
            .bind(response.id)
            .bind(response.request_id)
            .bind(response.downstream_model)
            .bind(response.route_id)
            .bind(response.target_id)
            .bind(response.provider_id)
            .bind(response.upstream_model)
            .bind(response.wire_api)
            .bind(response.upstream_response_id)
            .bind(response.state_scope)
            .bind(response.previous_response_id)
            .bind(response.status)
            .bind(response.usage_json)
            .bind(response.error_json)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// Get a response by ID.
pub async fn get_response(db: &DbPool, id: &str) -> Result<Option<ResponseRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, ResponseRecord>(
                r#"
                SELECT id, request_id, downstream_model, route_id, target_id,
                       provider_id, upstream_model, wire_api, upstream_response_id,
                       state_scope, status, previous_response_id, usage_json,
                       error_json, created_at, completed_at
                FROM responses WHERE id = ?
                "#,
            )
            .bind(id)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, ResponseRecord>(
                r#"
                SELECT id, request_id, downstream_model, route_id, target_id,
                       provider_id, upstream_model, wire_api, upstream_response_id,
                       state_scope, status, previous_response_id, usage_json::text AS usage_json,
                       error_json::text AS error_json, created_at::text AS created_at,
                       completed_at::text AS completed_at
                FROM responses WHERE id = $1
                "#,
            )
            .bind(id)
            .fetch_optional(pool)
            .await
        }
    }
}

/// Response record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ResponseRecord {
    pub id: String,
    pub request_id: String,
    pub downstream_model: String,
    pub route_id: Option<String>,
    pub target_id: Option<String>,
    pub provider_id: Option<String>,
    pub upstream_model: Option<String>,
    pub wire_api: Option<String>,
    pub upstream_response_id: Option<String>,
    pub state_scope: Option<String>,
    pub status: String,
    pub previous_response_id: Option<String>,
    pub usage_json: Option<String>,
    pub error_json: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Store a response item.
#[allow(clippy::too_many_arguments)]
pub async fn store_item(
    db: &DbPool,
    id: &str,
    response_id: &str,
    sequence: i64,
    item_type: &str,
    role: Option<&str>,
    call_id: Option<&str>,
    content_json: &str,
    visible: bool,
) -> Result<(), sqlx::Error> {
    store_response_item(
        db,
        &ResponseItemInsert {
            id,
            response_id,
            sequence,
            item_type,
            role,
            call_id,
            content_json,
            visible,
        },
    )
    .await
}

/// Store a response item from a structured insert object.
pub async fn store_response_item(
    db: &DbPool,
    item: &ResponseItemInsert<'_>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO response_items (id, response_id, sequence, item_type, role, call_id, content_json, visible, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(item.id)
            .bind(item.response_id)
            .bind(item.sequence)
            .bind(item.item_type)
            .bind(item.role)
            .bind(item.call_id)
            .bind(item.content_json)
            .bind(item.visible as i32)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO response_items (id, response_id, sequence, item_type, role, call_id, content_json, visible, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8, NOW())
                "#,
            )
            .bind(item.id)
            .bind(item.response_id)
            .bind(item.sequence)
            .bind(item.item_type)
            .bind(item.role)
            .bind(item.call_id)
            .bind(item.content_json)
            .bind(item.visible)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// Get response items for a response.
pub async fn get_items(db: &DbPool, response_id: &str) -> Result<Vec<ItemRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, ItemRecord>(
                r#"
                SELECT id, sequence, item_type, role, call_id, content_json, visible
                FROM response_items WHERE response_id = ? ORDER BY sequence
                "#,
            )
            .bind(response_id)
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, ItemRecord>(
                r#"
                SELECT id, sequence, item_type, role, call_id,
                       content_json::text AS content_json,
                       CASE WHEN visible THEN 1 ELSE 0 END AS visible
                FROM response_items WHERE response_id = $1 ORDER BY sequence
                "#,
            )
            .bind(response_id)
            .fetch_all(pool)
            .await
        }
    }
}

/// Item record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemRecord {
    pub id: String,
    pub sequence: i64,
    pub item_type: String,
    pub role: Option<String>,
    pub call_id: Option<String>,
    pub content_json: String,
    pub visible: i32,
}

/// Store a private upstream handle.
pub async fn store_upstream_handle(
    db: &DbPool,
    handle: &UpstreamHandleInsert<'_>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO upstream_handles (
                    id, modelwire_response_id, provider_id, credential_hash,
                    upstream_model, wire_api, state_scope, upstream_response_id,
                    handle_json, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(handle.id)
            .bind(handle.modelwire_response_id)
            .bind(handle.provider_id)
            .bind(handle.credential_hash)
            .bind(handle.upstream_model)
            .bind(handle.wire_api)
            .bind(handle.state_scope)
            .bind(handle.upstream_response_id)
            .bind(handle.handle_json)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO upstream_handles (
                    id, modelwire_response_id, provider_id, credential_hash,
                    upstream_model, wire_api, state_scope, upstream_response_id,
                    handle_json, created_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, NOW())
                "#,
            )
            .bind(handle.id)
            .bind(handle.modelwire_response_id)
            .bind(handle.provider_id)
            .bind(handle.credential_hash)
            .bind(handle.upstream_model)
            .bind(handle.wire_api)
            .bind(handle.state_scope)
            .bind(handle.upstream_response_id)
            .bind(handle.handle_json)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// Get latest upstream handle for a ModelWire response.
pub async fn get_latest_upstream_handle(
    db: &DbPool,
    modelwire_response_id: &str,
) -> Result<Option<UpstreamHandleRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, UpstreamHandleRecord>(
                r#"
                SELECT id, modelwire_response_id, provider_id, credential_hash,
                       upstream_model, wire_api, state_scope, upstream_response_id,
                       handle_json, created_at
                FROM upstream_handles
                WHERE modelwire_response_id = ?
                ORDER BY created_at DESC
                LIMIT 1
                "#,
            )
            .bind(modelwire_response_id)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, UpstreamHandleRecord>(
                r#"
                SELECT id, modelwire_response_id, provider_id, credential_hash,
                       upstream_model, wire_api, state_scope, upstream_response_id,
                       handle_json::text AS handle_json, created_at::text AS created_at
                FROM upstream_handles
                WHERE modelwire_response_id = $1
                ORDER BY created_at DESC
                LIMIT 1
                "#,
            )
            .bind(modelwire_response_id)
            .fetch_optional(pool)
            .await
        }
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbPool;

    #[tokio::test]
    async fn store_response_metadata_roundtrips_sqlite() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        store_response_metadata(
            &db,
            &ResponseInsert {
                id: "resp_mw_test",
                request_id: "req_mw_test",
                downstream_model: "codex-main",
                route_id: Some("route-main"),
                target_id: Some("target-a"),
                provider_id: Some("provider-a"),
                upstream_model: Some("gpt-upstream"),
                wire_api: Some("responses"),
                upstream_response_id: Some("resp_upstream_private"),
                state_scope: Some("scope-a"),
                previous_response_id: None,
                status: "completed",
                usage_json: Some(r#"{"input_tokens":1,"output_tokens":2}"#),
                error_json: None,
            },
        )
        .await
        .unwrap();

        let record = get_response(&db, "resp_mw_test").await.unwrap().unwrap();

        assert_eq!(record.route_id.as_deref(), Some("route-main"));
        assert_eq!(record.target_id.as_deref(), Some("target-a"));
        assert_eq!(record.provider_id.as_deref(), Some("provider-a"));
        assert_eq!(record.upstream_model.as_deref(), Some("gpt-upstream"));
        assert_eq!(record.wire_api.as_deref(), Some("responses"));
        assert_eq!(
            record.upstream_response_id.as_deref(),
            Some("resp_upstream_private")
        );
        assert!(record.completed_at.is_some());
    }

    #[tokio::test]
    async fn store_response_item_orders_by_sequence_sqlite() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();
        store_response(
            &db,
            "resp_mw_test",
            "req_mw_test",
            "codex-main",
            "completed",
        )
        .await
        .unwrap();

        store_response_item(
            &db,
            &ResponseItemInsert {
                id: "msg_mw_2",
                response_id: "resp_mw_test",
                sequence: 2,
                item_type: "message",
                role: Some("assistant"),
                call_id: None,
                content_json: r#"{"text":"second"}"#,
                visible: true,
            },
        )
        .await
        .unwrap();
        store_response_item(
            &db,
            &ResponseItemInsert {
                id: "msg_mw_1",
                response_id: "resp_mw_test",
                sequence: 1,
                item_type: "message",
                role: Some("assistant"),
                call_id: None,
                content_json: r#"{"text":"first"}"#,
                visible: true,
            },
        )
        .await
        .unwrap();

        let items = get_items(&db, "resp_mw_test").await.unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "msg_mw_1");
        assert_eq!(items[1].id, "msg_mw_2");
    }
}
