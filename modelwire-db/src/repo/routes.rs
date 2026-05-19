//! Routes and route-targets repository.

use crate::DbPool;

/// Route row model.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RouteRecord {
    pub id: String,
    pub downstream_model: String,
    pub description: Option<String>,
    pub enabled: i32,
}

/// Route target row model.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TargetRecord {
    pub id: String,
    pub route_id: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub wire_api: String,
    pub priority: i32,
    pub enabled: i32,
    pub config_json: String,
    pub provider_base_url: String,
}

/// Insert payload for routes.
#[derive(Debug, Clone)]
pub struct RouteInsert<'a> {
    pub id: &'a str,
    pub downstream_model: &'a str,
    pub description: Option<&'a str>,
    pub enabled: bool,
}

/// Update payload for routes.
#[derive(Debug, Clone)]
pub struct RouteUpdate<'a> {
    pub id: &'a str,
    pub downstream_model: &'a str,
    pub description: Option<&'a str>,
    pub enabled: bool,
}

/// Insert payload for targets.
#[derive(Debug, Clone)]
pub struct TargetInsert<'a> {
    pub id: &'a str,
    pub route_id: &'a str,
    pub provider_id: &'a str,
    pub upstream_model: &'a str,
    pub wire_api: &'a str,
    pub priority: i32,
    pub enabled: bool,
    pub config_json: &'a str,
}

/// Update payload for targets.
#[derive(Debug, Clone)]
pub struct TargetUpdate<'a> {
    pub id: &'a str,
    pub provider_id: &'a str,
    pub upstream_model: &'a str,
    pub wire_api: &'a str,
    pub priority: i32,
    pub enabled: bool,
    pub config_json: &'a str,
}

/// List all routes ordered by id.
pub async fn list_routes(db: &DbPool) -> Result<Vec<RouteRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, RouteRecord>(
                r#"
                SELECT id, downstream_model, description, enabled
                FROM routes
                ORDER BY id
                "#,
            )
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, RouteRecord>(
                r#"
                SELECT id, downstream_model, description, CASE WHEN enabled THEN 1 ELSE 0 END AS enabled
                FROM routes
                ORDER BY id
                "#,
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Get route by downstream model.
pub async fn get_route(
    db: &DbPool,
    downstream_model: &str,
) -> Result<Option<RouteRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, RouteRecord>(
                r#"
                SELECT id, downstream_model, description, enabled
                FROM routes WHERE downstream_model = ?
                "#,
            )
            .bind(downstream_model)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, RouteRecord>(
                r#"
                SELECT id, downstream_model, description, CASE WHEN enabled THEN 1 ELSE 0 END AS enabled
                FROM routes WHERE downstream_model = $1
                "#,
            )
            .bind(downstream_model)
            .fetch_optional(pool)
            .await
        }
    }
}

/// Get route by id.
pub async fn get_route_by_id(
    db: &DbPool,
    route_id: &str,
) -> Result<Option<RouteRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, RouteRecord>(
                r#"
                SELECT id, downstream_model, description, enabled
                FROM routes WHERE id = ?
                "#,
            )
            .bind(route_id)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, RouteRecord>(
                r#"
                SELECT id, downstream_model, description, CASE WHEN enabled THEN 1 ELSE 0 END AS enabled
                FROM routes WHERE id = $1
                "#,
            )
            .bind(route_id)
            .fetch_optional(pool)
            .await
        }
    }
}

/// Insert route.
pub async fn insert_route(db: &DbPool, route: &RouteInsert<'_>) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO routes
                (id, downstream_model, description, enabled, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(route.id)
            .bind(route.downstream_model)
            .bind(route.description)
            .bind(if route.enabled { 1_i32 } else { 0_i32 })
            .bind(&now)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO routes
                (id, downstream_model, description, enabled, created_at, updated_at)
                VALUES ($1, $2, $3, $4, NOW(), NOW())
                "#,
            )
            .bind(route.id)
            .bind(route.downstream_model)
            .bind(route.description)
            .bind(route.enabled)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Update route by id.
pub async fn update_route(db: &DbPool, route: &RouteUpdate<'_>) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query(
                r#"
                UPDATE routes
                SET downstream_model = ?, description = ?, enabled = ?, updated_at = ?
                WHERE id = ?
                "#,
            )
            .bind(route.downstream_model)
            .bind(route.description)
            .bind(if route.enabled { 1_i32 } else { 0_i32 })
            .bind(&now)
            .bind(route.id)
            .execute(pool)
            .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query(
                r#"
                UPDATE routes
                SET downstream_model = $1, description = $2, enabled = $3, updated_at = NOW()
                WHERE id = $4
                "#,
            )
            .bind(route.downstream_model)
            .bind(route.description)
            .bind(route.enabled)
            .bind(route.id)
            .execute(pool)
            .await?;
            Ok(result.rows_affected())
        }
    }
}

