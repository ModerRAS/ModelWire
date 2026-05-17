# AGENTS.md

This repository contains ModelWire, a Rust API relay built primarily for Codex.
It exposes the OpenAI Responses-compatible downstream API Codex expects and
routes requests to one or more upstream model providers.

Read this file before making any change. Then read
`docs/modelwire-implementation-plan.md`. That document is the source of truth
for product behavior.

## Project summary

ModelWire exists because Codex expects a Responses API provider, while many
models and gateways only expose Anthropic Messages or OpenAI Chat Completions.
ModelWire makes those providers usable by presenting one downstream Responses
API and translating requests/responses internally. Other clients are secondary;
do not weaken Codex behavior to support a generic API gateway use case.

The core flow is:

```text
Downstream client such as Codex
  -> ModelWire /v1/responses
  -> model mapping
  -> ordered upstream targets
  -> lazy per-model protocol probe
  -> upstream adapter
  -> normalized Responses JSON or SSE
```

## Hard requirements

1. Downstream API must be OpenAI Responses-compatible.
2. ModelWire owns downstream response IDs.
3. Upstream response IDs must stay private and be stored as handles.
4. Never forward a downstream `previous_response_id` directly upstream.
5. If the selected upstream can use a known upstream response handle, use it.
6. If the upstream changes or handle reuse fails, materialize ModelWire's
   canonical transcript and replay it to the new upstream.
7. Protocol detection is lazy and keyed by provider + credential hash +
   upstream model.
8. Model mapping happens before protocol detection.
9. Multiple upstream targets and pre-commit fallback must be supported.
10. Do not fallback after the first downstream SSE event has been sent.
11. Function/tool calling is required. Do not silently strip tools.
12. Raw hidden reasoning or provider thinking text must not be exposed as normal
    assistant output.
13. API keys, prompts, and tool outputs must be redacted by default in logs.
14. The service may run on the public internet, so auth, rate limits, timeouts,
    body limits, and request IDs are required.
15. Operational state must be database-backed. In-memory storage is only a cache.
16. Conversation archive collection must be opt-in, parseable as files, and
    separate from operational state.
17. Security tests are required for public-ready features. No auth, key storage,
    archive, admin, or upstream URL feature is accepted without security tests.
18. Context window handling must be conservative. Do not present a mapped model
    as having more context than the real upstream target can safely handle.
19. Codex compatibility is the v1 priority. Tool loops, streaming, context
    guard, `previous_response_id`, and Codex-style errors must be covered by
    slice tests.

## Architecture rules

Use three layers:

```text
Downstream API layer
Canonical core
Upstream adapters
```

The canonical core must contain provider-neutral structs for requests, events,
tools, transcript items, usage, and errors. Do not pass raw provider JSON through
the entire application.

Adapters to implement:

```text
responses
anthropic
openai_chat
```

The native Responses adapter may use upstream `previous_response_id` only when
ModelWire has mapped the downstream response ID to a compatible upstream handle.

The OpenAI Chat adapter should always use materialized transcript replay because
Chat Completions has no portable `previous_response_id`.

## State and fallback rules

Response lifecycle:

```text
not committed
  ModelWire has not sent HTTP response body or first SSE event downstream.
  Fallback is allowed for eligible upstream failures.

committed
  ModelWire has started returning a response to the downstream client.
  Fallback is not allowed for this response.
```

Fallback-friendly failures:

```text
protocol unsupported
connection error
startup timeout
429
500
502
503
504
```

Do not fallback by default for:

```text
401
403
malformed request
safety block
malformed tool result
client disconnect
```

Cross-provider upstream response ID reuse is allowed only when both providers
share the same configured `state_scope`. If reuse fails before commit, replay
canonical history instead.

## Context window rules

Every route target must have explicit context metadata when known:

```text
context_window_tokens
max_output_tokens
auto_compact_recommended_tokens
context_safety_margin_tokens
context_overflow_policy
```

Do not rely on Codex or cloud compaction for correctness. ModelWire must
estimate request size before calling upstream and must reject, fallback, or run
an explicitly configured summarization flow when the selected target would
overflow.

Never silently truncate history, tool outputs, file references, or instructions.

OpenAI `/v1/responses/compact` is capability-dependent. It is not a generic
Chat/Anthropic feature and must not be faked as a portable provider-neutral
state item. Use native compaction only with compatible Responses targets in the
same `state_scope`. For other targets, use reject/fallback or explicit
ModelWire-local visible transcript summarization.

