//! ModelWire Server
//!
//! HTTP server with OpenAI Responses-compatible API.

pub mod admin;
pub mod error;
pub mod janitor;
pub mod middleware;
pub mod relay;
pub mod request_limiter;
pub mod routes;
pub mod runtime_config;
pub mod secrets;
pub mod server;

pub use janitor::{run_janitor_periodically, CleanupReport, Janitor};
pub use modelwire_core::error::ErrorKind;
pub use server::*;

use modelwire_archive::writer::ArchiveWriter;
use modelwire_core as core;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub type AppState = Arc<ServerState>;

#[derive(Debug, Clone, Default)]
pub struct KeyRateLimitState {
    pub window_started_at_secs: u64,
    pub request_count: u32,
}

#[derive(Debug, Clone)]
pub struct KeyLimiterCounters {
    pub in_flight: u32,
    pub rate: KeyRateLimitState,
}

impl Default for KeyLimiterCounters {
    fn default() -> Self {
        Self {
            in_flight: 0,
            rate: KeyRateLimitState {
                window_started_at_secs: current_unix_secs(),
                request_count: 0,
            },
        }
    }
}

pub fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub struct ServerState {
    pub config: core::Config,
    pub db: modelwire_db::Database,
    pub probe_cache: dashmap::DashMap<String, core::ProbeResult>,
    /// Per-target probe locks to enforce single-flight probing for identical keys.
    pub probe_locks: dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    pub key_limiter_counters: dashmap::DashMap<String, KeyLimiterCounters>,
    pub ip_limiter_counters: dashmap::DashMap<String, KeyRateLimitState>,
    /// Archive writers keyed by root + mode + year-month period (lazily initialized).
    pub archive_writers: tokio::sync::Mutex<HashMap<String, ArchiveWriter>>,
}
