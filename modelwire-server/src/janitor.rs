//! Janitor task for cleaning expired operational state.
//!
//! Handles cleanup of:
//! - Expired response chains (only if not referenced by non-expired children)
//! - Expired probe cache results
//! - Expired request logs
//! - Periodic SQLite VACUUM

use modelwire_db::DbPool;
use std::time::Duration;
use tracing::{info, warn};

/// Default state TTL: 1 day
const DEFAULT_STATE_TTL_SECS: u64 = 86400;

/// Default probe cache TTL: 1 hour
const DEFAULT_PROBE_TTL_SECS: u64 = 3600;

/// Default log TTL: 7 days
const DEFAULT_LOG_TTL_SECS: u64 = 604800;

/// Cleanup statistics report.
#[derive(Debug, Clone, Default)]
pub struct CleanupReport {
    /// Number of response chains deleted.
    pub responses_deleted: u64,
    /// Number of response items deleted.
    pub response_items_deleted: u64,
    /// Number of upstream handles deleted.
    pub handles_deleted: u64,
    /// Number of probe results deleted.
    pub probes_deleted: u64,
    /// Number of request logs deleted.
    pub logs_deleted: u64,
    /// Whether vacuum was performed.
    pub vacuum_performed: bool,
}

/// Janitor for cleaning expired operational state.
pub struct Janitor {
    db: DbPool,
    state_ttl: Duration,
    probe_ttl: Duration,
    log_ttl: Duration,
    vacuum_interval: Duration,
    last_vacuum: std::sync::Mutex<std::time::Instant>,
}

