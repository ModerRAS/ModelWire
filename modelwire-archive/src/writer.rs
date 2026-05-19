//! Archive writer for conversation records.

use super::{ArchiveError, ArchiveFile, ArchiveManifest};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::info;

/// Archive writer for creating conversation archives.
pub struct ArchiveWriter {
    root: String,
    capture_mode: super::manifest::CaptureMode,
    current_segment: Option<SegmentWriter>,
    manifest: ArchiveManifest,
}

struct SegmentWriter {
    temp_path: String,
    final_path: String,
    file: Option<tokio::fs::File>,
    conversation_count: usize,
    item_count: usize,
    bytes_written: usize,
}

impl ArchiveWriter {
    /// Create a new archive writer.
    pub async fn new(
        root: String,
        capture_mode: super::manifest::CaptureMode,
    ) -> Result<Self, ArchiveError> {
        let archive_id = format!("arch_{}", uuid::Uuid::now_v7());

        // Ensure root directory exists
        fs::create_dir_all(&root).await?;

        // Create archive directory
        let archive_dir = format!("{}/{}", root, archive_id);
        fs::create_dir_all(&archive_dir).await?;

        let manifest = ArchiveManifest::new(archive_id.clone(), capture_mode);

        Ok(Self {
            root,
            capture_mode,
            current_segment: None,
            manifest,
        })
    }

    /// Write a conversation record.
    pub async fn write_conversation(
        &mut self,
        record: &ConversationRecord,
    ) -> Result<(), ArchiveError> {
        if self.capture_mode == super::manifest::CaptureMode::Off {
            return Ok(());
        }

        // Ensure we have an active segment
        if self.current_segment.is_none() {
            self.start_new_segment().await?;
        }

        // Serialize record
        let json =
            serde_json::to_string(record).map_err(|e| ArchiveError::IoError(e.to_string()))?;

        // Write to segment
        if let Some(ref mut segment) = self.current_segment {
            segment
                .file
                .as_mut()
                .unwrap()
                .write_all(json.as_bytes())
                .await?;
            segment.file.as_mut().unwrap().write_all(b"\n").await?;
            segment.conversation_count += 1;
            segment.item_count += record.messages.len();
            segment.bytes_written += json.len() + 1;
        }

        Ok(())
    }

    /// Start a new segment file.
    async fn start_new_segment(&mut self) -> Result<(), ArchiveError> {
        let segment_index = self.manifest.files.len() + 1;
        let segment_name = format!("conversations-{:06}.jsonl.zst", segment_index);
        let temp_name = format!("conversations-{:06}.jsonl.tmp", segment_index);
        let final_path = format!("{}/{}", self.manifest.archive_id, segment_name);
        let temp_path = format!("{}/{}", self.manifest.archive_id, temp_name);
        validate_archive_relative_path(&final_path)?;
        validate_archive_relative_path(&temp_path)?;

        let file = fs::File::create(format!("{}/{}", self.root, temp_path)).await?;

        self.current_segment = Some(SegmentWriter {
            temp_path,
            final_path,
            file: Some(file),
            conversation_count: 0,
            item_count: 0,
            bytes_written: 0,
        });

        Ok(())
    }

    /// Close the current segment and update manifest.
    pub async fn close_segment(&mut self) -> Result<(), ArchiveError> {
        if let Some(segment) = self.current_segment.take() {
            if let Some(mut file) = segment.file {
                file.flush().await?;
                drop(file);
            }

            // Compress completed temp JSONL to final zstd segment.
            let temp_fs_path = format!("{}/{}", self.root, segment.temp_path);
            let final_fs_path = format!("{}/{}", self.root, segment.final_path);
            let jsonl_bytes = fs::read(&temp_fs_path).await?;
            let compressed = zstd::stream::encode_all(&jsonl_bytes[..], 3)
                .map_err(|e| ArchiveError::IoError(e.to_string()))?;
            fs::write(&final_fs_path, &compressed).await?;
            // Remove temp segment once final compressed file is durable.
            let _ = fs::remove_file(&temp_fs_path).await;

            // Compute checksum from finalized compressed bytes.
            use sha2::Digest;
            let mut hasher = Sha256::new();
            hasher.update(&compressed);
            let result = hasher.finalize();
            let checksum = format!("{:x}", result);

            self.manifest.add_file(ArchiveFile {
                path: segment.final_path,
                format: "conversation_jsonl_zstd".to_string(),
                checksum,
                conversation_count: Some(segment.conversation_count),
                item_count: Some(segment.item_count),
            });

            // Persist manifest after each sealed segment so archives remain recoverable
            // even when the process keeps the writer open for future appends.
            let manifest_json = serde_json::to_string_pretty(&self.manifest)
                .map_err(|e| ArchiveError::IoError(e.to_string()))?;
            let manifest_path = format!("{}/{}/manifest.json", self.root, self.manifest.archive_id);
            fs::write(manifest_path, manifest_json).await?;
        }

        Ok(())
    }