If Codex is configured with `model_context_window = 1000000` but a mapped
upstream target only has 200k context, ModelWire must still enforce the 200k
target budget. Prefer reporting conservative context metadata to downstream
clients when a model catalog endpoint is implemented.

Required context tests:

```text
context_guard_rejects_before_upstream
context_guard_fallback_to_larger_target
context_guard_does_not_mark_protocol_unsupported
context_metadata_reports_conservative_window
materialized_replay_budget_includes_history
tool_schema_budget_counts_against_context
no_silent_truncation
native_compact_forwarded_only_to_compatible_responses_target
native_compact_not_sent_to_chat_or_anthropic
native_compact_not_replayed_across_state_scope
local_summary_marks_lineage
```

## Persistence and retention

Persist operational state in SQLite or Postgres:

```text
responses
response_items
upstream_handles
tool id maps
probe_results
request_logs
retention metadata
```

Do not invent a custom file format as the source of truth for
`previous_response_id` chains or tool-call state. Files are allowed, and
preferred, for long-term conversation archives.

Memory caches are allowed for hot routes, providers, probe results, recent
response chains, and rate limits. The process must be restartable without losing
the ability to continue non-expired response chains.

Every persisted operational state type needs an expiry policy. Add a janitor
task that deletes expired state in batches.

## Conversation archive collection

The operator may want to collect their own conversations for future
fine-tuning/distillation. Keep this separate from operational state and store it
as parseable archive files, not as primary SQL rows.

Default capture mode is `off`.

Supported capture modes should include:

```text
off
metadata_only
visible_only
full_visible
debug_raw
```

Never include raw hidden reasoning or provider thinking text in training
archives. Redact secrets before archive write/export unless an explicit
local-only debug mode is enabled.

Preferred archive shape:

```text
archives/<yyyy-mm>/<archive-id>/
  manifest.json
  conversations-000001.jsonl.zst
  items-000001.jsonl.zst
```

Use schema-versioned JSON records. Optional SQL rows may index archive files for
the UI, but the archive files must be sufficient to rebuild that index.

Each archived conversation or item must preserve upstream lineage:

```text
downstream_model
upstream_model
provider_id
provider_name
provider_base_url_hash
provider_config_hash
configured_wire_api
detected_wire_api
state_scope
route_id
target_id
fallback attempts
latency and usage
```

Hash upstream response IDs by default. Do not put raw provider-private response
handles in training archives unless explicit debug mode is enabled.

## Reasoning policy

Default behavior:

```text
expose_reasoning_summary = false
store_encrypted_reasoning = true
log_reasoning = false
strip_provider_thinking_text = true
```

Do not display raw reasoning. Do not store raw reasoning unless an explicit
debug setting says so. Encrypted reasoning is opaque state, not user-visible
text.

## Implementation order

Follow this order unless the user explicitly asks otherwise:

1. Rust workspace and config loader.
2. `/healthz` and tracing.
3. `POST /v1/responses` non-streaming text via one native Responses upstream.
4. OpenAI Chat adapter non-streaming text.
5. Responses-compatible SSE streaming.
6. Function tool calling.
7. Lazy per-model protocol probing.
8. Multi-target routing and fallback.
9. ModelWire-owned response state and transcript replay.
10. Anthropic adapter.
11. Admin API.
12. Vite React WebUI.
13. Public deployment hardening.

## Testing expectations

Add tests with mock upstreams before relying on real providers.

Use slice-first tests for protocol behavior. A slice test starts from a
downstream request, runs real ModelWire routing/adapter code, hits a mock
upstream HTTP server, captures the upstream request, returns a mock upstream
response, and asserts the downstream response plus persistence/archive effects.

Every adapter feature must have at least one slice test that verifies what the
mock upstream actually received.

Never mark a milestone complete just because the code compiles. Each milestone
must satisfy the matching acceptance criteria in
`docs/modelwire-implementation-plan.md`.

Minimum integration tests:

1. Codex-style non-stream text.
2. Codex-style streaming text.
3. Codex-style tool loop across two turns.
4. Codex-style context overflow before upstream call.
5. Native Responses text with upstream request capture.
6. OpenAI Chat text with upstream request capture.
7. Streaming text with upstream and downstream SSE fixtures.
8. Tool call roundtrip with upstream request capture on both turns.
9. Fallback before commit with first and second upstream attempt capture.
10. No fallback after SSE commit.
11. Same-upstream previous response continuation.
12. Cross-upstream replay.
13. Probe cache by provider + credential hash + upstream model.
14. Redacted logging.
15. Process restart with non-expired response continuation.
16. Retention janitor deletes expired state but keeps referenced chains.
17. Conversation archive export redacts secrets and excludes raw reasoning.
18. Archive records preserve upstream lineage.

