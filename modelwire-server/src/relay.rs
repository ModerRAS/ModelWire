//! Data-plane relay framework for `/v1/responses`.
//!
//! This module is intentionally explicit. The remaining milestones should fill
//! these seams instead of growing ad-hoc logic in the route handler.

use bytes::Bytes;
use modelwire_adapters::{
    anthropic::AnthropicAdapter,
    openai_chat::OpenAiChatAdapter,
    responses::ResponsesAdapter,
    sse::{canonical_to_sse, extract_sse_frames, RawSseFrame, SseEventType, SseWriter},
    UpstreamAdapter, UpstreamError,
};
use modelwire_archive::manifest::CaptureMode;
use modelwire_archive::redact::Redactor;
use modelwire_archive::writer::{
    ConversationRecord, MessageRecord, ModelInfo, QualityInfo, RedactionStatus, RequestInfo,
    RoutingAttempt, RoutingInfo, ToolRecord, UsageInfo,
};
use modelwire_core::{
    CanonicalEvent, CanonicalInputItem, CanonicalInstructions, CanonicalOutputItem,
    CanonicalReasoningOptions, CanonicalResponseRequest, CanonicalTool, CanonicalToolChoice,
    ContentBlock, Error, ErrorKind, ProbeResult, ResponseUsage, WireApi,
};
use modelwire_db::repo::{
    compactions::{store_compaction_lineage, CompactionLineageInsert},
    logs::store_log,
    probes::{get_probe_result, store_probe_result, store_probe_result_detailed},
    responses::{
        get_items, get_latest_upstream_handle, get_response, store_upstream_handle, ItemRecord,
        ResponseInsert, ResponseItemInsert, UpstreamHandleInsert,
    },
};
use serde::Serialize;
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{info, warn};

use crate::ServerState;

#[derive(Debug, Clone)]
struct ResolvedTargetProtocol {
    wire_api: WireApi,
    credential_hash: String,
    tool_support_known: bool,
    supports_tools: bool,
}

#[derive(Debug, Clone)]
struct TargetCallContext {
    upstream_key: Option<String>,
    resolved: ResolvedTargetProtocol,
}

struct NonStreamingAttemptContext<'a> {
    continuation: Option<&'a ContinuationContext>,
    archive_capture_mode_override: Option<&'a str>,
    routing_attempts: &'a mut Vec<RoutingAttempt>,
    downstream_key_hash: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
struct PersistHints<'a> {
    upstream_response_id: Option<&'a str>,
    previous_response_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
struct ArchivePersistHints<'a> {
    upstream_response_id: Option<&'a str>,
    capture_mode_override: Option<&'a str>,
    routing_attempts: &'a [RoutingAttempt],
    winning_attempt_index: usize,
}

#[derive(Debug, Clone)]
struct ContinuationContext {
    previous_upstream_handle: Option<String>,
    previous_provider_id: Option<String>,
    previous_upstream_model: Option<String>,
    previous_wire_api: Option<WireApi>,
    previous_credential_hash: Option<String>,
    replay_items: Vec<CanonicalInputItem>,
    known_call_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct CompactionSourceState {
    response_id: String,
    provider_id: String,
    upstream_model: Option<String>,
    state_scope: Option<String>,
}

/// Immutable route data selected at request start.
#[derive(Debug, Clone)]
pub struct RouteSnapshot {
    pub route_id: String,
    pub downstream_model: String,
    pub targets: Vec<TargetSnapshot>,
}

/// Immutable target data selected at request start.
#[derive(Debug, Clone)]
pub struct TargetSnapshot {
    pub target_id: String,
    pub provider_id: String,
    pub provider_name: String,
    pub provider_base_url: String,
    pub provider_auth_mode: String,
    pub provider_api_key: Option<String>,
    pub state_scope: Option<String>,
    pub upstream_model: String,
    pub configured_wire_api: WireApi,
    pub priority: i32,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub context_safety_margin_tokens: Option<u64>,
    pub context_overflow_policy: String,
}

/// Result returned to the downstream Responses client.
#[derive(Debug, Clone, Serialize)]
pub struct DownstreamResponse {
    pub id: String,
    pub object: &'static str,
    pub created_at: i64,
    pub model: String,
    pub status: &'static str,
    pub output: Vec<DownstreamOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponseUsage>,
}

/// Streaming relay output bytes (SSE frames).
#[derive(Debug, Clone, Default)]
pub struct StreamingRelayResult {
    pub sse_frames: Vec<Bytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamTimeoutKind {
    Idle,
    MaxDuration,
}

/// Downstream Responses output item.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownstreamOutputItem {
    Message {
        id: String,
        status: &'static str,
        role: String,
        content: Vec<DownstreamContentBlock>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: &'static str,
    },
    Reasoning {
        id: String,
        summary: Vec<serde_json::Value>,
    },
}

/// Downstream Responses content block.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownstreamContentBlock {
    OutputText {
        text: String,
        annotations: Vec<serde_json::Value>,
    },
}

/// Run the non-streaming Responses relay path.
pub async fn relay_non_streaming_response(
    state: Arc<ServerState>,
    request_id: String,
    raw_json: serde_json::Value,
    downstream_authorization: Option<String>,
) -> Result<DownstreamResponse, Error> {
    relay_non_streaming_response_scoped(
        state,
        request_id,
        raw_json,
        downstream_authorization,
        None,
        None,
        None,
    )
    .await
}

