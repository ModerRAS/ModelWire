//! ModelWire Database
//!
//! Database layer with SQLite/Postgres support.

pub mod repo;
pub mod schema;

use sqlx::{postgres::PgPoolOptions, sqlite::SqlitePoolOptions, PgPool, SqlitePool};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::info;

pub type Database = DbPool;

/// Database pool wrapper.
#[derive(Clone)]
pub enum DbPool {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

impl DbPool {
    fn sqlite_path_from_url(database_url: &str) -> Option<PathBuf> {
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(database_url).ok()?;
        let filename = opts.get_filename().to_str()?;
        if filename == ":memory:" || filename.starts_with("file:") {
            return None;
        }
        Some(PathBuf::from(filename))
    }

    #[cfg(unix)]
    fn enforce_owner_only_permissions(
        path: &std::path::Path,
        file_mode: bool,
    ) -> Result<(), sqlx::Error> {
        use std::os::unix::fs::PermissionsExt;
        let target_mode = if file_mode { 0o600 } else { 0o700 };
        let perms = std::fs::Permissions::from_mode(target_mode);
        std::fs::set_permissions(path, perms).map_err(sqlx::Error::Io)
    }

    #[cfg(not(unix))]
    fn enforce_owner_only_permissions(
        _path: &std::path::Path,
        _file_mode: bool,
    ) -> Result<(), sqlx::Error> {
        Ok(())
    }

    fn is_remote_postgres_host(host: &str) -> bool {
        let host_lower = host.to_ascii_lowercase();
        if host_lower.is_empty() {
            return false;
        }
        if host_lower == "localhost" {
            return false;
        }
        if let Ok(ip) = host_lower.parse::<std::net::IpAddr>() {
            return !ip.is_loopback();
        }
        true
    }

    fn validate_postgres_tls(database_url: &str) -> Result<(), sqlx::Error> {
        if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
            return Ok(());
        }

        let opts = sqlx::postgres::PgConnectOptions::from_str(database_url)
            .map_err(|e| sqlx::Error::Configuration(Box::new(e)))?;

        if Self::is_remote_postgres_host(opts.get_host())
            && matches!(
                opts.get_ssl_mode(),
                sqlx::postgres::PgSslMode::Disable
                    | sqlx::postgres::PgSslMode::Allow
                    | sqlx::postgres::PgSslMode::Prefer
            )
        {
            return Err(sqlx::Error::Configuration(
                "remote Postgres connections must set sslmode=require, verify-ca, or verify-full"
                    .into(),
            ));
        }

        Ok(())
    }

    /// Connect to database.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        if database_url.starts_with("sqlite") {
            let sqlite_path = Self::sqlite_path_from_url(database_url);

            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .acquire_timeout(Duration::from_secs(5))
                .connect(database_url)
                .await?;