/// Delete route by id.
pub async fn delete_route(db: &DbPool, route_id: &str) -> Result<u64, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query("DELETE FROM routes WHERE id = ?")
                .bind(route_id)
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query("DELETE FROM routes WHERE id = $1")
                .bind(route_id)
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
    }
}

/// Get targets for a route.
pub async fn get_targets(db: &DbPool, route_id: &str) -> Result<Vec<TargetRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, TargetRecord>(
                r#"
                SELECT t.id, t.route_id, t.provider_id, t.upstream_model, t.wire_api,
                       t.priority, t.enabled, t.config_json, p.base_url as provider_base_url
                FROM route_targets t
                JOIN providers p ON t.provider_id = p.id
                WHERE t.route_id = ?
                ORDER BY t.priority
                "#,
            )
            .bind(route_id)
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, TargetRecord>(
                r#"
                SELECT t.id, t.route_id, t.provider_id, t.upstream_model, t.wire_api,
                       t.priority, CASE WHEN t.enabled THEN 1 ELSE 0 END AS enabled,
                       t.config_json, p.base_url as provider_base_url
                FROM route_targets t
                JOIN providers p ON t.provider_id = p.id
                WHERE t.route_id = $1
                ORDER BY t.priority
                "#,
            )
            .bind(route_id)
            .fetch_all(pool)
            .await
        }
    }
}

/// Get target by id.
pub async fn get_target_by_id(
    db: &DbPool,
    target_id: &str,
) -> Result<Option<TargetRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, TargetRecord>(
                r#"
                SELECT t.id, t.route_id, t.provider_id, t.upstream_model, t.wire_api,
                       t.priority, t.enabled, t.config_json, p.base_url as provider_base_url
                FROM route_targets t
                JOIN providers p ON t.provider_id = p.id
                WHERE t.id = ?
                "#,
            )
            .bind(target_id)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, TargetRecord>(
                r#"
                SELECT t.id, t.route_id, t.provider_id, t.upstream_model, t.wire_api,
                       t.priority, CASE WHEN t.enabled THEN 1 ELSE 0 END AS enabled,
                       t.config_json, p.base_url as provider_base_url
                FROM route_targets t
                JOIN providers p ON t.provider_id = p.id
                WHERE t.id = $1
                "#,
            )
            .bind(target_id)
            .fetch_optional(pool)
            .await
        }
    }
}

/// Insert target.
pub async fn insert_target(db: &DbPool, target: &TargetInsert<'_>) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO route_targets
                (id, route_id, provider_id, upstream_model, wire_api, priority, enabled, config_json, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(target.id)
            .bind(target.route_id)
            .bind(target.provider_id)
            .bind(target.upstream_model)
            .bind(target.wire_api)
            .bind(target.priority)
            .bind(if target.enabled { 1_i32 } else { 0_i32 })
            .bind(target.config_json)
            .bind(&now)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO route_targets
                (id, route_id, provider_id, upstream_model, wire_api, priority, enabled, config_json, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW())
                "#,
            )
            .bind(target.id)
            .bind(target.route_id)
            .bind(target.provider_id)
            .bind(target.upstream_model)
            .bind(target.wire_api)
            .bind(target.priority)
            .bind(target.enabled)
            .bind(target.config_json)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Update target by id.
pub async fn update_target(db: &DbPool, target: &TargetUpdate<'_>) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query(
                r#"
                UPDATE route_targets
                SET provider_id = ?, upstream_model = ?, wire_api = ?, priority = ?, enabled = ?, config_json = ?, updated_at = ?
                WHERE id = ?
                "#,
            )
            .bind(target.provider_id)
            .bind(target.upstream_model)
            .bind(target.wire_api)
            .bind(target.priority)
            .bind(if target.enabled { 1_i32 } else { 0_i32 })
            .bind(target.config_json)
            .bind(&now)
            .bind(target.id)
            .execute(pool)
            .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query(
                r#"
                UPDATE route_targets
                SET provider_id = $1, upstream_model = $2, wire_api = $3, priority = $4, enabled = $5, config_json = $6, updated_at = NOW()
                WHERE id = $7
                "#,
            )
            .bind(target.provider_id)
            .bind(target.upstream_model)
            .bind(target.wire_api)
            .bind(target.priority)
            .bind(target.enabled)
            .bind(target.config_json)
            .bind(target.id)
            .execute(pool)
            .await?;
            Ok(result.rows_affected())
        }
    }
}

/// Delete target by id.
pub async fn delete_target(db: &DbPool, target_id: &str) -> Result<u64, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query("DELETE FROM route_targets WHERE id = ?")
                .bind(target_id)
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query("DELETE FROM route_targets WHERE id = $1")
                .bind(target_id)
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
    }
}