/// Run the non-streaming Responses relay path with optional provider scope.
pub async fn relay_non_streaming_response_scoped(
    state: Arc<ServerState>,
    request_id: String,
    raw_json: serde_json::Value,
    downstream_authorization: Option<String>,
    downstream_key_hash: Option<String>,
    allowed_providers: Option<Vec<String>>,
    archive_capture_mode_override: Option<String>,
) -> Result<DownstreamResponse, Error> {
    let downstream_model = require_string(&raw_json, "model")?.to_string();

    if raw_json
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Use relay_streaming_response for stream=true requests",
        ));
    }

    let route = snapshot_route(&state, &downstream_model, allowed_providers.as_deref())?;
    let continuation = load_continuation_context(
        &state,
        &raw_json,
        &route,
        downstream_authorization.as_deref(),
    )
    .await?;
    let mut last_error: Option<Error> = None;
    let mut routing_attempts: Vec<RoutingAttempt> = Vec::new();
    let request_has_tools = raw_json
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);

    for target in &route.targets {
        let mut canonical = parse_canonical_request(
            &request_id,
            &raw_json,
            &route.downstream_model,
            &target.upstream_model,
        )?;

        apply_continuation_to_canonical(
            &mut canonical,
            continuation.as_ref(),
            target,
            downstream_authorization.as_deref(),
        )?;

        match context_guard_check(target, &canonical) {
            Ok(()) => {}
            Err(error) if should_fallback_on_context_guard(target) => {
                warn!(
                    request_id = %request_id,
                    target_id = %target.target_id,
                    error_kind = %error.kind,
                    "Context guard overflow, falling back to next target"
                );
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        }

        let upstream_key = resolve_upstream_key_checked(
            &state,
            &request_id,
            &route,
            target,
            &route.downstream_model,
            downstream_authorization.as_deref(),
        )
        .await?;
        let resolved =
            resolve_target_protocol(state.as_ref(), target, upstream_key.as_deref()).await?;
        let target_context = TargetCallContext {
            upstream_key,
            resolved,
        };
        if should_skip_target_for_tool_support(target, &canonical, &target_context.resolved) {
            warn!(
                request_id = %request_id,
                target_id = %target.target_id,
                provider_id = %target.provider_id,
                "Skipping target because tool support is unknown/unsupported for tool-bearing request"
            );
            last_error = Some(Error::new(
                ErrorKind::ProtocolNotSupported,
                format!(
                    "Target '{}' cannot be used for tool-bearing request because tool support is unknown/unsupported",
                    target.target_id
                ),
            ));
            continue;
        }

        match try_target(
            &state,
            &route,
            target,
            canonical,
            target_context,
            NonStreamingAttemptContext {
                continuation: continuation.as_ref(),
                archive_capture_mode_override: archive_capture_mode_override.as_deref(),
                routing_attempts: &mut routing_attempts,
                downstream_key_hash: downstream_key_hash.as_deref(),
            },
        )
        .await
        {
            Ok(response) => return Ok(response),
            Err(error) if error.kind.is_fallback_eligible() => {
                warn!(
                    request_id = %request_id,
                    target_id = %target.target_id,
                    error_kind = %error.kind,
                    "Target failed before downstream commit; trying next target"
                );
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    if request_has_tools {
        if let Some(error) = last_error.as_ref() {
            if error.kind == ErrorKind::ProtocolNotSupported {
                return Err(Error::new(
                    ErrorKind::ProtocolNotSupported,
                    "No upstream target with known tool support could satisfy this tool-bearing request",
                ));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::new(
            ErrorKind::UpstreamUnavailable,
            "No upstream target could satisfy the request",
        )
    }))
}

/// Run streaming Responses relay path and return downstream SSE frames.
pub async fn relay_streaming_response(
    state: Arc<ServerState>,
    request_id: String,
    raw_json: serde_json::Value,
    downstream_authorization: Option<String>,
) -> Result<StreamingRelayResult, Error> {
    relay_streaming_response_scoped(state, request_id, raw_json, downstream_authorization, None)
        .await
}

/// Run streaming Responses relay path with optional provider scope and return downstream SSE frames.
pub async fn relay_streaming_response_scoped(
    state: Arc<ServerState>,
    request_id: String,
    raw_json: serde_json::Value,
    downstream_authorization: Option<String>,
    allowed_providers: Option<Vec<String>>,
) -> Result<StreamingRelayResult, Error> {
    let downstream_model = require_string(&raw_json, "model")?.to_string();
    let route = snapshot_route(&state, &downstream_model, allowed_providers.as_deref())?;
    let continuation = load_continuation_context(
        &state,
        &raw_json,
        &route,
        downstream_authorization.as_deref(),
    )
    .await?;

    let mut last_error: Option<Error> = None;
    let request_has_tools = raw_json
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    for target in &route.targets {
        let mut canonical = parse_canonical_request(
            &request_id,
            &raw_json,
            &route.downstream_model,
            &target.upstream_model,
        )?;

        apply_continuation_to_canonical(
            &mut canonical,
            continuation.as_ref(),
            target,
            downstream_authorization.as_deref(),
        )?;

        match context_guard_check(target, &canonical) {
            Ok(()) => {}
            Err(error) if should_fallback_on_context_guard(target) => {
                warn!(
                    request_id = %request_id,
                    target_id = %target.target_id,
                    error_kind = %error.kind,
                    "Streaming context guard overflow, falling back to next target"
                );
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        }

        let upstream_key = resolve_upstream_key_checked(
            &state,
            &request_id,
            &route,
            target,
            &route.downstream_model,
            downstream_authorization.as_deref(),
        )
        .await?;
        let resolved =
            resolve_target_protocol(state.as_ref(), target, upstream_key.as_deref()).await?;
        let target_context = TargetCallContext {
            upstream_key,
            resolved,
        };
        if should_skip_target_for_tool_support(target, &canonical, &target_context.resolved) {
            warn!(
                request_id = %request_id,
                target_id = %target.target_id,
                provider_id = %target.provider_id,
                "Skipping streaming target because tool support is unknown/unsupported for tool-bearing request"
            );
            last_error = Some(Error::new(
                ErrorKind::ProtocolNotSupported,
                format!(
                    "Target '{}' cannot be used for tool-bearing request because tool support is unknown/unsupported",
                    target.target_id
                ),
            ));
            continue;
        }

        match try_target_streaming(&state, &route, target, canonical, target_context).await {
            Ok(result) => return Ok(result),
            Err(error) if error.kind.is_fallback_eligible() => {
                warn!(
                    request_id = %request_id,
                    target_id = %target.target_id,
                    error_kind = %error.kind,
                    "Streaming target failed before commit; trying next target"
                );
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    if request_has_tools {
        if let Some(error) = last_error.as_ref() {
            if error.kind == ErrorKind::ProtocolNotSupported {
                return Err(Error::new(
                    ErrorKind::ProtocolNotSupported,
                    "No upstream target with known tool support could satisfy this tool-bearing request",
                ));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::new(
            ErrorKind::UpstreamUnavailable,
            "No upstream target could satisfy the streaming request",
        )
    }))
}

/// Run capability-dependent Responses compaction path.
///
/// This endpoint is only forwarded to upstream targets that resolve to native
/// Responses protocol. Chat and Anthropic targets are skipped.
pub async fn relay_compact_response(
    state: Arc<ServerState>,
    request_id: String,
    raw_json: serde_json::Value,
    downstream_authorization: Option<String>,
) -> Result<serde_json::Value, Error> {
    relay_compact_response_scoped(
        state,
        request_id,
        raw_json,
        downstream_authorization,
        None,
        None,
    )
    .await
}

/// Run capability-dependent Responses compaction path with optional provider scope.
pub async fn relay_compact_response_scoped(
    state: Arc<ServerState>,
    request_id: String,
    raw_json: serde_json::Value,
    downstream_authorization: Option<String>,
    allowed_providers: Option<Vec<String>>,
    _archive_capture_mode_override: Option<String>,
) -> Result<serde_json::Value, Error> {
    let downstream_model = require_string(&raw_json, "model")?.to_string();
    let route = snapshot_route(&state, &downstream_model, allowed_providers.as_deref())?;
    let source_state = load_compaction_source_state(state.as_ref(), &raw_json).await?;
    let compact_mode = state.config.server.compaction_mode.as_str();
    let allow_native = matches!(compact_mode, "native_responses" | "hybrid");
    let allow_local_summary = matches!(compact_mode, "local_summary" | "hybrid");

    let mut last_error: Option<Error> = None;
    if allow_native {
        for target in &route.targets {
            if !target_matches_compaction_source(target, source_state.as_ref()) {
                continue;
            }

            let upstream_key = resolve_upstream_key(target, downstream_authorization.as_deref());
            let resolved =
                resolve_target_protocol(state.as_ref(), target, upstream_key.as_deref()).await?;
            if resolved.wire_api != WireApi::Responses {
                continue;
            }

            match try_target_compact(
                state.as_ref(),
                &route,
                target,
                source_state.as_ref(),
                &raw_json,
                upstream_key.as_deref(),
                &request_id,
            )
            .await
            {
                Ok(value) => return Ok(value),
                Err(error) if error.kind.is_fallback_eligible() => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
    }

    if allow_local_summary {
        return perform_local_summary_compact(
            state.as_ref(),
            &route,
            source_state.as_ref(),
            &request_id,
        )
        .await;
    }

    Err(last_error.unwrap_or_else(|| {
        Error::new(
            ErrorKind::ProtocolNotSupported,
            format!(
                "Compaction unavailable for mode '{compact_mode}'; no compatible /v1/responses/compact target"
            ),
        )
    }))
}

fn snapshot_route(
    state: &ServerState,
    downstream_model: &str,
    allowed_providers: Option<&[String]>,
) -> Result<RouteSnapshot, Error> {
    let route = match state.config.get_route(downstream_model) {
        Some(route) if route.enabled => route,
        Some(_) => {
            return Err(Error::new(
                ErrorKind::ModelNotFound,
                format!("Model '{downstream_model}' is disabled"),
            ));
        }
        None => {
            return Err(Error::new(
                ErrorKind::ModelNotFound,
                format!("Model '{downstream_model}' not found"),
            ));
        }
    };

    let mut targets = Vec::new();
    for target in state.config.get_sorted_targets(route) {
        let Some(provider) = state.config.get_provider(&target.provider) else {
            warn!(
                downstream_model = %downstream_model,
                provider_id = %target.provider,
                "Route target references missing provider; skipping"
            );
            continue;
        };

        if let Some(allowed_provider_ids) = allowed_providers {
            if !allowed_provider_ids
                .iter()
                .any(|provider_id| provider_id == &provider.id)
            {
                continue;
            }
        }

        // SSRF protection: validate provider URL
        use modelwire_core::validate_provider_url_for_provider;

        // Skip validation if provider explicitly opts out (for testing/trusted networks)
        if !provider.skip_ssrf_validation {
            match validate_provider_url_for_provider(&provider.base_url, provider.allow_private_ips)
            {
                modelwire_core::SsrfValidationResult::Blocked { reason } => {
                    warn!(
                        downstream_model = %downstream_model,
                        provider_id = %provider.id,
                        base_url = %provider.base_url,
                        reason = %reason,
                        "Provider URL blocked by SSRF protection; skipping"
                    );
                    continue;
                }
                modelwire_core::SsrfValidationResult::Safe => {}
            }
        }

        let configured_wire_api = WireApi::parse(&target.wire_api)
            .or_else(|| WireApi::parse(&provider.default_wire_api))
            .unwrap_or(WireApi::Auto);

        targets.push(TargetSnapshot {
            target_id: format!(
                "{}:{}:{}",
                route
                    .id
                    .clone()
                    .unwrap_or_else(|| route.downstream_model.clone()),
                target.provider,
                target.priority
            ),
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            provider_base_url: provider.base_url.clone(),
            provider_auth_mode: provider.auth_mode.clone(),
            provider_api_key: provider.api_key.clone(),
            state_scope: provider.state_scope.clone(),
            upstream_model: target.upstream_model.clone(),
            configured_wire_api,
            priority: target.priority,
            context_window_tokens: target.context_window_tokens,
            max_output_tokens: target.max_output_tokens,
            context_safety_margin_tokens: target.context_safety_margin_tokens,
            context_overflow_policy: target.context_overflow_policy.clone(),
        });
    }

    if targets.is_empty() {
        if allowed_providers.is_some() {
            return Err(Error::new(
                ErrorKind::AuthFailed,
                format!(
                    "Relay key is not allowed to access any provider for model '{}'",
                    route.downstream_model
                ),
            ));
        }

        return Err(Error::new(
            ErrorKind::UpstreamUnavailable,
            "No upstream targets configured",
        ));
    }

    Ok(RouteSnapshot {
        route_id: route
            .id
            .clone()
            .unwrap_or_else(|| route.downstream_model.clone()),
        downstream_model: route.downstream_model.clone(),
        targets,
    })
}

async fn try_target(
    state: &ServerState,
    route: &RouteSnapshot,
    target: &TargetSnapshot,
    canonical: CanonicalResponseRequest,
    target_context: TargetCallContext,
    attempt_context: NonStreamingAttemptContext<'_>,
) -> Result<DownstreamResponse, Error> {
    let request_start = std::time::Instant::now();
    let TargetCallContext {
        upstream_key,
        resolved,
    } = target_context;
    let adapter: Box<dyn UpstreamAdapter> = adapter_for_wire_api(resolved.wire_api);
    let mut upstream_request = adapter.build_request(
        &canonical,
        &target.provider_base_url,
        upstream_key.as_deref(),
    );
    let mut url = join_url(&target.provider_base_url, &upstream_request.path);

    info!(
        request_id = %canonical.request_id,
        route_id = %route.route_id,
        target_id = %target.target_id,
        provider_id = %target.provider_id,
        upstream_model = %target.upstream_model,
        wire_api = %resolved.wire_api.as_str(),
        "Calling upstream target"
    );

    let client = build_upstream_client(state.config.server.upstream_timeout_secs)?;

    let method = upstream_request
        .method
        .parse()
        .map_err(|_| Error::new(ErrorKind::InternalError, "Invalid upstream HTTP method"))?;
    let mut builder = client.request(method, url);
    for (name, value) in upstream_request.headers {
        builder = builder.header(name, value);
    }

    let mut upstream_response = match builder.json(&upstream_request.body).send().await {
        Ok(response) => response,
        Err(error) => {
            let mapped = map_reqwest_error(error);
            let latency_ms = request_start.elapsed().as_millis() as i64;
            let _ = store_log(
                &state.db,
                &canonical.request_id,
                attempt_context.downstream_key_hash,
                Some(&route.downstream_model),
                Some(&route.route_id),
                Some(&target.target_id),
                Some(&target.provider_id),
                Some(&target.upstream_model),
                Some(resolved.wire_api.as_str()),
                None,
                Some(&mapped.kind.to_string()),
                Some(latency_ms),
                None,
                None,
            )
            .await;
            attempt_context.routing_attempts.push(build_routing_attempt(
                target,
                &resolved,
                "failed",
                Some(mapped.kind),
                Some(latency_ms as u64),
            ));
            return Err(mapped);
        }
    };

    let mut status = upstream_response.status();
    let mut bytes = match upstream_response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            let mapped = map_reqwest_error(error);
            let latency_ms = request_start.elapsed().as_millis() as i64;
            let _ = store_log(
                &state.db,
                &canonical.request_id,
                attempt_context.downstream_key_hash,
                Some(&route.downstream_model),
                Some(&route.route_id),
                Some(&target.target_id),
                Some(&target.provider_id),
                Some(&target.upstream_model),
                Some(resolved.wire_api.as_str()),
                None,
                Some(&mapped.kind.to_string()),
                Some(latency_ms),
                None,
                None,
            )
            .await;
            attempt_context.routing_attempts.push(build_routing_attempt(
                target,
                &resolved,
                "failed",
                Some(mapped.kind),
                Some(latency_ms as u64),
            ));
            return Err(mapped);
        }
    };

    if should_retry_with_replay_on_missing_handle(status.as_u16(), &bytes, &canonical) {
        let mut replay_canonical = canonical.clone();
        replay_canonical.previous_response_id = None;
        let replay_input =
            replay_input_for_canonical(&replay_canonical, attempt_context.continuation);
        replay_canonical.input = replay_input;
        upstream_request = adapter.build_request(
            &replay_canonical,
            &target.provider_base_url,
            upstream_key.as_deref(),
        );
        url = join_url(&target.provider_base_url, &upstream_request.path);

        let method = upstream_request
            .method
            .parse()
            .map_err(|_| Error::new(ErrorKind::InternalError, "Invalid upstream HTTP method"))?;
        let mut retry_builder = client.request(method, url);
        for (name, value) in upstream_request.headers {
            retry_builder = retry_builder.header(name, value);
        }

        upstream_response = match retry_builder.json(&upstream_request.body).send().await {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_reqwest_error(error);
                let latency_ms = request_start.elapsed().as_millis() as i64;
                let _ = store_log(
                    &state.db,
                    &canonical.request_id,
                    attempt_context.downstream_key_hash,
                    Some(&route.downstream_model),
                    Some(&route.route_id),
                    Some(&target.target_id),
                    Some(&target.provider_id),
                    Some(&target.upstream_model),
                    Some(resolved.wire_api.as_str()),
                    None,
                    Some(&mapped.kind.to_string()),
                    Some(latency_ms),
                    None,
                    None,
                )
                .await;
                attempt_context.routing_attempts.push(build_routing_attempt(
                    target,
                    &resolved,
                    "failed",
                    Some(mapped.kind),
                    Some(latency_ms as u64),
                ));
                return Err(mapped);
            }
        };
        status = upstream_response.status();
        bytes = match upstream_response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                let mapped = map_reqwest_error(error);
                let latency_ms = request_start.elapsed().as_millis() as i64;
                let _ = store_log(
                    &state.db,
                    &canonical.request_id,
                    attempt_context.downstream_key_hash,
                    Some(&route.downstream_model),
                    Some(&route.route_id),
                    Some(&target.target_id),
                    Some(&target.provider_id),
                    Some(&target.upstream_model),
                    Some(resolved.wire_api.as_str()),
                    None,
                    Some(&mapped.kind.to_string()),
                    Some(latency_ms),
                    None,
                    None,
                )
                .await;
                attempt_context.routing_attempts.push(build_routing_attempt(
                    target,
                    &resolved,
                    "failed",
                    Some(mapped.kind),
                    Some(latency_ms as u64),
                ));
                return Err(mapped);
            }
        };
    }

    if !status.is_success() {
        let mapped = map_upstream_status(status.as_u16(), &bytes);
        // Log failed request for audit
        let latency_ms = request_start.elapsed().as_millis() as i64;
        let _ = store_log(
            &state.db,
            &canonical.request_id,
            attempt_context.downstream_key_hash,
            Some(&route.downstream_model),
            Some(&route.route_id),
            Some(&target.target_id),
            Some(&target.provider_id),
            Some(&target.upstream_model),
            Some(resolved.wire_api.as_str()),
            Some(status.as_u16() as i32),
            Some("upstream_error"),
            Some(latency_ms),
            None,
            None,
        )
        .await;
        attempt_context.routing_attempts.push(build_routing_attempt(
            target,
            &resolved,
            "failed",
            Some(mapped.kind),
            Some(latency_ms as u64),
        ));
        return Err(mapped);
    }

    let events = match adapter.parse_response(&bytes).await {
        Ok(events) => events,
        Err(error) => {
            let mapped = map_adapter_error(error);
            let latency_ms = request_start.elapsed().as_millis() as i64;
            let _ = store_log(
                &state.db,
                &canonical.request_id,
                attempt_context.downstream_key_hash,
                Some(&route.downstream_model),
                Some(&route.route_id),
                Some(&target.target_id),
                Some(&target.provider_id),
                Some(&target.upstream_model),
                Some(resolved.wire_api.as_str()),
                None,
                Some(&mapped.kind.to_string()),
                Some(latency_ms),
                None,
                None,
            )
            .await;
            attempt_context.routing_attempts.push(build_routing_attempt(
                target,
                &resolved,
                "failed",
                Some(mapped.kind),
                Some(latency_ms as u64),
            ));
            return Err(mapped);
        }
    };
    let upstream_response_id = extract_upstream_response_id(&events);
    let response = normalize_downstream_response(&route.downstream_model, events)?;

    persist_response_shell(
        state,
        &canonical.request_id,
        route,
        target,
        &resolved,
        &response,
        PersistHints {
            upstream_response_id: upstream_response_id.as_deref(),
            previous_response_id: canonical.previous_response_id.as_deref(),
        },
    )
    .await;

    // Log the request for audit/analytics (redacted - no secrets)
    let usage = response.usage.as_ref();
    let _ = store_log(
        &state.db,
        &canonical.request_id,
        attempt_context.downstream_key_hash,
        Some(&route.downstream_model),
        Some(&route.route_id),
        Some(&target.target_id),
        Some(&target.provider_id),
        Some(&target.upstream_model),
        Some(resolved.wire_api.as_str()),
        Some(200), // success
        None,      // no error
        None,      // latency not tracked here
        usage.map(|u| u.input_tokens as i64),
        usage.map(|u| u.output_tokens as i64),
    )
    .await;

    let success_latency_ms = request_start.elapsed().as_millis() as u64;
    attempt_context.routing_attempts.push(build_routing_attempt(
        target,
        &resolved,
        "success",
        None,
        Some(success_latency_ms),
    ));
    let winning_attempt_index = attempt_context.routing_attempts.len().saturating_sub(1);

    // Archive conversation if enabled (async, non-blocking)
    if let Err(error) = archive_successful_response(
        state,
        &canonical,
        route,
        target,
        &resolved,
        &response,
        ArchivePersistHints {
            upstream_response_id: upstream_response_id.as_deref(),
            capture_mode_override: attempt_context.archive_capture_mode_override,
            routing_attempts: attempt_context.routing_attempts,
            winning_attempt_index,
        },
    )
    .await
    {
        warn!(
            request_id = %canonical.request_id,
            response_id = %response.id,
            route_id = %route.route_id,
            target_id = %target.target_id,
            error = %error,
            "Archive write failed; continuing without blocking response"
        );
    }

    Ok(response)
}

async fn try_target_streaming(
    state: &ServerState,
    route: &RouteSnapshot,
    target: &TargetSnapshot,
    canonical: CanonicalResponseRequest,
    target_context: TargetCallContext,
) -> Result<StreamingRelayResult, Error> {
    let mut canonical = canonical;
    canonical.stream = true;
    let TargetCallContext {
        upstream_key,
        resolved,
    } = target_context;
    let adapter: Box<dyn UpstreamAdapter> = adapter_for_wire_api(resolved.wire_api);
    let upstream_request = adapter.build_request(
        &canonical,
        &target.provider_base_url,
        upstream_key.as_deref(),
    );
    let url = join_url(&target.provider_base_url, &upstream_request.path);

    let client = build_upstream_client(state.config.server.upstream_timeout_secs)?;

    let method = upstream_request
        .method
        .parse()
        .map_err(|_| Error::new(ErrorKind::InternalError, "Invalid upstream HTTP method"))?;
    let mut builder = client.request(method, url);
    for (name, value) in upstream_request.headers {
        builder = builder.header(name, value);
    }

    let upstream_response = builder
        .json(&upstream_request.body)
        .send()
        .await
        .map_err(map_reqwest_error)?;

    if !upstream_response.status().is_success() {
        let status = upstream_response.status().as_u16();
        let bytes = upstream_response.bytes().await.map_err(map_reqwest_error)?;
        // Log failed streaming request for audit
        let _ = store_log(
            &state.db,
            &canonical.request_id,
            None,
            Some(&route.downstream_model),
            Some(&route.route_id),
            Some(&target.target_id),
            Some(&target.provider_id),
            Some(&target.upstream_model),
            Some(resolved.wire_api.as_str()),
            Some(status as i32),
            Some("upstream_error"),
            None, // latency
            None, // input_tokens
            None, // output_tokens
        )
        .await;
        return Err(map_upstream_status(status, &bytes));
    }

    let mut upstream_body = upstream_response.bytes_stream();
    let mut parse_buffer = bytes::BytesMut::new();
    let mut sse_writer = SseWriter::new();
    let mut emitted_any_semantic = false;
    let mut collected_events: Vec<CanonicalEvent> = Vec::new();
    let mut fallback_after_commit_error: Option<Error> = None;
    let stream_started_at = Instant::now();
    let mut last_activity_at = stream_started_at;
    let idle_timeout = duration_from_secs_option(state.config.server.stream_idle_timeout_secs);
    let max_duration = duration_from_secs_option(state.config.server.max_stream_duration_secs);

    loop {
        let deadline = next_stream_deadline(
            stream_started_at,
            last_activity_at,
            idle_timeout,
            max_duration,
        );

        let next_chunk = if let Some((wait_for, timeout_kind)) = deadline {
            match tokio::time::timeout(wait_for, futures::StreamExt::next(&mut upstream_body)).await
            {
                Ok(chunk) => chunk,
                Err(_) => {
                    let timeout_error = stream_timeout_error(timeout_kind, state);
                    if emitted_any_semantic {
                        fallback_after_commit_error = Some(timeout_error);
                        break;
                    }
                    return Err(timeout_error);
                }
            }
        } else {
            futures::StreamExt::next(&mut upstream_body).await
        };

        let Some(next_chunk) = next_chunk else {
            break;
        };

        let chunk = match next_chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let mapped = map_reqwest_error(error);
                if emitted_any_semantic {
                    fallback_after_commit_error = Some(mapped);
                    break;
                }
                return Err(mapped);
            }
        };
        last_activity_at = Instant::now();
        let frames = extract_sse_frames(&mut parse_buffer, &chunk);

        for frame in frames {
            let parsed = parse_raw_sse_frame(&*adapter, frame);
            let parsed_event = match parsed {
                Ok(event) => event,
                Err(error) => {
                    if emitted_any_semantic {
                        fallback_after_commit_error = Some(error);
                        break;
                    }
                    return Err(error);
                }
            };
            let Some(event) = parsed_event else {
                continue;
            };
            let is_semantic = matches!(
                event,
                CanonicalEvent::ResponseCreated { .. }
                    | CanonicalEvent::OutputItemAdded { .. }
                    | CanonicalEvent::OutputTextDelta { .. }
                    | CanonicalEvent::FunctionCallArgumentsDelta { .. }
                    | CanonicalEvent::OutputItemDone { .. }
                    | CanonicalEvent::ReasoningSummaryDelta { .. }
                    | CanonicalEvent::ResponseCompleted { .. }
                    | CanonicalEvent::ResponseFailed { .. }
            );
            if is_semantic {
                emitted_any_semantic = true;
            }

            let (event_type, payload) = canonical_to_sse(&event);
            if event_type != SseEventType::Unknown {
                sse_writer.write_event(event_type, &payload);
            }
            collected_events.push(event);
        }
        if fallback_after_commit_error.is_some() {
            break;
        }
    }

    if !emitted_any_semantic {
        return Err(Error::new(
            ErrorKind::UpstreamUnavailable,
            "Upstream stream ended before first semantic event",
        ));
    }

    if let Some(error) = fallback_after_commit_error.take() {
        let payload = serde_json::json!({
            "error": {
                "message": error.message,
                "type": error.kind.to_string(),
                "code": error.kind.to_string(),
            }
        });
        sse_writer.write_event(SseEventType::ResponseFailed, &payload);
    }

    let downstream =
        normalize_downstream_response(&route.downstream_model, collected_events.clone())?;
    let upstream_response_id = extract_upstream_response_id(&collected_events);
    persist_response_shell(
        state,
        &canonical.request_id,
        route,
        target,
        &resolved,
        &downstream,
        PersistHints {
            upstream_response_id: upstream_response_id.as_deref(),
            previous_response_id: canonical.previous_response_id.as_deref(),
        },
    )
    .await;

    let mut frames = Vec::new();
    let bytes = sse_writer.flush();
    if !bytes.is_empty() {
        frames.push(bytes);
    }
    Ok(StreamingRelayResult { sse_frames: frames })
}

fn duration_from_secs_option(seconds: u64) -> Option<Duration> {
    if seconds == 0 {
        None
    } else {
        Some(Duration::from_secs(seconds))
    }
}

fn saturating_remaining(limit: Duration, elapsed: Duration) -> Duration {
    if elapsed >= limit {
        Duration::ZERO
    } else {
        limit - elapsed
    }
}

fn next_stream_deadline(
    stream_started_at: Instant,
    last_activity_at: Instant,
    idle_timeout: Option<Duration>,
    max_duration: Option<Duration>,
) -> Option<(Duration, StreamTimeoutKind)> {
    let now = Instant::now();
    let idle_remaining = idle_timeout
        .map(|limit| saturating_remaining(limit, now.saturating_duration_since(last_activity_at)));
    let max_remaining = max_duration
        .map(|limit| saturating_remaining(limit, now.saturating_duration_since(stream_started_at)));

    match (idle_remaining, max_remaining) {
        (Some(idle), Some(max)) => {
            if max <= idle {
                Some((max, StreamTimeoutKind::MaxDuration))
            } else {
                Some((idle, StreamTimeoutKind::Idle))
            }
        }
        (Some(idle), None) => Some((idle, StreamTimeoutKind::Idle)),
        (None, Some(max)) => Some((max, StreamTimeoutKind::MaxDuration)),
        (None, None) => None,
    }
}

fn stream_timeout_error(kind: StreamTimeoutKind, state: &ServerState) -> Error {
    match kind {
        StreamTimeoutKind::Idle => Error::new(
            ErrorKind::UpstreamTimeout,
            format!(
                "Upstream stream idle timeout after {}s",
                state.config.server.stream_idle_timeout_secs
            ),
        ),
        StreamTimeoutKind::MaxDuration => Error::new(
            ErrorKind::UpstreamTimeout,
            format!(
                "Upstream stream exceeded max duration {}s",
                state.config.server.max_stream_duration_secs
            ),
        ),
    }
}

fn adapter_for_wire_api(wire_api: WireApi) -> Box<dyn UpstreamAdapter> {
    match wire_api {
        WireApi::Responses => Box::new(ResponsesAdapter::new()),
        WireApi::OpenAiChat => Box::new(OpenAiChatAdapter::new()),
        WireApi::Anthropic => Box::new(AnthropicAdapter::new()),
        WireApi::Auto => Box::new(ResponsesAdapter::new()),
    }
}

async fn load_compaction_source_state(
    state: &ServerState,
    raw_json: &serde_json::Value,
) -> Result<Option<CompactionSourceState>, Error> {
    let source_response_id = raw_json
        .get("previous_response_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            raw_json
                .get("response_id")
                .and_then(serde_json::Value::as_str)
        });

    let Some(source_response_id) = source_response_id else {
        return Ok(None);
    };

    let Some(record) = get_response(&state.db, source_response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to load compact source response state: {error}"),
            )
        })?
    else {
        return Err(Error::new(
            ErrorKind::StateNotFound,
            format!("Compaction source response '{source_response_id}' was not found"),
        ));
    };

    let Some(provider_id) = record.provider_id else {
        return Err(Error::new(
            ErrorKind::StateNotContinuable,
            format!(
                "Compaction source response '{}' is missing provider lineage",
                record.id
            ),
        ));
    };

    Ok(Some(CompactionSourceState {
        response_id: record.id,
        provider_id,
        upstream_model: record.upstream_model,
        state_scope: record.state_scope,
    }))
}