impl Janitor {
    /// Build a SQLite placeholder list like "?,?,?" for `IN` clauses.
    fn sqlite_in_placeholders(count: usize) -> String {
        std::iter::repeat("?")
            .take(count)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Delete rows from a SQLite table using a fully parameterized `IN` clause.
    async fn sqlite_delete_by_ids(
        pool: &sqlx::SqlitePool,
        table: &str,
        column: &str,
        ids: &[String],
    ) -> Result<u64, sqlx::Error> {
        if ids.is_empty() {
            return Ok(0);
        }

        let placeholders = Self::sqlite_in_placeholders(ids.len());
        let sql = format!("DELETE FROM {table} WHERE {column} IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id);
        }

        let result = query.execute(pool).await?;
        Ok(result.rows_affected())
    }

    /// Create a new janitor with default TTLs.
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            state_ttl: Duration::from_secs(DEFAULT_STATE_TTL_SECS),
            probe_ttl: Duration::from_secs(DEFAULT_PROBE_TTL_SECS),
            log_ttl: Duration::from_secs(DEFAULT_LOG_TTL_SECS),
            vacuum_interval: Duration::from_secs(3600), // Every hour
            last_vacuum: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    /// Create a janitor with custom TTLs.
    pub fn with_ttls(
        db: DbPool,
        state_ttl: Duration,
        probe_ttl: Duration,
        log_ttl: Duration,
    ) -> Self {
        Self {
            db,
            state_ttl,
            probe_ttl,
            log_ttl,
            vacuum_interval: Duration::from_secs(3600),
            last_vacuum: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    /// Run a full cleanup cycle.
    pub async fn run_cleanup(&self) -> Result<CleanupReport, JanitorError> {
        let mut report = CleanupReport::default();

        // Clean expired probe results first (lowest risk)
        match self.cleanup_expired_probes().await {
            Ok(count) => {
                report.probes_deleted = count;
                info!(count = count, "Cleaned expired probe results");
            }
            Err(e) => {
                warn!(error = %e, "Failed to clean expired probes");
            }
        }

        // Clean expired request logs
        match self.cleanup_expired_logs().await {
            Ok(count) => {
                report.logs_deleted = count;
                info!(count = count, "Cleaned expired request logs");
            }
            Err(e) => {
                warn!(error = %e, "Failed to clean expired logs");
            }
        }

        // Clean expired response chains (most complex - must respect chain integrity)
        match self.cleanup_expired_responses().await {
            Ok((responses, items, handles)) => {
                report.responses_deleted = responses;
                report.response_items_deleted = items;
                report.handles_deleted = handles;
                info!(
                    responses = responses,
                    items = items,
                    handles = handles,
                    "Cleaned expired response chains"
                );
            }
            Err(e) => {
                warn!(error = %e, "Failed to clean expired responses");
            }
        }

        // Vacuum SQLite if needed
        if self.should_vacuum() {
            match self.vacuum_sqlite().await {
                Ok(()) => {
                    report.vacuum_performed = true;
                    info!("Performed SQLite VACUUM");
                }
                Err(e) => {
                    warn!(error = %e, "Failed to vacuum SQLite");
                }
            }
        }

        Ok(report)
    }

    /// Clean expired probe cache entries.
    async fn cleanup_expired_probes(&self) -> Result<u64, JanitorError> {
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::seconds(self.probe_ttl.as_secs() as i64);

        match &self.db {
            DbPool::Sqlite(pool) => {
                let result = sqlx::query("DELETE FROM probe_results WHERE expires_at < ?")
                    .bind(cutoff.to_rfc3339())
                    .execute(pool)
                    .await?;
                Ok(result.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let result = sqlx::query("DELETE FROM probe_results WHERE expires_at < $1")
                    .bind(cutoff)
                    .execute(pool)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// Clean expired request logs.
    async fn cleanup_expired_logs(&self) -> Result<u64, JanitorError> {
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::seconds(self.log_ttl.as_secs() as i64);

        match &self.db {
            DbPool::Sqlite(pool) => {
                let result = sqlx::query("DELETE FROM request_logs WHERE created_at < ?")
                    .bind(cutoff.to_rfc3339())
                    .execute(pool)
                    .await?;
                Ok(result.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let result = sqlx::query("DELETE FROM request_logs WHERE created_at < $1")
                    .bind(cutoff)
                    .execute(pool)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// Clean expired response chains, respecting chain integrity.
    ///
    /// A response is only deleted if:
    /// 1. Its `completed_at` is older than the state TTL
    /// 2. No non-expired response has `previous_response_id` pointing to it
    async fn cleanup_expired_responses(&self) -> Result<(u64, u64, u64), JanitorError> {
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::seconds(self.state_ttl.as_secs() as i64);

        // Collect response IDs to delete, filtering out those referenced by non-expired responses
        let expired_responses = self.find_expired_response_ids(cutoff).await?;

        if expired_responses.is_empty() {
            return Ok((0, 0, 0));
        }

        // Delete in cascading order: items -> handles -> responses
        // (items and handles have ON DELETE CASCADE but we count explicitly for reporting)

        let mut response_items_deleted = 0u64;
        let mut handles_deleted = 0u64;
        let mut responses_deleted = 0u64;

        match &self.db {
            DbPool::Sqlite(pool) => {
                // Count and delete response items (directly by response_id)
                let items_result = Self::sqlite_delete_by_ids(
                    pool,
                    "response_items",
                    "response_id",
                    &expired_responses,
                )
                .await;

                if let Ok(r) = items_result {
                    response_items_deleted = r;
                }

                // Delete upstream handles (directly by modelwire_response_id)
                let handles_result = Self::sqlite_delete_by_ids(
                    pool,
                    "upstream_handles",
                    "modelwire_response_id",
                    &expired_responses,
                )
                .await;

                if let Ok(r) = handles_result {
                    handles_deleted = r;
                }

                // Delete response records
                let responses_result =
                    Self::sqlite_delete_by_ids(pool, "responses", "id", &expired_responses).await;

                if let Ok(r) = responses_result {
                    responses_deleted = r;
                }
            }
            DbPool::Postgres(pool) => {
                // For Postgres, use array contains operator
                let response_ids: Vec<&str> =
                    expired_responses.iter().map(|s| s.as_str()).collect();

                // Count and delete response items
                let items_result =
                    sqlx::query("DELETE FROM response_items WHERE response_id = ANY($1)")
                        .bind(&response_ids)
                        .execute(pool)
                        .await;

                if let Ok(r) = items_result {
                    response_items_deleted = r.rows_affected();
                }

                // Delete upstream handles
                let handles_result = sqlx::query(
                    "DELETE FROM upstream_handles WHERE modelwire_response_id = ANY($1)",
                )
                .bind(&response_ids)
                .execute(pool)
                .await;

                if let Ok(r) = handles_result {
                    handles_deleted = r.rows_affected();
                }

                // Delete response records
                let responses_result = sqlx::query("DELETE FROM responses WHERE id = ANY($1)")
                    .bind(&response_ids)
                    .execute(pool)
                    .await;

                if let Ok(r) = responses_result {
                    responses_deleted = r.rows_affected();
                }
            }
        }

        Ok((responses_deleted, response_items_deleted, handles_deleted))
    }

    /// Find expired response IDs, excluding those referenced by non-expired responses.
    async fn find_expired_response_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>, JanitorError> {
        // First, collect all expired response IDs
        // Then, find which ones are referenced by non-expired responses as previous_response_id
        // Finally, return only the truly deletable ones

        #[derive(sqlx::FromRow)]
        struct ExpiredResponse {
            id: String,
        }

        #[derive(sqlx::FromRow)]
        struct ReferencingResponse {
            id: String,
            previous_response_id: Option<String>,
        }

        match &self.db {
            DbPool::Sqlite(pool) => {
                // Get all expired responses
                let expired: Vec<ExpiredResponse> = sqlx::query_as(
                    "SELECT id FROM responses WHERE completed_at IS NOT NULL AND completed_at < ?",
                )
                .bind(cutoff.to_rfc3339())
                .fetch_all(pool)
                .await?;

                if expired.is_empty() {
                    return Ok(Vec::new());
                }

                let expired_ids: Vec<String> = expired.iter().map(|r| r.id.clone()).collect();
                let placeholders = Self::sqlite_in_placeholders(expired_ids.len());

                // Find all responses that reference any expired response
                // We need to check if these referencing responses themselves are non-expired
                let sql = format!(
                    "SELECT id, previous_response_id FROM responses WHERE previous_response_id IN ({placeholders})",
                );
                let mut query = sqlx::query_as::<_, ReferencingResponse>(&sql);
                for id in &expired_ids {
                    query = query.bind(id);
                }
                let referencing_responses: Vec<ReferencingResponse> =
                    query.fetch_all(pool).await.unwrap_or_default();

                // Collect IDs of expired responses that ARE referenced by non-expired responses
                let mut referenced_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();

                for ref_resp in referencing_responses {
                    if let Some(prev_id) = ref_resp.previous_response_id {
                        // Check if this referencing response is non-expired
                        let is_non_expired: Option<(String,)> = sqlx::query_as(
                            "SELECT id FROM responses WHERE id = ? AND (completed_at IS NULL OR completed_at >= ?)",
                        )
                        .bind(&ref_resp.id)
                        .bind(cutoff.to_rfc3339())
                        .fetch_optional(pool)
                        .await
                        .unwrap_or(None);

                        if is_non_expired.is_some() {
                            referenced_ids.insert(prev_id);
                        }
                    }
                }

                // Return expired IDs that are NOT referenced by non-expired responses
                Ok(expired_ids
                    .into_iter()
                    .filter(|id| !referenced_ids.contains(id))
                    .collect())
            }
            DbPool::Postgres(pool) => {
                // Get all expired responses
                let expired: Vec<ExpiredResponse> = sqlx::query_as(
                    "SELECT id FROM responses WHERE completed_at IS NOT NULL AND completed_at < $1",
                )
                .bind(cutoff)
                .fetch_all(pool)
                .await?;

                if expired.is_empty() {
                    return Ok(Vec::new());
                }

                let expired_ids: Vec<String> = expired.iter().map(|r| r.id.clone()).collect();
                let expired_ids_ref: Vec<&str> = expired_ids.iter().map(|s| s.as_str()).collect();

                // Find all responses that reference any expired response
                let referencing_responses: Vec<ReferencingResponse> = sqlx::query_as(
                    "SELECT id, previous_response_id FROM responses WHERE previous_response_id = ANY($1)",
                )
                .bind(&expired_ids_ref)
                .fetch_all(pool)
                .await
                .unwrap_or_default();

                // Collect IDs of expired responses that ARE referenced by non-expired responses
                let mut referenced_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();

                for ref_resp in referencing_responses {
                    if let Some(prev_id) = ref_resp.previous_response_id {
                        // Check if this referencing response is non-expired
                        let is_non_expired: Option<(String,)> = sqlx::query_as(
                            "SELECT id FROM responses WHERE id = $1 AND (completed_at IS NULL OR completed_at >= $2)",
                        )
                        .bind(&ref_resp.id)
                        .bind(cutoff)
                        .fetch_optional(pool)
                        .await
                        .unwrap_or(None);

                        if is_non_expired.is_some() {
                            referenced_ids.insert(prev_id);
                        }
                    }
                }

                // Return expired IDs that are NOT referenced by non-expired responses
                Ok(expired_ids
                    .into_iter()
                    .filter(|id| !referenced_ids.contains(id))
                    .collect())
            }
        }
    }

    /// Check if vacuum should be performed.
    fn should_vacuum(&self) -> bool {
        let last = *self.last_vacuum.lock().unwrap();
        last.elapsed() >= self.vacuum_interval
    }

    /// Perform SQLite VACUUM.
    async fn vacuum_sqlite(&self) -> Result<(), JanitorError> {
        match &self.db {
            DbPool::Sqlite(pool) => {
                sqlx::query("VACUUM").execute(pool).await?;
                // Reset vacuum timer
                *self.last_vacuum.lock().unwrap() = std::time::Instant::now();
                Ok(())
            }
            DbPool::Postgres(_) => {
                // Postgres handles vacuuming via autovacuum, no manual VACUUM needed
                Ok(())
            }
        }
    }
}

/// Janitor error types.
#[derive(Debug, thiserror::Error)]
pub enum JanitorError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Run the janitor task on a schedule.
pub async fn run_janitor_periodically(db: modelwire_db::Database, interval: Duration) {
    let janitor = Janitor::new(db);
    let mut interval_timer = tokio::time::interval(interval);
    interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval_timer.tick().await;

        info!("Starting scheduled janitor cleanup");

        match janitor.run_cleanup().await {
            Ok(report) => {
                info!(
                    responses = report.responses_deleted,
                    items = report.response_items_deleted,
                    handles = report.handles_deleted,
                    probes = report.probes_deleted,
                    logs = report.logs_deleted,
                    vacuum = report.vacuum_performed,
                    "Janitor cleanup completed"
                );
            }
            Err(e) => {
                warn!(error = %e, "Janitor cleanup failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn janitor_deletes_expired_state() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        // Use 1 hour TTL - response is 2 hours old so it should be expired
        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(3600), // 1 hour
            Duration::from_secs(0),
            Duration::from_secs(0),
        );

        // Insert an expired response (completed 2 hours ago)
        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();

        match &db {
            DbPool::Sqlite(pool) => {
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind("expired_resp_1")
                .bind("req_1")
                .bind("test-model")
                .bind("completed")
                .bind(&two_hours_ago)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();

                // Insert response items
                sqlx::query(
                    "INSERT INTO response_items (id, response_id, sequence, item_type, content_json, visible, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind("item_1")
                .bind("expired_resp_1")
                .bind(0)
                .bind("message")
                .bind(r#"{"text":"hello"}"#)
                .bind(1)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        // Run cleanup
        let report = janitor.run_cleanup().await.unwrap();

        assert_eq!(report.responses_deleted, 1);
        assert_eq!(report.response_items_deleted, 1);
    }

    #[tokio::test]
    async fn janitor_keeps_referenced_chain() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        // Use a 1-hour TTL to expire 2-hours-old parent
        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(3600), // 1 hour - parent is 2 hours old, child is now
            Duration::from_secs(0),
            Duration::from_secs(0),
        );

        // Insert parent response (completed 2 hours ago)
        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();

        match &db {
            DbPool::Sqlite(pool) => {
                // Parent response (would be expired)
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind("parent_resp")
                .bind("req_parent")
                .bind("test-model")
                .bind("completed")
                .bind(&two_hours_ago)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();

                // Child response (NOT expired, references parent via previous_response_id)
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, previous_response_id, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind("child_resp")
                .bind("req_child")
                .bind("test-model")
                .bind("completed")
                .bind("parent_resp") // References expired parent
                .bind(chrono::Utc::now().to_rfc3339()) // Now, so not expired
                .bind(chrono::Utc::now().to_rfc3339())
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        // Run cleanup
        let report = janitor.run_cleanup().await.unwrap();

        // Parent should NOT be deleted because child references it
        assert_eq!(report.responses_deleted, 0);

        // Verify parent still exists
        match &db {
            DbPool::Sqlite(pool) => {
                let count: (i64,) =
                    sqlx::query_as("SELECT COUNT(*) FROM responses WHERE id = 'parent_resp'")
                        .fetch_one(pool)
                        .await
                        .unwrap();
                assert_eq!(count.0, 1, "Parent response should still exist");
            }
            DbPool::Postgres(_) => unreachable!(),
        }
    }

    #[tokio::test]
    async fn janitor_deletes_orphaned_parent() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(3600), // 1 hour TTL
            Duration::from_secs(0),
            Duration::from_secs(0),
        );

        // Insert parent response (created 2 hours ago, should be expired)
        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();

        match &db {
            DbPool::Sqlite(pool) => {
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind("orphan_parent")
                .bind("req_orphan")
                .bind("test-model")
                .bind("completed")
                .bind(&two_hours_ago)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();

                // Child response (ALSO expired, so no one references the parent)
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, previous_response_id, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind("orphan_child")
                .bind("req_orphan_child")
                .bind("test-model")
                .bind("completed")
                .bind("orphan_parent")
                .bind(&two_hours_ago)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        // Run cleanup
        let report = janitor.run_cleanup().await.unwrap();

        // Both should be deleted since child is also expired (no one references parent anymore)
        assert_eq!(report.responses_deleted, 2);
    }

    #[tokio::test]
    async fn janitor_cleans_expired_probes() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(86400),
            Duration::from_secs(0), // Expire immediately
            Duration::from_secs(86400),
        );

        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();

        match &db {
            DbPool::Sqlite(pool) => {
                sqlx::query(
                    "INSERT INTO probe_results (id, provider_id, credential_hash, upstream_model, wire_api, status, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind("probe_1")
                .bind("provider-a")
                .bind("hash123")
                .bind("model-x")
                .bind("responses")
                .bind("success")
                .bind(&two_hours_ago)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        let report = janitor.run_cleanup().await.unwrap();

        assert_eq!(report.probes_deleted, 1);
    }

    #[tokio::test]
    async fn janitor_cleans_expired_logs() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(86400),
            Duration::from_secs(86400),
            Duration::from_secs(0), // Expire immediately
        );

        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();

        match &db {
            DbPool::Sqlite(pool) => {
                sqlx::query(
                    "INSERT INTO request_logs (id, request_id, created_at) VALUES (?, ?, ?)",
                )
                .bind("log_1")
                .bind("req_old")
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        let report = janitor.run_cleanup().await.unwrap();

        assert_eq!(report.logs_deleted, 1);
    }

    #[tokio::test]
    async fn janitor_skips_nonexpired_responses() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        // Use a 24-hour TTL so responses from "2 hours ago" aren't expired
        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(86400), // 24 hours - not 0
            Duration::from_secs(0),
            Duration::from_secs(0),
        );

        let now = chrono::Utc::now().to_rfc3339();

        match &db {
            DbPool::Sqlite(pool) => {
                // Response with no completed_at (still in progress) - should not be deleted
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at) VALUES (?, ?, ?, ?, ?)",
                )
                .bind("in_progress_resp")
                .bind("req_ip")
                .bind("test-model")
                .bind("in_progress")
                .bind(&now)
                .execute(pool)
                .await
                .unwrap();

                // Response with recent completed_at - should not be deleted
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind("recent_resp")
                .bind("req_recent")
                .bind("test-model")
                .bind("completed")
                .bind(&now)
                .bind(&now)
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        let report = janitor.run_cleanup().await.unwrap();

        assert_eq!(report.responses_deleted, 0);

        // Verify both still exist
        match &db {
            DbPool::Sqlite(pool) => {
                let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM responses")
                    .fetch_one(pool)
                    .await
                    .unwrap();
                assert_eq!(count.0, 2, "Both non-expired responses should still exist");
            }
            DbPool::Postgres(_) => unreachable!(),
        }
    }

    #[tokio::test]
    async fn janitor_sqlite_parameterized_ids_handle_quotes_safely() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        let janitor = Janitor::with_ttls(
            db.clone(),
            Duration::from_secs(3600), // 1 hour
            Duration::from_secs(86400),
            Duration::from_secs(86400),
        );

        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let now = chrono::Utc::now().to_rfc3339();
        let quoted_id = "resp_with_quote_'_and_tokens";

        match &db {
            DbPool::Sqlite(pool) => {
                // Expired response with quote characters in ID should still be deleted safely.
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(quoted_id)
                .bind("req_quoted")
                .bind("test-model")
                .bind("completed")
                .bind(&two_hours_ago)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();

                sqlx::query(
                    "INSERT INTO response_items (id, response_id, sequence, item_type, content_json, visible, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind("item_quoted")
                .bind(quoted_id)
                .bind(0)
                .bind("message")
                .bind(r#"{"text":"safe"}"#)
                .bind(1)
                .bind(&two_hours_ago)
                .execute(pool)
                .await
                .unwrap();

                // Control response should remain untouched.
                sqlx::query(
                    "INSERT INTO responses (id, request_id, downstream_model, status, created_at, completed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind("recent_control")
                .bind("req_control")
                .bind("test-model")
                .bind("completed")
                .bind(&now)
                .bind(&now)
                .execute(pool)
                .await
                .unwrap();
            }
            DbPool::Postgres(_) => unreachable!(),
        }

        let report = janitor.run_cleanup().await.unwrap();
        assert_eq!(report.responses_deleted, 1);
        assert_eq!(report.response_items_deleted, 1);

        match &db {
            DbPool::Sqlite(pool) => {
                let quoted_exists: (i64,) =
                    sqlx::query_as("SELECT COUNT(*) FROM responses WHERE id = ?")
                        .bind(quoted_id)
                        .fetch_one(pool)
                        .await
                        .unwrap();
                assert_eq!(
                    quoted_exists.0, 0,
                    "Quoted expired response should be deleted"
                );

                let control_exists: (i64,) =
                    sqlx::query_as("SELECT COUNT(*) FROM responses WHERE id = 'recent_control'")
                        .fetch_one(pool)
                        .await
                        .unwrap();
                assert_eq!(control_exists.0, 1, "Recent control response should remain");
            }
            DbPool::Postgres(_) => unreachable!(),
        }
    }
}