## Acceptance discipline

Use this rule for every implementation task:

```text
No test, no done.
No edge-case behavior, no done.
No redaction check, no public-ready feature.
No mock-upstream test, no adapter is accepted.
No security test, no public deployment feature is accepted.
```

When implementing a feature, write down:

1. Files edited.
2. Expected downstream request shape.
3. Expected upstream request shape.
4. Expected downstream response shape.
5. Error status codes.
6. Persistence changes.
7. Cache invalidation behavior.
8. Archive behavior, if conversation content is involved.
9. Tests added.
10. Commands run.

If a feature touches streaming, explicitly test:

1. Upstream failure before downstream commit.
2. Upstream failure after downstream commit.
3. Downstream disconnect.
4. UTF-8 split across chunks.
5. Final usage handling.

If a feature touches tools, explicitly test:

1. Tool definition conversion.
2. Tool call ID mapping.
3. Streaming argument deltas.
4. Tool result roundtrip.
5. Unknown tool result ID.

If a feature touches state, explicitly test:

1. Same-upstream continuation.
2. Cross-upstream replay.
3. Expired state.
4. Restart with non-expired state.
5. Materialized replay context budget.

If a feature touches archives, explicitly test:

1. Capture mode `off`.
2. Capture mode `visible_only`.
3. Secret redaction.
4. Upstream lineage fields.
5. Manifest checksum.

If a feature touches auth, admin, provider URLs, logging, secrets, database, or
archives, explicitly test the relevant security behavior from
`docs/modelwire-implementation-plan.md` section 28.1.

Minimum security tests for public-ready code:

1. Public bind without downstream auth fails startup.
2. Missing/invalid downstream auth returns `401`.
3. Valid key without route permission returns `403`.
4. Rate limit by key works.
5. Managed upstream keys are encrypted at rest.
6. Relay keys are stored only as hashes.
7. Logs never contain raw Authorization or `x-api-key` values.
8. Config export redacts secrets by default.
9. Admin API requires auth.
10. Admin state-changing requests require CSRF protection when cookie auth is
    used.
11. WebUI escapes model output, logs, and upstream error text.
12. Provider URLs reject localhost, private IPs, metadata IPs, and non-HTTP(S)
    schemes by default.
13. Redirects to blocked upstream addresses are rejected.
14. Admin cookies and CSRF tokens are never forwarded upstream.
15. Archive paths cannot escape the archive root.
16. Archive deletion does not follow symlinks outside archive root.
17. Archive output redacts bearer tokens, API keys, PEM private keys, and `.env`
    secrets.
18. Raw hidden reasoning is not logged or archived.
19. `debug_raw` fails on public bind unless an explicit unsafe flag is set.
20. Health/readiness endpoints do not expose secrets or config.

## Task template for small implementer models

Future implementation work may be delegated to a small model. Give it tasks in
this exact shape:

```text
Task:
  Implement one concrete feature.

Files to edit:
  - path/to/file

Do not edit:
  - path/to/unrelated/file

Behavior:
  1. Exact behavior.
  2. Exact behavior.

Edge cases:
  1. Exact edge case.
  2. Exact edge case.

Tests:
  1. Exact test name or scenario.
  2. Exact test name or scenario.

Acceptance:
  The task is complete only when these commands pass:
  - cargo fmt --check
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo test --workspace
```

Do not accept vague reports such as "done", "implemented", or "should work".
Require the test names and the actual command results.

## Documentation expectations

When changing behavior, update `docs/modelwire-implementation-plan.md`.

When adding config fields, include:

1. Field name.
2. Type.
3. Default.
4. Security implication.
5. Example TOML.

When adding adapter behavior, document:

1. Downstream shape.
2. Canonical shape.
3. Upstream shape.
4. Streaming mapping.
5. Tool mapping.
6. Error mapping.

## Style

Prefer boring, explicit Rust. Avoid clever protocol abstractions until at least
two adapters need the same behavior.

Use structured errors. Use `tracing` spans with request IDs. Never print secrets.

Keep the data plane independent of the WebUI. The service must run headless from
config files and environment variables.