fn target_matches_compaction_source(
    target: &TargetSnapshot,
    source_state: Option<&CompactionSourceState>,
) -> bool {
    let Some(source_state) = source_state else {
        return true;
    };

    if target.provider_id != source_state.provider_id {
        return false;
    }
    if target.state_scope != source_state.state_scope {
        return false;
    }
    if let Some(source_model) = source_state.upstream_model.as_deref() {
        if target.upstream_model != source_model {
            return false;
        }
    }
    true
}

async fn try_target_compact(
    state: &ServerState,
    route: &RouteSnapshot,
    target: &TargetSnapshot,
    source_state: Option<&CompactionSourceState>,
    raw_json: &serde_json::Value,
    upstream_key: Option<&str>,
    request_id: &str,
) -> Result<serde_json::Value, Error> {
    let url = join_url(&target.provider_base_url, "/responses/compact");
    let client = build_upstream_client(state.config.server.upstream_timeout_secs)?;

    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .json(raw_json);
    if let Some(key) = upstream_key {
        request = request.header("authorization", format!("Bearer {key}"));
    }

    let response = request.send().await.map_err(map_reqwest_error)?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await.map_err(map_reqwest_error)?;
    if !(200..300).contains(&status) {
        // Extract downstream_model from raw_json for logging
        let downstream_model = raw_json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Log failed compact request for audit (not critical, so we don't block on error)
        let _ = store_log(
            &state.db,
            request_id,
            None,                         // downstream_key_hash
            Some(downstream_model),       // downstream_model
            None,                         // route_id (not available in compact context)
            Some(&target.target_id),      // target_id
            Some(&target.provider_id),    // provider_id
            Some(&target.upstream_model), // upstream_model
            Some("responses"),            // wire_api
            Some(status as i32),          // status_code
            Some("compact_error"),        // error_kind
            None,                         // latency_ms
            None,                         // input_tokens
            None,                         // output_tokens
        )
        .await;
        return Err(map_upstream_status(status, &bytes));
    }

    let parsed = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
        Error::new(
            ErrorKind::UpstreamUnavailable,
            format!("Compact endpoint returned invalid JSON: {error}"),
        )
    })?;

    let source_response_ids_json = if let Some(source) = source_state {
        serde_json::json!([source.response_id.clone()]).to_string()
    } else {
        "[]".to_string()
    };
    let _ = store_compaction_lineage(
        &state.db,
        &CompactionLineageInsert {
            id: &format!("clg_{}", uuid::Uuid::now_v7()),
            request_id,
            route_id: Some(&route.route_id),
            downstream_model: &route.downstream_model,
            source_response_ids_json: &source_response_ids_json,
            provider_id: Some(&target.provider_id),
            upstream_model: Some(&target.upstream_model),
            state_scope: target.state_scope.as_deref(),
            method: "native_responses",
            provider_native: true,
            summarizer_model: None,
            prompt_version: None,
            source_tokens: None,
            summary_tokens: None,
        },
    )
    .await;

    Ok(parsed)
}

async fn perform_local_summary_compact(
    state: &ServerState,
    route: &RouteSnapshot,
    source_state: Option<&CompactionSourceState>,
    request_id: &str,
) -> Result<serde_json::Value, Error> {
    let Some(source_state) = source_state else {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "local_summary compaction requires response_id or previous_response_id",
        ));
    };

    let items = get_items(&state.db, &source_state.response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to load source response items for local summary: {error}"),
            )
        })?;

    let mut transcript = build_visible_transcript_for_summary(&items);
    if transcript.trim().is_empty() {
        transcript = "No visible transcript available for local summary.".to_string();
    }

    let source_tokens = approximate_tokens(&transcript);
    let max_chars = state.config.server.local_summary_max_chars.max(1);
    if transcript.chars().count() > max_chars {
        transcript = transcript.chars().take(max_chars).collect::<String>();
    }
    let summary_tokens = approximate_tokens(&transcript);

    let summarizer_model = state
        .config
        .server
        .local_summary_model
        .clone()
        .unwrap_or_else(|| "modelwire-local-summary".to_string());
    let prompt_version = state
        .config
        .server
        .local_summary_prompt_version
        .clone()
        .unwrap_or_else(|| "v1".to_string());

    let lineage_id = format!("clg_{}", uuid::Uuid::now_v7());
    let source_response_ids_json =
        serde_json::json!([source_state.response_id.clone()]).to_string();
    store_compaction_lineage(
        &state.db,
        &CompactionLineageInsert {
            id: &lineage_id,
            request_id,
            route_id: Some(&route.route_id),
            downstream_model: &route.downstream_model,
            source_response_ids_json: &source_response_ids_json,
            provider_id: Some(&source_state.provider_id),
            upstream_model: source_state.upstream_model.as_deref(),
            state_scope: source_state.state_scope.as_deref(),
            method: "local_summary",
            provider_native: false,
            summarizer_model: Some(&summarizer_model),
            prompt_version: Some(&prompt_version),
            source_tokens: Some(source_tokens),
            summary_tokens: Some(summary_tokens),
        },
    )
    .await
    .map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to persist local summary lineage: {error}"),
        )
    })?;

    let response = serde_json::json!({
        "id": format!("cmp_mw_{}", uuid::Uuid::now_v7()),
        "object": "response.compaction",
        "status": "completed",
        "model": route.downstream_model,
        "method": "local_summary",
        "provider_native": false,
        "lineage_id": lineage_id,
        "source_response_ids": [source_state.response_id.clone()],
        "summary": {
            "type": "modelwire_local_summary",
            "text": transcript,
            "summarizer_model": summarizer_model,
            "prompt_version": prompt_version
        },
        "usage": {
            "input_tokens": source_tokens,
            "output_tokens": summary_tokens,
            "total_tokens": source_tokens + summary_tokens
        }
    });
    Ok(response)
}

fn build_visible_transcript_for_summary(items: &[ItemRecord]) -> String {
    let mut text = String::new();
    for item in items {
        match item.item_type.as_str() {
            "message" if item.visible != 0 => {
                let value: serde_json::Value =
                    serde_json::from_str(&item.content_json).unwrap_or(serde_json::Value::Null);
                if let Some(blocks) = value.as_array() {
                    for block in blocks {
                        if let Some(line) = block.get("text").and_then(serde_json::Value::as_str) {
                            text.push_str(line);
                            text.push('\n');
                        }
                    }
                }
            }
            "function_call" => {
                let value: serde_json::Value =
                    serde_json::from_str(&item.content_json).unwrap_or(serde_json::Value::Null);
                let name = value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("tool");
                let args = value
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("{}");
                text.push_str(&format!("tool {name}: {args}\n"));
            }
            _ => {}
        }
    }
    text
}

fn approximate_tokens(text: &str) -> i64 {
    let chars = text.chars().count() as i64;
    ((chars + 3) / 4).max(1)
}

fn parse_canonical_request(
    request_id: &str,
    raw: &serde_json::Value,
    downstream_model: &str,
    upstream_model: &str,
) -> Result<CanonicalResponseRequest, Error> {
    let input = parse_input(raw.get("input"))?;
    let tools = parse_tools(raw.get("tools"))?;

    Ok(CanonicalResponseRequest {
        request_id: request_id.to_string(),
        downstream_model: downstream_model.to_string(),
        upstream_model: upstream_model.to_string(),
        instructions: parse_instructions(raw.get("instructions"))?,
        input,
        previous_response_id: raw
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        tools,
        tool_choice: parse_tool_choice(raw.get("tool_choice"))?,
        parallel_tool_calls: raw
            .get("parallel_tool_calls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        max_output_tokens: raw
            .get("max_output_tokens")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as u32),
        temperature: raw
            .get("temperature")
            .and_then(serde_json::Value::as_f64)
            .map(|value| value as f32),
        top_p: raw
            .get("top_p")
            .and_then(serde_json::Value::as_f64)
            .map(|value| value as f32),
        stream: raw
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        reasoning: parse_reasoning(raw.get("reasoning"))?,
        include: raw
            .get("include")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        metadata: raw
            .get("metadata")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        store: raw
            .get("store")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        raw_downstream: raw.clone(),
    })
}

fn parse_input(input: Option<&serde_json::Value>) -> Result<Vec<CanonicalInputItem>, Error> {
    let Some(input) = input else {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Missing 'input' field in request",
        ));
    };

    if let Some(text) = input.as_str() {
        return Ok(vec![CanonicalInputItem::Text {
            content: text.to_string(),
        }]);
    }

    let Some(items) = input.as_array() else {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "'input' must be a string or an array",
        ));
    };

    let mut canonical = Vec::with_capacity(items.len());
    for item in items {
        let item_type = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("message");

        match item_type {
            "message" => {
                let role = item
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("user")
                    .to_string();
                canonical.push(CanonicalInputItem::Message {
                    role,
                    content: parse_content_blocks(item.get("content"))?,
                });
            }
            "function_call_output" => {
                canonical.push(CanonicalInputItem::FunctionCallOutput {
                    call_id: require_string(item, "call_id")?.to_string(),
                    output: require_string(item, "output")?.to_string(),
                });
            }
            unsupported => {
                return Err(Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Unsupported input item type '{unsupported}'"),
                ));
            }
        }
    }

    Ok(canonical)
}

fn parse_content_blocks(content: Option<&serde_json::Value>) -> Result<Vec<ContentBlock>, Error> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };

    if let Some(text) = content.as_str() {
        return Ok(vec![ContentBlock::Text {
            text: text.to_string(),
        }]);
    }

    let Some(blocks) = content.as_array() else {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Message content must be a string or an array",
        ));
    };

    let mut canonical = Vec::with_capacity(blocks.len());
    for block in blocks {
        let block_type = block
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("input_text");
        match block_type {
            "input_text" | "output_text" | "text" => {
                canonical.push(ContentBlock::Text {
                    text: require_string(block, "text")?.to_string(),
                });
            }
            unsupported => {
                return Err(Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Unsupported content block type '{unsupported}'"),
                ));
            }
        }
    }

    Ok(canonical)
}

fn parse_instructions(
    instructions: Option<&serde_json::Value>,
) -> Result<Option<CanonicalInstructions>, Error> {
    let Some(instructions) = instructions else {
        return Ok(None);
    };

    if let Some(content) = instructions.as_str() {
        return Ok(Some(CanonicalInstructions {
            role: "developer".to_string(),
            content: content.to_string(),
        }));
    }

    if instructions.is_object() {
        return Ok(Some(CanonicalInstructions {
            role: instructions
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("developer")
                .to_string(),
            content: require_string(instructions, "content")?.to_string(),
        }));
    }

    Err(Error::new(
        ErrorKind::RequestInvalid,
        "'instructions' must be a string or object",
    ))
}

fn parse_tools(tools: Option<&serde_json::Value>) -> Result<Vec<CanonicalTool>, Error> {
    let Some(tools) = tools else {
        return Ok(Vec::new());
    };

    let Some(items) = tools.as_array() else {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "'tools' must be an array",
        ));
    };

    let mut parsed = Vec::with_capacity(items.len());
    let mut names = HashSet::new();
    for tool in items {
        let tool_type = tool
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("function");
        if tool_type != "function" {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!("Unsupported tool type '{tool_type}'"),
            ));
        }

        let function = tool.get("function").unwrap_or(tool);
        let name = require_string(function, "name")?.to_string();
        if !names.insert(name.clone()) {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!("Duplicate tool name '{name}'"),
            ));
        }
        parsed.push(CanonicalTool {
            name,
            description: function
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            parameters: function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
        });
    }

    Ok(parsed)
}

fn parse_tool_choice(choice: Option<&serde_json::Value>) -> Result<CanonicalToolChoice, Error> {
    let Some(choice) = choice else {
        return Ok(CanonicalToolChoice::Auto);
    };

    if let Some(value) = choice.as_str() {
        return match value {
            "auto" => Ok(CanonicalToolChoice::Auto),
            "none" => Ok(CanonicalToolChoice::None),
            other => Ok(CanonicalToolChoice::Specific(other.to_string())),
        };
    }

    if choice.is_object() {
        let function = choice.get("function").unwrap_or(choice);
        return Ok(CanonicalToolChoice::Specific(
            require_string(function, "name")?.to_string(),
        ));
    }

    Err(Error::new(
        ErrorKind::RequestInvalid,
        "'tool_choice' must be a string or object",
    ))
}

fn parse_reasoning(
    reasoning: Option<&serde_json::Value>,
) -> Result<Option<CanonicalReasoningOptions>, Error> {
    let Some(reasoning) = reasoning else {
        return Ok(None);
    };

    if !reasoning.is_object() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "'reasoning' must be an object",
        ));
    }

    Ok(Some(CanonicalReasoningOptions {
        include_summary: reasoning
            .get("include_summary")
            .and_then(serde_json::Value::as_bool),
        include_encrypted_content: reasoning
            .get("include_encrypted_content")
            .and_then(serde_json::Value::as_bool),
        effort: reasoning
            .get("effort")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    }))
}

fn context_guard_check(
    target: &TargetSnapshot,
    canonical: &CanonicalResponseRequest,
) -> Result<(), Error> {
    let Some(window) = target.context_window_tokens else {
        return Ok(());
    };
    let margin = target.context_safety_margin_tokens.unwrap_or(2048);
    let safe_budget = window.saturating_sub(margin);
    let estimated = estimate_request_tokens(canonical);
    let requested_output = canonical
        .max_output_tokens
        .or(target.max_output_tokens.map(|v| v as u32))
        .unwrap_or(0) as u64;
    let total = estimated.saturating_add(requested_output);

    if total > safe_budget {
        return Err(Error::new(
            ErrorKind::ContextLengthExceeded,
            format!(
                "Estimated token budget {} exceeds safe context budget {} for target '{}'",
                total, safe_budget, target.target_id
            ),
        ));
    }

    Ok(())
}

fn should_fallback_on_context_guard(target: &TargetSnapshot) -> bool {
    matches!(target.context_overflow_policy.as_str(), "fallback")
}

fn should_skip_target_for_tool_support(
    target: &TargetSnapshot,
    canonical: &CanonicalResponseRequest,
    resolved: &ResolvedTargetProtocol,
) -> bool {
    if canonical.tools.is_empty() {
        return false;
    }

    // Forced protocols are assumed operator-owned configuration.
    if target.configured_wire_api != WireApi::Auto {
        return false;
    }

    // Unknown support from text-only probes should not be used for tool-bearing requests.
    if !resolved.tool_support_known {
        return true;
    }

    !resolved.supports_tools
}

fn estimate_request_tokens(canonical: &CanonicalResponseRequest) -> u64 {
    let mut chars = 0usize;
    if let Some(instructions) = canonical.instructions.as_ref() {
        chars = chars.saturating_add(instructions.content.len());
    }

    for item in &canonical.input {
        match item {
            CanonicalInputItem::Text { content } => {
                chars = chars.saturating_add(content.len());
            }
            CanonicalInputItem::Message { content, .. } => {
                for block in content {
                    match block {
                        ContentBlock::Text { text } => {
                            chars = chars.saturating_add(text.len());
                        }
                        ContentBlock::InputJson { json } => {
                            chars = chars.saturating_add(json.len());
                        }
                        ContentBlock::Reasoning { summary, .. } => {
                            for part in summary {
                                if let Some(text) = part.text.as_ref() {
                                    chars = chars.saturating_add(text.len());
                                }
                            }
                        }
                        ContentBlock::Image { data, .. } => {
                            chars = chars.saturating_add(data.len());
                        }
                    }
                }
            }
            CanonicalInputItem::FunctionCallOutput { output, .. } => {
                chars = chars.saturating_add(output.len());
            }
        }
    }

    for tool in &canonical.tools {
        chars = chars.saturating_add(tool.name.len());
        chars = chars.saturating_add(tool.description.len());
        chars = chars.saturating_add(tool.parameters.to_string().len());
    }

    let approx_tokens = (chars as u64).saturating_add(3) / 4;
    approx_tokens.max(1)
}

