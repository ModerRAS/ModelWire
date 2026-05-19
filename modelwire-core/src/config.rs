//! Configuration management for ModelWire.
//!
//! Supports TOML configuration files with fallback to environment variables.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Root configuration structure.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    /// Server configuration.
    #[serde(default)]
    pub server: ServerConfig,

    /// Security configuration.
    #[serde(default)]
    pub security: SecurityConfig,

    /// Archive configuration for conversation recording.
    #[serde(default)]
    pub archive: ArchiveConfig,

    /// List of upstream providers.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,

    /// List of model routes.
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
}

/// Server configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    /// Bind address for the HTTP server.
    #[serde(default = "default_bind")]
    pub bind: String,

    /// Public base URL for callbacks and redirects.
    #[serde(default)]
    pub public_base_url: Option<String>,

    /// Database connection URL.
    #[serde(default = "default_database_url")]
    pub database_url: String,

    /// Upstream request timeout in seconds.
    #[serde(default = "default_upstream_timeout")]
    pub upstream_timeout_secs: u64,

    /// Stream idle timeout in seconds.
    #[serde(default = "default_stream_idle_timeout")]
    pub stream_idle_timeout_secs: u64,

    /// Maximum stream duration in seconds.
    #[serde(default = "default_max_stream_duration")]
    pub max_stream_duration_secs: u64,

    /// Maximum request body size in bytes.
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,

    /// Data directory for SQLite and archives.
    #[serde(default = "default_data_dir")]
    pub data_dir: String,

    /// Compaction mode for `/v1/responses/compact`.
    /// One of: `none`, `native_responses`, `local_summary`, `hybrid`.
    #[serde(default = "default_compaction_mode")]
    pub compaction_mode: String,

    /// Logical summarizer model name recorded in local-summary lineage.
    #[serde(default)]
    pub local_summary_model: Option<String>,

    /// Local summary prompt version recorded in lineage.
    #[serde(default)]
    pub local_summary_prompt_version: Option<String>,

    /// Max characters retained in generated local summary text.
    #[serde(default = "default_local_summary_max_chars")]
    pub local_summary_max_chars: usize,
}

/// Archive configuration for conversation recording.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ArchiveConfig {
    /// Capture mode: off, metadata_only, visible_only, full_visible, debug_raw.
    #[serde(default = "default_archive_capture_mode")]
    pub capture_mode: String,

    /// Root directory for archive files.
    #[serde(default = "default_archive_root")]
    pub root: String,

    /// Whether to include upstream lineage in archives.
    #[serde(default = "default_true")]
    pub include_lineage: bool,
}

fn default_archive_capture_mode() -> String {
    "off".to_string()
}

fn default_archive_root() -> String {
    "./archives".to_string()
}

fn default_true() -> bool {
    true
}

fn default_bind() -> String {
    "127.0.0.1:8787".to_string()
}

fn default_database_url() -> String {
    "sqlite://modelwire.db".to_string()
}

fn default_upstream_timeout() -> u64 {
    120
}

fn default_stream_idle_timeout() -> u64 {
    60
}

fn default_max_stream_duration() -> u64 {
    1800 // 30 minutes
}

fn default_max_body_size() -> usize {
    10 * 1024 * 1024 // 10 MB
}

fn default_data_dir() -> String {
    "./data".to_string()
}

fn default_compaction_mode() -> String {
    "native_responses".to_string()
}

fn default_local_summary_max_chars() -> usize {
    4000
}

/// Security configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    /// Admin authentication mode.
    #[serde(default = "default_admin_auth")]
    pub admin_auth: String,

    /// Admin password for `admin_auth = "local_password"`.
    #[serde(default)]
    pub admin_password: Option<String>,

    /// Downstream authentication mode.
    #[serde(default = "default_downstream_auth")]
    pub downstream_auth: String,

    /// Whether to allow passthrough keys (dangerous on public internet).
    #[serde(default)]
    pub allow_passthrough_keys: bool,

    /// Log prompts (disabled by default for security).
    #[serde(default)]
    pub log_prompts: bool,

    /// Log tool outputs (disabled by default for security).
    #[serde(default)]
    pub log_tool_outputs: bool,

    /// Secret key for hashing downstream keys in logs.
    #[serde(default)]
    pub log_secret: Option<String>,

    /// Whether this is a public deployment (enables additional checks).
    #[serde(default)]
    pub public_deployment: bool,

    /// Optional per-IP request rate limit (requests per minute) for public API.
    #[serde(default)]
    pub ip_requests_per_minute: Option<u32>,

    /// Scoped relay keys for downstream auth/authorization.
    ///
    /// When this list is non-empty and `downstream_auth = "relay_key"`,
    /// ModelWire requires the incoming key hash to match an enabled entry.
    #[serde(default)]
    pub relay_keys: Vec<RelayKeyConfig>,

    /// Required header name for trusted passthrough mode.
    /// Example: `x-gateway-token`.
    #[serde(default)]
    pub trusted_passthrough_header: Option<String>,

    /// Required header value for trusted passthrough mode.
    #[serde(default)]
    pub trusted_passthrough_value: Option<String>,
}

