//! Providers repository.

use crate::DbPool;

/// Provider row model for admin and relay lookups.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub auth_mode: String,
    pub default_wire_api: String,
    pub state_scope: Option<String>,
    pub config_json: String,
}

/// Insert payload for provider persistence.
#[derive(Debug, Clone)]
pub struct ProviderInsert<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub base_url: &'a str,
    pub auth_mode: &'a str,
    pub default_wire_api: &'a str,
    pub state_scope: Option<&'a str>,
    pub config_json: &'a str,
}

/// Update payload for provider persistence.
#[derive(Debug, Clone)]
pub struct ProviderUpdate<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub base_url: &'a str,
    pub auth_mode: &'a str,
    pub default_wire_api: &'a str,
    pub state_scope: Option<&'a str>,
    pub config_json: &'a str,
}

/// List all providers ordered by id.
pub async fn list_providers(db: &DbPool) -> Result<Vec<ProviderRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, ProviderRecord>(
                r#"
                SELECT id, name, base_url, auth_mode, default_wire_api, state_scope, config_json
                FROM providers
                ORDER BY id
                "#,
            )
            .fetch_all(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, ProviderRecord>(
                r#"
                SELECT id, name, base_url, auth_mode, default_wire_api, state_scope, config_json
                FROM providers
                ORDER BY id
                "#,
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Get provider by ID.
pub async fn get_provider(db: &DbPool, id: &str) -> Result<Option<ProviderRecord>, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query_as::<_, ProviderRecord>(
                r#"
                SELECT id, name, base_url, auth_mode, default_wire_api, state_scope, config_json
                FROM providers WHERE id = ?
                "#,
            )
            .bind(id)
            .fetch_optional(pool)
            .await
        }
        DbPool::Postgres(pool) => {
            sqlx::query_as::<_, ProviderRecord>(
                r#"
                SELECT id, name, base_url, auth_mode, default_wire_api, state_scope, config_json
                FROM providers WHERE id = $1
                "#,
            )
            .bind(id)
            .fetch_optional(pool)
            .await
        }
    }
}

/// Insert provider.
pub async fn insert_provider(
    db: &DbPool,
    provider: &ProviderInsert<'_>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO providers
                (id, name, base_url, auth_mode, default_wire_api, state_scope, config_json, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(provider.id)
            .bind(provider.name)
            .bind(provider.base_url)
            .bind(provider.auth_mode)
            .bind(provider.default_wire_api)
            .bind(provider.state_scope)
            .bind(provider.config_json)
            .bind(&now)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO providers
                (id, name, base_url, auth_mode, default_wire_api, state_scope, config_json, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
                "#,
            )
            .bind(provider.id)
            .bind(provider.name)
            .bind(provider.base_url)
            .bind(provider.auth_mode)
            .bind(provider.default_wire_api)
            .bind(provider.state_scope)
            .bind(provider.config_json)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Update provider by id.
pub async fn update_provider(
    db: &DbPool,
    provider: &ProviderUpdate<'_>,
) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query(
                r#"
                UPDATE providers
                SET name = ?, base_url = ?, auth_mode = ?, default_wire_api = ?, state_scope = ?, config_json = ?, updated_at = ?
                WHERE id = ?
                "#,
            )
            .bind(provider.name)
            .bind(provider.base_url)
            .bind(provider.auth_mode)
            .bind(provider.default_wire_api)
            .bind(provider.state_scope)
            .bind(provider.config_json)
            .bind(&now)
            .bind(provider.id)
            .execute(pool)
            .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query(
                r#"
                UPDATE providers
                SET name = $1, base_url = $2, auth_mode = $3, default_wire_api = $4, state_scope = $5, config_json = $6, updated_at = NOW()
                WHERE id = $7
                "#,
            )
            .bind(provider.name)
            .bind(provider.base_url)
            .bind(provider.auth_mode)
            .bind(provider.default_wire_api)
            .bind(provider.state_scope)
            .bind(provider.config_json)
            .bind(provider.id)
            .execute(pool)
            .await?;
            Ok(result.rows_affected())
        }
    }
}

/// Delete provider by id.
pub async fn delete_provider(db: &DbPool, id: &str) -> Result<u64, sqlx::Error> {
    match db {
        DbPool::Sqlite(pool) => {
            let result = sqlx::query("DELETE FROM providers WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
        DbPool::Postgres(pool) => {
            let result = sqlx::query("DELETE FROM providers WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
            Ok(result.rows_affected())
        }
    }
}