fn normalize_downstream_response(
    downstream_model: &str,
    events: Vec<CanonicalEvent>,
) -> Result<DownstreamResponse, Error> {
    let response_id = modelwire_core::generate_response_id();
    let mut output = Vec::new();
    let mut usage = None;

    for event in events {
        match event {
            CanonicalEvent::OutputItemDone { item, .. } => {
                output.push(normalize_output_item(item));
            }
            CanonicalEvent::ResponseCompleted {
                usage: completed_usage,
                ..
            } => {
                usage = completed_usage;
            }
            _ => {}
        }
    }

    Ok(DownstreamResponse {
        id: response_id,
        object: "response",
        created_at: chrono::Utc::now().timestamp(),
        model: downstream_model.to_string(),
        status: "completed",
        output,
        usage,
    })
}

fn normalize_output_item(item: CanonicalOutputItem) -> DownstreamOutputItem {
    match item {
        CanonicalOutputItem::Message { role, content, .. } => {
            let content = content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(DownstreamContentBlock::OutputText {
                        text,
                        annotations: Vec::new(),
                    }),
                    _ => None,
                })
                .collect();

            DownstreamOutputItem::Message {
                id: modelwire_core::generate_message_id(),
                status: "completed",
                role,
                content,
            }
        }
        CanonicalOutputItem::FunctionCall {
            name, arguments, ..
        } => {
            let call_id = modelwire_core::generate_call_id();
            DownstreamOutputItem::FunctionCall {
                id: modelwire_core::generate_call_id(),
                call_id,
                name,
                arguments,
                status: "completed",
            }
        }
        CanonicalOutputItem::Reasoning { summary, .. } => DownstreamOutputItem::Reasoning {
            id: modelwire_core::generate_message_id(),
            summary: summary
                .into_iter()
                .map(|part| serde_json::to_value(part).unwrap_or(serde_json::Value::Null))
                .collect(),
        },
    }
}

async fn persist_response_shell(
    state: &ServerState,
    request_id: &str,
    route: &RouteSnapshot,
    target: &TargetSnapshot,
    resolved: &ResolvedTargetProtocol,
    response: &DownstreamResponse,
    hints: PersistHints<'_>,
) {
    let usage_json = response
        .usage
        .as_ref()
        .and_then(|usage| serde_json::to_string(usage).ok());

    if let Err(error) = modelwire_db::repo::responses::store_response_metadata(
        &state.db,
        &ResponseInsert {
            id: &response.id,
            request_id,
            downstream_model: &route.downstream_model,
            route_id: Some(&route.route_id),
            target_id: Some(&target.target_id),
            provider_id: Some(&target.provider_id),
            upstream_model: Some(&target.upstream_model),
            wire_api: Some(resolved.wire_api.as_str()),
            upstream_response_id: hints.upstream_response_id,
            state_scope: target.state_scope.as_deref(),
            previous_response_id: hints.previous_response_id,
            status: response.status,
            usage_json: usage_json.as_deref(),
            error_json: None,
        },
    )
    .await
    {
        warn!(
            request_id = %request_id,
            response_id = %response.id,
            target_id = %target.target_id,
            error = %error,
            "Failed to persist response shell"
        );
        return;
    }

    if let Some(private_upstream_id) = hints.upstream_response_id {
        let handle_json = serde_json::json!({
            "upstream_response_id": private_upstream_id,
            "wire_api": resolved.wire_api.as_str(),
            "provider_id": target.provider_id,
            "upstream_model": target.upstream_model,
        })
        .to_string();

        if let Err(error) = store_upstream_handle(
            &state.db,
            &UpstreamHandleInsert {
                id: &format!("uh_{}", uuid::Uuid::now_v7()),
                modelwire_response_id: &response.id,
                provider_id: &target.provider_id,
                credential_hash: &resolved.credential_hash,
                upstream_model: &target.upstream_model,
                wire_api: resolved.wire_api.as_str(),
                state_scope: target.state_scope.as_deref(),
                upstream_response_id: Some(private_upstream_id),
                handle_json: &handle_json,
            },
        )
        .await
        {
            warn!(
                request_id = %request_id,
                response_id = %response.id,
                target_id = %target.target_id,
                error = %error,
                "Failed to persist upstream handle"
            );
        }
    }

    for (sequence, item) in response.output.iter().enumerate() {
        if let Err(error) = persist_response_item(state, &response.id, sequence as i64, item).await
        {
            warn!(
                request_id = %request_id,
                response_id = %response.id,
                target_id = %target.target_id,
                error = %error,
                "Failed to persist response item"
            );
        }
    }
}

async fn persist_response_item(
    state: &ServerState,
    response_id: &str,
    sequence: i64,
    item: &DownstreamOutputItem,
) -> Result<(), sqlx::Error> {
    match item {
        DownstreamOutputItem::Message {
            id, role, content, ..
        } => {
            let content_json = serde_json::to_string(content).unwrap_or_else(|_| "[]".to_string());
            modelwire_db::repo::responses::store_response_item(
                &state.db,
                &ResponseItemInsert {
                    id,
                    response_id,
                    sequence,
                    item_type: "message",
                    role: Some(role),
                    call_id: None,
                    content_json: &content_json,
                    visible: true,
                },
            )
            .await
        }
        DownstreamOutputItem::FunctionCall {
            id,
            call_id,
            name,
            arguments,
            ..
        } => {
            let content_json = serde_json::json!({
                "name": name,
                "arguments": arguments,
            })
            .to_string();
            modelwire_db::repo::responses::store_response_item(
                &state.db,
                &ResponseItemInsert {
                    id,
                    response_id,
                    sequence,
                    item_type: "function_call",
                    role: None,
                    call_id: Some(call_id),
                    content_json: &content_json,
                    visible: true,
                },
            )
            .await
        }
        DownstreamOutputItem::Reasoning { id, summary } => {
            let content_json = serde_json::to_string(summary).unwrap_or_else(|_| "[]".to_string());
            modelwire_db::repo::responses::store_response_item(
                &state.db,
                &ResponseItemInsert {
                    id,
                    response_id,
                    sequence,
                    item_type: "reasoning",
                    role: None,
                    call_id: None,
                    content_json: &content_json,
                    visible: false,
                },
            )
            .await
        }
    }
}

fn resolve_upstream_key(
    target: &TargetSnapshot,
    downstream_authorization: Option<&str>,
) -> Option<String> {
    match target.provider_auth_mode.as_str() {
        "managed" => target.provider_api_key.clone(),
        "pass_authorization" => downstream_authorization
            .and_then(strip_bearer)
            .map(ToOwned::to_owned)
            .or_else(|| target.provider_api_key.clone()),
        _ => target.provider_api_key.clone(),
    }
}

async fn resolve_upstream_key_checked(
    state: &ServerState,
    request_id: &str,
    route: &RouteSnapshot,
    target: &TargetSnapshot,
    downstream_model: &str,
    downstream_authorization: Option<&str>,
) -> Result<Option<String>, Error> {
    let upstream_key = resolve_upstream_key(target, downstream_authorization);
    if target.provider_auth_mode == "managed" && upstream_key.is_none() {
        let _ = store_log(
            &state.db,
            request_id,
            None,
            Some(downstream_model),
            Some(&route.route_id),
            Some(&target.target_id),
            Some(&target.provider_id),
            Some(&target.upstream_model),
            None,
            Some(500),
            Some("provider_key_missing"),
            None,
            None,
            None,
        )
        .await;

        return Err(Error::new(
            ErrorKind::InternalError,
            "Managed provider key is missing",
        ));
    }
    Ok(upstream_key)
}

fn strip_bearer(value: &str) -> Option<&str> {
    value.strip_prefix("Bearer ")
}

fn join_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn build_upstream_client(timeout_secs: u64) -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to build upstream client: {error}"),
            )
        })
}

fn require_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, Error> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::RequestInvalid,
                format!("Missing or invalid string field '{field}'"),
            )
        })
}

fn map_reqwest_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::new(ErrorKind::UpstreamTimeout, "Upstream request timed out")
    } else {
        Error::new(
            ErrorKind::UpstreamUnavailable,
            format!("Upstream request failed: {error}"),
        )
    }
}

fn map_upstream_status(status: u16, body: &[u8]) -> Error {
    let message = redacted_upstream_error(body);
    match status {
        401 | 403 => Error::new(ErrorKind::AuthFailed, message),
        429 => Error::new(ErrorKind::RateLimited, message),
        504 => Error::new(ErrorKind::UpstreamTimeout, message),
        500 | 502 | 503 => Error::new(ErrorKind::UpstreamUnavailable, message),
        400 => Error::new(ErrorKind::RequestInvalid, message),
        _ if status >= 500 => Error::new(ErrorKind::UpstreamUnavailable, message),
        _ => Error::new(ErrorKind::UpstreamUnavailable, message),
    }
}

fn map_adapter_error(error: UpstreamError) -> Error {
    match error {
        UpstreamError::HttpError { status, message } => {
            map_upstream_status(status, message.as_bytes())
        }
        UpstreamError::Timeout => {
            Error::new(ErrorKind::UpstreamTimeout, "Upstream adapter timed out")
        }
        UpstreamError::ConnectionError(message) => {
            Error::new(ErrorKind::UpstreamUnavailable, message)
        }
        UpstreamError::ProtocolNotSupported => Error::new(
            ErrorKind::ProtocolNotSupported,
            "Upstream protocol is not supported by this target",
        ),
        UpstreamError::ParseError(message) | UpstreamError::InvalidResponse(message) => {
            Error::new(ErrorKind::UpstreamUnavailable, message)
        }
    }
}

fn redacted_upstream_error(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "Upstream returned an error".to_string()
    } else {
        trimmed.chars().take(512).collect()
    }
}

async fn resolve_target_protocol(
    state: &ServerState,
    target: &TargetSnapshot,
    upstream_key: Option<&str>,
) -> Result<ResolvedTargetProtocol, Error> {
    let credential_hash = credential_hash_for_probe(upstream_key, &target.provider_id);

    if target.configured_wire_api != WireApi::Auto {
        remember_forced_probe_visibility(
            state,
            target,
            &credential_hash,
            target.configured_wire_api,
        )
        .await;
        return Ok(ResolvedTargetProtocol {
            wire_api: target.configured_wire_api,
            credential_hash,
            tool_support_known: false,
            supports_tools: false,
        });
    }

    let cache_key = probe_cache_key(
        &target.provider_id,
        &credential_hash,
        &target.upstream_model,
    );
    if let Some(probe) = load_probe_from_memory_cache(state, &cache_key) {
        return Ok(ResolvedTargetProtocol {
            wire_api: probe.wire_api,
            credential_hash,
            tool_support_known: probe.tool_support_known,
            supports_tools: probe.supports_tools,
        });
    }

    if let Some(probe) = load_probe_from_db(state, target, &credential_hash, &cache_key).await? {
        return Ok(ResolvedTargetProtocol {
            wire_api: probe.wire_api,
            credential_hash,
            tool_support_known: probe.tool_support_known,
            supports_tools: probe.supports_tools,
        });
    }

    let probe_lock = state
        .probe_locks
        .entry(cache_key.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = probe_lock.lock().await;

    // Re-check after lock acquisition so concurrent identical probes collapse to single-flight.
    if let Some(probe) = load_probe_from_memory_cache(state, &cache_key) {
        return Ok(ResolvedTargetProtocol {
            wire_api: probe.wire_api,
            credential_hash,
            tool_support_known: probe.tool_support_known,
            supports_tools: probe.supports_tools,
        });
    }
    if let Some(probe) = load_probe_from_db(state, target, &credential_hash, &cache_key).await? {
        return Ok(ResolvedTargetProtocol {
            wire_api: probe.wire_api,
            credential_hash,
            tool_support_known: probe.tool_support_known,
            supports_tools: probe.supports_tools,
        });
    }

    let result = probe_wire_api(state, target, upstream_key, &credential_hash, &cache_key).await;

    drop(_guard);
    // Always remove stale lock entry; in-flight waiters hold Arc clones safely.
    state.probe_locks.remove(&cache_key);

    result
}

fn load_probe_from_memory_cache(state: &ServerState, cache_key: &str) -> Option<ProbeResult> {
    if let Some(entry) = state.probe_cache.get(cache_key) {
        let value = entry.value().clone();
        if value.expires_at > chrono::Utc::now().timestamp() {
            return Some(value);
        }
        state.probe_cache.remove(cache_key);
    }
    None
}

async fn load_probe_from_db(
    state: &ServerState,
    target: &TargetSnapshot,
    credential_hash: &str,
    cache_key: &str,
) -> Result<Option<ProbeResult>, Error> {
    if let Some(row) = get_probe_result(
        &state.db,
        &target.provider_id,
        credential_hash,
        &target.upstream_model,
    )
    .await
    .map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to read probe cache: {error}"),
        )
    })? {
        if row.status == "success" {
            let Some(wire_api) = WireApi::parse(&row.wire_api) else {
                return Err(Error::new(
                    ErrorKind::InternalError,
                    "Probe cache contains unknown wire_api",
                ));
            };
            remember_probe(
                state,
                cache_key,
                &ProbeResult {
                    provider_id: target.provider_id.clone(),
                    credential_hash: credential_hash.to_string(),
                    upstream_model: target.upstream_model.clone(),
                    wire_api,
                    supports_streaming: row.supports_streaming.unwrap_or(0) != 0,
                    supports_tools: row.supports_tools.unwrap_or(0) != 0,
                    supports_parallel_tool_calls: row.supports_parallel_tool_calls.unwrap_or(0)
                        != 0,
                    tool_support_known: row.supports_tools.is_some(),
                    supports_previous_response_id: row.supports_previous_response_id.unwrap_or(0)
                        != 0,
                    supports_reasoning_encrypted_content: row
                        .supports_reasoning_encrypted_content
                        .unwrap_or(0)
                        != 0,
                    supports_reasoning_summary: row.supports_reasoning_summary.unwrap_or(0) != 0,
                    last_success_at: row
                        .last_success_at
                        .as_deref()
                        .and_then(parse_db_timestamp_to_unix),
                    last_failure_at: row
                        .last_failure_at
                        .as_deref()
                        .and_then(parse_db_timestamp_to_unix),
                    failure_kind: row.failure_kind.clone(),
                    failure_message_redacted: row.failure_message_redacted.clone(),
                    expires_at: parse_db_timestamp_to_unix(row.expires_at.as_str())
                        .unwrap_or_else(|| chrono::Utc::now().timestamp() + 3600),
                },
            );
            return Ok(Some(ProbeResult {
                provider_id: target.provider_id.clone(),
                credential_hash: credential_hash.to_string(),
                upstream_model: target.upstream_model.clone(),
                wire_api,
                supports_streaming: row.supports_streaming.unwrap_or(0) != 0,
                supports_tools: row.supports_tools.unwrap_or(0) != 0,
                supports_parallel_tool_calls: row.supports_parallel_tool_calls.unwrap_or(0) != 0,
                tool_support_known: row.supports_tools.is_some(),
                supports_previous_response_id: row.supports_previous_response_id.unwrap_or(0) != 0,
                supports_reasoning_encrypted_content: row
                    .supports_reasoning_encrypted_content
                    .unwrap_or(0)
                    != 0,
                supports_reasoning_summary: row.supports_reasoning_summary.unwrap_or(0) != 0,
                last_success_at: row
                    .last_success_at
                    .as_deref()
                    .and_then(parse_db_timestamp_to_unix),
                last_failure_at: row
                    .last_failure_at
                    .as_deref()
                    .and_then(parse_db_timestamp_to_unix),
                failure_kind: row.failure_kind.clone(),
                failure_message_redacted: row.failure_message_redacted.clone(),
                expires_at: parse_db_timestamp_to_unix(row.expires_at.as_str())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp() + 3600),
            }));
        }
    }
    Ok(None)
}

