//! Archive files metadata repository.

use crate::DbPool;

/// Upsert archive file metadata row.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_archive_file(
    db: &DbPool,
    id: &str,
    archive_id: &str,
    format: &str,
    path: &str,
    byte_size: Option<i64>,
    conversation_count: Option<i64>,
    item_count: Option<i64>,
    checksum: Option<&str>,
    manifest_json: &str,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();

    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO archive_files (
                    id, archive_id, format, path, byte_size, conversation_count,
                    item_count, checksum, manifest_json, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(path) DO UPDATE SET
                    archive_id = excluded.archive_id,
                    format = excluded.format,
                    byte_size = excluded.byte_size,
                    conversation_count = excluded.conversation_count,
                    item_count = excluded.item_count,
                    checksum = excluded.checksum,
                    manifest_json = excluded.manifest_json
                "#,
            )
            .bind(id)
            .bind(archive_id)
            .bind(format)
            .bind(path)
            .bind(byte_size)
            .bind(conversation_count)
            .bind(item_count)
            .bind(checksum)
            .bind(manifest_json)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        DbPool::Postgres(pool) => {
            sqlx::query(
                r#"
                INSERT INTO archive_files (
                    id, archive_id, format, path, byte_size, conversation_count,
                    item_count, checksum, manifest_json, created_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, NOW())
                ON CONFLICT(path) DO UPDATE SET
                    archive_id = excluded.archive_id,
                    format = excluded.format,
                    byte_size = excluded.byte_size,
                    conversation_count = excluded.conversation_count,
                    item_count = excluded.item_count,
                    checksum = excluded.checksum,
                    manifest_json = excluded.manifest_json
                "#,
            )
            .bind(id)
            .bind(archive_id)
            .bind(format)
            .bind(path)
            .bind(byte_size)
            .bind(conversation_count)
            .bind(item_count)
            .bind(checksum)
            .bind(manifest_json)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}
