//! Schema definitions for ModelWire database.
//!
//! SQL table definitions for SQLite and Postgres.

/// SQLite schema.
pub const SQLITE_SCHEMA: &str = r#"
-- Providers table
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
);

-- Routes table
CREATE TABLE IF NOT EXISTS routes (
    id TEXT PRIMARY KEY,
    downstream_model TEXT NOT NULL UNIQUE,
    description TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Route targets table
CREATE TABLE IF NOT EXISTS route_targets (
    id TEXT PRIMARY KEY,
    route_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    upstream_model TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 10,
    enabled INTEGER NOT NULL DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (route_id) REFERENCES routes(id) ON DELETE CASCADE,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE
);

-- Responses table (core state ownership)
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
);

-- Response items (canonical transcript)
CREATE TABLE IF NOT EXISTS response_items (
    id TEXT PRIMARY KEY,
    response_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    item_type TEXT NOT NULL,
    role TEXT,
    call_id TEXT,
    content_json TEXT NOT NULL,
    visible INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    FOREIGN KEY (response_id) REFERENCES responses(id) ON DELETE CASCADE
);

-- Upstream handles (private state mapping)
CREATE TABLE IF NOT EXISTS upstream_handles (
    id TEXT PRIMARY KEY,
    modelwire_response_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    credential_hash TEXT NOT NULL,
    upstream_model TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    state_scope TEXT,
    upstream_response_id TEXT,
    handle_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    FOREIGN KEY (modelwire_response_id) REFERENCES responses(id) ON DELETE CASCADE
);

-- Probe results (lazy protocol detection cache)
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
    expires_at TEXT NOT NULL,
    UNIQUE(provider_id, credential_hash, upstream_model)
);

-- Request logs (audit)
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
);

-- Compaction lineage
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
    provider_native INTEGER NOT NULL DEFAULT 0,
    summarizer_model TEXT,
    prompt_version TEXT,
    source_tokens INTEGER,
    summary_tokens INTEGER,
    created_at TEXT NOT NULL
);