async fn probe_wire_api(
    state: &ServerState,
    target: &TargetSnapshot,
    upstream_key: Option<&str>,
    credential_hash: &str,
    cache_key: &str,
) -> Result<ResolvedTargetProtocol, Error> {
    let probe_body = serde_json::json!({
        "model": target.upstream_model,
        "input": "Reply with OK.",
        "max_output_tokens": 1,
        "stream": false
    });

    let mut last_retryable: Option<Error> = None;
    for candidate in [WireApi::Responses, WireApi::Anthropic, WireApi::OpenAiChat] {
        let probe_result =
            probe_candidate_once(state, target, candidate, upstream_key, &probe_body).await;

        match probe_result {
            ProbeAttemptResult::Supported => {
                let now_ts = chrono::Utc::now().timestamp();
                let probe = ProbeResult {
                    provider_id: target.provider_id.clone(),
                    credential_hash: credential_hash.to_string(),
                    upstream_model: target.upstream_model.clone(),
                    wire_api: candidate,
                    supports_streaming: false,
                    supports_tools: false,
                    supports_parallel_tool_calls: false,
                    tool_support_known: false,
                    supports_previous_response_id: false,
                    supports_reasoning_encrypted_content: false,
                    supports_reasoning_summary: false,
                    last_success_at: Some(now_ts),
                    last_failure_at: None,
                    failure_kind: None,
                    failure_message_redacted: None,
                    expires_at: now_ts + 3600,
                };
                store_probe_result_detailed(&state.db, &probe, "success")
                    .await
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::InternalError,
                            format!("Failed to store probe success: {error}"),
                        )
                    })?;

                remember_probe(state, cache_key, &probe);

                return Ok(ResolvedTargetProtocol {
                    wire_api: candidate,
                    credential_hash: credential_hash.to_string(),
                    tool_support_known: probe.tool_support_known,
                    supports_tools: probe.supports_tools,
                });
            }
            ProbeAttemptResult::ProtocolUnsupported => continue,
            ProbeAttemptResult::AuthError(error) => return Err(error),
            ProbeAttemptResult::RetryableFailure(error) => {
                last_retryable = Some(error);
                break;
            }
            ProbeAttemptResult::ModelInvalid(error) => return Err(error),
        }
    }

    if let Some(error) = last_retryable {
        return Err(error);
    }

    store_probe_result(
        &state.db,
        &target.provider_id,
        credential_hash,
        &target.upstream_model,
        WireApi::Auto.as_str(),
        "failed",
    )
    .await
    .ok();
    Err(Error::new(
        ErrorKind::ProtocolNotSupported,
        "No supported upstream protocol found for target",
    ))
}

enum ProbeAttemptResult {
    Supported,
    ProtocolUnsupported,
    AuthError(Error),
    RetryableFailure(Error),
    ModelInvalid(Error),
}

async fn probe_candidate_once(
    state: &ServerState,
    target: &TargetSnapshot,
    wire_api: WireApi,
    upstream_key: Option<&str>,
    probe_body: &serde_json::Value,
) -> ProbeAttemptResult {
    let path = match wire_api {
        WireApi::Responses => "/responses",
        WireApi::Anthropic => "/messages",
        WireApi::OpenAiChat => "/chat/completions",
        WireApi::Auto => "/responses",
    };
    let url = join_url(&target.provider_base_url, path);

    let request = build_upstream_client(state.config.server.upstream_timeout_secs).map(|client| {
        let mut builder = client.post(url);
        builder = builder.header("content-type", "application/json");
        if let Some(key) = upstream_key {
            builder = builder.header("authorization", format!("Bearer {key}"));
            if matches!(wire_api, WireApi::Anthropic) {
                builder = builder.header("x-api-key", key);
                builder = builder.header("anthropic-version", "2023-06-01");
            }
        }
        builder
    });

    let response = match request {
        Ok(builder) => builder.json(probe_body).send().await,
        Err(error) => return ProbeAttemptResult::RetryableFailure(error),
    };

    let response = match response {
        Ok(response) => response,
        Err(error) => return ProbeAttemptResult::RetryableFailure(map_reqwest_error(error)),
    };

    let status = response.status().as_u16();
    let body = response
        .bytes()
        .await
        .unwrap_or_else(|_| bytes::Bytes::from_static(b""));
    let message = redacted_upstream_error(&body);

    if (200..300).contains(&status) {
        return ProbeAttemptResult::Supported;
    }

    if is_protocol_unsupported_probe_status(status, &message) {
        return ProbeAttemptResult::ProtocolUnsupported;
    }

    if status == 401 || status == 403 {
        return ProbeAttemptResult::AuthError(Error::new(ErrorKind::AuthFailed, message));
    }
    if status == 400 && looks_like_model_invalid(&message) {
        return ProbeAttemptResult::ModelInvalid(Error::new(ErrorKind::ModelNotFound, message));
    }
    if status == 429 || status >= 500 {
        return ProbeAttemptResult::RetryableFailure(map_upstream_status(status, &body));
    }

    if status == 400 {
        return ProbeAttemptResult::ProtocolUnsupported;
    }

    ProbeAttemptResult::RetryableFailure(map_upstream_status(status, &body))
}

fn looks_like_model_invalid(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("model")
        && (lower.contains("not found")
            || lower.contains("does not exist")
            || lower.contains("unknown"))
}

fn is_protocol_unsupported_probe_status(status: u16, message: &str) -> bool {
    if matches!(status, 404 | 405 | 501) {
        return true;
    }
    if status != 400 {
        return false;
    }
    let lower = message.to_lowercase();
    lower.contains("unknown parameter")
        || lower.contains("unsupported endpoint")
        || lower.contains("not supported")
}

fn probe_cache_key(provider_id: &str, credential_hash: &str, upstream_model: &str) -> String {
    format!("{provider_id}:{credential_hash}:{upstream_model}")
}

fn credential_hash_for_probe(upstream_key: Option<&str>, provider_id: &str) -> String {
    match upstream_key {
        Some(key) => {
            let secret = format!("probe::{provider_id}");
            modelwire_core::hash_key_for_logging(key, &secret)
        }
        None => "no-key".to_string(),
    }
}

fn remember_probe(state: &ServerState, cache_key: &str, probe: &ProbeResult) {
    state
        .probe_cache
        .insert(cache_key.to_string(), probe.clone());
}

async fn remember_forced_probe_visibility(
    state: &ServerState,
    target: &TargetSnapshot,
    credential_hash: &str,
    wire_api: WireApi,
) {
    let cache_key = probe_cache_key(&target.provider_id, credential_hash, &target.upstream_model);
    let now_ts = chrono::Utc::now().timestamp();
    let needs_refresh = state
        .probe_cache
        .get(&cache_key)
        .map(|entry| {
            entry.value().wire_api != wire_api
                || entry.value().expires_at <= chrono::Utc::now().timestamp()
        })
        .unwrap_or(true);
    if !needs_refresh {
        return;
    }

    let probe = ProbeResult {
        provider_id: target.provider_id.clone(),
        credential_hash: credential_hash.to_string(),
        upstream_model: target.upstream_model.clone(),
        wire_api,
        supports_streaming: false,
        supports_tools: false,
        supports_parallel_tool_calls: false,
        tool_support_known: false,
        supports_previous_response_id: false,
        supports_reasoning_encrypted_content: false,
        supports_reasoning_summary: false,
        last_success_at: Some(now_ts),
        last_failure_at: None,
        failure_kind: None,
        failure_message_redacted: None,
        expires_at: now_ts + 3600,
    };
    remember_probe(state, &cache_key, &probe);

    if let Err(error) = store_probe_result_detailed(&state.db, &probe, "success").await {
        warn!(
            provider_id = %target.provider_id,
            upstream_model = %target.upstream_model,
            wire_api = %wire_api.as_str(),
            error = %error,
            "Failed to persist synthetic forced-wire-api probe visibility record"
        );
    }
}

fn parse_db_timestamp_to_unix(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}

fn extract_upstream_response_id(events: &[CanonicalEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        CanonicalEvent::ResponseCreated { response_id, .. } if !response_id.is_empty() => {
            Some(response_id.clone())
        }
        CanonicalEvent::ResponseCompleted { response_id, .. } if !response_id.is_empty() => {
            Some(response_id.clone())
        }
        _ => None,
    })
}

fn parse_raw_sse_frame(
    adapter: &dyn UpstreamAdapter,
    frame: RawSseFrame,
) -> Result<Option<CanonicalEvent>, Error> {
    let event_name = frame.event.unwrap_or_default();
    if event_name == "[DONE]" {
        return Ok(None);
    }
    adapter
        .parse_sse_event(&event_name, &frame.data)
        .map_err(map_adapter_error)
}

async fn load_continuation_context(
    state: &ServerState,
    raw_json: &serde_json::Value,
    _route: &RouteSnapshot,
    _downstream_authorization: Option<&str>,
) -> Result<Option<ContinuationContext>, Error> {
    let Some(previous_response_id) = raw_json
        .get("previous_response_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };

    let Some(previous) = get_response(&state.db, previous_response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to load previous response state: {error}"),
            )
        })?
    else {
        return Err(Error::new(
            ErrorKind::StateNotFound,
            format!("previous_response_id '{previous_response_id}' was not found"),
        ));
    };

    let items = get_items(&state.db, previous_response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to load previous response items: {error}"),
            )
        })?;

    let replay_items = to_replay_input_items(&items)?;
    let known_call_ids = extract_known_call_ids(&items);
    let previous_handle = get_latest_upstream_handle(&state.db, previous_response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to load previous upstream handle: {error}"),
            )
        })?;

    let (previous_upstream_handle, previous_credential_hash) = previous_handle
        .map_or((None, None), |handle| {
            (handle.upstream_response_id, Some(handle.credential_hash))
        });

    Ok(Some(ContinuationContext {
        previous_upstream_handle,
        previous_provider_id: previous.provider_id,
        previous_upstream_model: previous.upstream_model,
        previous_wire_api: previous.wire_api.and_then(|value| WireApi::parse(&value)),
        previous_credential_hash,
        replay_items,
        known_call_ids,
    }))
}

fn to_replay_input_items(items: &[ItemRecord]) -> Result<Vec<CanonicalInputItem>, Error> {
    let mut replay = Vec::new();

    for item in items {
        match item.item_type.as_str() {
            "message" => {
                if item.visible == 0 {
                    continue;
                }
                let role = item.role.clone().unwrap_or_else(|| "assistant".to_string());
                let value: serde_json::Value =
                    serde_json::from_str(&item.content_json).unwrap_or(serde_json::Value::Null);
                let blocks = parse_output_content_json_to_blocks(value);
                if !blocks.is_empty() {
                    replay.push(CanonicalInputItem::Message {
                        role,
                        content: blocks,
                    });
                }
            }
            "function_call" => {
                let value: serde_json::Value =
                    serde_json::from_str(&item.content_json).unwrap_or(serde_json::Value::Null);
                let output = value
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("{}")
                    .to_string();
                let call_id = item
                    .call_id
                    .clone()
                    .unwrap_or_else(modelwire_core::generate_call_id);
                replay.push(CanonicalInputItem::FunctionCallOutput { call_id, output });
            }
            "reasoning" => {}
            other => {
                return Err(Error::new(
                    ErrorKind::StateReplayFailed,
                    format!("Unsupported persisted item type '{other}' in replay chain"),
                ));
            }
        }
    }

    Ok(replay)
}

fn extract_known_call_ids(items: &[ItemRecord]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for item in items {
        if item.item_type == "function_call" {
            if let Some(call_id) = item.call_id.as_ref() {
                ids.insert(call_id.clone());
            }
        }
    }
    ids
}

fn parse_output_content_json_to_blocks(content: serde_json::Value) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if let Some(array) = content.as_array() {
        for block in array {
            if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                blocks.push(ContentBlock::Text {
                    text: text.to_string(),
                });
            }
        }
        return blocks;
    }

    if let Some(text) = content
        .get("text")
        .and_then(serde_json::Value::as_str)
        .or_else(|| content.as_str())
    {
        blocks.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }
    blocks
}

fn apply_continuation_to_canonical(
    canonical: &mut CanonicalResponseRequest,
    continuation: Option<&ContinuationContext>,
    target: &TargetSnapshot,
    downstream_authorization: Option<&str>,
) -> Result<(), Error> {
    let Some(continuation) = continuation else {
        return Ok(());
    };

    validate_tool_result_ids(canonical, continuation)?;
    canonical.previous_response_id = None;
    canonical.input = replay_input_for_canonical(canonical, Some(continuation));

    if can_send_upstream_previous_response_id(continuation, target, downstream_authorization) {
        canonical.previous_response_id = continuation.previous_upstream_handle.clone();
    }
    Ok(())
}

fn validate_tool_result_ids(
    canonical: &CanonicalResponseRequest,
    continuation: &ContinuationContext,
) -> Result<(), Error> {
    for item in &canonical.input {
        if let CanonicalInputItem::FunctionCallOutput { call_id, .. } = item {
            if !continuation.known_call_ids.contains(call_id) {
                return Err(Error::new(
                    ErrorKind::ToolMappingFailed,
                    format!("Unknown tool result call_id '{call_id}'"),
                ));
            }
        }
    }
    Ok(())
}

fn replay_input_for_canonical(
    canonical: &CanonicalResponseRequest,
    continuation: Option<&ContinuationContext>,
) -> Vec<CanonicalInputItem> {
    let Some(continuation) = continuation else {
        return canonical.input.clone();
    };
    let mut merged = continuation.replay_items.clone();
    merged.extend(canonical.input.clone());
    merged
}

fn can_send_upstream_previous_response_id(
    continuation: &ContinuationContext,
    target: &TargetSnapshot,
    downstream_authorization: Option<&str>,
) -> bool {
    let Some(previous_handle) = continuation.previous_upstream_handle.as_ref() else {
        return false;
    };
    if previous_handle.is_empty() {
        return false;
    }
    let Some(previous_provider_id) = continuation.previous_provider_id.as_deref() else {
        return false;
    };
    let Some(previous_upstream_model) = continuation.previous_upstream_model.as_deref() else {
        return false;
    };
    let Some(previous_wire_api) = continuation.previous_wire_api else {
        return false;
    };
    let Some(previous_credential_hash) = continuation.previous_credential_hash.as_deref() else {
        return false;
    };

    if target.provider_id != previous_provider_id {
        return false;
    }
    if target.upstream_model != previous_upstream_model {
        return false;
    }
    if target.configured_wire_api != previous_wire_api
        && target.configured_wire_api != WireApi::Auto
    {
        return false;
    }

    let current_key = resolve_upstream_key(target, downstream_authorization);
    let current_hash = credential_hash_for_probe(current_key.as_deref(), &target.provider_id);
    current_hash == previous_credential_hash
}

fn should_retry_with_replay_on_missing_handle(
    status: u16,
    body: &[u8],
    canonical: &CanonicalResponseRequest,
) -> bool {
    if canonical.previous_response_id.is_none() {
        return false;
    }
    if status != 404 && status != 400 {
        return false;
    }
    let lower = redacted_upstream_error(body).to_lowercase();
    lower.contains("previous_response_id")
        || lower.contains("response not found")
        || lower.contains("state not found")
}

fn is_public_bind_address(bind: &str) -> bool {
    let addr = bind.trim();
    if addr.is_empty() {
        return false;
    }

    if addr.starts_with('[') {
        if let Some(close_index) = addr.find(']') {
            return &addr[1..close_index] == "::";
        }
    }

    if let Some((host, _port)) = addr.rsplit_once(':') {
        return host == "0.0.0.0" || host == "::";
    }

    addr == "0.0.0.0" || addr == "::"
}

fn parse_capture_mode(mode: &str) -> CaptureMode {
    match mode.trim().to_ascii_lowercase().as_str() {
        "off" => CaptureMode::Off,
        "metadata_only" => CaptureMode::MetadataOnly,
        "visible_only" => CaptureMode::VisibleOnly,
        "full_visible" => CaptureMode::FullVisible,
        "debug_raw" => CaptureMode::DebugRaw,
        _ => CaptureMode::Off,
    }
}

fn stable_hash_text(value: &str, salt: &str) -> String {
    let digest = modelwire_core::hash_key_for_logging(value, salt);
    format!("sha256:{digest}")
}

fn build_routing_attempt(
    target: &TargetSnapshot,
    resolved: &ResolvedTargetProtocol,
    status: &str,
    error_kind: Option<ErrorKind>,
    latency_ms: Option<u64>,
) -> RoutingAttempt {
    RoutingAttempt {
        target_id: target.target_id.clone(),
        provider_id: target.provider_id.clone(),
        upstream_model: target.upstream_model.clone(),
        wire_api: resolved.wire_api.as_str().to_string(),
        status: status.to_string(),
        error_kind: error_kind.map(|kind| kind.to_string()),
        latency_ms,
    }
}

