//! Archive manifest management.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Archive manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifest {
    /// Schema version.
    pub schema: String,

    /// Archive ID.
    pub archive_id: String,

    /// Creation timestamp.
    pub created_at: DateTime<Utc>,

    /// Capture mode.
    pub capture_mode: CaptureMode,

    /// Redaction policy.
    pub redaction_policy: String,

    /// Source identifier.
    pub source: String,

    /// Lineage policy.
    pub lineage_policy: String,

    /// Files in this archive.
    pub files: Vec<ArchiveFile>,
}

/// Capture modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// No archive.
    #[default]
    Off,
    /// Metadata only.
    MetadataOnly,
    /// Visible text only.
    VisibleOnly,
    /// Full visible content.
    FullVisible,
    /// Debug raw mode.
    DebugRaw,
}

impl CaptureMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaptureMode::Off => "off",
            CaptureMode::MetadataOnly => "metadata_only",
            CaptureMode::VisibleOnly => "visible_only",
            CaptureMode::FullVisible => "full_visible",
            CaptureMode::DebugRaw => "debug_raw",
        }
    }
}

/// Archive file metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveFile {
    /// Relative path.
    pub path: String,

    /// File format.
    pub format: String,

    /// SHA256 checksum.
    pub checksum: String,

    /// Conversation count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_count: Option<usize>,

    /// Item count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_count: Option<usize>,
}

impl ArchiveManifest {
    /// Create a new manifest.
    pub fn new(archive_id: String, capture_mode: CaptureMode) -> Self {
        Self {
            schema: "modelwire.archive.v1".to_string(),
            archive_id,
            created_at: Utc::now(),
            capture_mode,
            redaction_policy: "default".to_string(),
            source: "modelwire".to_string(),
            lineage_policy: "full_upstream_metadata".to_string(),
            files: Vec::new(),
        }
    }

    /// Add a file to the manifest.
    pub fn add_file(&mut self, file: ArchiveFile) {
        self.files.push(file);
    }

    /// Validate the manifest.
    pub fn validate(&self) -> Result<(), ArchiveError> {
        if self.schema != "modelwire.archive.v1" {
            return Err(ArchiveError::InvalidSchema(self.schema.clone()));
        }
        if self.files.is_empty() {
            return Err(ArchiveError::EmptyArchive);
        }
        Ok(())
    }
}

/// Rebuilt archive index item from existing archive files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuiltArchiveIndexEntry {
    /// Archive directory ID.
    pub archive_id: String,
    /// Capture mode from manifest.
    pub capture_mode: CaptureMode,
    /// Number of files listed in manifest.
    pub file_count: usize,
    /// Number of files physically present and checksum-validated.
    pub validated_file_count: usize,
}

/// Scan archive root and rebuild archive index metadata from manifests/files.
///
/// This is intentionally filesystem-first so optional DB indexes can be reconstructed
/// without operational SQL rows.
pub fn rebuild_archive_index_from_files(
    root: &std::path::Path,
) -> Result<Vec<RebuiltArchiveIndexEntry>, ArchiveError> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    fn collect_manifests(
        dir: &std::path::Path,
        out: &mut Vec<std::path::PathBuf>,
    ) -> Result<(), ArchiveError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect_manifests(&path, out)?;
            } else if path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name == "manifest.json")
                    .unwrap_or(false)
            {
                out.push(path);
            }
        }
        Ok(())
    }

    let mut rebuilt = Vec::new();
    let mut manifests = Vec::new();
    collect_manifests(root, &mut manifests)?;
    for manifest_path in manifests {
        let manifest_bytes = std::fs::read(&manifest_path)?;
        let manifest: ArchiveManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| ArchiveError::IoError(e.to_string()))?;
        manifest.validate()?;

        let mut validated = 0usize;
        for file in &manifest.files {
            let file_path = root.join(&file.path);
            if !file_path.is_file() {
                return Err(ArchiveError::IoError(format!(
                    "Archive file missing for manifest entry: {}",
                    file.path
                )));
            }

            let bytes = std::fs::read(&file_path)?;
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(&bytes);
            let checksum = format!("{:x}", hasher.finalize());
            if checksum != file.checksum {
                return Err(ArchiveError::ChecksumMismatch);
            }
            validated = validated.saturating_add(1);
        }

        rebuilt.push(RebuiltArchiveIndexEntry {
            archive_id: manifest.archive_id.clone(),
            capture_mode: manifest.capture_mode,
            file_count: manifest.files.len(),
            validated_file_count: validated,
        });
    }

    // Stable order for deterministic index rebuild output and tests.
    rebuilt.sort_by(|a, b| a.archive_id.cmp(&b.archive_id));
    Ok(rebuilt)
}

