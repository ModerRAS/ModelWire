//! ModelWire Archive
//!
//! Optional conversation archive system for training/distillation data.

pub mod manifest;
pub mod redact;
pub mod writer;

// Re-export common types
pub use manifest::{ArchiveError, ArchiveFile, ArchiveManifest};
pub use writer::ConversationRecord;