    /// Finalize the archive.
    pub async fn finalize(mut self) -> Result<ArchiveManifest, ArchiveError> {
        self.close_segment().await?;

        // Write manifest
        let manifest_json = serde_json::to_string_pretty(&self.manifest)
            .map_err(|e| ArchiveError::IoError(e.to_string()))?;

        let manifest_path = format!("{}/manifest.json", self.manifest.archive_id);
        fs::write(format!("{}/{}", self.root, manifest_path), manifest_json).await?;

        info!(
            archive_id = %self.manifest.archive_id,
            files = self.manifest.files.len(),
            "Archive finalized"
        );

        Ok(self.manifest)
    }
}

fn validate_archive_relative_path(path: &str) -> Result<(), ArchiveError> {
    let trimmed = path.trim();
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains(':')
    {
        return Err(ArchiveError::PathTraversal(path.to_string()));
    }
    Ok(())
}

/// Conversation record for archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    /// Schema version.
    pub schema: String,

    /// Conversation ID.
    pub conversation_id: String,

    /// Root response ID.
    pub root_response_id: String,

    /// Creation timestamp.
    pub created_at: String,

    /// Capture mode.
    pub capture_mode: String,

    /// Request info.
    pub request: RequestInfo,

    /// Model info.
    pub models: ModelInfo,

    /// Routing info.
    pub routing: RoutingInfo,

    /// Messages.
    pub messages: Vec<MessageRecord>,

    /// Tools.
    pub tools: Vec<ToolRecord>,

    /// Usage.
    pub usage: UsageInfo,

    /// Quality metrics.
    pub quality: QualityInfo,

    /// Redaction status.
    pub redaction: RedactionStatus,

    /// Extra metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestInfo {
    pub request_id: String,
    pub response_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_attempt: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub downstream_model: String,
    pub upstream_model: String,
    pub provider_id: String,
    pub provider_name: String,
    pub provider_base_url_hash: String,
    pub provider_config_hash: String,
    pub state_scope: String,
    pub wire_api: String,
    pub detected_wire_api: String,
    pub upstream_response_id_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingInfo {
    pub had_fallback: bool,
    pub attempts: Vec<RoutingAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingAttempt {
    pub target_id: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub wire_api: String,
    pub status: String,
    pub error_kind: Option<String>,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub role: String,
    pub content: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRecord {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityInfo {
    pub user_rating: Option<f32>,
    pub had_error: bool,
    pub had_fallback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionStatus {
    pub status: String,
    pub policy: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CaptureMode;
    use chrono::Utc;

    #[tokio::test]
    async fn test_write_archive() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = ArchiveWriter::new(
            root.path().to_string_lossy().to_string(),
            CaptureMode::VisibleOnly,
        )
        .await
        .unwrap();

        let record = ConversationRecord {
            schema: "modelwire.conversation.v1".to_string(),
            conversation_id: "conv_test".to_string(),
            root_response_id: "resp_test".to_string(),
            created_at: Utc::now().to_rfc3339(),
            capture_mode: "visible_only".to_string(),
            request: RequestInfo {
                request_id: "req_test".to_string(),
                response_id: "resp_test".to_string(),
                previous_response_id: None,
                route_id: None,
                target_id: None,
                fallback_attempt: None,
            },
            models: ModelInfo {
                downstream_model: "gpt-4".to_string(),
                upstream_model: "gpt-4".to_string(),
                provider_id: "openai".to_string(),
                provider_name: "OpenAI".to_string(),
                provider_base_url_hash: "sha256:abc".to_string(),
                provider_config_hash: "sha256:def".to_string(),
                state_scope: "openai-main".to_string(),
                wire_api: "responses".to_string(),
                detected_wire_api: "responses".to_string(),
                upstream_response_id_hash: "sha256:ghi".to_string(),
            },
            routing: RoutingInfo {
                had_fallback: false,
                attempts: vec![],
            },
            messages: vec![MessageRecord {
                role: "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            tools: vec![],
            usage: UsageInfo {
                input_tokens: 10,
                output_tokens: 20,
                reasoning_tokens: 0,
            },
            quality: QualityInfo {
                user_rating: None,
                had_error: false,
                had_fallback: false,
            },
            redaction: RedactionStatus {
                status: "clean".to_string(),
                policy: "default".to_string(),
            },
            metadata: None,
        };

        writer.write_conversation(&record).await.unwrap();
        let manifest = writer.finalize().await.unwrap();

        assert_eq!(manifest.files.len(), 1);
        let first = &manifest.files[0];
        assert_eq!(first.format, "conversation_jsonl_zstd");
        let compressed_path = root.path().join(&first.path);
        assert!(
            compressed_path.exists(),
            "compressed archive segment should exist"
        );

        let compressed_bytes = std::fs::read(&compressed_path).unwrap();
        let decompressed = zstd::stream::decode_all(&compressed_bytes[..]).unwrap();
        let text = String::from_utf8(decompressed).unwrap();
        assert!(text.contains("\"conversation_id\":\"conv_test\""));
        assert!(text.contains("\"capture_mode\":\"visible_only\""));
    }

    #[test]
    fn validate_archive_relative_path_rejects_traversal() {
        assert!(validate_archive_relative_path("../escape").is_err());
        assert!(validate_archive_relative_path("C:\\bad\\path").is_err());
        assert!(validate_archive_relative_path("/absolute").is_err());
        assert!(validate_archive_relative_path("archives/ok/file.jsonl").is_ok());
    }
}