/// Archive error types.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("Invalid schema: {0}")]
    InvalidSchema(String),

    #[error("Empty archive")]
    EmptyArchive,

    #[error("Path traversal detected: {0}")]
    PathTraversal(String),

    #[error("Checksum mismatch")]
    ChecksumMismatch,

    #[error("IO error: {0}")]
    IoError(String),
}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::IoError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::{ConversationRecord, RequestInfo, RoutingInfo, ToolRecord, UsageInfo};
    use crate::writer::{MessageRecord, ModelInfo, QualityInfo, RedactionStatus, RoutingAttempt};

    #[test]
    fn test_capture_mode_as_str() {
        assert_eq!(CaptureMode::Off.as_str(), "off");
        assert_eq!(CaptureMode::MetadataOnly.as_str(), "metadata_only");
        assert_eq!(CaptureMode::VisibleOnly.as_str(), "visible_only");
    }

    #[test]
    fn test_manifest_new() {
        let manifest = ArchiveManifest::new("test_archive".to_string(), CaptureMode::VisibleOnly);
        assert_eq!(manifest.schema, "modelwire.archive.v1");
        assert_eq!(manifest.archive_id, "test_archive");
        assert_eq!(manifest.capture_mode, CaptureMode::VisibleOnly);
    }

    #[test]
    fn test_manifest_validate() {
        let mut manifest = ArchiveManifest::new("test".to_string(), CaptureMode::VisibleOnly);
        manifest.add_file(ArchiveFile {
            path: "test.jsonl".to_string(),
            format: "jsonl".to_string(),
            checksum: "abc123".to_string(),
            conversation_count: Some(10),
            item_count: Some(100),
        });
        assert!(manifest.validate().is_ok());
    }

    #[tokio::test]
    async fn rebuild_archive_index_from_files_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = crate::writer::ArchiveWriter::new(
            root.path().to_string_lossy().to_string(),
            CaptureMode::VisibleOnly,
        )
        .await
        .unwrap();

        let record = ConversationRecord {
            schema: "modelwire.conversation.v1".to_string(),
            conversation_id: "conv_idx_1".to_string(),
            root_response_id: "resp_idx_1".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            capture_mode: "visible_only".to_string(),
            request: RequestInfo {
                request_id: "req_idx_1".to_string(),
                response_id: "resp_idx_1".to_string(),
                previous_response_id: None,
                route_id: Some("route-1".to_string()),
                target_id: Some("target-1".to_string()),
                fallback_attempt: Some(0),
            },
            models: ModelInfo {
                downstream_model: "codex-main".to_string(),
                upstream_model: "gpt-upstream".to_string(),
                provider_id: "provider-a".to_string(),
                provider_name: "Provider A".to_string(),
                provider_base_url_hash: "sha256:base".to_string(),
                provider_config_hash: "sha256:cfg".to_string(),
                state_scope: "scope-a".to_string(),
                wire_api: "responses".to_string(),
                detected_wire_api: "responses".to_string(),
                upstream_response_id_hash: "sha256:resp".to_string(),
            },
            routing: RoutingInfo {
                had_fallback: false,
                attempts: vec![RoutingAttempt {
                    target_id: "target-1".to_string(),
                    provider_id: "provider-a".to_string(),
                    upstream_model: "gpt-upstream".to_string(),
                    wire_api: "responses".to_string(),
                    status: "success".to_string(),
                    error_kind: None,
                    latency_ms: Some(1),
                }],
            },
            messages: vec![MessageRecord {
                role: "assistant".to_string(),
                content: vec![serde_json::json!({"type":"text","text":"hello"})],
            }],
            tools: vec![ToolRecord {
                name: "tool_a".to_string(),
            }],
            usage: UsageInfo {
                input_tokens: 1,
                output_tokens: 1,
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
        writer.close_segment().await.unwrap();

        let rebuilt = rebuild_archive_index_from_files(root.path()).unwrap();
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].capture_mode, CaptureMode::VisibleOnly);
        assert_eq!(rebuilt[0].file_count, 1);
        assert_eq!(rebuilt[0].validated_file_count, 1);
    }
}