fn conversation_messages_for_capture_mode(
    canonical: &CanonicalResponseRequest,
    response: &DownstreamResponse,
    mode: CaptureMode,
    redactor: &Redactor,
) -> (Vec<MessageRecord>, String) {
    let mut messages = Vec::new();
    let mut redaction_count = 0usize;

    if mode == CaptureMode::MetadataOnly {
        return (messages, "clean".to_string());
    }

    for input in &canonical.input {
        match input {
            CanonicalInputItem::Text { content } => {
                let detailed = redactor.redact_detailed(content);
                redaction_count += detailed.redaction_count;
                messages.push(MessageRecord {
                    role: "user".to_string(),
                    content: vec![serde_json::json!({
                        "type": "text",
                        "text": detailed.text
                    })],
                });
            }
            CanonicalInputItem::Message { role, content } => {
                let mut blocks = Vec::new();
                for block in content {
                    if let ContentBlock::Text { text } = block {
                        let detailed = redactor.redact_detailed(text);
                        redaction_count += detailed.redaction_count;
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": detailed.text
                        }));
                    }
                }
                if !blocks.is_empty() {
                    messages.push(MessageRecord {
                        role: role.clone(),
                        content: blocks,
                    });
                }
            }
            CanonicalInputItem::FunctionCallOutput { call_id, output } => {
                if mode == CaptureMode::VisibleOnly {
                    let detailed = redactor.redact_detailed(output);
                    redaction_count += detailed.redaction_count;
                    messages.push(MessageRecord {
                        role: "tool".to_string(),
                        content: vec![serde_json::json!({
                            "type": "tool_result_summary",
                            "call_id": call_id,
                            "summary": detailed.text.chars().take(256).collect::<String>()
                        })],
                    });
                } else {
                    let detailed = redactor.redact_detailed(output);
                    redaction_count += detailed.redaction_count;
                    messages.push(MessageRecord {
                        role: "tool".to_string(),
                        content: vec![serde_json::json!({
                            "type": "tool_result",
                            "call_id": call_id,
                            "output": detailed.text
                        })],
                    });
                }
            }
        }
    }

    for item in &response.output {
        match item {
            DownstreamOutputItem::Message { role, content, .. } => {
                let mut blocks = Vec::new();
                for block in content {
                    match block {
                        DownstreamContentBlock::OutputText { text, .. } => {
                            let detailed = redactor.redact_detailed(text);
                            redaction_count += detailed.redaction_count;
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": detailed.text
                            }));
                        }
                    }
                }
                if !blocks.is_empty() {
                    messages.push(MessageRecord {
                        role: role.clone(),
                        content: blocks,
                    });
                }
            }
            DownstreamOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let detailed = redactor.redact_detailed(arguments);
                redaction_count += detailed.redaction_count;
                if mode == CaptureMode::VisibleOnly {
                    messages.push(MessageRecord {
                        role: "assistant".to_string(),
                        content: vec![serde_json::json!({
                            "type": "tool_call_summary",
                            "name": name,
                            "call_id": call_id,
                            "arguments_summary": detailed.text.chars().take(256).collect::<String>()
                        })],
                    });
                } else {
                    messages.push(MessageRecord {
                        role: "assistant".to_string(),
                        content: vec![serde_json::json!({
                            "type": "tool_call",
                            "name": name,
                            "call_id": call_id,
                            "arguments": detailed.text
                        })],
                    });
                }
            }
            DownstreamOutputItem::Reasoning { .. } => {
                // Never archive hidden/raw reasoning text as visible conversation data.
            }
        }
    }

    let redaction_status = if redaction_count > 0 {
        "redacted".to_string()
    } else {
        "clean".to_string()
    };
    (messages, redaction_status)
}

fn conversation_tools(canonical: &CanonicalResponseRequest) -> Vec<ToolRecord> {
    canonical
        .tools
        .iter()
        .map(|tool| ToolRecord {
            name: tool.name.clone(),
        })
        .collect()
}

async fn archive_successful_response(
    state: &ServerState,
    canonical: &CanonicalResponseRequest,
    route: &RouteSnapshot,
    target: &TargetSnapshot,
    resolved: &ResolvedTargetProtocol,
    response: &DownstreamResponse,
    archive_hints: ArchivePersistHints<'_>,
) -> Result<(), String> {
    let capture_mode = parse_capture_mode(
        archive_hints
            .capture_mode_override
            .unwrap_or(state.config.archive.capture_mode.as_str()),
    );
    if capture_mode == CaptureMode::Off {
        return Ok(());
    }
    if capture_mode == CaptureMode::DebugRaw && is_public_bind_address(&state.config.server.bind) {
        return Err("debug_raw archive mode is not allowed on public bind".to_string());
    }

    let redactor = Redactor::new();
    let (messages, redaction_status) =
        conversation_messages_for_capture_mode(canonical, response, capture_mode, &redactor);
    let tools = conversation_tools(canonical);

    let usage = response.usage.clone().unwrap_or(ResponseUsage {
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        reasoning_tokens: Some(0),
    });
    let salt = state
        .config
        .security
        .log_secret
        .as_deref()
        .unwrap_or("modelwire-archive-default-secret");

    let record = ConversationRecord {
        schema: "modelwire.conversation.v1".to_string(),
        conversation_id: format!("conv_{}", response.id),
        root_response_id: response.id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        capture_mode: capture_mode.as_str().to_string(),
        request: RequestInfo {
            request_id: canonical.request_id.clone(),
            response_id: response.id.clone(),
            previous_response_id: canonical.previous_response_id.clone(),
            route_id: Some(route.route_id.clone()),
            target_id: Some(target.target_id.clone()),
            fallback_attempt: Some(archive_hints.winning_attempt_index as u32),
        },
        models: ModelInfo {
            downstream_model: route.downstream_model.clone(),
            upstream_model: target.upstream_model.clone(),
            provider_id: target.provider_id.clone(),
            provider_name: target.provider_name.clone(),
            provider_base_url_hash: stable_hash_text(&target.provider_base_url, salt),
            provider_config_hash: stable_hash_text(
                &format!(
                    "{}|{}|{}",
                    target.provider_id,
                    target.provider_auth_mode,
                    resolved.wire_api.as_str()
                ),
                salt,
            ),
            state_scope: target.state_scope.clone().unwrap_or_default(),
            wire_api: target.configured_wire_api.as_str().to_string(),
            detected_wire_api: resolved.wire_api.as_str().to_string(),
            upstream_response_id_hash: stable_hash_text(
                archive_hints.upstream_response_id.unwrap_or(""),
                salt,
            ),
        },
        routing: RoutingInfo {
            had_fallback: archive_hints.routing_attempts.len() > 1,
            attempts: archive_hints.routing_attempts.to_vec(),
        },
        messages,
        tools,
        usage: UsageInfo {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
        },
        quality: QualityInfo {
            user_rating: None,
            had_error: false,
            had_fallback: archive_hints.routing_attempts.len() > 1,
        },
        redaction: RedactionStatus {
            status: redaction_status,
            policy: "default".to_string(),
        },
        metadata: None,
    };

    let mut guard = state.archive_writer.lock().await;
    if guard.is_none() {
        let writer = modelwire_archive::writer::ArchiveWriter::new(
            state.config.archive.root.clone(),
            capture_mode,
        )
        .await
        .map_err(|e| e.to_string())?;
        *guard = Some(writer);
    }

    if let Some(writer) = guard.as_mut() {
        writer
            .write_conversation(&record)
            .await
            .map_err(|e| e.to_string())?;
        writer.close_segment().await.map_err(|e| e.to_string())?;
    }

    Ok(())
}

// #[allow(dead_code)]
// /// Build a conversation record for archiving.
// /// NOTE: This function is defined but not currently used. Archive writing requires
// /// proper concurrency handling (ArchiveWriter is not Clone and needs interior mutability).
// /// For future implementation, consider a background task queue.
// fn build_conversation_record(
//     canonical: &CanonicalResponseRequest,
//     route: &RouteSnapshot,
//     target: &TargetSnapshot,
//     resolved: &ResolvedTargetProtocol,
//     response: &DownstreamResponse,
// ) -> ConversationRecord {
//     use chrono::Utc;
//     use modelwire_archive::writer::*;
//     use sha2::{Digest, Sha256};

//     // Hash upstream response ID if present
//     let upstream_response_id_hash = response
//         .id
//         .strip_prefix("mw_")
//         .map(|id| format!("sha256:{}", &id[..16.min(id.len())]))
//         .unwrap_or_else(|| "sha256:unknown".to_string());

//     // Hash provider base URL
//     let mut hasher = Sha256::new();
//     hasher.update(target.provider_base_url.as_bytes());
//     let provider_base_url_hash = format!("sha256:{:x}", hasher.finalize());