/// Scoped relay key configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RelayKeyConfig {
    /// Stable hash of the relay key (for example HMAC-SHA256 prefix),
    /// never the raw key value.
    pub key_hash: String,

    /// Whether this key is active.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Allowed downstream model aliases. Empty means all configured routes.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Optional allowed provider IDs.
    #[serde(default)]
    pub allowed_providers: Vec<String>,

    /// Optional per-key request rate limit.
    #[serde(default)]
    pub requests_per_minute: Option<u32>,

    /// Optional per-key concurrency limit.
    #[serde(default)]
    pub max_concurrency: Option<u32>,

    /// Optional archive capture policy override.
    #[serde(default)]
    pub archive_capture_mode: Option<String>,
}

fn default_admin_auth() -> String {
    "local_password".to_string()
}

fn default_downstream_auth() -> String {
    "relay_key".to_string()
}

/// Upstream provider configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Unique provider ID.
    pub id: String,

    /// Human-readable name.
    pub name: String,

    /// Base URL for the provider API.
    pub base_url: String,

    /// Authentication mode.
    #[serde(default = "default_auth_mode")]
    pub auth_mode: String,

    /// Default wire API protocol.
    #[serde(default = "default_wire_api")]
    pub default_wire_api: String,

    /// State scope for cross-provider ID reuse.
    #[serde(default)]
    pub state_scope: Option<String>,

    /// API key (for managed auth mode).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Allow private IPs for this provider (default: false for security).
    /// WARNING: Only set to true for internal/trusted providers.
    #[serde(default)]
    pub allow_private_ips: bool,

    /// Skip SSRF validation for this provider (default: false).
    /// WARNING: Only set to true for testing or trusted internal networks.
    #[serde(default)]
    pub skip_ssrf_validation: bool,

    /// Provider-specific configuration.
    #[serde(default)]
    pub config_json: Option<serde_json::Value>,
}

fn default_auth_mode() -> String {
    "pass_authorization".to_string()
}

fn default_wire_api() -> String {
    "auto".to_string()
}

/// Model route configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteConfig {
    /// Unique route ID (optional, auto-generated from downstream_model).
    #[serde(default)]
    pub id: Option<String>,

    /// Downstream model ID that triggers this route.
    pub downstream_model: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Whether this route is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Ordered list of upstream targets.
    pub targets: Vec<TargetConfig>,
}

fn default_enabled() -> bool {
    true
}

/// Upstream target configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TargetConfig {
    /// Reference to a provider by ID.
    pub provider: String,

    /// Upstream model ID to call.
    pub upstream_model: String,

    /// Wire API protocol override.
    #[serde(default = "default_wire_api")]
    pub wire_api: String,

    /// Priority (lower = tried first).
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Whether this target is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Context window in tokens.
    #[serde(default)]
    pub context_window_tokens: Option<u64>,

    /// Maximum output tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,

    /// Recommended auto-compaction threshold.
    #[serde(default)]
    pub auto_compact_recommended_tokens: Option<u64>,

    /// Safety margin for context estimation.
    #[serde(default)]
    pub context_safety_margin_tokens: Option<u64>,

    /// Token estimation strategy.
    #[serde(default)]
    pub token_estimator: Option<String>,

    /// Context overflow policy.
    #[serde(default = "default_overflow_policy")]
    pub context_overflow_policy: String,

    /// Target-specific configuration.
    #[serde(default)]
    pub config_json: Option<serde_json::Value>,
}

fn default_priority() -> i32 {
    10
}

