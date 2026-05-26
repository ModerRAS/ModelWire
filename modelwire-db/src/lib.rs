//! ModelWire Database
//!
//! Database layer with SQLite/Postgres support.

pub mod repo;
pub mod schema;

use crate::schema::{POSTGRES_SCHEMA, SQLITE_SCHEMA};
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
        match self {
            DbPool::Sqlite(pool) => {
                for statement in split_sql_statements(SQLITE_SCHEMA) {
                    sqlx::query(statement).execute(pool).await?;
                }
                info!("SQLite schema initialized");
            }
            DbPool::Postgres(pool) => {
                for statement in split_sql_statements(POSTGRES_SCHEMA) {
                    sqlx::query(statement).execute(pool).await?;
                }
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

fn split_sql_statements(sql: &str) -> Vec<&str> {
    sql.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .collect()
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