-- Retention policies
CREATE TABLE IF NOT EXISTS retention_policies (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    state_ttl_seconds INTEGER NOT NULL DEFAULT 86400,
    log_ttl_seconds INTEGER NOT NULL DEFAULT 604800,
    archive_ttl_seconds INTEGER,
    keep_archives INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Archive files metadata
CREATE TABLE IF NOT EXISTS archive_files (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    format TEXT NOT NULL,
    path TEXT NOT NULL,
    byte_size INTEGER,
    conversation_count INTEGER,
    item_count INTEGER,
    checksum TEXT,
    manifest_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_responses_request_id ON responses(request_id);
CREATE INDEX IF NOT EXISTS idx_responses_previous_response_id ON responses(previous_response_id);
CREATE INDEX IF NOT EXISTS idx_responses_status ON responses(status);
CREATE INDEX IF NOT EXISTS idx_responses_created_at ON responses(created_at);
CREATE INDEX IF NOT EXISTS idx_response_items_response_id ON response_items(response_id);
CREATE INDEX IF NOT EXISTS idx_response_items_sequence ON response_items(response_id, sequence);
CREATE INDEX IF NOT EXISTS idx_upstream_handles_response_id ON upstream_handles(modelwire_response_id);
CREATE INDEX IF NOT EXISTS idx_probe_results_cache_key ON probe_results(provider_id, credential_hash, upstream_model);
CREATE INDEX IF NOT EXISTS idx_probe_results_expires ON probe_results(expires_at);
CREATE INDEX IF NOT EXISTS idx_request_logs_request_id ON request_logs(request_id);
CREATE INDEX IF NOT EXISTS idx_request_logs_created ON request_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_compaction_lineage_request_id ON compaction_lineage(request_id);
CREATE INDEX IF NOT EXISTS idx_compaction_lineage_created_at ON compaction_lineage(created_at);
"#;

/// Postgres schema.
pub const POSTGRES_SCHEMA: &str = r#"
-- Providers table
CREATE TABLE IF NOT EXISTS providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    auth_mode TEXT NOT NULL,
    default_wire_api TEXT NOT NULL,
    state_scope TEXT,
    config_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Routes table
CREATE TABLE IF NOT EXISTS routes (
    id TEXT PRIMARY KEY,
    downstream_model TEXT NOT NULL UNIQUE,
    description TEXT,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Route targets table
CREATE TABLE IF NOT EXISTS route_targets (
    id TEXT PRIMARY KEY,
    route_id TEXT NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    upstream_model TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 10,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    config_json TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Responses table
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
    usage_json JSONB,
    error_json JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

-- Response items
CREATE TABLE IF NOT EXISTS response_items (
    id TEXT PRIMARY KEY,
    response_id TEXT NOT NULL REFERENCES responses(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    item_type TEXT NOT NULL,
    role TEXT,
    call_id TEXT,
    content_json JSONB NOT NULL,
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Upstream handles
CREATE TABLE IF NOT EXISTS upstream_handles (
    id TEXT PRIMARY KEY,
    modelwire_response_id TEXT NOT NULL REFERENCES responses(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    credential_hash TEXT NOT NULL,
    upstream_model TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    state_scope TEXT,
    upstream_response_id TEXT,
    handle_json JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Probe results
CREATE TABLE IF NOT EXISTS probe_results (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    credential_hash TEXT NOT NULL,
    upstream_model TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    supports_streaming BOOLEAN,
    supports_tools BOOLEAN,
    supports_parallel_tool_calls BOOLEAN,
    supports_previous_response_id BOOLEAN,
    supports_reasoning_encrypted_content BOOLEAN,
    supports_reasoning_summary BOOLEAN,
    status TEXT NOT NULL,
    failure_kind TEXT,
    failure_message_redacted TEXT,
    last_success_at TIMESTAMPTZ,
    last_failure_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    UNIQUE(provider_id, credential_hash, upstream_model)
);

-- Request logs
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Compaction lineage
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
);

-- Retention policies
CREATE TABLE IF NOT EXISTS retention_policies (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    state_ttl_seconds INTEGER NOT NULL DEFAULT 86400,
    log_ttl_seconds INTEGER NOT NULL DEFAULT 604800,
    archive_ttl_seconds INTEGER,
    keep_archives BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Archive files
CREATE TABLE IF NOT EXISTS archive_files (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    format TEXT NOT NULL,
    path TEXT NOT NULL,
    byte_size BIGINT,
    conversation_count INTEGER,
    item_count INTEGER,
    checksum TEXT,
    manifest_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_responses_request_id ON responses(request_id);
CREATE INDEX IF NOT EXISTS idx_responses_previous_response_id ON responses(previous_response_id);
CREATE INDEX IF NOT EXISTS idx_responses_status ON responses(status);
CREATE INDEX IF NOT EXISTS idx_responses_created_at ON responses(created_at);
CREATE INDEX IF NOT EXISTS idx_response_items_response_id ON response_items(response_id);
CREATE INDEX IF NOT EXISTS idx_response_items_sequence ON response_items(response_id, sequence);
CREATE INDEX IF NOT EXISTS idx_upstream_handles_response_id ON upstream_handles(modelwire_response_id);
CREATE INDEX IF NOT EXISTS idx_probe_results_cache_key ON probe_results(provider_id, credential_hash, upstream_model);
CREATE INDEX IF NOT EXISTS idx_probe_results_expires ON probe_results(expires_at);
CREATE INDEX IF NOT EXISTS idx_request_logs_request_id ON request_logs(request_id);
CREATE INDEX IF NOT EXISTS idx_request_logs_created ON request_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_compaction_lineage_request_id ON compaction_lineage(request_id);
CREATE INDEX IF NOT EXISTS idx_compaction_lineage_created_at ON compaction_lineage(created_at);
"#;