fn default_overflow_policy() -> String {
    "reject".to_string()
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| ConfigError::FileRead(e.to_string()))?;

        Self::from_toml(&contents)
    }

    /// Parse configuration from TOML string.
    pub fn from_toml(toml: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(toml).map_err(|e| ConfigError::Parse(e.to_string()))?;

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration.
    fn validate(&self) -> Result<(), ConfigError> {
        // Check for duplicate provider IDs
        let mut provider_ids = std::collections::HashSet::new();
        for provider in &self.providers {
            if !provider_ids.insert(&provider.id) {
                return Err(ConfigError::DuplicateProvider(provider.id.clone()));
            }
        }

        // Check for duplicate downstream models
        let mut downstream_models = std::collections::HashSet::new();
        for route in &self.routes {
            if !downstream_models.insert(&route.downstream_model) {
                return Err(ConfigError::DuplicateRoute(route.downstream_model.clone()));
            }

            // Check that targets reference valid providers
            for target in &route.targets {
                if !provider_ids.contains(&target.provider) {
                    return Err(ConfigError::InvalidTarget(
                        route.downstream_model.clone(),
                        target.provider.clone(),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Find a provider by ID.
    pub fn get_provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Find a route by downstream model.
    pub fn get_route(&self, downstream_model: &str) -> Option<&RouteConfig> {
        self.routes
            .iter()
            .find(|r| r.downstream_model == downstream_model)
    }

    /// Get enabled targets for a route, sorted by priority.
    pub fn get_sorted_targets<'a>(&'a self, route: &'a RouteConfig) -> Vec<&'a TargetConfig> {
        let mut targets: Vec<&'a TargetConfig> =
            route.targets.iter().filter(|t| t.enabled).collect();

        targets.sort_by_key(|t| t.priority);
        targets
    }
}

/// Configuration error types.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    FileRead(String),

    #[error("Failed to parse config: {0}")]
    Parse(String),

    #[error("Duplicate provider ID: {0}")]
    DuplicateProvider(String),

    #[error("Duplicate downstream model: {0}")]
    DuplicateRoute(String),

    #[error("Route '{0}' references unknown provider '{1}'")]
    InvalidTarget(String, String),
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_loads_valid_toml() {
        let toml = r#"
[server]
bind = "127.0.0.1:8787"
database_url = "sqlite://test.db"

[security]
admin_auth = "local_password"
downstream_auth = "relay_key"

[[providers]]
id = "test-provider"
name = "Test Provider"
base_url = "https://api.test.com/v1"
auth_mode = "pass_authorization"
default_wire_api = "responses"

[[routes]]
downstream_model = "test-model"

[[routes.targets]]
provider = "test-provider"
upstream_model = "test-model"
wire_api = "responses"
priority = 10
"#;
        let config = Config::from_toml(toml).expect("should parse");
        assert_eq!(config.server.bind, "127.0.0.1:8787");
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.routes.len(), 1);
    }

    #[test]
    fn test_config_rejects_invalid_toml() {
        let toml = "not valid toml {{{";
        let result = Config::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_rejects_missing_provider_id() {
        let toml = r#"
[[providers]]
name = "Missing ID"
base_url = "https://api.test.com/v1"
"#;
        let result = Config::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_rejects_duplicate_providers() {
        let toml = r#"
[[providers]]
id = "dup"
name = "First"
base_url = "https://a.test.com/v1"

[[providers]]
id = "dup"
name = "Second"
base_url = "https://b.test.com/v1"
"#;
        let result = Config::from_toml(toml);
        assert!(matches!(result, Err(ConfigError::DuplicateProvider(_))));
    }

    #[test]
    fn test_config_rejects_invalid_target_provider() {
        let toml = r#"
[[routes]]
downstream_model = "test"

[[routes.targets]]
provider = "nonexistent"
upstream_model = "test"
"#;
        let result = Config::from_toml(toml);
        assert!(matches!(result, Err(ConfigError::InvalidTarget(_, _))));
    }

    #[test]
    fn test_config_get_provider() {
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "test".to_string(),
                name: "Test".to_string(),
                base_url: "https://api.test.com".to_string(),
                ..Default::default()
            }],
            routes: vec![],
        };
        assert!(config.get_provider("test").is_some());
        assert!(config.get_provider("nonexistent").is_none());
    }

    #[test]
    fn test_config_get_route() {
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![],
            routes: vec![RouteConfig {
                downstream_model: "test-model".to_string(),
                ..Default::default()
            }],
        };
        assert!(config.get_route("test-model").is_some());
        assert!(config.get_route("nonexistent").is_none());
    }

    #[test]
    fn test_config_sorted_targets() {
        let config = Config {
            server: ServerConfig::default(),
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "provider".to_string(),
                name: "Provider".to_string(),
                base_url: "https://api.test.com".to_string(),
                ..Default::default()
            }],
            routes: vec![RouteConfig {
                downstream_model: "test".to_string(),
                targets: vec![
                    TargetConfig {
                        provider: "provider".to_string(),
                        upstream_model: "model-c".to_string(),
                        priority: 30,
                        ..Default::default()
                    },
                    TargetConfig {
                        provider: "provider".to_string(),
                        upstream_model: "model-a".to_string(),
                        priority: 10,
                        ..Default::default()
                    },
                    TargetConfig {
                        provider: "provider".to_string(),
                        upstream_model: "model-b".to_string(),
                        priority: 20,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
        };

        let route = config.get_route("test").unwrap();
        let targets = config.get_sorted_targets(route);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].upstream_model, "model-a");
        assert_eq!(targets[1].upstream_model, "model-b");
        assert_eq!(targets[2].upstream_model, "model-c");
    }

    #[test]
    fn test_config_loads_security_relay_keys() {
        let toml = r#"
[security]
downstream_auth = "relay_key"
log_secret = "test-secret"
ip_requests_per_minute = 60

[[security.relay_keys]]
key_hash = "deadbeef"
enabled = true
allowed_models = ["codex-main"]
allowed_providers = ["provider-a"]
requests_per_minute = 120
max_concurrency = 8
archive_capture_mode = "metadata_only"
"#;

        let config = Config::from_toml(toml).expect("should parse relay key scopes");
        assert_eq!(config.security.ip_requests_per_minute, Some(60));
        assert_eq!(config.security.relay_keys.len(), 1);
        let key = &config.security.relay_keys[0];
        assert_eq!(key.key_hash, "deadbeef");
        assert_eq!(key.allowed_models, vec!["codex-main".to_string()]);
        assert_eq!(key.allowed_providers, vec!["provider-a".to_string()]);
        assert_eq!(key.requests_per_minute, Some(120));
        assert_eq!(key.max_concurrency, Some(8));
        assert_eq!(key.archive_capture_mode.as_deref(), Some("metadata_only"));
    }

    #[test]
    fn test_config_loads_trusted_passthrough_gate() {
        let toml = r#"
[security]
downstream_auth = "trusted_passthrough"
trusted_passthrough_header = "x-gateway-token"
trusted_passthrough_value = "gw-123"
"#;

        let config = Config::from_toml(toml).expect("should parse trusted passthrough gate");
        assert_eq!(
            config.security.trusted_passthrough_header.as_deref(),
            Some("x-gateway-token")
        );
        assert_eq!(
            config.security.trusted_passthrough_value.as_deref(),
            Some("gw-123")
        );
    }
}

// Provide Default implementations (used in tests and integration)
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            public_base_url: None,
            database_url: default_database_url(),
            upstream_timeout_secs: default_upstream_timeout(),
            stream_idle_timeout_secs: default_stream_idle_timeout(),
            max_stream_duration_secs: default_max_stream_duration(),
            max_body_size: default_max_body_size(),
            data_dir: default_data_dir(),
            compaction_mode: default_compaction_mode(),
            local_summary_model: None,
            local_summary_prompt_version: None,
            local_summary_max_chars: default_local_summary_max_chars(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            admin_auth: default_admin_auth(),
            admin_password: None,
            downstream_auth: default_downstream_auth(),
            allow_passthrough_keys: false,
            log_prompts: false,
            log_tool_outputs: false,
            log_secret: None,
            public_deployment: false,
            ip_requests_per_minute: None,
            relay_keys: vec![],
            trusted_passthrough_header: None,
            trusted_passthrough_value: None,
        }
    }
}

impl Default for RelayKeyConfig {
    fn default() -> Self {
        Self {
            key_hash: String::new(),
            enabled: true,
            allowed_models: vec![],
            allowed_providers: vec![],
            requests_per_minute: None,
            max_concurrency: None,
            archive_capture_mode: None,
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            base_url: String::new(),
            auth_mode: default_auth_mode(),
            default_wire_api: default_wire_api(),
            state_scope: None,
            api_key: None,
            allow_private_ips: false,
            skip_ssrf_validation: false,
            config_json: None,
        }
    }
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            id: None,
            downstream_model: String::new(),
            description: None,
            enabled: true,
            targets: vec![],
        }
    }
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            upstream_model: String::new(),
            wire_api: default_wire_api(),
            priority: default_priority(),
            enabled: true,
            context_window_tokens: None,
            max_output_tokens: None,
            auto_compact_recommended_tokens: None,
            context_safety_margin_tokens: None,
            token_estimator: None,
            context_overflow_policy: default_overflow_policy(),
            config_json: None,
        }
    }
}