            if let Some(path) = sqlite_path {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        Self::enforce_owner_only_permissions(parent, false)?;
                    }
                }
                if path.exists() {
                    Self::enforce_owner_only_permissions(&path, true)?;
                }
            }

            Ok(DbPool::Sqlite(pool))
        } else {
            Self::validate_postgres_tls(database_url)?;
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(Duration::from_secs(5))
                .connect(database_url)
                .await?;

            Ok(DbPool::Postgres(pool))
        }
    }

    /// Ping the database.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        match self {
            DbPool::Sqlite(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
            DbPool::Postgres(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
        }
        Ok(())
    }

    /// Run migrations.
    pub async fn run_migrations(&self) -> Result<(), sqlx::Error> {
        // For now, just ensure tables exist
        // Full migrations would use sqlx migrations
        match self {
            DbPool::Sqlite(pool) => {
                // Create tables for SQLite
                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS providers (
                        id TEXT PRIMARY KEY,
                        name TEXT NOT NULL,
                        base_url TEXT NOT NULL,
                        auth_mode TEXT NOT NULL,
                        default_wire_api TEXT NOT NULL,
                        state_scope TEXT,
                        config_json TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS routes (
                        id TEXT PRIMARY KEY,
                        downstream_model TEXT NOT NULL UNIQUE,
                        description TEXT,
                        enabled INTEGER NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS route_targets (
                        id TEXT PRIMARY KEY,
                        route_id TEXT NOT NULL,
                        provider_id TEXT NOT NULL,
                        upstream_model TEXT NOT NULL,
                        wire_api TEXT NOT NULL,
                        priority INTEGER NOT NULL,
                        enabled INTEGER NOT NULL,
                        config_json TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        FOREIGN KEY (route_id) REFERENCES routes(id),
                        FOREIGN KEY (provider_id) REFERENCES providers(id)
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS responses (
                        id TEXT PRIMARY KEY,
                        request_id TEXT NOT NULL,
                        downstream_model TEXT NOT NULL,
                        route_id TEXT,
                        target_id TEXT,
                        provider_id TEXT,
                        upstream_model TEXT,
                        wire_api TEXT,
                        upstream_response_id TEXT,
                        state_scope TEXT,
                        previous_response_id TEXT,
                        status TEXT NOT NULL,
                        usage_json TEXT,
                        error_json TEXT,
                        created_at TEXT NOT NULL,
                        completed_at TEXT
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS response_items (
                        id TEXT PRIMARY KEY,
                        response_id TEXT NOT NULL,
                        sequence INTEGER NOT NULL,
                        item_type TEXT NOT NULL,
                        role TEXT,
                        call_id TEXT,
                        content_json TEXT NOT NULL,
                        visible INTEGER NOT NULL,
                        created_at TEXT NOT NULL,
                        FOREIGN KEY (response_id) REFERENCES responses(id)
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS upstream_handles (
                        id TEXT PRIMARY KEY,
                        modelwire_response_id TEXT NOT NULL,
                        provider_id TEXT NOT NULL,
                        credential_hash TEXT NOT NULL,
                        upstream_model TEXT NOT NULL,
                        wire_api TEXT NOT NULL,
                        state_scope TEXT,
                        upstream_response_id TEXT,
                        handle_json TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        FOREIGN KEY (modelwire_response_id) REFERENCES responses(id)
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS request_logs (
                        id TEXT PRIMARY KEY,
                        request_id TEXT NOT NULL,
                        downstream_key_hash TEXT,
                        downstream_model TEXT,
                        route_id TEXT,
                        target_id TEXT,
                        provider_id TEXT,
                        upstream_model TEXT,
                        wire_api TEXT,
                        status_code INTEGER,
                        error_kind TEXT,
                        latency_ms INTEGER,
                        input_tokens INTEGER,
                        output_tokens INTEGER,
                        reasoning_tokens INTEGER,
                        created_at TEXT NOT NULL
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS compaction_lineage (
                        id TEXT PRIMARY KEY,
                        request_id TEXT NOT NULL,
                        route_id TEXT,
                        downstream_model TEXT NOT NULL,
                        source_response_ids_json TEXT NOT NULL,
                        provider_id TEXT,
                        upstream_model TEXT,
                        state_scope TEXT,
                        method TEXT NOT NULL,
                        provider_native INTEGER NOT NULL,
                        summarizer_model TEXT,
                        prompt_version TEXT,
                        source_tokens INTEGER,
                        summary_tokens INTEGER,
                        created_at TEXT NOT NULL
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS probe_results (
                        id TEXT PRIMARY KEY,
                        provider_id TEXT NOT NULL,
                        credential_hash TEXT NOT NULL,
                        upstream_model TEXT NOT NULL,
                        wire_api TEXT NOT NULL,
                        supports_streaming INTEGER,
                        supports_tools INTEGER,
                        supports_parallel_tool_calls INTEGER,
                        supports_previous_response_id INTEGER,
                        supports_reasoning_encrypted_content INTEGER,
                        supports_reasoning_summary INTEGER,
                        status TEXT NOT NULL,
                        failure_kind TEXT,
                        failure_message_redacted TEXT,
                        last_success_at TEXT,
                        last_failure_at TEXT,
                        created_at TEXT,
                        expires_at TEXT NOT NULL,
                        UNIQUE(provider_id, credential_hash, upstream_model)
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                info!("SQLite schema initialized");
            }
            DbPool::Postgres(pool) => {
                // Create tables for Postgres
                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS providers (
                        id TEXT PRIMARY KEY,
                        name TEXT NOT NULL,
                        base_url TEXT NOT NULL,
                        auth_mode TEXT NOT NULL,
                        default_wire_api TEXT NOT NULL,
                        state_scope TEXT,
                        config_json TEXT NOT NULL,
                        created_at TIMESTAMPTZ NOT NULL,
                        updated_at TIMESTAMPTZ NOT NULL
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS compaction_lineage (
                        id TEXT PRIMARY KEY,
                        request_id TEXT NOT NULL,
                        route_id TEXT,
                        downstream_model TEXT NOT NULL,
                        source_response_ids_json JSONB NOT NULL,
                        provider_id TEXT,
                        upstream_model TEXT,
                        state_scope TEXT,
                        method TEXT NOT NULL,
                        provider_native BOOLEAN NOT NULL DEFAULT FALSE,
                        summarizer_model TEXT,
                        prompt_version TEXT,
                        source_tokens BIGINT,
                        summary_tokens BIGINT,
                        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                    )
                    "#,
                )
                .execute(pool)
                .await?;

                // ... more Postgres tables
                info!("Postgres schema initialized");
            }
        }

        Ok(())
    }

    /// Store a response (placeholder for response repo).
    pub async fn store_response(&self, _response: &serde_json::Value) -> Result<(), sqlx::Error> {
        // Placeholder - full implementation would use response repository
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires database
    async fn test_database_connect() {
        let db = DbPool::connect("sqlite::memory:").await;
        assert!(db.is_ok());
    }
}