//     ConversationRecord {
//         schema: "modelwire.conversation.v1".to_string(),
//         conversation_id: canonical.request_id.clone(),
//         root_response_id: response.id.clone(),
//         created_at: Utc::now().to_rfc3339(),
//         capture_mode: "visible_only".to_string(), // TODO: get from config
//         request: RequestInfo {
//             request_id: canonical.request_id.clone(),
//             response_id: response.id.clone(),
//             previous_response_id: canonical.previous_response_id.clone(),
//             route_id: Some(route.route_id.clone()),
//             target_id: Some(target.target_id.clone()),
//             fallback_attempt: None,
//         },
//         models: ModelInfo {
//             downstream_model: route.downstream_model.clone(),
//             upstream_model: target.upstream_model.clone(),
//             provider_id: target.provider_id.clone(),
//             provider_name: target.provider_name.clone(),
//             provider_base_url_hash,
//             provider_config_hash: "sha256:config".to_string(),
//             state_scope: target.state_scope.clone().unwrap_or_default(),
//             wire_api: target.configured_wire_api.as_str().to_string(),
//             detected_wire_api: resolved.wire_api.as_str().to_string(),
//             upstream_response_id_hash,
//         },
//         routing: RoutingInfo {
//             had_fallback: false,
//             attempts: vec![RoutingAttempt {
//                 target_id: target.target_id.clone(),
//                 provider_id: target.provider_id.clone(),
//                 upstream_model: target.upstream_model.clone(),
//                 wire_api: resolved.wire_api.as_str().to_string(),
//                 status: "success".to_string(),
//                 error_kind: None,
//                 latency_ms: None,
//             }],
//         },
//         messages: vec![],
//         tools: vec![],
//         usage: response.usage.map(|u| UsageInfo {
//             input_tokens: u.input_tokens,
//             output_tokens: u.output_tokens,
//             reasoning_tokens: u.reasoning_tokens.unwrap_or(0),
//         }).unwrap_or(UsageInfo {
//             input_tokens: 0,
//             output_tokens: 0,
//             reasoning_tokens: 0,
//         }),
//         quality: modelwire_archive::writer::QualityInfo {
//             user_rating: None,
//             had_error: false,
//             had_fallback: false,
//         },
//         redaction: modelwire_archive::writer::RedactionStatus {
//             status: "clean".to_string(),
//             policy: "default".to_string(),
//         },
//         metadata: None,
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;
    use modelwire_core::{
        ArchiveConfig, Config, ProviderConfig, RouteConfig, SecurityConfig, ServerConfig,
        TargetConfig,
    };
    use modelwire_db::Database;
    use std::sync::Arc;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[test]
    fn parses_string_input_to_canonical_text() {
        let raw = serde_json::json!({
            "model": "codex-main",
            "input": "hello"
        });

        let canonical =
            parse_canonical_request("req_mw_test", &raw, "codex-main", "gpt-upstream").unwrap();

        assert_eq!(canonical.downstream_model, "codex-main");
        assert_eq!(canonical.upstream_model, "gpt-upstream");
        assert!(matches!(
            canonical.input.first(),
            Some(CanonicalInputItem::Text { content }) if content == "hello"
        ));
    }

    #[test]
    fn rejects_unsupported_content_block() {
        let raw = serde_json::json!({
            "model": "codex-main",
            "input": [{
                "role": "user",
                "content": [{"type": "input_image", "image_url": "https://example.test/a.png"}]
            }]
        });

        let error =
            parse_canonical_request("req_mw_test", &raw, "codex-main", "gpt-upstream").unwrap_err();

        assert_eq!(error.kind, ErrorKind::RequestInvalid);
    }

    #[tokio::test]
    async fn probe_cache_hit_does_not_call_upstream() {
        let mock = MockServer::start().await;
        Mock::given(path("/responses"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock)
            .await;

        let state = Arc::new(
            build_state_with_single_target(&mock.uri(), "auto", Some("provider-key")).await,
        );
        let credential_hash = credential_hash_for_probe(Some("provider-key"), "provider-a");
        state.probe_cache.insert(
            probe_cache_key("provider-a", &credential_hash, "gpt-upstream"),
            ProbeResult {
                provider_id: "provider-a".to_string(),
                credential_hash,
                upstream_model: "gpt-upstream".to_string(),
                wire_api: WireApi::Responses,
                supports_streaming: true,
                supports_tools: true,
                supports_parallel_tool_calls: false,
                tool_support_known: true,
                supports_previous_response_id: false,
                supports_reasoning_encrypted_content: false,
                supports_reasoning_summary: false,
                last_success_at: Some(chrono::Utc::now().timestamp()),
                last_failure_at: None,
                failure_kind: None,
                failure_message_redacted: None,
                expires_at: chrono::Utc::now().timestamp() + 3600,
            },
        );

        let route = snapshot_route(state.as_ref(), "codex-main", None).unwrap();
        let target = route.targets.first().unwrap();
        let resolved = resolve_target_protocol(state.as_ref(), target, Some("provider-key"))
            .await
            .unwrap();

        assert_eq!(resolved.wire_api, WireApi::Responses);
    }

    #[tokio::test]
    async fn probe_404_advances_to_next_protocol() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_1",
                "model": "claude"
            })))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock)
            .await;

        let state = Arc::new(build_state_with_single_target(&mock.uri(), "auto", Some("k1")).await);
        let route = snapshot_route(state.as_ref(), "codex-main", None).unwrap();
        let target = route.targets.first().unwrap();
        let resolved = resolve_target_protocol(state.as_ref(), target, Some("k1"))
            .await
            .unwrap();
        assert_eq!(resolved.wire_api, WireApi::Anthropic);
    }

    #[tokio::test]
    async fn probe_401_stops_and_returns_auth_error() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock)
            .await;

        let state = Arc::new(build_state_with_single_target(&mock.uri(), "auto", Some("k1")).await);
        let route = snapshot_route(state.as_ref(), "codex-main", None).unwrap();
        let target = route.targets.first().unwrap();
        let error = resolve_target_protocol(state.as_ref(), target, Some("k1"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthFailed);
    }

    #[tokio::test]
    async fn forced_wire_api_records_synthetic_probe_visibility() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_forced_probe_visibility",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_forced_probe_visibility",
                    "role": "assistant",
                    "content": [{"type":"output_text","text":"ok"}]
                }]
            })))
            .expect(0)
            .mount(&mock)
            .await;

        let state =
            Arc::new(build_state_with_single_target(&mock.uri(), "responses", Some("k1")).await);
        let route = snapshot_route(state.as_ref(), "codex-main", None).unwrap();
        let target = route.targets.first().unwrap();

        let resolved = resolve_target_protocol(state.as_ref(), target, Some("k1"))
            .await
            .expect("forced wire api resolution should succeed");
        assert_eq!(resolved.wire_api, WireApi::Responses);

        let credential_hash = credential_hash_for_probe(Some("k1"), "provider-a");
        let persisted = get_probe_result(&state.db, "provider-a", &credential_hash, "gpt-upstream")
            .await
            .expect("probe row should be queryable")
            .expect("synthetic probe row should exist");
        assert_eq!(persisted.status, "success");
        assert_eq!(persisted.wire_api, "responses");
    }

    #[tokio::test]
    async fn tool_request_skips_auto_target_with_unknown_tool_support() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        // First target only receives text probe; actual tool-bearing request must be skipped.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_probe_only",
                "model": "gpt-upstream-a",
                "output": [{
                    "type": "message",
                    "id": "msg_probe_only",
                    "role": "assistant",
                    "content": [{"type":"output_text","text":"ok"}]
                }]
            })))
            .expect(1)
            .mount(&first)
            .await;

        // Second target receives real tool-bearing request and returns function_call.
        let captured_tools = Arc::new(std::sync::Mutex::new(None));
        let captured_tools_clone = Arc::clone(&captured_tools);
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *captured_tools_clone.lock().unwrap() = body.get("tools").cloned();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "resp_second_target",
                    "model": "gpt-upstream-b",
                    "output": [{
                        "type": "function_call",
                        "id": "fc_second",
                        "call_id": "call_second",
                        "name": "get_weather",
                        "arguments": "{\"location\":\"Boston\"}"
                    }]
                }))
            })
            .expect(1)
            .mount(&second)
            .await;

        let mut state = build_state_with_two_targets(&first.uri(), &second.uri(), "auto").await;
        state.config.routes[0].targets[1].wire_api = "responses".to_string();
        let state = Arc::new(state);
        let route = snapshot_route(state.as_ref(), "codex-main", None).unwrap();
        assert_eq!(route.targets.len(), 2);

        let raw = serde_json::json!({
            "model":"codex-main",
            "input":"What's the weather in Boston?",
            "tools":[{
                "type":"function",
                "name":"get_weather",
                "description":"Get weather",
                "parameters":{
                    "type":"object",
                    "properties":{"location":{"type":"string"}},
                    "required":["location"]
                }
            }]
        });

        let response = relay_non_streaming_response_scoped(
            Arc::clone(&state),
            "req_tool_skip_unknown_support".to_string(),
            raw,
            Some("Bearer mw_key".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect("second target should satisfy tool-bearing request");

        let has_function_call = response
            .output
            .iter()
            .any(|item| matches!(item, DownstreamOutputItem::FunctionCall { .. }));
        assert!(has_function_call);

        // Ensure the second target really received tools (no stripping).
        let tools = captured_tools.lock().unwrap();
        let tools = tools.as_ref().expect("tools should be captured");
        assert!(tools.as_array().is_some_and(|items| !items.is_empty()));
    }

    #[tokio::test]
    async fn probe_concurrent_identical_requests_single_flight() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(120))
                    .set_body_json(serde_json::json!({"id":"msg_1"})),
            )
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock)
            .await;

        let state = Arc::new(build_state_with_single_target(&mock.uri(), "auto", Some("k1")).await);
        let route = snapshot_route(state.as_ref(), "codex-main", None).unwrap();
        let target = route.targets.first().unwrap().clone();

        let (first, second) = tokio::join!(
            resolve_target_protocol(state.as_ref(), &target, Some("k1")),
            resolve_target_protocol(state.as_ref(), &target, Some("k1"))
        );

        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.wire_api, WireApi::Anthropic);
        assert_eq!(second.wire_api, WireApi::Anthropic);
        assert_eq!(state.probe_locks.len(), 0);
    }

    #[tokio::test]
    async fn native_compact_not_sent_to_chat_or_anthropic() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock)
            .await;

        let state =
            Arc::new(build_state_with_single_target(&mock.uri(), "openai_chat", Some("k1")).await);
        let error = relay_compact_response(
            Arc::clone(&state),
            "req_compact_chat".to_string(),
            serde_json::json!({
                "model": "codex-main"
            }),
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ProtocolNotSupported);

        let state =
            Arc::new(build_state_with_single_target(&mock.uri(), "anthropic", Some("k1")).await);
        let error = relay_compact_response(
            Arc::clone(&state),
            "req_compact_anthropic".to_string(),
            serde_json::json!({
                "model": "codex-main"
            }),
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ProtocolNotSupported);
    }

    #[tokio::test]
    async fn native_compact_not_replayed_across_state_scope() {
        let upstream_a = MockServer::start().await;
        let upstream_b = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream_a)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream_b)
            .await;

        let state = Arc::new(
            build_state_with_scope_mismatch_for_compact(&upstream_a.uri(), &upstream_b.uri()).await,
        );
        modelwire_db::repo::responses::store_response_metadata(
            &state.db,
            &ResponseInsert {
                id: "resp_mw_prev_scope_a",
                request_id: "req_prev_scope",
                downstream_model: "codex-main",
                route_id: Some("route-a"),
                target_id: Some("route-a:provider-a:10"),
                provider_id: Some("provider-a"),
                upstream_model: Some("gpt-upstream"),
                wire_api: Some("responses"),
                upstream_response_id: Some("resp_up_prev"),
                state_scope: Some("scope-a"),
                previous_response_id: None,
                status: "completed",
                usage_json: None,
                error_json: None,
            },
        )
        .await
        .unwrap();

        let error = relay_compact_response(
            Arc::clone(&state),
            "req_scope_guard".to_string(),
            serde_json::json!({
                "model":"codex-main",
                "response_id":"resp_mw_prev_scope_a"
            }),
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind, ErrorKind::ProtocolNotSupported);
    }

    #[tokio::test]
    async fn native_compact_forwarded_only_to_compatible_responses_target() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&first)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&first)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id":"cmp_upstream_ok",
                "object":"response.compaction",
                "status":"completed"
            })))
            .expect(1)
            .mount(&second)
            .await;

        let state =
            Arc::new(build_state_with_two_targets_mixed_compact(&first.uri(), &second.uri()).await);
        let response = relay_compact_response(
            Arc::clone(&state),
            "req_compact_mixed".to_string(),
            serde_json::json!({
                "model":"codex-main"
            }),
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(
            response.get("id").and_then(serde_json::Value::as_str),
            Some("cmp_upstream_ok")
        );
    }

    #[tokio::test]
    async fn local_summary_marks_lineage() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state =
            build_state_with_single_target(&upstream.uri(), "openai_chat", Some("k1")).await;
        state.config.server.compaction_mode = "local_summary".to_string();
        state.config.server.local_summary_model = Some("summary-model-a".to_string());
        state.config.server.local_summary_prompt_version = Some("prompt-v42".to_string());
        let state = Arc::new(state);

        seed_previous_response_state_with_message(
            state.as_ref(),
            "resp_mw_summary_source",
            "resp_upstream_source",
            "k1",
            "alpha beta gamma delta",
        )
        .await;

        let response = relay_compact_response(
            Arc::clone(&state),
            "req_local_summary".to_string(),
            serde_json::json!({
                "model":"codex-main",
                "response_id":"resp_mw_summary_source"
            }),
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(
            response.get("method").and_then(serde_json::Value::as_str),
            Some("local_summary")
        );
        assert_eq!(
            response
                .get("summary")
                .and_then(|v| v.get("summarizer_model"))
                .and_then(serde_json::Value::as_str),
            Some("summary-model-a")
        );
        assert_eq!(
            response
                .get("summary")
                .and_then(|v| v.get("prompt_version"))
                .and_then(serde_json::Value::as_str),
            Some("prompt-v42")
        );

        let lineage = modelwire_db::repo::compactions::get_latest_compaction_lineage(&state.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lineage.method, "local_summary");
        assert_eq!(lineage.provider_native, 0);
        assert_eq!(lineage.summarizer_model.as_deref(), Some("summary-model-a"));
        assert_eq!(lineage.prompt_version.as_deref(), Some("prompt-v42"));
        assert!(lineage.source_tokens.unwrap_or_default() > 0);
        assert!(lineage.summary_tokens.unwrap_or_default() > 0);
        assert!(lineage
            .source_response_ids_json
            .contains("resp_mw_summary_source"));
    }

    #[tokio::test]
    async fn missing_compact_support_falls_back_to_context_policy() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state =
            build_state_with_single_target(&upstream.uri(), "openai_chat", Some("k1")).await;
        state.config.server.compaction_mode = "none".to_string();
        let state = Arc::new(state);
        let error = relay_compact_response(
            Arc::clone(&state),
            "req_compact_none".to_string(),
            serde_json::json!({
                "model":"codex-main"
            }),
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ProtocolNotSupported);
    }

    #[tokio::test]
    async fn previous_response_id_missing_returns_state_not_found() {
        let state = Arc::new(
            build_state_with_single_target("https://example.invalid", "responses", Some("k1"))
                .await,
        );
        let request = serde_json::json!({
            "model": "codex-main",
            "input": "hello",
            "previous_response_id": "resp_mw_not_exists"
        });
        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_mw_test".to_string(),
            request,
            Some("Bearer mw_key".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::StateNotFound);
    }

    #[tokio::test]
    async fn previous_response_same_upstream() {
        let mock = MockServer::start().await;
        let previous_upstream = "resp_upstream_prev";
        let current_upstream = "resp_upstream_curr";

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(
                    body.get("previous_response_id")
                        .and_then(serde_json::Value::as_str),
                    Some(previous_upstream)
                );
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": current_upstream,
                    "model": "gpt-upstream",
                    "output": [{
                        "type": "message",
                        "id": "msg_upstream_2",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "continued"}]
                    }],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                }))
            })
            .expect(1)
            .mount(&mock)
            .await;

        let state =
            Arc::new(build_state_with_single_target(&mock.uri(), "responses", Some("k1")).await);
        seed_previous_response_state(state.as_ref(), "resp_mw_prev", previous_upstream, "k1").await;

        let request = serde_json::json!({
            "model": "codex-main",
            "input": "next",
            "previous_response_id": "resp_mw_prev"
        });
        let response = relay_non_streaming_response(
            Arc::clone(&state),
            "req_mw_next".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();

        assert!(response.id.starts_with("resp_mw_"));
    }

    #[tokio::test]
    async fn previous_response_handle_not_found_replays_history() {
        let mock = MockServer::start().await;
        let previous_upstream = "resp_upstream_prev";

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                if body
                    .get("previous_response_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(previous_upstream)
                {
                    ResponseTemplate::new(404).set_body_string("response not found")
                } else {
                    let input = body
                        .get("input")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    assert!(input.len() >= 2);
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "id": "resp_upstream_new",
                        "model": "gpt-upstream",
                        "output": [{
                            "type": "message",
                            "id": "msg_upstream_3",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "replayed"}]
                        }]
                    }))
                }
            })
            .expect(2)
            .mount(&mock)
            .await;

        let state =
            Arc::new(build_state_with_single_target(&mock.uri(), "responses", Some("k1")).await);
        seed_previous_response_state(state.as_ref(), "resp_mw_prev", previous_upstream, "k1").await;

        let request = serde_json::json!({
            "model": "codex-main",
            "input": "next",
            "previous_response_id": "resp_mw_prev"
        });
        let response = relay_non_streaming_response(
            Arc::clone(&state),
            "req_mw_next".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();
        assert!(response.id.starts_with("resp_mw_"));
    }

    #[tokio::test]
    async fn unknown_tool_result_id_returns_tool_mapping_failed() {
        let state = Arc::new(
            build_state_with_single_target("https://example.invalid", "responses", Some("k1"))
                .await,
        );
        seed_previous_response_state(state.as_ref(), "resp_mw_prev", "resp_upstream_prev", "k1")
            .await;

        let request = serde_json::json!({
            "model": "codex-main",
            "previous_response_id": "resp_mw_prev",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_unknown",
                "output": "{\"ok\":true}"
            }]
        });
        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_unknown_tool".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ToolMappingFailed);
    }

    #[tokio::test]
    async fn context_guard_rejects_before_upstream() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state = Arc::new(
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await,
        );
        let request = serde_json::json!({
            "model": "codex-main",
            "input": "x".repeat(1_200_000),
            "max_output_tokens": 20000
        });
        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_ctx_reject".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ContextLengthExceeded);
    }

    #[tokio::test]
    async fn context_guard_fallback_to_larger_target() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&first)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_second",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_second",
                    "role": "assistant",
                    "content": [{"type":"output_text","text":"ok"}]
                }]
            })))
            .expect(1)
            .mount(&second)
            .await;

        let state =
            Arc::new(build_state_with_two_targets(&first.uri(), &second.uri(), "responses").await);
        let request = serde_json::json!({
            "model": "codex-main",
            "input": "x".repeat(500_000),
            "max_output_tokens": 2000
        });
        let response = relay_non_streaming_response(
            Arc::clone(&state),
            "req_ctx_fallback".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();
        assert!(response.id.starts_with("resp_mw_"));
    }

    #[tokio::test]
    async fn materialized_replay_budget_includes_history() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.routes[0].targets[0].context_window_tokens = Some(50_000);
        state.config.routes[0].targets[0].context_safety_margin_tokens = Some(1_000);
        let state = Arc::new(state);
        seed_previous_response_state_with_message(
            state.as_ref(),
            "resp_mw_prev_big",
            "resp_upstream_prev_big",
            "k1",
            &"h".repeat(260_000),
        )
        .await;

        let request = serde_json::json!({
            "model":"codex-main",
            "previous_response_id":"resp_mw_prev_big",
            "input":"next"
        });

        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_ctx_replay".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ContextLengthExceeded);
    }

    #[tokio::test]
    async fn tool_schema_budget_counts_against_context() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.routes[0].targets[0].context_window_tokens = Some(8_000);
        state.config.routes[0].targets[0].context_safety_margin_tokens = Some(500);
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"short",
            "tools":[
                {
                    "type":"function",
                    "function":{
                        "name":"huge_schema",
                        "description":"large schema",
                        "parameters":{
                            "type":"object",
                            "properties":{
                                "blob":{
                                    "type":"string",
                                    "description": "x".repeat(60_000)
                                }
                            }
                        }
                    }
                }
            ]
        });

        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_ctx_tool_schema".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ContextLengthExceeded);
    }

    #[tokio::test]
    async fn context_guard_does_not_mark_protocol_unsupported() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state = build_state_with_single_target(&upstream.uri(), "auto", Some("k1")).await;
        state.config.routes[0].targets[0].context_window_tokens = Some(8_000);
        state.config.routes[0].targets[0].context_safety_margin_tokens = Some(500);
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"x".repeat(100_000)
        });
        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_ctx_probe".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ContextLengthExceeded);
        assert_eq!(state.probe_cache.len(), 0);
    }

    #[tokio::test]
    async fn no_silent_truncation() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.routes[0].targets[0].context_window_tokens = Some(10_000);
        state.config.routes[0].targets[0].context_safety_margin_tokens = Some(500);
        state.config.routes[0].targets[0].context_overflow_policy = "reject".to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"x".repeat(120_000)
        });

        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_no_truncate".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ContextLengthExceeded);
    }

    #[tokio::test]
    async fn managed_provider_key_missing_returns_internal_error_non_streaming() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state =
            Arc::new(build_state_with_single_target(&upstream.uri(), "responses", None).await);
        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello"
        });
        let error = relay_non_streaming_response(
            Arc::clone(&state),
            "req_missing_managed_key_nonstream".to_string(),
            request,
            Some("Bearer mw_any".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::InternalError);
        assert!(error.message.contains("Managed provider key is missing"));
    }

    #[tokio::test]
    async fn managed_provider_key_missing_returns_internal_error_streaming() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&upstream)
            .await;

        let state =
            Arc::new(build_state_with_single_target(&upstream.uri(), "responses", None).await);
        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello",
            "stream":true
        });
        let error = relay_streaming_response(
            Arc::clone(&state),
            "req_missing_managed_key_stream".to_string(),
            request,
            Some("Bearer mw_any".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::InternalError);
        assert!(error.message.contains("Managed provider key is missing"));
    }

    #[tokio::test]
    async fn streaming_fallback_before_commit_uses_second_target() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(1)
            .mount(&first)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_upstream\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.text.delta\n\
                 data: {\"item_id\":\"msg_1\",\"delta\":{\"text\":\"ok\"}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_upstream\",\"output\":[]}}\n\n",
            ))
            .expect(1)
            .mount(&second)
            .await;

        let state =
            Arc::new(build_state_with_two_targets(&first.uri(), &second.uri(), "responses").await);
        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello",
            "stream": true
        });
        let result = relay_streaming_response(
            Arc::clone(&state),
            "req_stream_1".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();

        let merged = flatten_sse(&result);
        assert!(merged.contains("event: response.created"));
        assert!(merged.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn streaming_failure_after_commit_emits_failure_without_fallback() {
        let first = MockServer::start().await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_upstream\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.output_item.added\n\
                 data: {\"item\":invalid_json}\n\n",
            ))
            .expect(1)
            .mount(&first)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_second\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n",
            ))
            .expect(0)
            .mount(&second)
            .await;

        let state =
            Arc::new(build_state_with_two_targets(&first.uri(), &second.uri(), "responses").await);
        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello",
            "stream": true
        });
        let result = relay_streaming_response(
            Arc::clone(&state),
            "req_stream_2".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();

        let merged = flatten_sse(&result);
        assert!(merged.contains("event: response.created"));
        assert!(merged.contains("event: response.failed"));
        assert!(!merged.contains("resp_second"));
    }

    #[tokio::test]
    async fn streaming_idle_timeout_before_commit_falls_back_to_second_target() {
        let first_base = spawn_delayed_sse_upstream(
            Duration::from_millis(1_500),
            Duration::ZERO,
            vec![
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_slow\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n",
            ],
        )
        .await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_fast\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n\
                 event: response.text.delta\n\
                 data: {\"item_id\":\"msg_fast\",\"delta\":{\"text\":\"fallback-ok\"}}\n\n\
                 event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_fast\",\"output\":[]}}\n\n",
            ))
            .expect(1)
            .mount(&second)
            .await;

        let mut state = build_state_with_two_targets(&first_base, &second.uri(), "responses").await;
        state.config.server.stream_idle_timeout_secs = 1;
        state.config.server.max_stream_duration_secs = 30;
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello",
            "stream": true
        });
        let result = relay_streaming_response(
            Arc::clone(&state),
            "req_stream_idle_fallback".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();

        let merged = flatten_sse(&result);
        assert!(merged.contains("event: response.created"));
        assert!(merged.contains("fallback-ok"));
        assert!(merged.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn streaming_max_duration_after_commit_emits_failed_without_fallback() {
        let first_base = spawn_delayed_sse_upstream(
            Duration::ZERO,
            Duration::from_millis(1_500),
            vec![
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_long\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n",
                "event: response.completed\n\
                 data: {\"response\":{\"id\":\"resp_long\",\"output\":[]}}\n\n",
            ],
        )
        .await;
        let second = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: response.created\n\
                 data: {\"response\":{\"id\":\"resp_second_should_not_run\",\"model\":\"gpt-upstream\",\"created_at\":1}}\n\n",
            ))
            .expect(0)
            .mount(&second)
            .await;

        let mut state = build_state_with_two_targets(&first_base, &second.uri(), "responses").await;
        state.config.server.stream_idle_timeout_secs = 10;
        state.config.server.max_stream_duration_secs = 1;
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello",
            "stream": true
        });
        let result = relay_streaming_response(
            Arc::clone(&state),
            "req_stream_max_duration".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await
        .unwrap();

        let merged = flatten_sse(&result);
        assert!(merged.contains("event: response.created"));
        assert!(merged.contains("event: response.failed"));
        assert!(!merged.contains("resp_second_should_not_run"));
    }

    #[tokio::test]
    async fn archive_off_writes_nothing() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_archive_off",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_archive_off",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "archive off response"
                    }]
                }],
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 3,
                    "total_tokens": 8
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let archive_root = tempfile::tempdir().unwrap();
        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.archive.capture_mode = "off".to_string();
        state.config.archive.root = archive_root.path().to_string_lossy().to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"hello archive off"
        });
        let result = relay_non_streaming_response(
            Arc::clone(&state),
            "req_archive_off".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await;
        assert!(result.is_ok(), "relay should succeed with archive off");

        let entries: Vec<std::path::PathBuf> = std::fs::read_dir(archive_root.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        assert!(
            entries.is_empty(),
            "archive root should remain empty when capture mode is off"
        );
    }

    #[tokio::test]
    async fn archive_visible_only_lineage() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_archive_visible",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_archive_visible",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "assistant says Bearer sk-visible-secret"
                    }]
                }],
                "usage": {
                    "input_tokens": 7,
                    "output_tokens": 4,
                    "total_tokens": 11
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let archive_root = tempfile::tempdir().unwrap();
        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.archive.capture_mode = "visible_only".to_string();
        state.config.archive.root = archive_root.path().to_string_lossy().to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"my token is Bearer sk-user-secret"
        });
        let result = relay_non_streaming_response(
            Arc::clone(&state),
            "req_archive_visible".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "relay should succeed with visible_only archive"
        );

        let mut archives = Vec::new();
        for entry in std::fs::read_dir(archive_root.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_dir() {
                archives.push(entry.path());
            }
        }
        assert_eq!(archives.len(), 1, "one archive directory should be created");

        let archive_dir = &archives[0];
        let manifest_path = archive_dir.join("manifest.json");
        assert!(manifest_path.exists(), "manifest should be written");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["capture_mode"], "visible_only");
        let first_path = manifest["files"][0]["path"]
            .as_str()
            .expect("manifest file path should be present");

        let segment_path = archive_root.path().join(first_path);
        assert!(segment_path.exists(), "compressed segment should exist");
        let compressed = std::fs::read(&segment_path).unwrap();
        let decompressed = zstd::stream::decode_all(&compressed[..]).unwrap();
        let text = String::from_utf8(decompressed).unwrap();
        assert!(
            text.contains("\"capture_mode\":\"visible_only\""),
            "record should include capture mode"
        );
        assert!(
            text.contains("\"provider_id\":\"provider-a\""),
            "record should preserve upstream lineage fields"
        );
        assert!(
            text.contains("\"upstream_response_id_hash\":\"sha256:"),
            "record should hash upstream response id"
        );
        assert!(
            !text.contains("resp_upstream_archive_visible"),
            "raw upstream response id should not be written to archive"
        );
        assert!(
            !text.contains("sk-user-secret") && !text.contains("sk-visible-secret"),
            "archive record should redact secret-like token material"
        );
        assert!(
            text.contains("[REDACTED]"),
            "redaction marker should appear for sensitive text"
        );
    }

    #[tokio::test]
    async fn archive_capture_metadata_only_excludes_visible_messages() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_archive_metadata",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_archive_metadata",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "assistant metadata only"
                    }]
                }],
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 2,
                    "total_tokens": 5
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let archive_root = tempfile::tempdir().unwrap();
        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.archive.capture_mode = "metadata_only".to_string();
        state.config.archive.root = archive_root.path().to_string_lossy().to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"metadata should not include this text"
        });
        let result = relay_non_streaming_response(
            Arc::clone(&state),
            "req_archive_metadata_only".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "relay should succeed with metadata_only archive"
        );

        let archive_dir = std::fs::read_dir(archive_root.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .find(|path| path.is_dir())
            .expect("archive directory should exist");
        let manifest_path = archive_dir.join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["capture_mode"], "metadata_only");
        let first_path = manifest["files"][0]["path"].as_str().unwrap();
        let segment_path = archive_root.path().join(first_path);
        let compressed = std::fs::read(&segment_path).unwrap();
        let decompressed = zstd::stream::decode_all(&compressed[..]).unwrap();
        let text = String::from_utf8(decompressed).unwrap();
        assert!(
            text.contains("\"messages\":[]"),
            "metadata_only should not archive visible message content"
        );
    }

    #[tokio::test]
    async fn archive_capture_full_visible_keeps_full_tool_result() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_archive_full_visible",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_archive_full_visible",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "ack tool output"
                    }]
                }],
                "usage": {
                    "input_tokens": 9,
                    "output_tokens": 4,
                    "total_tokens": 13
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let archive_root = tempfile::tempdir().unwrap();
        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.archive.capture_mode = "full_visible".to_string();
        state.config.archive.root = archive_root.path().to_string_lossy().to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"run tool"}]},
                {"type":"function_call_output","call_id":"call_tool_1","output":"{\"result\":\"very long tool output line\"}"}
            ]
        });
        let result = relay_non_streaming_response(
            Arc::clone(&state),
            "req_archive_full_visible".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "relay should succeed with full_visible archive"
        );

        let archive_dir = std::fs::read_dir(archive_root.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .find(|path| path.is_dir())
            .expect("archive directory should exist");
        let manifest_path = archive_dir.join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["capture_mode"], "full_visible");
        let first_path = manifest["files"][0]["path"].as_str().unwrap();
        let segment_path = archive_root.path().join(first_path);
        let compressed = std::fs::read(&segment_path).unwrap();
        let decompressed = zstd::stream::decode_all(&compressed[..]).unwrap();
        let text = String::from_utf8(decompressed).unwrap();
        assert!(
            text.contains("\"type\":\"tool_result\""),
            "full_visible should store full tool result entries"
        );
        assert!(
            text.contains("\"output\":\"{\\\"result\\\":\\\"very long tool output line\\\"}\""),
            "full_visible should preserve full tool output text"
        );
        assert!(
            !text.contains("\"tool_result_summary\""),
            "full_visible should not downscope to summary-only tool output"
        );
    }

    #[tokio::test]
    async fn archive_capture_mode_override_from_relay_key_is_applied() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_archive_override",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_archive_override",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "override archive capture mode"
                    }]
                }],
                "usage": {
                    "input_tokens": 4,
                    "output_tokens": 2,
                    "total_tokens": 6
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let archive_root = tempfile::tempdir().unwrap();
        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.archive.capture_mode = "off".to_string();
        state.config.archive.root = archive_root.path().to_string_lossy().to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"override with relay key scope"
        });
        let result = relay_non_streaming_response_scoped(
            Arc::clone(&state),
            "req_archive_override".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
            None,
            None,
            Some("metadata_only".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "relay should succeed with override capture mode"
        );

        let archive_dir = std::fs::read_dir(archive_root.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .find(|path| path.is_dir())
            .expect("archive directory should exist");
        let manifest_path = archive_dir.join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["capture_mode"], "metadata_only",
            "relay key override should win over global archive.capture_mode"
        );
    }

    #[tokio::test]
    async fn archive_debug_raw_public_bind_is_best_effort_non_blocking() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_upstream_archive_debug_raw",
                "model": "gpt-upstream",
                "output": [{
                    "type": "message",
                    "id": "msg_archive_debug_raw",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "debug raw should not archive on public bind"
                    }]
                }],
                "usage": {
                    "input_tokens": 2,
                    "output_tokens": 1,
                    "total_tokens": 3
                }
            })))
            .expect(1)
            .mount(&upstream)
            .await;

        let archive_root = tempfile::tempdir().unwrap();
        let mut state =
            build_state_with_single_target(&upstream.uri(), "responses", Some("k1")).await;
        state.config.server.bind = "0.0.0.0:8787".to_string();
        state.config.archive.capture_mode = "debug_raw".to_string();
        state.config.archive.root = archive_root.path().to_string_lossy().to_string();
        let state = Arc::new(state);

        let request = serde_json::json!({
            "model":"codex-main",
            "input":"request should succeed even if debug_raw archiving is blocked"
        });
        let result = relay_non_streaming_response(
            Arc::clone(&state),
            "req_archive_debug_raw_public".to_string(),
            request,
            Some("Bearer mw_k1".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "archive failure in debug_raw/public-bind mode must not fail relay response"
        );

        let entries: Vec<std::path::PathBuf> = std::fs::read_dir(archive_root.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        assert!(
            entries.is_empty(),
            "debug_raw on public bind should not write archive data"
        );
    }

    async fn build_state_with_single_target(
        base_url: &str,
        wire_api: &str,
        provider_api_key: Option<&str>,
    ) -> ServerState {
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..Default::default()
            },
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![ProviderConfig {
                id: "provider-a".to_string(),
                name: "Provider A".to_string(),
                base_url: base_url.to_string(),
                auth_mode: "managed".to_string(),
                default_wire_api: "responses".to_string(),
                state_scope: Some("scope-a".to_string()),
                api_key: provider_api_key.map(|value| value.to_string()),
                allow_private_ips: false,
                skip_ssrf_validation: true, // Allow localhost URLs in tests
                config_json: None,
            }],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![TargetConfig {
                    provider: "provider-a".to_string(),
                    upstream_model: "gpt-upstream".to_string(),
                    wire_api: wire_api.to_string(),
                    priority: 10,
                    enabled: true,
                    context_window_tokens: Some(200_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "reject".to_string(),
                    config_json: None,
                }],
            }],
        };
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }

    async fn build_state_with_two_targets(
        first_base_url: &str,
        second_base_url: &str,
        wire_api: &str,
    ) -> ServerState {
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..Default::default()
            },
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: first_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true, // Allow localhost URLs in tests
                    config_json: None,
                },
                ProviderConfig {
                    id: "provider-b".to_string(),
                    name: "Provider B".to_string(),
                    base_url: second_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-b".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true, // Allow localhost URLs in tests
                    config_json: None,
                },
            ],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![
                    TargetConfig {
                        provider: "provider-a".to_string(),
                        upstream_model: "gpt-upstream".to_string(),
                        wire_api: wire_api.to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(100_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "fallback".to_string(),
                        config_json: None,
                    },
                    TargetConfig {
                        provider: "provider-b".to_string(),
                        upstream_model: "gpt-upstream".to_string(),
                        wire_api: wire_api.to_string(),
                        priority: 20,
                        enabled: true,
                        context_window_tokens: Some(300_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "fallback".to_string(),
                        config_json: None,
                    },
                ],
            }],
        };
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }

    async fn spawn_delayed_sse_upstream(
        first_chunk_delay: Duration,
        between_chunks_delay: Duration,
        chunks: Vec<&'static str>,
    ) -> String {
        use axum::{body::Body, http::header::CONTENT_TYPE, routing::post, Router};
        use bytes::Bytes;
        use std::convert::Infallible;
        use tokio::sync::mpsc;
        use tokio_stream::wrappers::ReceiverStream;

        let chunks: Arc<Vec<String>> =
            Arc::new(chunks.into_iter().map(ToOwned::to_owned).collect());
        let app = Router::new().route(
            "/responses",
            post({
                let chunks = Arc::clone(&chunks);
                move || {
                    let chunks = Arc::clone(&chunks);
                    async move {
                        let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(16);
                        tokio::spawn(async move {
                            tokio::time::sleep(first_chunk_delay).await;
                            for (idx, chunk) in chunks.iter().enumerate() {
                                if tx.send(Ok(Bytes::from(chunk.clone()))).await.is_err() {
                                    return;
                                }
                                if idx + 1 < chunks.len() && !between_chunks_delay.is_zero() {
                                    tokio::time::sleep(between_chunks_delay).await;
                                }
                            }
                        });
                        (
                            [(CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(ReceiverStream::new(rx)),
                        )
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    async fn build_state_with_scope_mismatch_for_compact(
        first_base_url: &str,
        second_base_url: &str,
    ) -> ServerState {
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..Default::default()
            },
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: first_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true, // Allow localhost URLs in tests
                    config_json: None,
                },
                ProviderConfig {
                    id: "provider-b".to_string(),
                    name: "Provider B".to_string(),
                    base_url: second_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-b".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true, // Allow localhost URLs in tests
                    config_json: None,
                },
            ],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![TargetConfig {
                    provider: "provider-b".to_string(),
                    upstream_model: "gpt-upstream".to_string(),
                    wire_api: "responses".to_string(),
                    priority: 10,
                    enabled: true,
                    context_window_tokens: Some(100_000),
                    max_output_tokens: None,
                    auto_compact_recommended_tokens: None,
                    context_safety_margin_tokens: Some(2_000),
                    token_estimator: None,
                    context_overflow_policy: "fallback".to_string(),
                    config_json: None,
                }],
            }],
        };
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }

    async fn build_state_with_two_targets_mixed_compact(
        first_base_url: &str,
        second_base_url: &str,
    ) -> ServerState {
        let config = Config {
            server: ServerConfig {
                upstream_timeout_secs: 5,
                ..Default::default()
            },
            security: SecurityConfig::default(),
            archive: ArchiveConfig::default(),
            providers: vec![
                ProviderConfig {
                    id: "provider-a".to_string(),
                    name: "Provider A".to_string(),
                    base_url: first_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true,
                    config_json: None,
                },
                ProviderConfig {
                    id: "provider-b".to_string(),
                    name: "Provider B".to_string(),
                    base_url: second_base_url.to_string(),
                    auth_mode: "managed".to_string(),
                    default_wire_api: "responses".to_string(),
                    state_scope: Some("scope-a".to_string()),
                    api_key: Some("k1".to_string()),
                    allow_private_ips: false,
                    skip_ssrf_validation: true,
                    config_json: None,
                },
            ],
            routes: vec![RouteConfig {
                id: Some("route-a".to_string()),
                downstream_model: "codex-main".to_string(),
                description: None,
                enabled: true,
                targets: vec![
                    TargetConfig {
                        provider: "provider-a".to_string(),
                        upstream_model: "gpt-chat".to_string(),
                        wire_api: "openai_chat".to_string(),
                        priority: 10,
                        enabled: true,
                        context_window_tokens: Some(100_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "fallback".to_string(),
                        config_json: None,
                    },
                    TargetConfig {
                        provider: "provider-b".to_string(),
                        upstream_model: "gpt-upstream".to_string(),
                        wire_api: "responses".to_string(),
                        priority: 20,
                        enabled: true,
                        context_window_tokens: Some(100_000),
                        max_output_tokens: None,
                        auto_compact_recommended_tokens: None,
                        context_safety_margin_tokens: Some(2_000),
                        token_estimator: None,
                        context_overflow_policy: "fallback".to_string(),
                        config_json: None,
                    },
                ],
            }],
        };
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.run_migrations().await.unwrap();

        ServerState {
            config,
            db,
            probe_cache: dashmap::DashMap::new(),
            probe_locks: dashmap::DashMap::new(),
            key_limiter_counters: dashmap::DashMap::new(),
            ip_limiter_counters: dashmap::DashMap::new(),
            archive_writer: tokio::sync::Mutex::new(None),
        }
    }

    fn flatten_sse(result: &StreamingRelayResult) -> String {
        let mut bytes = Vec::new();
        for frame in &result.sse_frames {
            bytes.extend_from_slice(frame);
        }
        String::from_utf8(bytes).unwrap_or_default()
    }

    async fn seed_previous_response_state(
        state: &ServerState,
        response_id: &str,
        upstream_response_id: &str,
        upstream_key: &str,
    ) {
        let credential_hash = credential_hash_for_probe(Some(upstream_key), "provider-a");
        modelwire_db::repo::responses::store_response_metadata(
            &state.db,
            &ResponseInsert {
                id: response_id,
                request_id: "req_prev",
                downstream_model: "codex-main",
                route_id: Some("route-a"),
                target_id: Some("route-a:provider-a:10"),
                provider_id: Some("provider-a"),
                upstream_model: Some("gpt-upstream"),
                wire_api: Some("responses"),
                upstream_response_id: Some(upstream_response_id),
                state_scope: Some("scope-a"),
                previous_response_id: None,
                status: "completed",
                usage_json: None,
                error_json: None,
            },
        )
        .await
        .unwrap();

        modelwire_db::repo::responses::store_response_item(
            &state.db,
            &ResponseItemInsert {
                id: "msg_mw_prev_assistant",
                response_id,
                sequence: 0,
                item_type: "message",
                role: Some("assistant"),
                call_id: None,
                content_json: r#"[{"type":"output_text","text":"previous answer","annotations":[]}]"#,
                visible: true,
            },
        )
        .await
        .unwrap();

        modelwire_db::repo::responses::store_upstream_handle(
            &state.db,
            &UpstreamHandleInsert {
                id: "uh_prev",
                modelwire_response_id: response_id,
                provider_id: "provider-a",
                credential_hash: &credential_hash,
                upstream_model: "gpt-upstream",
                wire_api: "responses",
                state_scope: Some("scope-a"),
                upstream_response_id: Some(upstream_response_id),
                handle_json: r#"{"upstream_response_id":"resp_upstream_prev"}"#,
            },
        )
        .await
        .unwrap();
    }

    async fn seed_previous_response_state_with_message(
        state: &ServerState,
        response_id: &str,
        upstream_response_id: &str,
        upstream_key: &str,
        message_text: &str,
    ) {
        let credential_hash = credential_hash_for_probe(Some(upstream_key), "provider-a");
        modelwire_db::repo::responses::store_response_metadata(
            &state.db,
            &ResponseInsert {
                id: response_id,
                request_id: "req_prev_big",
                downstream_model: "codex-main",
                route_id: Some("route-a"),
                target_id: Some("route-a:provider-a:10"),
                provider_id: Some("provider-a"),
                upstream_model: Some("gpt-upstream"),
                wire_api: Some("responses"),
                upstream_response_id: Some(upstream_response_id),
                state_scope: Some("scope-a"),
                previous_response_id: None,
                status: "completed",
                usage_json: None,
                error_json: None,
            },
        )
        .await
        .unwrap();

        let content_json = serde_json::json!([{
            "type":"output_text",
            "text": message_text,
            "annotations":[]
        }])
        .to_string();

        modelwire_db::repo::responses::store_response_item(
            &state.db,
            &ResponseItemInsert {
                id: "msg_mw_prev_big_assistant",
                response_id,
                sequence: 0,
                item_type: "message",
                role: Some("assistant"),
                call_id: None,
                content_json: &content_json,
                visible: true,
            },
        )
        .await
        .unwrap();

        modelwire_db::repo::responses::store_upstream_handle(
            &state.db,
            &UpstreamHandleInsert {
                id: "uh_prev_big",
                modelwire_response_id: response_id,
                provider_id: "provider-a",
                credential_hash: &credential_hash,
                upstream_model: "gpt-upstream",
                wire_api: "responses",
                state_scope: Some("scope-a"),
                upstream_response_id: Some(upstream_response_id),
                handle_json: r#"{"upstream_response_id":"resp_upstream_prev_big"}"#,
            },
        )
        .await
        .unwrap();
    }
}
