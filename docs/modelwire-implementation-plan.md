# ModelWire implementation plan

Last updated: 2026-05-19

This document is the implementation specification for ModelWire. It is intentionally
very detailed because future implementers may only be able to follow explicit
instructions. When in doubt, implement the behavior in this document before adding
new abstractions.

## 1. Product goal

ModelWire is a public-network-capable API relay built primarily for Codex. It
exposes the OpenAI Responses-compatible surface Codex expects while routing each
requested model to one or more upstream providers.

Codex is the primary product target, not just the first client. Other clients
may work if they use the same Responses subset, but they must not drive v1
design decisions at the expense of Codex compatibility. Codex expects a
Responses-style wire API, while many model providers and model gateways only
expose Anthropic Messages or OpenAI Chat Completions. ModelWire fills that gap.

The intended shape is:

```text
Codex
  -> POST /v1/responses
  -> ModelWire
  -> route by downstream model id
  -> map to upstream provider + upstream model id
  -> lazy detect upstream protocol for that mapped model
  -> call native Responses, Anthropic Messages, or OpenAI Chat Completions
  -> normalize result back to Responses objects and Responses SSE events
```

ModelWire must not be treated as a generic blind reverse proxy. It owns the
downstream Responses state and translates between downstream state and upstream
state.

Design priority:

```text
1. Codex works correctly.
2. Codex tool loops are stable.
3. Codex streaming is stable.
4. Codex context/compaction behavior is safe.
5. Other Responses-compatible clients may work as a secondary benefit.
```

## 2. Explicit user requirements

The user wants the following behavior:

1. Codex-facing API must be Responses-compatible because Codex provider config
   uses `wire_api = "responses"`.
2. MiniMax, New API, Anthropic-style providers, and traditional OpenAI-compatible
   `/v1/chat/completions` providers should be usable as upstreams.
3. The downstream API key should be accepted in one place and then passed to the
   upstream in the header shape required by the selected protocol.
4. Upstream protocol detection must be lazy per model, not only per provider,
   because a gateway such as New API can route different model IDs to different
   real providers.
5. Model mapping must be supported. A downstream model ID may map to a different
   upstream model ID.
6. Multiple upstream targets and fallback must be supported for a single
   downstream model.
7. Protocol probing follows the mapped upstream model, not the downstream model.
8. If a request switches upstreams, ModelWire must handle `previous_response_id`
   correctly.
9. When possible, ModelWire may optimistically try to reuse an upstream
   `previous_response_id` across different upstream entries if they are declared
   to share the same state namespace.
10. If upstream response ID reuse fails or is unsafe, ModelWire must materialize
    history from its own stored canonical transcript and send that to the new
    upstream.
11. Reasoning or hidden thinking content must not be exposed as normal assistant
    text. Store or pass opaque reasoning state only when it is safe and useful.
12. A WebUI is desired, but the WebUI is a control plane. The API relay is the
    data plane and must remain stable and observable.
13. The project may be deployed on the public internet as a downstream for New
    API or similar gateways, so auth, rate limiting, logging, request size
    limits, timeout handling, and secret redaction are required from the start.

## 3. Official OpenAI Responses facts used in this plan

These facts were checked against official OpenAI documentation on 2026-05-16.
Re-check before changing protocol behavior.

References:

- OpenAI Responses API reference:
  <https://developers.openai.com/api/reference/resources/responses/methods/create>
- OpenAI Conversation state guide:
  <https://developers.openai.com/api/docs/guides/conversation-state>
- OpenAI Reasoning guide:
  <https://developers.openai.com/api/docs/guides/reasoning>
- OpenAI streaming guide:
  <https://developers.openai.com/api/docs/guides/streaming-responses>

Important facts:

1. `POST /v1/responses` creates model responses and supports stateful
   interactions.
2. `previous_response_id` is the standard way to continue a prior response state.
3. `previous_response_id` cannot be assumed portable across providers or
   gateways. Treat it as an opaque upstream-owned handle.
4. OpenAI's docs allow conversation state to be managed either by using
   `previous_response_id` or by manually passing prior output items as input to
   a new request.
5. When `instructions` and `previous_response_id` are used together, previous
   instructions are not automatically carried forward. ModelWire must store and
   replay current instructions deliberately when materializing history.
6. Responses streaming uses server-sent events. A downstream client expects
   typed events such as response lifecycle events, output item events, text
   delta events, tool-call argument delta events, and completion/failure events.
7. Reasoning models may produce reasoning items. Raw hidden reasoning tokens are
   not meant to be exposed as user-visible text.
8. Reasoning token counts can appear in usage details.
9. Encrypted reasoning content may be requested and passed forward as opaque
   state, but it is not human-readable reasoning text.
10. Reasoning summaries, when requested and returned, are summaries. They are
    not the full hidden chain of thought.

## 4. Recommended technology stack

Backend:

```text
Rust
axum
tokio
reqwest
tower
tower-http
serde
serde_json
tracing
tracing-subscriber
uuid or uuidv7-compatible crate
sqlx
```

Storage:

```text
SQLite for local/single-node development
Postgres for public long-running or multi-replica deployment
```

Frontend:

```text
Vite
React
TypeScript
TanStack Query
TanStack Router or React Router
```

Packaging:

```text
Single Rust binary for the data plane and admin API
Vite build artifacts served by the Rust backend
Docker image for deployment
Reverse proxy such as Caddy/Nginx/Cloudflare for TLS and edge controls
```

Do not start with Next.js or a separate frontend server. The WebUI is a control
panel, not the core product.

## 5. Core architecture

ModelWire has three conceptual layers:

```text
Downstream API layer
  - OpenAI Responses-compatible HTTP API
  - Receives Codex/New API/client traffic
  - Emits Responses-compatible JSON and SSE

Canonical core
  - Owns model mappings
  - Owns downstream response IDs
  - Owns canonical transcript state
  - Owns route selection, fallback, probing, and retry policy

Upstream adapter layer
  - Native Responses adapter
  - Anthropic Messages adapter
  - OpenAI Chat Completions adapter
  - Future adapters if needed
```

Critical design rule:

```text
Downstream always sees ModelWire response IDs.
Upstream response IDs are private handles stored by ModelWire.
```

That rule is what makes multi-upstream behavior possible.

## 6. Data plane request flow

For every `POST /v1/responses` request:

1. Assign a request ID immediately.
2. Authenticate the downstream request.
3. Enforce body size limit before parsing the full body.
4. Parse the request into a loose raw JSON structure first.
5. Convert raw JSON into `CanonicalResponseRequest`.
6. Resolve the downstream model ID.
7. Apply model mapping and route selection.
8. For each candidate target:
   1. Resolve upstream protocol using lazy per-target/per-model probing.
   2. Build upstream request with mapped model ID.
   3. Apply header transformation.
   4. Send upstream request.
   5. If the upstream fails before downstream response commit and the error is
      fallback-eligible, try the next target.
   6. If the upstream succeeds, bind this ModelWire response to that target.
9. Convert upstream result into canonical events.
10. Emit downstream Responses JSON or SSE.
11. Persist canonical transcript, upstream handles, final status, usage, and
    audit metadata.

For non-streaming requests, response commit happens when ModelWire returns the
HTTP status and response body.

For streaming requests, response commit happens when ModelWire sends the first
downstream SSE event.

Before commit, fallback is allowed. After commit, fallback is not allowed for
that response.

## 7. Control plane request flow

The WebUI and admin API should manage:

1. Providers.
2. Routes and model mappings.
3. Ordered upstream targets per downstream model.
4. Per-target wire API override:
   `auto`, `responses`, `anthropic`, or `openai_chat`.
5. Probe results and manual re-probe.
6. Auth mode.
7. Rate limit policy.
8. Request logs and trace IDs.
9. Error summaries.
10. Health and metrics.

The WebUI must not be required for the data plane to run. Config file and
environment variable bootstrapping must be enough for headless deployment.

## 8. Naming and terminology

Use these terms consistently in code and documentation:

```text
downstream
  The client calling ModelWire. Example: Codex or New API.

upstream
  The model provider or model gateway called by ModelWire.

provider
  A configured upstream endpoint with base URL, auth behavior, and defaults.

route
  A downstream model mapping rule with one or more ordered targets.

target
  One candidate upstream provider + upstream model + protocol policy.

wire_api
  The upstream protocol used by a target:
  responses, anthropic, openai_chat, or auto.

canonical request
  ModelWire's internal normalized representation of a response request.

canonical transcript
  ModelWire's persisted provider-neutral history.

downstream response id
  A ModelWire-owned response ID returned to the client.

upstream response id
  A provider-owned opaque response ID stored privately by ModelWire.

state_scope
  A configured label declaring that two providers share the same upstream
  response-state namespace.
```

## 9. Configuration model

Start with a config file and allow the database/WebUI to override it later.

Example TOML:

```toml
[server]
bind = "0.0.0.0:8787"
public_base_url = "https://modelwire.example.com"
database_url = "sqlite://modelwire.db"
compaction_mode = "native_responses"
local_summary_model = "modelwire-local-summary"
local_summary_prompt_version = "v1"
local_summary_max_chars = 4000

[security]
admin_auth = "local_password"
downstream_auth = "relay_key"
allow_passthrough_keys = true
log_prompts = false
log_tool_outputs = false
log_secret = "replace-with-real-secret"
ip_requests_per_minute = 240
trusted_passthrough_header = "x-gateway-token"
trusted_passthrough_value = "replace-with-gateway-token"

[[security.relay_keys]]
key_hash = "2f7b8a4c"
allowed_models = ["gpt-5.5"]
allowed_providers = ["openai-direct"]
requests_per_minute = 120
max_concurrency = 8
archive_capture_mode = "metadata_only"

[[providers]]
id = "openai-direct"
name = "OpenAI direct"
base_url = "https://api.openai.com/v1"
auth_mode = "pass_authorization"
default_wire_api = "responses"
state_scope = "openai-main"

[[providers]]
id = "new-api-a"
name = "New API cluster A"
base_url = "https://newapi-a.example.com/v1"
auth_mode = "pass_authorization"
default_wire_api = "auto"
state_scope = "newapi-a"

[[providers]]
id = "minimax"
name = "MiniMax OpenAI compatible"
base_url = "https://api.minimax.example/v1"
auth_mode = "pass_authorization"
default_wire_api = "openai_chat"
state_scope = "minimax"

[[routes]]
downstream_model = "gpt-5.5"
description = "Main Codex model alias"

[[routes.targets]]
provider = "openai-direct"
upstream_model = "gpt-5.5"
wire_api = "responses"
priority = 10

[[routes.targets]]
provider = "new-api-a"
upstream_model = "claude-sonnet-4.5"
wire_api = "auto"
priority = 20

[[routes.targets]]
provider = "minimax"
upstream_model = "MiniMax-M1"
wire_api = "openai_chat"
priority = 30
context_window_tokens = 200000
max_output_tokens = 32768
auto_compact_recommended_tokens = 150000
token_estimator = "approx"
context_overflow_policy = "reject"
```

### 9.1 Compaction Config Fields

`server.compaction_mode`
  Type: `string`
  Default: `"native_responses"`
  Security implication: `"local_summary"` and `"hybrid"` can persist compacted
  transcript summaries; keep archives/log redaction enabled.
  Example TOML: `compaction_mode = "hybrid"`

`server.local_summary_model`
  Type: `string | null`
  Default: `null` (runtime fallback: `"modelwire-local-summary"`)
  Security implication: recorded in compaction lineage; should not contain
  secrets or internal credentials.
  Example TOML: `local_summary_model = "summary-router-v1"`

`server.local_summary_prompt_version`
  Type: `string | null`
  Default: `null` (runtime fallback: `"v1"`)
  Security implication: lineage metadata only; use stable version tags.
  Example TOML: `local_summary_prompt_version = "2026-05-compact-v2"`

`server.local_summary_max_chars`
  Type: `integer`
  Default: `4000`
  Security implication: larger values may retain more sensitive transcript
  content in summary output.
  Example TOML: `local_summary_max_chars = 6000`

### 9.2 Relay Key Scope Config Fields

`security.log_secret`
  Type: `string | null`
  Default: `null`
  Security implication: used when hashing downstream relay keys for matching
  and logging; must be high-entropy and managed as a secret.
  Example TOML: `log_secret = "replace-with-real-secret"`

`security.relay_keys`
  Type: `array<table>`
  Default: `[]`
  Security implication: when non-empty, only keys whose hash matches an enabled
  entry are accepted; route/provider scoping is enforced before relay execution.
  Example TOML:
  `[[security.relay_keys]] key_hash = "2f7b8a4c" allowed_models = ["gpt-5.5"]`

`security.relay_keys.key_hash`
  Type: `string`
  Default: required
  Security implication: stores only key hash, never plaintext relay key.
  Example TOML: `key_hash = "2f7b8a4c"`

`security.relay_keys.enabled`
  Type: `boolean`
  Default: `true`
  Security implication: disabled entries are ignored for authentication and
  authorization.
  Example TOML: `enabled = true`

`security.relay_keys.allowed_models`
  Type: `array<string>`
  Default: `[]` (empty means all configured routes)
  Security implication: if non-empty, requests to other downstream model aliases
  return `403`.
  Example TOML: `allowed_models = ["gpt-5.5", "gpt-5.5-mini"]`

`security.relay_keys.allowed_providers`
  Type: `array<string>`
  Default: `[]`
  Security implication: when non-empty, requests are limited to routes that
  include at least one allowed provider and relay target selection is filtered
  to those providers.
  Example TOML: `allowed_providers = ["openai-direct"]`

`security.relay_keys.requests_per_minute`
  Type: `integer | null`
  Default: `null`
  Security implication: when set, requests above this per-minute key budget are
  rejected with `429 rate_limited`.
  Example TOML: `requests_per_minute = 120`

`security.relay_keys.max_concurrency`
  Type: `integer | null`
  Default: `null`
  Security implication: when set, concurrent in-flight requests above this key
  cap are rejected with `429 rate_limited`.
  Example TOML: `max_concurrency = 8`

`security.relay_keys.archive_capture_mode`
  Type: `string | null`
  Default: `null`
  Security implication: optional per-key archive policy marker; should not be
  elevated without explicit operator intent.
  Example TOML: `archive_capture_mode = "metadata_only"`

`security.trusted_passthrough_header`
  Type: `string | null`
  Default: `null`
  Security implication: identifies the required gateway-control header for
  `trusted_passthrough`; missing/invalid config disables safe entry.
  Example TOML: `trusted_passthrough_header = "x-gateway-token"`

`security.trusted_passthrough_value`
  Type: `string | null`
  Default: `null`
  Security implication: shared secret value for trusted passthrough gate; treat
  as credential and never log raw value.
  Example TOML: `trusted_passthrough_value = "replace-with-gateway-token"`

`security.ip_requests_per_minute`
  Type: `integer | null`
  Default: `null`
  Security implication: when set, requests above the per-client-IP minute
  budget are rejected with `429 rate_limited`.
  Example TOML: `ip_requests_per_minute = 240`

Rules:

1. `downstream_model` is what Codex sends in the Responses request.
2. `upstream_model` is what ModelWire sends upstream.
3. `wire_api = "auto"` means probe lazily for that provider + key + upstream
   model.
4. `priority` sorts targets ascending. Lower number means try first.
5. The route must be resolved before probing. Probing must use the upstream
   model ID.
6. `context_window_tokens` is the real configured upstream context window for
   this target. Do not assume it matches the downstream model name.
7. `auto_compact_recommended_tokens` is the safe client-side compaction trigger
   that should be recommended for Codex. It should be lower than the real
   upstream context window.
8. `context_overflow_policy` is one of `reject`, `fallback`, or
   `summarize_explicit`. Default is `reject`.

## 10. Auth model

Downstream auth modes:

```text
relay_key
  The client uses a ModelWire-issued key. ModelWire then uses configured
  upstream credentials or pass-through rules.

passthrough
  The client sends an upstream key in Authorization. ModelWire forwards or
  rewrites it. This is convenient but dangerous on the public internet.

trusted_passthrough
  Passthrough is allowed only with additional protection such as IP allowlist,
  mTLS, Cloudflare Access, or an extra gateway token.

managed
  The client uses a ModelWire key. ModelWire stores and uses provider keys from
  the database or secret store.
```

Header transformation:

```text
responses adapter:
  Usually forward Authorization: Bearer <key>

openai_chat adapter:
  Usually forward Authorization: Bearer <key>

anthropic adapter:
  Convert downstream Bearer token to x-api-key when needed.
  Add anthropic-version if the provider requires it.
```

Do not log raw keys. Log only a short stable key hash such as the first 8-12 hex
characters of HMAC-SHA256(key, server_secret).

## 11. Lazy per-model protocol detection

Probe key:

```text
provider_id + credential_hash + upstream_model -> probe_result
```

If two downstream models map to the same provider + credential + upstream model,
they share the same probe result.

Probe result should contain:

```text
provider_id
credential_hash
upstream_model
wire_api
supports_streaming
supports_tools
supports_parallel_tool_calls
supports_previous_response_id
supports_reasoning_encrypted_content
supports_reasoning_summary
last_success_at
last_failure_at
failure_kind
failure_message_redacted
ttl_expires_at
```

Probe order for `wire_api = "auto"`:

```text
1. responses
2. anthropic
3. openai_chat
```

Probe request guidelines:

1. Use a tiny request, not the user's full request.
2. Use `max_output_tokens = 1` or closest equivalent.
3. Use a harmless prompt such as "Reply with OK."
4. Include the target model ID.
5. Do not include tools in the first probe.
6. If basic protocol probe succeeds, optionally run a second lightweight tool
   probe when the real request contains tools and the cached result does not yet
   know tool support.
7. Cache success with a longer TTL, for example 1 hour.
8. Cache protocol-not-supported failures with a shorter TTL, for example 5-10
   minutes.
9. Do not treat `401`, `403`, `429`, or `5xx` as proof that a protocol is not
   supported.

Error classification during probing:

```text
404 endpoint not found:
  protocol not supported, try next protocol

405 method not allowed:
  protocol not supported, try next protocol

501 not implemented:
  protocol not supported, try next protocol

400 with clear "unknown parameter" or "unsupported endpoint":
  likely protocol not supported, try next protocol

400 invalid model:
  model unsupported on that provider; stop for this target

401/403:
  auth failure; stop, do not try other protocols with the same credentials

429:
  rate limited; temporary failure, not a protocol decision

500/502/503/504:
  temporary upstream failure, not a protocol decision

timeout/connect error:
  temporary upstream failure, not a protocol decision
```

Manual override:

1. Admin API and WebUI must allow forcing `wire_api` for a target.
2. A forced protocol skips probing.
3. Admin API should provide "refresh probe" to clear cache and re-probe.

## 12. Multi-upstream routing and fallback

A route can have multiple targets:

```text
route downstream_model = "gpt-5.5"
  target 1 = openai-direct / gpt-5.5 / responses
  target 2 = new-api-a / claude-sonnet-4.5 / auto
  target 3 = minimax / MiniMax-M1 / openai_chat
```

Fallback is allowed only before downstream response commit.

Fallback-eligible failures:

```text
protocol not supported
connection error
request timeout before any output
429 rate limit
500 upstream internal error
502 bad gateway
503 unavailable
504 gateway timeout
stream opened but ended before any meaningful event
```

Failures that should not fallback by default:

```text
401 unauthorized
403 forbidden
400 malformed request
context length exceeded, unless a later target is explicitly configured as
  larger-context fallback
safety/content-policy block
tool result malformed
client disconnect
downstream cancellation
```

Streaming fallback:

1. Open upstream stream.
2. Read and buffer until either:
   1. a valid first semantic event arrives, or
   2. an error arrives, or
   3. a short startup timeout elapses.
3. If the startup phase fails with fallback-eligible error, close upstream and
   try next target.
4. Once ModelWire sends the first downstream SSE event, the response is
   committed. Do not fallback after this point.

Tool-call fallback:

1. If no downstream event has been emitted yet, fallback is allowed.
2. Once a tool call has been emitted to the downstream client, this response is
   bound to the selected upstream.
3. If the client sends tool results in the next request, ModelWire may route the
   next response to another upstream only by using canonical transcript replay
   or a safe shared-state response ID reuse path.

## 13. Responses state ownership

ModelWire owns all downstream IDs.

Downstream ID examples:

```text
resp_mw_01J...
msg_mw_01J...
fc_mw_01J...
call_mw_01J...
```

Do not expose upstream IDs directly to the downstream client unless they are
embedded in debug-only admin logs.

Persist mappings:

```text
modelwire_response_id
downstream_model
chosen_provider_id
chosen_upstream_model
chosen_wire_api
upstream_response_id
state_scope
canonical_input_items
canonical_output_items
tool_call_id_map
reasoning_state_refs
created_at
completed_at
status
```

When a downstream request includes `previous_response_id`:

1. Look up the ModelWire response record.
2. Load the canonical transcript chain needed for context.
3. Decide whether the next selected upstream can use an upstream response handle.
4. If yes, send upstream `previous_response_id`.
5. If no, materialize the canonical transcript into a new upstream request.

## 14. Same-upstream continuation

If the new target is the same provider, same upstream model, same credential
hash, and same wire API as the previous response, use the upstream state handle
when possible:

```text
downstream previous_response_id = resp_mw_123
ModelWire finds upstream_response_id = resp_openai_abc
ModelWire sends previous_response_id = resp_openai_abc upstream
```

Still persist the new canonical transcript when the response completes.

If upstream returns "previous response not found" or equivalent, fallback to
materialized transcript if the response has not been committed.

## 15. Cross-upstream optimistic response ID reuse

Sometimes two configured providers are different public endpoints but share the
same real backend state. For example, two gateways may both ultimately call the
same OpenAI account or state namespace.

Support this with an explicit `state_scope` setting.

Rules:

1. Only try cross-provider upstream response ID reuse when both targets have the
   same non-empty `state_scope`.
2. Never infer shared state from base URL similarity.
3. Never try reuse across different credentials unless config explicitly allows
   it.
4. If reuse fails before downstream commit, retry with materialized transcript.
5. If reuse succeeds, store the new upstream handle under the new target.
6. If reuse fails after downstream commit, return the upstream error; do not
   switch mid-stream.

Pseudo-flow:

```text
previous downstream id: resp_mw_a
previous upstream handle: resp_upstream_x
previous state_scope: openai-main

new target state_scope: openai-main

try:
  POST new target /responses with previous_response_id = resp_upstream_x
if success:
  continue with native upstream state
else if error clearly means invalid previous_response_id:
  materialize local transcript and retry before downstream commit
else:
  handle according to fallback policy
```

This feature is an optimization. Correctness must not depend on it.

## 16. Materialized transcript replay

When upstream state cannot be reused, rebuild the request from ModelWire's
canonical transcript.

Materialize these items:

```text
system/developer instructions
user input messages
assistant visible output messages
function/tool call items
function/tool result items
supported image/file references
reasoning summary only if policy allows and target supports it
encrypted reasoning only if the target is the same compatible provider family
```

Do not materialize:

```text
raw hidden chain-of-thought
provider-private response IDs
provider-private tool state
encrypted reasoning blob into an unrelated provider
admin-only logs
redacted prompt data
```

First implementation can replay full available history.

Later context management should add:

```text
recent messages kept verbatim
large tool outputs truncated or summarized
old turns compacted into a summary
file/diff/tool metadata preserved where useful
token budget estimation per target
```

If materialized transcript exceeds the target context window:

1. If another route target is explicitly configured as larger-context fallback,
   try it before commit.
2. Otherwise return a context length error.
3. Do not silently drop important tool outputs in v1.

## 16.1 Context window and compaction compatibility

This is a major compatibility risk when using Codex with non-OpenAI or mapped
models.

Codex has its own idea of the active model's context window and when to compact
conversation history. In the user's local config this can appear as fields such
as:

```toml
model_context_window = 1000000
model_auto_compact_token_limit = 900000
```

Codex model metadata can also expose a context window, for example a listed
model may report `context_window = 272000`, `max_context_window = 1000000`, and
an effective context percentage. The exact values are client/model dependent.

ModelWire cannot assume that Codex's configured context window matches the real
upstream model. If a 200k upstream model is presented to Codex as a 1M-capable
model, Codex may wait too long before compacting. The request can then exceed
the upstream context before Codex triggers its own compaction.

Important rule:

```text
Never overstate upstream context capacity to the downstream client.
```

If ModelWire exposes model metadata through `/v1/models` or any future model
catalog endpoint, it should report the safe effective context for the downstream
route, not the largest possible OpenAI/Codex model number. For a route with
fallback targets, report the lowest safe context window among normal targets
unless the route explicitly declares a larger-context fallback strategy.

Recommended target config fields:

```toml
context_window_tokens = 200000
max_output_tokens = 32768
auto_compact_recommended_tokens = 150000
context_safety_margin_tokens = 16000
token_estimator = "approx"
context_overflow_policy = "reject" # reject | fallback | summarize_explicit
```

Definitions:

```text
context_window_tokens
  Real upstream context window for this mapped upstream model.

max_output_tokens
  Maximum output budget to reserve.

auto_compact_recommended_tokens
  Recommended downstream compaction threshold. Should be conservative, for
  example 70-80% of context window after reserving output budget.

context_safety_margin_tokens
  Extra buffer for tokenizer mismatch, hidden provider overhead, tool schemas,
  system instructions, and reasoning overhead.

token_estimator
  Token counting strategy. `approx` is allowed in v1, but exact tokenizer support
  should be added per provider/model family when possible.

context_overflow_policy
  What ModelWire does when estimated request size exceeds the target budget.
```

Conservative budget formula:

```text
usable_input_budget =
  context_window_tokens
  - max_output_tokens
  - context_safety_margin_tokens
  - tool_schema_budget
```

For a 200k model, a reasonable first config might be:

```toml
context_window_tokens = 200000
max_output_tokens = 32768
context_safety_margin_tokens = 16000
auto_compact_recommended_tokens = 150000
```

This leaves room for output, tools, tokenizer mismatch, and provider overhead.

ModelWire must estimate request size before dispatch:

1. Estimate instructions.
2. Estimate input messages/items.
3. Estimate tool schemas.
4. Estimate replayed history if materializing transcript.
5. Reserve output budget.
6. Add safety margin.
7. Compare with target context window.

If the request is too large:

```text
context_overflow_policy = "reject"
  Return a normalized `context_length_exceeded` error before calling upstream.

context_overflow_policy = "fallback"
  Try a configured larger-context target before downstream commit.

context_overflow_policy = "summarize_explicit"
  Use an explicitly configured summarizer/compactor flow. Do not invent this
  silently in v1.
```

Do not silently drop old messages, tool outputs, files, or instructions to make
the request fit. Silent truncation is dangerous for coding agents.

### 16.1.1 Codex compaction interaction

Codex may have a local/cloud compaction mechanism. ModelWire should not depend
on it for correctness because:

1. The downstream model name may not reveal the true upstream context.
2. Mapped models can have different windows per target.
3. Fallback may switch from a large-context target to a smaller-context target.
4. OpenAI/Codex compaction may rely on model-specific behavior, hidden state, or
   model metadata that a generic upstream does not provide.
5. Some providers expose no reliable tokenizer or context metadata.

Therefore ModelWire needs its own context guard even if Codex also compacts.

Safe operating modes:

```text
Mode A: honest downstream config
  Configure Codex `model_context_window` and
  `model_auto_compact_token_limit` to match the smallest real ModelWire route
  target you plan to use.

Mode B: ModelWire route catalog
  ModelWire exposes model metadata that tells Codex a conservative context
  window for the route.

Mode C: ModelWire guardrail
  ModelWire rejects or falls back before calling upstream when its own estimate
  exceeds the selected target budget.
```

For the user's example, if the upstream model is around 200k context, do not run
Codex as if the model has 1M context unless ModelWire will always route to a
true 1M-context target or will safely compact/fallback before the 200k target is
called.

### 16.1.2 Can normal models implement compaction?

There are two different kinds of compaction:

```text
visible transcript summarization
  A normal model can summarize old visible messages and tool results. This can
  be implemented by ModelWire, but it is lossy and must be explicit.

provider/native state compaction
  OpenAI's Responses API has a server-side compaction endpoint. It produces a
  compaction item that can be passed back to compatible OpenAI Responses
  requests. A generic Chat/Anthropic upstream cannot be assumed to consume this
  item.
```

For v1, do not implement automatic summarization unless explicitly configured.
Return `context_length_exceeded` or fallback instead. Add explicit summarization
later with these requirements:

1. Dedicated summarizer model/route.
2. Summary prompt stored in config.
3. Summary output archived and versioned.
4. Original transcript retained until retention expiry.
5. Tests proving tool calls, file paths, errors, and user constraints survive
   summary.
6. User/admin-visible indication that summarization happened.

### 16.1.2.1 OpenAI Responses compaction endpoint

OpenAI documents a `POST /v1/responses/compact` endpoint for server-side
compaction. Treat it as a Responses capability, not as a universal Codex-only
or provider-neutral feature.

Rules:

1. Native Responses upstreams may support `/v1/responses/compact`.
2. Chat Completions upstreams do not support it.
3. Anthropic Messages upstreams do not support the OpenAI compaction item unless
   a specific gateway explicitly implements compatibility.
4. A compaction item produced by one upstream state namespace must not be sent to
   a different provider or different `state_scope`.
5. If ModelWire exposes `/v1/responses/compact` downstream, it must either:
   forward to a compatible native Responses upstream, or perform explicit local
   visible transcript summarization and label it as ModelWire-local compaction.
6. Do not fake OpenAI encrypted/native compaction items for non-OpenAI upstreams.
7. Store compaction lineage:
   source response ID, provider ID, upstream model, `state_scope`, compaction
   method, token counts, and whether the result is provider-native or
   ModelWire-local.
8. If a later request switches upstream, provider-native compaction items are
   not portable. Fall back to canonical transcript replay or ModelWire-local
   summaries.

Compaction modes:

```text
none
  No compaction. Reject or fallback when context is too large.

native_responses
  Use upstream `/v1/responses/compact` only for compatible native Responses
  targets.

local_summary
  Use a configured summarizer route to summarize visible transcript. This is
  lossy and must be visible in logs/archive lineage.

hybrid
  Prefer native compaction on compatible targets, otherwise use local_summary
  only if explicitly configured.
```

For Codex compatibility, missing native compaction is not automatically fatal.
ModelWire can still operate with:

1. Conservative context metadata.
2. Local context guard.
3. Reject-before-upstream on overflow.
4. Larger-context fallback target.
5. Explicit local summary compaction if configured.

The unsafe mode is pretending that a 200k model has 1M context and hoping a
provider-native compaction endpoint will rescue it.

Required compaction tests:

```text
native_compact_forwarded_only_to_compatible_responses_target
  `/v1/responses/compact` forwards only when selected target supports native
  Responses compaction.

native_compact_not_sent_to_chat_or_anthropic
  Chat and Anthropic adapters never receive OpenAI compaction items.

native_compact_not_replayed_across_state_scope
  Compaction item from one state scope is not sent to another.

local_summary_marks_lineage
  ModelWire-local summary records summarizer model, prompt version, source
  response IDs, and token counts.

missing_compact_support_falls_back_to_context_policy
  If compaction is unavailable, ModelWire uses reject/fallback/explicit summary
  policy instead of silently overflowing upstream.
```

### 16.1.3 Context metadata and probing

Protocol probing usually cannot reliably discover true context length. A model
may accept a small probe but fail on large input. Therefore:

1. Context window should be explicit provider/target config.
2. `/models` metadata from upstream can be used as a hint only.
3. Admin UI should show configured context and last context-length failure.
4. Request logs and archives should record estimated input tokens, reserved
   output tokens, context window, and overflow policy.
5. If an upstream returns context-length error, record it and optionally mark
   that target unhealthy for large requests, but do not treat the protocol as
   unsupported.

### 16.1.4 Required context tests

Add slice tests:

```text
context_guard_rejects_before_upstream
  Configure target context window small. Send oversized downstream request.
  Assert upstream mock receives zero requests and downstream gets
  `context_length_exceeded`.

context_guard_fallback_to_larger_target
  First target has small context. Second target has larger context. Oversized
  request skips/falls back before commit and upstream capture shows only the
  larger target was called.

context_guard_does_not_mark_protocol_unsupported
  Upstream returns context length error. Probe/protocol cache must not mark the
  target protocol unsupported.

context_metadata_reports_conservative_window
  Model catalog endpoint reports the conservative route context, not a larger
  unrelated model window.

materialized_replay_budget_includes_history
  Previous response chain replay estimates full materialized history before
  calling upstream.

tool_schema_budget_counts_against_context
  Large tool schemas reduce usable input budget.

no_silent_truncation
  Oversized history is not silently dropped when no explicit summarization
  policy is configured.
```

## 17. Canonical request and event model

Implement a provider-neutral internal model. Do not pass raw upstream/downstream
JSON through the entire system.

Suggested Rust types:

```rust
pub struct CanonicalResponseRequest {
    pub request_id: String,
    pub downstream_model: String,
    pub upstream_model: String,
    pub instructions: Option<CanonicalInstructions>,
    pub input: Vec<CanonicalInputItem>,
    pub previous_response_id: Option<String>,
    pub tools: Vec<CanonicalTool>,
    pub tool_choice: CanonicalToolChoice,
    pub parallel_tool_calls: bool,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stream: bool,
    pub reasoning: Option<CanonicalReasoningOptions>,
    pub include: Vec<String>,
    pub metadata: serde_json::Value,
    pub raw_downstream: serde_json::Value,
}

pub enum CanonicalEvent {
    ResponseCreated(CanonicalResponseMeta),
    OutputItemAdded(CanonicalOutputItem),
    OutputTextDelta { item_id: String, delta: String },
    FunctionCallArgumentsDelta { item_id: String, delta: String },
    OutputItemDone(CanonicalOutputItem),
    ReasoningSummaryDelta { item_id: String, delta: String },
    ResponseCompleted(CanonicalResponseComplete),
    ResponseFailed(CanonicalResponseError),
}
```

Keep `raw_downstream` and redacted `raw_upstream` for diagnostics only.

## 18. Downstream Responses compatibility scope

MVP endpoints:

```text
POST /v1/responses
GET /v1/models
GET /healthz
GET /readyz
```

Near-term endpoints:

```text
GET /v1/responses/{response_id}
DELETE /v1/responses/{response_id}
GET /v1/responses/{response_id}/input_items
POST /v1/responses/{response_id}/cancel
```

Optional later:

```text
POST /v1/responses/input_tokens
conversation object support
background responses
built-in OpenAI tools passthrough
```

Capability-dependent:

```text
POST /v1/responses/compact
```

Only expose `/v1/responses/compact` when ModelWire can either route it to a
compatible native Responses upstream or perform explicitly configured
ModelWire-local visible transcript summarization. Do not expose it as a generic
provider-neutral endpoint by default.

For Codex MVP, prioritize:

1. `POST /v1/responses`.
2. Streaming SSE.
3. Function tools.
4. Tool result roundtrip.
5. `previous_response_id`.
6. Stable error shapes.

## 18.1 Codex compatibility matrix

This project is allowed to be Codex-only where that makes the implementation
more reliable. Do not leave Codex behavior implicit. Each row below needs tests
or a documented non-support decision.

### 18.1.1 Codex provider configuration

Codex should be able to use ModelWire with config like:

```toml
model_provider = "ModelWire"
model = "codex-main"
model_context_window = 200000
model_auto_compact_token_limit = 150000

[model_providers.ModelWire]
name = "ModelWire"
base_url = "https://modelwire.example.com/v1"
wire_api = "responses"
```

Requirements:

1. `base_url` ending in `/v1` must work.
2. Codex calling `/v1/responses` must work.
3. Codex calling `/v1/models`, if it does so, must return conservative model
   metadata.
4. The downstream model name Codex sends may be an alias. Route mapping decides
   the real upstream model.
5. Codex-visible model name in downstream responses should remain the downstream
   alias unless a specific compatibility reason requires otherwise.
6. Context-related values returned by ModelWire must not exceed the smallest
   safe upstream target window for the route.

### 18.1.2 Codex request fields to support

Support these request fields from Codex:

```text
model
instructions
input
previous_response_id
tools
tool_choice
parallel_tool_calls
stream
max_output_tokens
temperature
top_p
reasoning
include
metadata
store
```

Behavior:

1. Unknown fields are preserved in `raw_downstream` and ignored if safe.
2. `store` must not cause unsafe upstream storage behavior. Respect local
   retention config.
3. `reasoning` is mapped only when the selected upstream supports a compatible
   field. Otherwise record that it was omitted.
4. `include` entries such as encrypted reasoning are honored only for compatible
   native Responses upstreams.
5. Unsupported fields must not crash the request. Return a clear error only if
   the unsupported field changes required behavior.

### 18.1.3 Codex input item coverage

Support or explicitly reject:

```text
string input
message input items
assistant output items replayed as input
function_call_output items
reasoning items from compatible native Responses state
image/file input items
computer-use or built-in tool items
```

v1 requirements:

1. String input works.
2. User/assistant message items work.
3. Function call outputs work.
4. Unknown input item types return `400 unsupported_input_type` unless safe
   pass-through is implemented.
5. Image/file input may be rejected in v1 with a clear error if not implemented.
6. Built-in OpenAI tools may be rejected in v1 unless native pass-through is
   implemented.

### 18.1.4 Codex streaming event coverage

Downstream SSE must support at least:

```text
response.created
response.output_item.added
response.output_text.delta
response.function_call_arguments.delta
response.output_item.done
response.completed
response.failed
```

Also handle if present:

```text
response.in_progress
response.output_text.done
response.reasoning_summary_text.delta
response.reasoning_summary_text.done
response.error
```

Rules:

1. Event names must be Responses-style.
2. Event ordering must be stable.
3. Text deltas must preserve exact text order.
4. Tool argument deltas must preserve exact JSON fragment order.
5. `response.completed` must include final output and usage when available.
6. Errors after stream commit should emit `response.failed` if possible.
7. Do not emit provider-native event names to Codex.

### 18.1.5 Codex tool-loop coverage

Codex usefulness depends on tool loops. Support:

```text
function tools
tool_choice auto/none/specific
parallel_tool_calls true/false
streaming function arguments
function_call output item
function_call_output input item
tool result continuation with previous_response_id
```

Rules:

1. Do not strip tools.
2. Do not rename tools.
3. Preserve tool schemas.
4. Maintain ModelWire-owned call IDs.
5. Map ModelWire call IDs to upstream call IDs internally.
6. Tool result continuation must survive process restart while state is
   non-expired.
7. If selected target does not support tools, skip/fallback before commit.

### 18.1.6 Codex context and compaction coverage

Codex may compact locally or through Responses compaction. ModelWire must still
be safe without native compaction.

Support:

```text
conservative model context metadata
target-level context_window_tokens
target-level auto_compact_recommended_tokens
pre-upstream context guard
context_length_exceeded error
larger-context fallback
capability-dependent /v1/responses/compact
optional ModelWire-local summary compaction
```

Rules:

1. Do not advertise 1M context for a 200k upstream target.
2. Do not wait for upstream context errors if ModelWire can estimate overflow
   first.
3. Do not treat upstream context errors as protocol failures.
4. Do not replay provider-native compaction items across `state_scope`.
5. Local summary compaction must be explicit and lineage-tracked.

### 18.1.7 Codex state coverage

Support:

```text
ModelWire-owned response IDs
previous_response_id lookup
same-upstream native continuation
cross-upstream state_scope handle reuse
cross-upstream materialized replay
expired state error
cancel endpoint if Codex calls it
input_items endpoint if Codex calls it
```

Rules:

1. Downstream never sees raw upstream response IDs.
2. Upstream never receives ModelWire response IDs.
3. Same-upstream continuation should use upstream native state when safe.
4. Cross-upstream continuation should use replay unless `state_scope` allows an
   optimistic native handle attempt.
5. Expired state returns a stable error instead of falling back to raw pass-
   through.

### 18.1.8 Codex error coverage

Codex should receive stable, OpenAI-like errors:

```text
request_invalid
auth_failed
model_not_found
upstream_unavailable
rate_limited
context_length_exceeded
tool_mapping_failed
state_not_found
state_not_continuable
stream_interrupted
internal_error
```

Rules:

1. Include `x-request-id`.
2. Do not expose raw upstream keys.
3. Do not expose raw provider stack traces.
4. Preserve enough error detail for Codex to recover or display useful context.
5. Keep status codes predictable.

### 18.1.9 Codex compatibility slice tests

Add these Codex-focused slices in addition to generic adapter slices:

```text
codex_config_base_url_v1
  Codex-style base URL `/v1` routes to `/v1/responses`.

codex_models_conservative_context
  `/v1/models` returns safe context metadata for a mapped 200k model.

codex_simple_text_nonstream
  Codex-style request returns Responses-shaped non-stream response.

codex_simple_text_stream
  Codex-style streaming request returns Responses SSE events.

codex_tool_loop_shell_like
  Function tool call and function_call_output roundtrip works across two turns.

codex_previous_response_same_upstream
  Codex previous_response_id continues through native upstream handle.

codex_previous_response_cross_upstream_replay
  Codex previous_response_id materializes history after fallback.

codex_context_overflow_before_upstream
  Oversized Codex request is rejected/fallbacked before upstream call.

codex_compact_native_if_supported
  `/v1/responses/compact` forwards only to compatible native Responses target.

codex_compact_missing_uses_policy
  Missing compact support falls back to reject/fallback/local summary policy.

codex_cancel_stream
  Downstream cancellation cancels upstream stream and marks state cancelled.

codex_unknown_input_item_clear_error
  Unknown Codex item gets stable unsupported error, not panic.
```

## 19. Native Responses adapter

Use for upstreams that support `/v1/responses`.

Behavior:

1. Convert `CanonicalResponseRequest` to upstream Responses request.
2. Replace model with mapped upstream model.
3. If same-upstream continuation is possible, pass upstream
   `previous_response_id`.
4. If materialized replay is required, omit upstream `previous_response_id` and
   include canonical history in `input`.
5. Forward `stream`, `tools`, `tool_choice`, `parallel_tool_calls`, and
   generation parameters when supported.
6. Request `reasoning.encrypted_content` only when configured and useful.
7. Convert upstream response or SSE events into canonical events.
8. Store upstream IDs privately.

Do not blindly pass downstream `previous_response_id` upstream because it is a
ModelWire ID, not an upstream ID.

## 20. Anthropic Messages adapter

Use for upstreams that expose Anthropic-style Messages.

Mapping:

```text
Responses instructions
  -> Anthropic system field or leading system/developer messages depending on
     provider compatibility

Responses input user message
  -> Anthropic user message content

Responses assistant output text
  -> Anthropic assistant message content

Responses function tool definitions
  -> Anthropic tools

Responses function_call item
  -> Anthropic tool_use block

Responses function_call_output item
  -> Anthropic tool_result block

Responses max_output_tokens
  -> Anthropic max_tokens

Responses stream=true
  -> Anthropic stream=true
```

Need to handle:

1. Anthropic content blocks.
2. Tool use IDs.
3. Tool result IDs.
4. Streaming deltas for text and tool input JSON.
5. Stop reasons.
6. Usage fields if returned.

The Anthropic adapter should preserve ModelWire IDs downstream and map them to
Anthropic tool IDs internally.

## 21. OpenAI Chat Completions adapter

Use for traditional OpenAI-compatible `/v1/chat/completions` upstreams.

Mapping:

```text
Responses instructions
  -> system/developer messages, depending on provider support

Responses input user message
  -> chat user message

Responses assistant output text
  -> chat assistant message

Responses function tools
  -> chat tools with type=function

Responses function_call item
  -> assistant message with tool_calls

Responses function_call_output item
  -> tool message with tool_call_id

Responses max_output_tokens
  -> max_tokens or max_completion_tokens depending on provider config

Responses stream=true
  -> chat stream=true
```

This adapter is the most fragile because Chat Completions has weaker native
state semantics. Always use materialized transcript for Chat Completions. There
is no portable upstream `previous_response_id`.

For streaming tool calls:

1. Accumulate `tool_calls[*].function.arguments` deltas by index/id.
2. Emit canonical function argument deltas downstream.
3. On finish reason `tool_calls`, emit output item done.

If a provider returns text that includes private thinking tags such as
`<think>...</think>`, do not expose it by default. Strip or store as private
provider reasoning according to policy.

## 22. Reasoning handling policy

Default policy for public deployment:

```text
expose_reasoning_summary = false
store_encrypted_reasoning = true
log_reasoning = false
strip_provider_thinking_text = true
```

Definitions:

```text
visible output
  Assistant text intended for the user/client.

reasoning summary
  A model-provided summary of reasoning. Optional and policy-controlled.

encrypted reasoning
  Opaque provider state. Store/pass only when compatible. Never display as text.

raw thinking text
  Chain-of-thought-like content from providers that expose it. Hide by default.
```

Handling:

1. Native OpenAI Responses reasoning items:
   1. Keep item metadata.
   2. Store encrypted content only if included and configured.
   3. Do not expose as assistant text.
2. Reasoning usage tokens:
   1. Store in usage fields.
   2. Show aggregate counts in admin logs.
3. Reasoning summaries:
   1. Store separately from assistant text.
   2. Expose downstream only if admin config enables it.
4. Provider thinking tags:
   1. Strip from visible text by default.
   2. Store redacted/debug copy only when prompt logging is explicitly enabled.
5. Cross-upstream replay:
   1. Do not replay encrypted reasoning to unrelated providers.
   2. Do not replay raw thinking text.
   3. Optionally replay an allowed summary as normal context only if policy
      explicitly enables it.

## 23. Tool calling

MVP supports only custom/function tools.

Responses downstream shapes to support:

```text
tools: [{ type: "function", name, description, parameters }]
tool_choice: "auto" | "none" | required/specific function
parallel_tool_calls: true/false
function_call output items
function_call_output input items
```

ModelWire must maintain tool ID maps:

```text
downstream call id <-> canonical call id <-> upstream call id
```

Rules:

1. Downstream never sees upstream call IDs.
2. When the model emits a tool call, allocate a stable ModelWire call ID if the
   upstream did not provide one.
3. When the downstream sends tool results, map ModelWire call IDs back to the
   upstream/canonical IDs.
4. If switching upstream after tool calls, materialize the assistant tool call
   and tool result history into the new provider's expected format.
5. If the target protocol does not support tools and the request includes tools,
   that target is not eligible unless config explicitly allows tool stripping.
6. Do not strip tools by default for Codex. Codex needs tools to work.

## 24. SSE normalization

Downstream streaming must look like Responses SSE.

Minimum event sequence:

```text
event: response.created
data: {...}

event: response.output_item.added
data: {...}

event: response.output_text.delta
data: {"delta":"..."}

event: response.output_item.done
data: {...}

event: response.completed
data: {...}
```

For tool calls:

```text
event: response.output_item.added
data: function_call item

event: response.function_call_arguments.delta
data: {"delta":"{\"path\""}

event: response.function_call_arguments.delta
data: {"delta":":\"...\"}"}

event: response.output_item.done
data: completed function_call item

event: response.completed
data: response with output items
```

Implementation requirements:

1. Preserve event ordering.
2. Generate monotonically increasing sequence numbers internally.
3. Flush after each SSE event.
4. Detect downstream disconnect and cancel upstream request.
5. Apply stream idle timeout.
6. Apply max stream duration.
7. Do not fallback after the first downstream SSE event is sent.
8. Include final usage if upstream provides it.
9. If upstream errors after commit, emit `response.failed` in Responses shape
   when possible.

## 25. Error normalization

Downstream error object should be stable and OpenAI-like.

Suggested shape:

```json
{
  "error": {
    "message": "Upstream rate limited",
    "type": "upstream_rate_limited",
    "param": null,
    "code": "upstream_429"
  }
}
```

Internal error categories:

```text
auth_failed
rate_limited
protocol_not_supported
model_not_found
request_invalid
context_length_exceeded
upstream_timeout
upstream_unavailable
stream_interrupted
tool_mapping_failed
state_not_found
state_replay_failed
internal_error
```

Always include request ID in response headers:

```text
x-request-id: req_mw_...
```

Never include raw upstream keys, full prompts, or full tool outputs in user-facing
errors.

## 26. Storage schema

Start with SQL migrations. Keep the schema boring and explicit.

Tables:

```sql
providers (
  id text primary key,
  name text not null,
  base_url text not null,
  auth_mode text not null,
  default_wire_api text not null,
  state_scope text,
  config_json text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
)

routes (
  id text primary key,
  downstream_model text not null unique,
  description text,
  enabled boolean not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
)

route_targets (
  id text primary key,
  route_id text not null references routes(id),
  provider_id text not null references providers(id),
  upstream_model text not null,
  wire_api text not null,
  priority integer not null,
  enabled boolean not null,
  config_json text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
)

probe_results (
  id text primary key,
  provider_id text not null references providers(id),
  credential_hash text not null,
  upstream_model text not null,
  wire_api text not null,
  supports_streaming boolean,
  supports_tools boolean,
  supports_previous_response_id boolean,
  supports_reasoning_encrypted_content boolean,
  status text not null,
  failure_kind text,
  failure_message_redacted text,
  last_success_at timestamptz,
  last_failure_at timestamptz,
  expires_at timestamptz not null,
  unique(provider_id, credential_hash, upstream_model)
)

responses (
  id text primary key,
  request_id text not null,
  downstream_model text not null,
  route_id text,
  target_id text,
  provider_id text,
  upstream_model text,
  wire_api text,
  upstream_response_id text,
  state_scope text,
  previous_response_id text,
  status text not null,
  usage_json text,
  error_json text,
  created_at timestamptz not null,
  completed_at timestamptz
)

response_items (
  id text primary key,
  response_id text not null references responses(id),
  sequence integer not null,
  item_type text not null,
  role text,
  call_id text,
  content_json text not null,
  visible boolean not null,
  created_at timestamptz not null
)

upstream_handles (
  id text primary key,
  modelwire_response_id text not null references responses(id),
  provider_id text not null,
  credential_hash text not null,
  upstream_model text not null,
  wire_api text not null,
  state_scope text,
  upstream_response_id text,
  handle_json text not null,
  created_at timestamptz not null
)

request_logs (
  id text primary key,
  request_id text not null,
  downstream_key_hash text,
  downstream_model text,
  route_id text,
  target_id text,
  provider_id text,
  upstream_model text,
  wire_api text,
  status_code integer,
  error_kind text,
  latency_ms integer,
  input_tokens integer,
  output_tokens integer,
  reasoning_tokens integer,
  created_at timestamptz not null
)

retention_policies (
  id text primary key,
  name text not null,
  state_ttl_seconds integer not null,
  log_ttl_seconds integer not null,
  archive_ttl_seconds integer,
  keep_archives boolean not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
)

archive_files (
  id text primary key,
  archive_id text not null,
  format text not null,
  path text not null,
  byte_size integer,
  conversation_count integer,
  item_count integer,
  checksum text,
  manifest_json text not null,
  created_at timestamptz not null
)
```

For SQLite, map `timestamptz` to text or integer milliseconds. SQLx migrations
can have separate SQLite and Postgres variants if needed.

Operational state and conversation archives are deliberately separated:

```text
operational state
  Required for ModelWire to keep Responses semantics working.
  Examples: responses, response_items, upstream_handles, probe_results.
  This data has short TTL by default: hours to days.

request logs
  Required for debugging, metrics, and audit.
  This data should be compact and redacted by default.

conversation archives
  Optional long-term training/distillation corpus owned by the operator.
  This data should be written as parseable archive files, not normalized SQL
  rows. SQL may store only archive metadata and indexes.
```

Do not use custom flat files as the source of truth for operational state.
`previous_response_id`, tool-call ID maps, upstream handles, and probe results
must survive process restarts and must be queryable transactionally. Use SQLite
or Postgres for that.

Use custom archive files as the source of truth for conversation history:

```text
manifest.json
  One metadata file per archive directory. Contains schema version, creation
  time, capture policy, redaction policy, file list, counts, and checksums.

conversations.jsonl or conversations.jsonl.zst
  Default append-friendly archive. One JSON object per conversation or training
  sample.

items.jsonl or items.jsonl.zst
  Optional lower-level archive. One JSON object per message/item/tool event.

Parquet
  Good for larger analytics/training pipelines.

object storage path
  Good for large archive artifacts, while optional metadata/index rows stay in
  SQL.
```

The database may store archive metadata and checksums for search and UI display,
but the archive file is the durable training corpus. The implementation must be
able to rebuild any optional archive index from the archive files.

## 27. In-memory caches

Use the database as the authoritative persistence layer. Use memory only as a
cache.

Recommended cache layers:

```text
L1 in-process memory cache
  Route config, provider config, hot probe results, hot response chains, rate
  limit counters for single-node deployments.

L2 SQL database
  Authoritative operational state, canonical transcripts, upstream handles,
  probe results, request logs, retention metadata, optional archive metadata.

L3 optional distributed cache
  Redis or equivalent for multi-replica rate limits, distributed locks, and
  high-volume hot caches.

L4 optional object/archive storage
  Compressed conversation archives, large redacted transcript archives, analytics
  snapshots.
```

Use in-memory caches for:

```text
route cache
provider cache
probe cache
rate limit counters
recent response chain cache
recent tool id map cache
```

The in-memory cache must be safe to lose at any time. If killing and restarting
the process breaks `previous_response_id`, tool result continuation, or probe
state, the implementation is wrong.

If running multiple replicas:

1. Store canonical state in Postgres.
2. Store probe results in Postgres.
3. Use Redis or database-backed rate limits.
4. Use sticky sessions only as an optimization, not correctness.

Cache invalidation rules:

```text
provider updated
  Invalidate provider cache, affected route cache entries, and affected probe
  results.

route or target updated
  Invalidate route cache for the downstream model.

credential changed
  Invalidate probe results for old credential hash.

state_scope changed
  Invalidate cached continuation decisions.

retention policy changed
  Recompute expiry scheduling for affected operational state and archive files.
```

Default TTL recommendations:

```text
route/provider cache
  30-300 seconds, or event-driven invalidation plus long TTL.

successful probe cache
  1 hour.

failed protocol-not-supported probe
  5-10 minutes.

temporary upstream failure probe result
  30-120 seconds.

hot response chain cache
  10-30 minutes in memory, backed by SQL.

operational response state
  6 hours to 7 days, configurable per route/key.

request logs
  7-30 days, compact/redacted by default.

conversation archives
  Operator-controlled. Can be indefinite for private single-user use, but must
  have list/verify/delete tooling and a documented retention policy.
```

Add a background janitor task:

1. Delete expired operational response state.
2. Delete expired probe results.
3. Delete or compact expired request logs.
4. Delete expired conversation archive files only if their capture policy allows
   deletion.
5. Vacuum/optimize SQLite periodically when using SQLite.
6. For Postgres, rely on normal autovacuum but avoid huge single delete
   transactions. Delete in batches.

The janitor must never delete a response chain that is still referenced by a
non-expired child response.

## 27.1 Conversation archive collection

The operator wants to collect their own conversation history because a large
enough corpus may be useful for training or distilling smaller models. Support
this explicitly as parseable archive files, not as a large normalized database
feature. Keep archives separate from operational state.

Principles:

1. Conversation archive collection is opt-in per deployment, route, or
   downstream key.
2. Operational retention and archive retention are separate settings.
3. Archive files are the source of truth for training/distillation data.
4. Secrets must be redacted before archive write unless the capture policy is an
   explicit local-only debug mode.
5. Raw hidden reasoning must not be collected as training text.
6. Tool outputs may contain secrets. They need their own capture policy.
7. Admin UI must make it clear whether a route/key is writing conversation
   archives.
8. Archive writes should be append-only and resilient to process crashes.
9. Archive files must include schema versions so future tools can parse old
   data.

Recommended capture modes:

```text
off
  Do not store conversation content beyond operational state.

metadata_only
  Store model/provider/usage/status/timing, but not message content.

visible_only
  Store user messages, assistant visible messages, tool names, and tool result
  summaries. Do not store full tool outputs.

full_visible
  Store user messages, assistant visible messages, and full visible tool
  outputs after redaction.

debug_raw
  Store raw provider payloads. This must be disabled by default and should never
  be used on a public multi-user deployment without strong access controls.
```

Recommended archive layout:

```text
archives/
  2026-05/
    modelwire-archive-2026-05-16T12-00-00Z/
      manifest.json
      conversations-000001.jsonl.zst
      items-000001.jsonl.zst
      rejected-000001.jsonl.zst
```

`manifest.json` should contain:

```json
{
  "schema": "modelwire.archive.v1",
  "archive_id": "arch_01J...",
  "created_at": "2026-05-16T12:00:00Z",
  "capture_mode": "visible_only",
  "redaction_policy": "default",
  "source": "modelwire",
  "lineage_policy": "full_upstream_metadata",
  "files": [
    {
      "path": "conversations-000001.jsonl.zst",
      "format": "conversation_jsonl_zstd",
      "sha256": "...",
      "conversation_count": 123,
      "item_count": 456
    }
  ]
}
```

Recommended record formats:

```text
conversation_jsonl
  One JSON object per conversation or sample.

responses_jsonl
  Preserves Responses-style items, tool calls, tool outputs, and metadata.

preference_jsonl
  For future ranking data:
  {"prompt":..., "chosen":..., "rejected":...}

tool_trace_jsonl
  For agent/tool-use distillation:
  includes user request, assistant tool call, tool result, final answer.

parquet
  Optional analytics/training scale format.
```

Minimal `conversation_jsonl` record:

```json
{
  "schema": "modelwire.conversation.v1",
  "conversation_id": "conv_01J...",
  "root_response_id": "resp_mw_...",
  "created_at": "2026-05-16T12:00:00Z",
  "capture_mode": "visible_only",
  "request": {
    "request_id": "req_mw_...",
    "response_id": "resp_mw_...",
    "previous_response_id": "resp_mw_...",
    "route_id": "route_...",
    "target_id": "target_...",
    "fallback_attempt": 0
  },
  "models": {
    "downstream_model": "gpt-5.5",
    "upstream_model": "MiniMax-M1",
    "provider_id": "minimax",
    "provider_name": "MiniMax OpenAI compatible",
    "provider_base_url_hash": "sha256:...",
    "provider_config_hash": "sha256:...",
    "state_scope": "minimax",
    "wire_api": "openai_chat",
    "detected_wire_api": "openai_chat",
    "upstream_response_id_hash": "sha256:..."
  },
  "routing": {
    "had_fallback": false,
    "attempts": [
      {
        "target_id": "target_...",
        "provider_id": "minimax",
        "upstream_model": "MiniMax-M1",
        "wire_api": "openai_chat",
        "status": "success",
        "error_kind": null,
        "latency_ms": 1200
      }
    ]
  },
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "..."}]},
    {"role": "assistant", "content": [{"type": "text", "text": "..."}]}
  ],
  "tools": [],
  "usage": {
    "input_tokens": 0,
    "output_tokens": 0,
    "reasoning_tokens": 0
  },
  "quality": {
    "user_rating": null,
    "had_error": false,
    "had_fallback": false
  },
  "redaction": {
    "status": "clean",
    "policy": "default"
  },
  "metadata": {}
}
```

Archive records should include metadata useful for filtering:

```text
downstream_model
upstream_model
provider_id
provider_name
provider_base_url_hash
provider_config_hash
wire_api
detected_wire_api
state_scope
route_id
target_id
request_id
response_id
previous_response_id
upstream_response_id_hash
timestamp
latency_ms
input_tokens
output_tokens
reasoning_tokens
tool_names
had_fallback
had_error
user_rating
quality_score
redaction_status
```

Upstream lineage requirements:

1. Every archived conversation must preserve the downstream model ID and the
   actual upstream model ID used for generation.
2. Preserve provider identity: `provider_id`, human-readable provider name,
   provider base URL hash, and provider config hash.
3. Preserve protocol identity: configured `wire_api`, detected `wire_api`, and
   whether the target was forced or auto-probed.
4. Preserve route identity: route ID, target ID, priority, and fallback attempt
   order.
5. Preserve timing and usage per attempt when available.
6. Preserve final status and error kind.
7. Preserve upstream response IDs only as hashes by default. Raw upstream IDs are
   provider-private state handles and should not be written to training archives
   unless an explicit debug mode is enabled.
8. If multiple upstreams were attempted, archive all attempts, not only the
   winner. This is important for later data-quality filtering.
9. If a conversation switched upstream between turns, each turn/item must record
   its own upstream lineage. Do not assume one conversation equals one upstream.
10. Archive enough lineage to answer: "Which real model produced this
    assistant message, through which provider, using which protocol, after which
    fallback attempts?"

Archive write behavior:

1. Buffer records briefly in memory.
2. Flush to an append-only JSONL segment quickly.
3. Rotate files by size or time, for example 128-512 MB uncompressed or daily.
4. Compress completed segments with zstd.
5. Write a temporary file first, then atomically rename it into place.
6. Update `manifest.json` after segment completion.
7. Store optional `archive_files` metadata row after manifest update.
8. On startup, scan archives and repair/rebuild optional index rows if needed.

Training data redaction:

1. Redact API keys and bearer tokens.
2. Redact common cloud credentials.
3. Redact private SSH keys and PEM blocks.
4. Redact obvious passwords and `.env` values.
5. Redact local absolute paths only if configured. For a personal coding corpus,
   preserving paths may be useful.
6. Redact emails, phone numbers, and other PII if multi-user or shared.
7. Preserve code snippets unless they match secret patterns.
8. Mark each item with `redaction_status = pending | clean | redacted |
   rejected`.

Quality controls:

1. Allow manual keep/drop labels in the WebUI.
2. Allow a simple user rating on conversations.
3. Mark conversations with tool errors separately.
4. Mark fallback conversations separately.
5. Prefer exporting successful complete tool traces.
6. Exclude safety blocks and malformed tool calls by default.
7. Exclude raw chain-of-thought and provider thinking tags.

Export workflow:

1. Select archive filter in WebUI or CLI.
2. Run redaction pass.
3. Run validation pass.
4. Write JSONL/Parquet export file.
5. Update or create `manifest.json` with format, filter, count, destination,
   and checksum.
6. Show the exact export path and checksum.

Do not train directly from operational tables. Train from deliberate archive or
export artifacts.

## 28. Public deployment requirements

Mandatory:

1. Admin UI authentication.
2. Downstream API authentication.
3. Rate limit by downstream key and IP.
4. Concurrency limit by downstream key.
5. Upstream timeout.
6. Stream idle timeout.
7. Body size limit.
8. Header size limit if supported by server config.
9. Secret redaction in logs.
10. Prompt logging disabled by default.
11. Tool output logging disabled by default.
12. Audit log for config changes.
13. Health and readiness endpoints.
14. Request IDs in logs and responses.
15. Database-backed operational state.
16. Configurable retention and janitor cleanup.
17. Conversation archive capture disabled by default unless explicitly enabled.

Recommended edge setup:

```text
Cloudflare / Caddy / Nginx
  -> TLS
  -> optional IP allowlist
  -> optional WAF
  -> request size cap
  -> ModelWire
```

Do not expose admin endpoints without auth, even on a private server.

## 28.1 Security model and mandatory security tests

ModelWire is intended to run on the public internet. Treat it as an internet
gateway that can lose money, leak private code, leak training archives, or leak
API keys if implemented carelessly. Security requirements are part of v1, not a
later polish task.

### 28.1.1 Primary assets to protect

Protect these assets:

1. Downstream API keys.
2. Managed upstream API keys.
3. Passthrough upstream API keys.
4. Admin credentials and sessions.
5. SQLite/Postgres database.
6. Conversation archive files.
7. Config files and config exports.
8. Request logs.
9. Tool outputs, because tool outputs may contain secrets.
10. Provider state handles such as upstream response IDs.
11. Redaction rules and archive manifests.

### 28.1.2 Trust boundaries

Treat these as separate trust zones:

```text
public downstream API
  Untrusted clients may send arbitrary JSON and arbitrary headers.

admin WebUI/API
  Trusted only after strong admin auth and CSRF protection.

upstream providers
  Trusted to provide model output, but not trusted to send safe HTML, safe JSON,
  or well-formed SSE.

database
  Trusted persistence layer, but a DB dump must not reveal raw API keys.

archive directory/object storage
  Long-term corpus storage. Must not contain raw keys or hidden reasoning by
  default.

logs/metrics
  Operational data. Must be useful without containing secrets.
```

Never trust data because it came from a model provider. Upstream text can
contain HTML, JavaScript, fake logs, fake JSON, or secrets echoed from prompts.

### 28.1.3 Threats to explicitly handle

Handle these threats:

1. Open proxy abuse:
   an attacker uses ModelWire to spend someone else's upstream API quota.
2. API key theft through logs:
   keys appear in structured logs, panic traces, request logs, or archive files.
3. API key theft through config export:
   admin exports config and accidentally publishes raw secrets.
4. Database theft:
   attacker obtains the database file or dump.
5. Archive theft:
   attacker obtains conversation archives with private prompts/code/tool output.
6. Admin session theft:
   stolen session cookie grants config and archive access.
7. CSRF:
   attacker causes an authenticated admin browser to change provider config.
8. XSS:
   model output or request log content executes in the admin WebUI.
9. SSRF:
   attacker influences upstream base URL or redirects to reach localhost,
   private networks, metadata services, or internal admin services.
10. Header smuggling/leakage:
    downstream cookies/admin headers are accidentally forwarded upstream.
11. CORS misconfiguration:
    browsers on arbitrary origins can call admin APIs.
12. Request flooding:
    attacker exhausts CPU, memory, connections, DB pool, or upstream quota.
13. Oversized body/tool output:
    attacker fills memory, DB, logs, or archive disk.
14. Path traversal:
    archive paths or config import paths escape the intended directory.
15. Unsafe debug mode:
    `debug_raw` or prompt logging is accidentally enabled on public deployment.
16. Supply-chain vulnerability:
    dependency with known vulnerability ships in public binary/container.
17. Container privilege issue:
    compromised process has unnecessary filesystem or root privileges.

### 28.1.4 Key and secret handling

Rules:

1. Never store passthrough downstream keys.
2. Store relay keys only as hashes.
3. Store managed upstream keys encrypted at rest.
4. Encryption key must come from environment, file secret, KMS, or secret
   manager. Do not store the encryption key in the database.
5. Use authenticated encryption for managed secrets, for example
   `XChaCha20-Poly1305` or AES-GCM through a vetted Rust crypto crate.
6. Include key version in encrypted secret metadata so rotation is possible.
7. Log only stable secret hashes, never raw secret values.
8. Redact these patterns everywhere:
   Bearer tokens, API keys, `x-api-key`, cookies, session IDs, SSH private keys,
   PEM blocks, cloud credentials, `.env` assignments, and obvious passwords.
9. Panic/error reports must pass through the same redaction layer before being
   logged.
10. Config export must redact secrets by default.
11. A secure backup export that includes secrets must be explicitly requested,
    encrypted, and clearly named as sensitive.

Required tests:

```text
secret_not_logged_downstream_authorization
secret_not_logged_upstream_authorization
relay_key_stored_only_as_hash
managed_upstream_key_encrypted_at_rest
config_export_redacts_managed_keys
secure_backup_export_requires_explicit_flag
panic_error_redacts_authorization_header
archive_redacts_bearer_token
archive_redacts_pem_private_key
```

### 28.1.5 Public API auth and anti-open-proxy rules

Rules:

1. Downstream API auth is enabled by default for public bind addresses.
2. If binding to `0.0.0.0`, startup must fail unless downstream auth is enabled
   or an explicit `--i-know-this-is-public-without-auth` style unsafe flag is
   provided.
3. Relay keys have scopes:
   allowed routes, optional allowed providers, rate limits, concurrency limits,
   and archive capture policy.
4. Passthrough mode is disabled by default on public deployments.
5. Trusted passthrough requires at least one extra control:
   IP allowlist, mTLS, Cloudflare Access header validation, or gateway token.
6. Missing auth returns `401`.
7. Valid key without route permission returns `403`.
8. Rate limit failures return `429`.
9. Do not reveal whether a disabled/private model exists.

Required tests:

```text
public_bind_without_auth_fails_startup
missing_downstream_auth_returns_401
invalid_downstream_key_returns_401
valid_key_wrong_route_returns_403
disabled_route_does_not_leak_model_existence
passthrough_disabled_rejects_public_request
trusted_passthrough_requires_extra_gate
rate_limit_by_key_returns_429
concurrency_limit_by_key_returns_429_or_503
```

### 28.1.6 Admin WebUI and admin API security

Rules:

1. Admin API always requires admin auth.
2. Password auth, if implemented, must use Argon2id or another current strong
   password hashing scheme.
3. Admin sessions use `HttpOnly`, `Secure` when served over HTTPS, and
   `SameSite=Lax` or `Strict` cookies.
4. State-changing admin routes require CSRF protection when cookie auth is used.
5. Admin API CORS default is same-origin only.
6. Do not store admin tokens in browser localStorage.
7. WebUI must escape all model output, log text, prompt text, tool output, and
   upstream error messages.
8. Do not render untrusted Markdown/HTML in admin screens unless sanitized.
9. Add a Content Security Policy. Start strict:
   no inline scripts, no remote script origins.
10. Failed admin login attempts are rate limited.
11. Admin config changes are audited with admin identity, timestamp, changed
    resource, and redacted diff.
12. Admin logout invalidates the session.

Required tests:

```text
admin_api_requires_auth
admin_cookie_has_httponly_samesite_secure_when_https
admin_post_without_csrf_rejected
admin_cors_rejects_untrusted_origin
admin_login_rate_limited
admin_logout_invalidates_session
log_view_escapes_html_script_tag
provider_error_escapes_html
config_change_writes_redacted_audit_log
config_export_redacts_secrets
config_import_rejects_partial_invalid_payload
```

### 28.1.7 SSRF and upstream URL safety

Rules:

1. Public downstream clients must never be able to supply arbitrary upstream
   URLs.
2. Only admins can create or edit provider base URLs.
3. Provider URL scheme must be `https` by default.
4. `http` provider URLs are allowed only for local development or explicit
   trusted config.
5. Block provider base URLs resolving to:
   localhost, loopback, private RFC1918 networks, link-local addresses,
   multicast, unspecified addresses, and cloud metadata IPs.
6. Allow private addresses only with explicit `allow_private_upstream = true`
   per provider.
7. Re-check resolved IPs periodically because DNS can rebind.
8. Do not follow redirects to disallowed hosts or IPs.
9. Limit redirects. Default max redirects should be 0 or very small.
10. Strip hop-by-hop headers and internal headers before upstream calls.
11. Never forward admin cookies, browser cookies, or CSRF tokens upstream.

Required tests:

```text
downstream_cannot_set_upstream_base_url
provider_url_rejects_file_scheme
provider_url_rejects_localhost_by_default
provider_url_rejects_127_0_0_1_by_default
provider_url_rejects_private_ip_by_default
provider_url_rejects_metadata_ip_by_default
provider_url_allows_private_ip_only_with_explicit_flag
upstream_redirect_to_private_ip_rejected
hop_by_hop_headers_not_forwarded_upstream
admin_cookie_not_forwarded_upstream
```

### 28.1.8 Database and archive protection

Rules:

1. SQLite database file should be created with owner-only permissions where the
   platform supports it.
2. Archive directories should be created with owner-only permissions where the
   platform supports it.
3. Postgres connection must support TLS for remote DBs.
4. Use parameterized SQL only. No string-concatenated SQL with user input.
5. Database backups containing operational state are sensitive.
6. Conversation archives are sensitive even after redaction.
7. Archive writer must prevent path traversal. Archive paths stay inside the
   configured archive root.
8. Archive manifest must include checksums.
9. Archive deletion must not follow symlinks outside archive root.
10. Optional archive encryption should be supported for public deployments. If
    not implemented in v1, document it as a known hardening gap and recommend
    encrypted volume/object storage.

Required tests:

```text
sqlite_file_permissions_owner_only_when_supported
archive_directory_permissions_owner_only_when_supported
archive_path_traversal_rejected
archive_symlink_delete_does_not_escape_root
archive_manifest_checksum_validates
archive_index_rebuild_from_files
sql_queries_use_parameters_for_user_input
postgres_tls_required_when_configured
```

### 28.1.9 Logging, metrics, and archive redaction

Rules:

1. Prompt logging is disabled by default.
2. Tool output logging is disabled by default.
3. Archive capture is disabled by default.
4. Probe requests are never archived as user conversations.
5. Logs must include request IDs and stable secret hashes, not raw secrets.
6. Metrics must not include raw model prompts, tool outputs, or keys as labels.
7. Archive redaction runs before writing visible/full records unless
   `debug_raw` is explicitly enabled.
8. `debug_raw` requires explicit config, admin warning, and should fail startup
   on public bind unless another explicit unsafe flag is set.
9. Upstream response IDs are hashed in archives by default.
10. Raw hidden reasoning is excluded from logs and archives.

Required tests:

```text
prompt_logging_disabled_by_default
tool_output_logging_disabled_by_default
archive_capture_disabled_by_default
probe_request_not_archived
metrics_do_not_include_raw_key_or_prompt
debug_raw_fails_on_public_bind_without_unsafe_flag
upstream_response_id_hashed_in_archive
hidden_reasoning_not_archived
```

### 28.1.10 Container and deployment hardening

Rules:

1. Docker image should run as non-root.
2. Container filesystem should be read-only except configured data directories
   when possible.
3. Data directory must be explicitly mounted for SQLite/archive use.
4. Health endpoints must not expose secrets or config details.
5. Public deployment guide must document TLS, reverse proxy, auth, rate limits,
   backup sensitivity, and archive sensitivity.
6. CI should run dependency vulnerability checks such as `cargo audit` or
   equivalent.
7. CI should run license/policy checks if `cargo-deny` is configured.
8. Release builds should not enable unsafe debug logging by default.

Required tests/checks:

```text
docker_runs_as_non_root
healthz_does_not_expose_config
readyz_does_not_expose_config
cargo_audit_or_documented_equivalent_runs_in_ci
release_config_disables_debug_raw
```

### 28.1.11 Security test execution rule

Security tests are not optional. A public-ready milestone is blocked if any
required security test for touched functionality is missing.

If a security test cannot be automated immediately, document it as:

```text
security_test_name
manual verification steps
why automation is not yet possible
owner
deadline/milestone for automation
```

Do not mark public alpha complete with unresolved high-risk security gaps in:

```text
auth
secret storage
secret redaction
SSRF protection
admin CSRF/XSS
archive leakage
open proxy prevention
```

## 29. Admin API outline

Suggested endpoints:

```text
GET    /admin/api/providers
POST   /admin/api/providers
GET    /admin/api/providers/{id}
PATCH  /admin/api/providers/{id}
DELETE /admin/api/providers/{id}

GET    /admin/api/routes
POST   /admin/api/routes
GET    /admin/api/routes/{id}
PATCH  /admin/api/routes/{id}
DELETE /admin/api/routes/{id}

POST   /admin/api/routes/{id}/targets
PATCH  /admin/api/targets/{id}
DELETE /admin/api/targets/{id}

GET    /admin/api/probes
POST   /admin/api/probes/refresh

GET    /admin/api/logs
GET    /admin/api/metrics
GET    /admin/api/config/export
POST   /admin/api/config/import
```

Admin API responses should be JSON, deterministic, and easy for the WebUI to
consume.

## 30. WebUI screens

MVP screens:

```text
Login
Dashboard
Providers
Model routes
Route detail
Probe status
Request logs
Settings
```

Providers screen:

1. Provider name.
2. Base URL.
3. Auth mode.
4. Default wire API.
5. State scope.
6. Test connection.

Model routes screen:

1. Downstream model ID.
2. Ordered targets.
3. Upstream provider.
4. Upstream model.
5. Wire API policy.
6. Last probe status.
7. Reorder targets.
8. Disable target.

Probe status screen:

1. Provider.
2. Upstream model.
3. Credential hash.
4. Detected wire API.
5. Streaming support.
6. Tool support.
7. Previous response support.
8. Last success/failure.
9. Refresh probe button.

Logs screen:

1. Request ID.
2. Downstream model.
3. Mapped upstream model.
4. Provider.
5. Wire API.
6. Status.
7. Latency.
8. Token usage.
9. Error kind.
10. Link to redacted details.

Do not show raw API keys. Do not show prompts unless explicit config enables it.

## 31. Implementation milestones

Milestone 0: repository skeleton

1. Create Cargo workspace.
2. Add backend crate.
3. Add config loader.
4. Add tracing.
5. Add `/healthz`.
6. Add CI format/check/test.

Milestone 1: non-streaming text through native Responses

1. Implement `POST /v1/responses`.
2. Parse downstream request loosely.
3. Resolve one static route.
4. Call one native Responses upstream.
5. Return non-streaming Responses JSON.
6. Persist response metadata.

Milestone 2: OpenAI Chat adapter

1. Convert canonical request to Chat Completions.
2. Convert chat text response back to Responses JSON.
3. Support simple message history replay.
4. Add tests.

Milestone 3: streaming

1. Add downstream SSE writer.
2. Normalize native Responses SSE.
3. Normalize Chat Completions stream.
4. Implement pre-commit fallback buffer.
5. Cancel upstream on downstream disconnect.

Milestone 4: tool calling

1. Support function tool definitions.
2. Support tool call deltas.
3. Support tool result input.
4. Add tool ID mapping.
5. Test Codex shell/tool-like roundtrips.

Milestone 5: lazy per-model probing

1. Add probe cache.
2. Probe `responses`.
3. Probe `anthropic`.
4. Probe `openai_chat`.
5. Add error classification.
6. Add refresh probe endpoint.

Milestone 6: multiple upstream targets and fallback

1. Add route target ordering.
2. Add fallback policy.
3. Add retry before commit.
4. Add logs showing attempted targets.

Milestone 7: state ownership

1. Generate ModelWire response IDs.
2. Store canonical transcript.
3. Implement same-upstream `previous_response_id`.
4. Implement cross-upstream materialized replay.
5. Implement `state_scope` optimistic handle reuse.

Milestone 8: Anthropic adapter

1. Convert canonical messages to Anthropic Messages.
2. Convert tools to Anthropic tools.
3. Convert tool_use/tool_result.
4. Convert stream events.
5. Add auth header rewrite.

Milestone 9: WebUI

1. Vite React app.
2. Admin auth.
3. Provider management.
4. Route/target management.
5. Probe status.
6. Request logs.

Milestone 10: public hardening

1. Rate limits.
2. Concurrency limits.
3. Prompt/tool log policy.
4. Admin audit log.
5. Docker image.
6. Deployment docs.
7. Security review.

## 32. Test plan

Use slice-first testing as the default development method.

ModelWire is a protocol relay. Many important bugs appear only at the project
boundary: what the downstream client sends, what ModelWire sends upstream, what
the upstream returns, what ModelWire returns downstream, and what state/archive
side effects happen. Therefore implement tests as full request slices before
or alongside internal units.

Recommended test slice shape:

```text
slice name
  downstream request fixture
  route/provider config fixture
  mock upstream expected request capture
  mock upstream response fixture
  expected downstream response or SSE events
  expected database records
  expected archive record, if archive capture is enabled
  expected logs/metrics, if relevant
```

Each slice should run ModelWire almost like production:

```text
test client
  -> real ModelWire HTTP server or router service
  -> real route/probe/adapter code
  -> mock upstream HTTP server that captures requests
  -> real response normalization
  -> test assertions
```

Do not test adapters only by calling conversion functions. Conversion unit tests
are useful, but the acceptance tests must prove boundary behavior.

The mock upstream server must be able to:

1. Capture method, path, query, headers, and JSON body.
2. Assert no raw downstream ModelWire response ID is passed upstream.
3. Assert upstream model ID is the mapped model ID.
4. Assert auth headers are rewritten correctly.
5. Return JSON responses.
6. Return SSE streams.
7. Return malformed JSON.
8. Return malformed SSE.
9. Return chosen status codes such as `400`, `401`, `403`, `404`, `405`, `429`,
   `500`, `502`, `503`, and `504`.
10. Delay response start to test timeouts.
11. Delay stream events to test idle timeouts.
12. Close connection early to test fallback/cancellation.
13. Record whether ModelWire cancelled the upstream request after downstream
   disconnect.

Write slice fixtures as files when possible:

```text
tests/fixtures/slices/<slice-name>/
  config.toml
  downstream-request.json
  upstream-expected.json
  upstream-response.json
  downstream-expected.json
  db-expected.json
  archive-expected.json
```

For streaming slices:

```text
tests/fixtures/slices/<slice-name>/
  upstream-events.sse
  downstream-expected-events.sse
```

The test harness should provide strict and loose assertions:

```text
strict
  Exact JSON match except dynamic IDs/timestamps.

loose
  JSONPath-style assertions for fields that matter.
```

Dynamic fields should use matchers:

```text
"id": "$matches:resp_mw_*"
"created_at": "$is_timestamp"
"request_id": "$matches:req_mw_*"
"provider_base_url_hash": "$is_sha256"
```

Slice-first implementation order:

1. Write the slice fixture.
2. Write the mock upstream behavior.
3. Write expected upstream capture assertions.
4. Write expected downstream response assertions.
5. Write expected persistence/archive assertions.
6. Run the test and watch it fail.
7. Implement the minimum code needed to make the slice pass.
8. Add unit tests for any complex helper created while making the slice pass.

Minimum required slices:

```text
codex_simple_text_nonstream
  Codex-style request returns Responses-shaped non-stream response.

codex_simple_text_stream
  Codex-style streaming request returns Responses SSE events.

codex_tool_loop_shell_like
  Function tool call and function_call_output roundtrip works across two turns.

codex_context_overflow_before_upstream
  Oversized Codex request is rejected/fallbacked before upstream call.

responses_text_basic
  Downstream Responses request maps to native Responses upstream and returns
  Responses JSON.

chat_text_basic
  Downstream Responses request maps to Chat Completions upstream and returns
  Responses JSON.

responses_stream_text_basic
  Native Responses SSE maps to downstream Responses SSE.

chat_stream_text_basic
  Chat SSE maps to downstream Responses SSE.

model_mapping_capture
  Downstream model differs from upstream model and mock upstream captures the
  mapped upstream model.

auth_header_rewrite_openai
  Downstream Authorization reaches OpenAI-like upstream as Authorization.

auth_header_rewrite_anthropic
  Downstream Authorization reaches Anthropic-like upstream as x-api-key.

tool_call_roundtrip_chat
  Tools flow from downstream to Chat upstream, tool call returns downstream,
  tool result maps back upstream on next request.

tool_call_roundtrip_responses
  Tools flow through native Responses upstream with ModelWire-owned IDs.

fallback_429_before_commit
  First upstream returns 429 before output; second upstream succeeds.

no_fallback_after_sse_commit
  First upstream emits one downstream event then fails; second upstream is not
  called.

probe_per_upstream_model
  Same provider but two upstream models produce two probe cache entries.

probe_shared_for_same_upstream_model
  Two downstream model aliases mapping to the same upstream model share probe.

previous_response_same_upstream
  Downstream ModelWire previous ID maps to upstream previous ID.

previous_response_cross_upstream_replay
  Different state scope causes materialized replay, not raw upstream ID reuse.

state_scope_optimistic_reuse_success
  Same state scope tries old upstream response ID and succeeds.

state_scope_optimistic_reuse_failure_then_replay
  Same state scope ID reuse fails before commit, then replay succeeds.

archive_visible_only_lineage
  Conversation archive captures visible text and full upstream lineage.

archive_off_writes_nothing
  Capture mode off writes no archive records.

restart_preserves_state
  Non-expired response continuation works after process restart.

janitor_keeps_referenced_chain
  Cleanup does not delete state still referenced by a non-expired child.

context_guard_rejects_before_upstream
  Oversized request is rejected before any upstream call when no fallback or
  summarization policy is configured.

context_guard_fallback_to_larger_target
  Oversized request for a small target falls back to a configured larger-context
  target before commit.

context_metadata_reports_conservative_window
  Model metadata reports the safe route context window, not an unrelated larger
  Codex/OpenAI window.

no_silent_truncation
  Oversized history is not silently dropped to fit the upstream context.
```

Example slice fixture:

```text
tests/fixtures/slices/chat_text_basic/
  config.toml
  downstream-request.json
  upstream-expected.json
  upstream-response.json
  downstream-expected.json
```

`downstream-request.json`:

```json
{
  "model": "codex-main",
  "instructions": "You are concise.",
  "input": "Say hello.",
  "stream": false
}
```

`config.toml`:

```toml
[[providers]]
id = "mock-chat"
name = "Mock Chat"
base_url = "http://mock-upstream/v1"
auth_mode = "pass_authorization"
default_wire_api = "openai_chat"
state_scope = "mock-chat"

[[routes]]
downstream_model = "codex-main"

[[routes.targets]]
provider = "mock-chat"
upstream_model = "mock-chat-model"
wire_api = "openai_chat"
priority = 10
```

`upstream-expected.json`:

```json
{
  "method": "POST",
  "path": "/v1/chat/completions",
  "headers": {
    "authorization": "$matches:Bearer *"
  },
  "body": {
    "model": "mock-chat-model",
    "messages": [
      {"role": "system", "content": "You are concise."},
      {"role": "user", "content": "Say hello."}
    ],
    "stream": false
  },
  "must_not_contain": [
    "resp_mw_"
  ]
}
```

`upstream-response.json`:

```json
{
  "id": "chatcmpl_upstream_1",
  "object": "chat.completion",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello."
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 2,
    "total_tokens": 12
  }
}
```

`downstream-expected.json`:

```json
{
  "id": "$matches:resp_mw_*",
  "object": "response",
  "model": "codex-main",
  "output": [
    {
      "type": "message",
      "role": "assistant",
      "content": [
        {
          "type": "output_text",
          "text": "Hello."
        }
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "output_tokens": 2,
    "total_tokens": 12
  },
  "must_not_contain": [
    "chatcmpl_upstream_1"
  ]
}
```

The test for this slice must assert:

1. Mock upstream received exactly one request.
2. Request path was `/v1/chat/completions`.
3. Request body model was `mock-chat-model`, not `codex-main`.
4. Downstream response model was `codex-main`, not `mock-chat-model`.
5. Downstream response ID used ModelWire prefix.
6. Upstream ID was not exposed downstream.
7. SQL state stored the upstream ID privately.
8. Logs did not contain raw Authorization value.

Unit tests:

1. Route resolution.
2. Model mapping.
3. Probe cache keys.
4. Error classification.
5. Header transformation.
6. Responses-to-canonical parsing.
7. Canonical-to-chat conversion.
8. Canonical-to-anthropic conversion.
9. Tool ID mapping.
10. Reasoning stripping policy.

Integration tests with mock upstreams:

1. Native Responses non-streaming text.
2. Native Responses streaming text.
3. Chat Completions non-streaming text.
4. Chat Completions streaming text.
5. Anthropic Messages non-streaming text.
6. Anthropic Messages streaming text.
7. Function tool call roundtrip.
8. Fallback from 429 to next target before commit.
9. No fallback after first SSE event.
10. Same-upstream previous response continuation.
11. Cross-upstream `state_scope` handle reuse success.
12. Cross-upstream handle reuse failure followed by materialized replay.
13. Invalid downstream response ID.
14. Context too long.
15. Downstream disconnect cancels upstream.

Manual tests:

1. Configure Codex provider to `base_url = "http://127.0.0.1:8787/v1"` and
   `wire_api = "responses"`.
2. Ask Codex a simple text question.
3. Ask Codex to run a tool-like task.
4. Force first target to return 429 and confirm fallback.
5. Switch mapping from native Responses to Chat adapter and confirm replay.
6. Confirm logs redact keys and prompts by default.

## 33. Codex config example

Example Codex provider config:

```toml
model_provider = "ModelWire"
model = "gpt-5.5"

[model_providers.ModelWire]
name = "ModelWire"
base_url = "http://127.0.0.1:8787/v1"
wire_api = "responses"
```

Codex still believes it is calling a Responses API. ModelWire decides what
actually happens upstream.

## 34. Non-goals for v1

Do not implement these in v1 unless explicitly required:

1. Full OpenAI API surface.
2. Audio/realtime API.
3. Batch API.
4. Fine-tuning API.
5. Full provider marketplace.
6. Kubernetes operator.
7. Complex multi-tenant billing.
8. Prompt observability by default.
9. Built-in prompt rewriting.
10. Tool execution inside ModelWire.

## 35. Dangerous mistakes to avoid

1. Do not forward downstream `previous_response_id` directly upstream.
2. Do not expose upstream response IDs to downstream clients.
3. Do not assume protocol support is provider-wide. Probe per upstream model.
4. Do not fallback after streaming has started.
5. Do not strip tools silently for Codex requests.
6. Do not log API keys.
7. Do not log prompts by default.
8. Do not expose raw thinking text as assistant output.
9. Do not replay encrypted reasoning to unrelated providers.
10. Do not build the WebUI before the data plane works.
11. Do not make New API-specific assumptions in the core.
12. Do not treat `429` or `5xx` as protocol unsupported.
13. Do not treat `401` or `403` as fallback-friendly by default.
14. Do not use raw JSON everywhere. Normalize into canonical structs.
15. Do not create a proxy that only supports text. Codex needs streaming and
    tools.

## 36. Minimum viable definition of done

The first useful public alpha is done when:

1. Codex can point at ModelWire as a Responses provider.
2. A downstream model can map to a different upstream model.
3. Native Responses and OpenAI Chat upstreams work for text.
4. Streaming text works.
5. Function tools work well enough for Codex to continue tool loops.
6. Per-upstream-model lazy protocol detection works.
7. A route can contain at least two targets and fallback before commit.
8. ModelWire IDs are used downstream.
9. Same-upstream `previous_response_id` works.
10. Cross-upstream replay works when IDs cannot be reused.
11. Logs are redacted by default.
12. There is at least a minimal admin API or config file workflow.

## 37. Edge-case checklist

This section is deliberately mechanical. Future implementers may be small local
models that follow instructions literally. Do not skip an edge case because it
"seems obvious".

### 37.1 HTTP and request parsing

Handle these cases explicitly:

1. Unknown path:
   return `404` with normalized JSON error.
2. Unsupported method on known path:
   return `405` with normalized JSON error.
3. Missing `content-type`:
   accept JSON if the body parses as JSON, but log `content_type_missing`.
4. Non-JSON body:
   return `400 request_invalid`.
5. Malformed JSON:
   return `400 request_invalid`.
6. JSON body larger than configured limit:
   return `413 request_too_large`.
7. Empty body:
   return `400 request_invalid`.
8. Unknown top-level fields:
   preserve them in `raw_downstream`, ignore unless they conflict with known
   behavior.
9. Missing `model`:
   return `400 request_invalid`.
10. `stream` omitted:
   treat as `false`.
11. `input` as string:
   convert to one user text input item.
12. `input` as array:
   parse supported item types and preserve unknown items as unsupported
   canonical items.
13. `instructions` present:
   store with the response record.
14. `previous_response_id` present with missing local state:
   return `404 state_not_found` unless a configured compatibility mode allows
   direct upstream pass-through. Default is no direct pass-through.
15. Duplicate JSON object keys:
   use serde default behavior, but do not rely on duplicates. Add a test if a
   custom parser is used.

### 37.2 Authentication and authorization

Handle these cases explicitly:

1. Missing downstream auth when auth is enabled:
   return `401 auth_failed`.
2. Invalid downstream relay key:
   return `401 auth_failed`.
3. Downstream key valid but not allowed for requested route:
   return `403 auth_failed`.
4. Passthrough auth disabled but request supplies an upstream-looking key:
   ignore the passthrough key and use managed credentials, or reject according
   to config. Do not silently become an open proxy.
5. Provider managed key missing:
   return `500 internal_error` to downstream, log `provider_key_missing`
   redacted.
6. Header rewrite fails:
   return `500 internal_error`, never expose the key.
7. Admin auth missing:
   return `401`.
8. Admin auth valid but lacking permission:
   return `403`.

### 37.3 Model route resolution

Handle these cases explicitly:

1. No route for downstream model:
   return `404 model_not_found`.
2. Route disabled:
   return `404 model_not_found` by default so disabled routes do not leak.
3. Route exists but all targets disabled:
   return `503 upstream_unavailable`.
4. Route target references missing provider:
   skip target, log config error, try next target before commit.
5. Route has duplicate priorities:
   sort by priority, then stable target ID.
6. Downstream model maps to same upstream model:
   still record the mapping.
7. Downstream model maps to different upstream model:
   archive both IDs.
8. Wildcard mappings:
   do not implement in v1. Exact match only.
9. Admin changes route while request is running:
   the request uses a snapshot of the route selected at request start.
10. Admin deletes provider while streams are running:
   existing streams continue with their selected provider snapshot.

### 37.4 Protocol probing

Handle these cases explicitly:

1. Probe cache hit:
   do not re-probe.
2. Probe cache expired:
   re-probe lazily on next request.
3. Concurrent requests probe the same target:
   use a single-flight lock so only one probe runs.
4. Probe receives `404`, `405`, or `501`:
   mark protocol unsupported and try next protocol.
5. Probe receives `401` or `403`:
   stop probing that provider/key/model. Do not fallback to another protocol on
   the same target.
6. Probe receives `429`:
   temporary failure. Do not mark protocol unsupported.
7. Probe receives `5xx`:
   temporary failure. Do not mark protocol unsupported.
8. Probe succeeds for text but real request has tools:
   if tool support unknown, run lightweight tool probe or skip target if config
   requires known tool support.
9. Forced `wire_api`:
   skip probing, but still record a synthetic probe result for UI visibility.
10. Probe request itself must not be archived as user conversation data.
11. Context-length failure:
   do not mark protocol unsupported.
12. Context-length failure may update target health or max observed failure
   size, but it must not change detected `wire_api`.

### 37.5 Upstream request building

Handle these cases explicitly:

1. Upstream base URL with trailing slash:
   join paths without double slashes.
2. Upstream base URL without `/v1`:
   use exactly what the provider config says; do not guess.
3. Upstream timeout:
   use configured timeout. Default should be conservative, for example 120
   seconds for non-stream startup.
4. Stream idle timeout:
   abort if no event arrives for configured time, for example 60 seconds.
5. Max stream duration:
   abort long streams after configured limit, for example 30 minutes.
6. Unsupported generation parameter:
   omit it for that adapter and record `parameter_omitted` in debug metadata.
7. Unknown modality:
   return `400 request_invalid` unless pass-through support is implemented.
8. Image/file references:
   v1 may reject with clear `unsupported_input_type` unless explicitly
   implemented.
9. `max_output_tokens` too low or zero:
   pass through if upstream accepts it; otherwise normalize to upstream minimum
   only if config permits.
10. `temperature`, `top_p`, penalties:
   pass only when adapter/provider supports them.

### 37.6 Streaming and SSE

Handle these cases explicitly:

1. Upstream stream fails before first semantic event:
   fallback if eligible.
2. Upstream stream sends malformed event before commit:
   fallback if eligible.
3. Upstream stream sends keepalive comments:
   ignore or forward as comments, but do not count as response commit unless a
   downstream event is sent.
4. ModelWire sends `response.created`:
   downstream response is committed. No fallback after this.
5. Upstream stream fails after commit:
   emit `response.failed` if possible, then close stream.
6. Downstream client disconnects:
   cancel upstream request and mark response `cancelled`.
7. SSE data contains multi-byte UTF-8 split across chunks:
   parser must handle it correctly.
8. Text deltas arrive before output item event from upstream:
   synthesize an output item before emitting text delta downstream.
9. Tool argument deltas arrive split across chunks:
   accumulate exact bytes/string fragments in order.
10. Upstream sends final usage only at end:
   update response record after final event.
11. Upstream sends duplicate completion event:
   ignore duplicates after first terminal event.
12. No `[DONE]` marker:
   rely on stream close plus final event semantics by adapter.

### 37.7 Tool calling

Handle these cases explicitly:

1. Request includes tools but target does not support tools:
   skip target before commit; do not strip tools by default.
2. Tool schema is invalid JSON Schema:
   return `400 request_invalid`.
3. Tool name missing:
   return `400 request_invalid`.
4. Duplicate tool names:
   return `400 request_invalid`.
5. Upstream emits tool call without ID:
   generate stable ModelWire call ID and internal upstream pseudo-ID.
6. Upstream emits duplicate tool call IDs:
   disambiguate internally and log warning.
7. Upstream streams malformed JSON arguments:
   forward deltas, but mark final tool call invalid if it cannot be parsed when
   complete.
8. Downstream sends tool result for unknown call ID:
   return `400 tool_mapping_failed`.
9. Downstream sends tool result twice:
   accept only if exact duplicate and idempotency is enabled; otherwise return
   `400 tool_mapping_failed`.
10. Parallel tool calls disabled:
   if upstream emits multiple parallel calls, return them but log provider
   violation; do not reorder.
11. Tool output too large:
   store according to operational policy, truncate only for logs/archive, not
   for active replay unless context policy explicitly says so.
12. Tool result contains binary data:
   reject or encode according to supported input types. Do not guess.

### 37.8 Response state and replay

Handle these cases explicitly:

1. `previous_response_id` points to expired state:
   return `404 state_not_found`.
2. `previous_response_id` points to failed response:
   allow continuation only if canonical transcript has enough visible output;
   otherwise return `409 state_not_continuable`.
3. Response chain has a cycle due to database corruption:
   abort replay and return `500 state_replay_failed`.
4. Same upstream handle exists:
   try upstream `previous_response_id`.
5. Same upstream handle rejected as not found:
   materialize transcript before commit.
6. Cross-provider same `state_scope`:
   try upstream handle reuse only if config allows it.
7. Cross-provider different `state_scope`:
   never try raw upstream handle.
8. Materialized transcript exceeds context:
   apply configured context policy or return context-length error.
9. Instructions changed between turns:
   use the current downstream request instructions when present; otherwise use
   stored instructions according to Responses semantics documented in this plan.
10. Hidden reasoning state unavailable:
   continue with visible transcript. Do not invent reasoning.
11. Encrypted reasoning belongs to unrelated provider:
   do not replay it.

### 37.9 Retention and cleanup

Handle these cases explicitly:

1. Operational state reaches TTL:
   delete only when no non-expired child response depends on it.
2. Probe result reaches TTL:
   delete or mark expired.
3. Request log reaches TTL:
   delete or compact according to policy.
4. Archive retention disabled:
   keep archives indefinitely until manual delete.
5. Archive TTL reached:
   delete only archive files whose manifest policy permits deletion.
6. Janitor interrupted mid-delete:
   next run must be safe and idempotent.
7. SQLite vacuum:
   never run while long write transaction is active.
8. Database unavailable:
   data plane should return `503` rather than running in unsafe memory-only mode.

### 37.10 Conversation archives

Handle these cases explicitly:

1. Archive capture mode `off`:
   write no conversation archive content.
2. `metadata_only`:
   write no user/assistant text.
3. `visible_only`:
   write visible messages and tool summaries only.
4. `full_visible`:
   write visible messages and full visible tool outputs after redaction.
5. `debug_raw`:
   require explicit config flag and admin warning.
6. Archive segment write fails:
   do not fail the user request by default; log archive error and expose metric.
7. Disk full:
   disable archive writer and alert via logs/metrics.
8. Manifest update fails:
   keep segment temp file or mark archive dirty; repair on startup.
9. Startup finds dirty archive:
   scan segments, validate checksums if present, rebuild manifest/index.
10. Assistant message generated by multiple upstream attempts:
   record all attempts and mark the winning attempt.
11. A conversation switches upstream mid-chain:
   each assistant item records its own upstream lineage.
12. Redaction rejects a record:
   write to rejected segment with reason if policy permits, otherwise drop.

### 37.11 WebUI and admin API

Handle these cases explicitly:

1. Admin edits provider base URL:
   invalidate provider cache and relevant probes.
2. Admin edits route target:
   invalidate route cache.
3. Admin refreshes probe:
   clear probe cache and run fresh probe.
4. Admin disables route:
   existing streams continue, new requests fail route resolution.
5. Admin imports invalid config:
   reject entire import by default. Do not partially apply.
6. Admin exports config:
   redact secrets unless explicit secure backup mode is selected.
7. WebUI cannot reach admin API:
   show clear connection error.
8. WebUI log screen:
   never show raw key, prompt, or tool output unless enabled and authorized.

### 37.12 Observability

Every request should produce structured logs with:

```text
request_id
downstream_key_hash
downstream_model
route_id
target_id
provider_id
upstream_model
wire_api
detected_wire_api
attempt_number
fallback_reason
status
latency_ms
input_tokens
output_tokens
reasoning_tokens
error_kind
```

Metrics should include:

```text
requests_total
requests_in_flight
stream_requests_in_flight
upstream_attempts_total
fallbacks_total
probe_attempts_total
probe_failures_total
tool_calls_total
archive_write_failures_total
state_replay_failures_total
request_latency_ms
upstream_latency_ms
stream_duration_ms
```

## 38. Milestone acceptance criteria

A milestone is not done because code "looks right". It is done only when every
acceptance item for that milestone passes in automated tests or an explicitly
documented manual test.

Use mock upstream servers for acceptance. Real providers are optional smoke
tests only and must not be required for CI.

### 38.1 Milestone 0 acceptance

Required:

1. `cargo fmt --check` passes.
2. `cargo clippy --workspace --all-targets -- -D warnings` passes, or warnings
   are explicitly documented if the command is not yet available.
3. `cargo test --workspace` passes.
4. `modelwire --help` or equivalent binary help works.
5. `modelwire serve --config <file>` starts.
6. `GET /healthz` returns `200`.
7. Invalid config fails fast with a clear error.
8. Logs include request IDs for HTTP requests.

### 38.2 Milestone 1 acceptance

Required:

1. Mock native Responses upstream receives `/v1/responses`.
2. Downstream `model = "gpt-5.5"` maps to configured upstream model.
3. Non-streaming text request returns Responses-shaped JSON.
4. Response ID returned to downstream starts with ModelWire prefix.
5. Upstream response ID is not exposed in downstream JSON.
6. Response metadata is persisted in SQL.
7. Missing model returns `400`.
8. Unknown model returns `404`.
9. Upstream `401` returns normalized auth error.
10. Logs redact downstream and upstream keys.

### 38.3 Milestone 2 acceptance

Required:

1. Mock Chat Completions upstream receives `/v1/chat/completions`.
2. Canonical user input maps to chat `messages`.
3. Chat assistant text maps back to Responses output text.
4. `instructions` map to system/developer-compatible chat messages.
5. Same downstream request works with native Responses target and Chat target by
   changing only route config.
6. Chat adapter never sends `previous_response_id`.
7. Unsupported image/file input returns clear `400` unless implemented.

### 38.4 Milestone 3 acceptance

Required:

1. Streaming native Responses upstream maps to downstream Responses SSE.
2. Streaming Chat Completions upstream maps to downstream Responses SSE.
3. First downstream semantic event commits the response.
4. Upstream failure before first downstream event falls back to next target.
5. Upstream failure after first downstream event does not fallback.
6. Downstream disconnect cancels upstream request.
7. UTF-8 split across chunks is handled correctly.
8. Final usage is persisted when upstream sends it.

### 38.5 Milestone 4 acceptance

Required:

1. Function tool definitions pass to native Responses upstream.
2. Function tool definitions pass to Chat upstream.
3. Upstream streamed tool call arguments emit Responses tool argument deltas.
4. Completed tool call has a stable ModelWire call ID.
5. Downstream tool result maps back to upstream expected format.
6. Unknown tool result call ID returns `400 tool_mapping_failed`.
7. Target without tool support is skipped, not used with tools stripped.
8. Parallel tool calls are preserved in order received.

### 38.6 Milestone 5 acceptance

Required:

1. Probe cache key includes provider ID, credential hash, and upstream model.
2. Two downstream models mapping to different upstream models probe separately.
3. Two downstream models mapping to same upstream model share probe result.
4. Probe order is Responses, Anthropic, then Chat.
5. `404`/`405`/`501` protocol failures advance to next protocol.
6. `401`/`403` stop probing and return auth error.
7. `429` and `5xx` do not mark protocol unsupported.
8. Concurrent identical probes single-flight.
9. Manual refresh clears cache and re-probes.
10. Probe requests are not written to conversation archives.

### 38.7 Milestone 6 acceptance

Required:

1. Route with three targets tries them in priority order.
2. First target `429` before commit falls back to second target.
3. First target connection reset before commit falls back.
4. First target malformed stream before commit falls back.
5. First target emits downstream event then fails: no fallback.
6. Attempt log records all attempted targets.
7. Archive lineage records all attempts and winning target.
8. Route snapshot is stable for a running request even if admin edits route.

### 38.8 Milestone 7 acceptance

Required:

1. Downstream `previous_response_id` resolves local state.
2. Same upstream continuation sends upstream handle, not ModelWire ID.
3. Same upstream handle rejection falls back to materialized replay before
   commit.
4. Different `state_scope` never receives raw upstream handle.
5. Same `state_scope` tries optimistic handle reuse if enabled.
6. Optimistic handle reuse failure falls back to replay before commit.
7. Expired response state returns `404 state_not_found`.
8. Process restart preserves non-expired response continuation.
9. Replay includes user messages, assistant visible output, tool calls, and tool
   results in order.
10. Replay does not include raw hidden reasoning.

### 38.9 Milestone 8 acceptance

Required:

1. Anthropic adapter sends correct auth headers for configured provider.
2. System/developer instructions map to Anthropic-compatible system context.
3. Anthropic text response maps to Responses text.
4. Anthropic `tool_use` maps to Responses function call.
5. Responses tool result maps to Anthropic `tool_result`.
6. Anthropic streaming text maps to Responses SSE.
7. Anthropic streaming tool input maps to Responses tool argument deltas.
8. Anthropic usage maps to canonical usage when available.

### 38.10 Milestone 9 acceptance

Required:

1. Admin login is required.
2. Provider can be created, edited, disabled, and deleted.
3. Route can be created with multiple ordered targets.
4. Target priority can be changed.
5. Probe status screen shows protocol, tool support, last success/failure.
6. Refresh probe button works.
7. Request log screen shows redacted data only by default.
8. Config export redacts secrets by default.
9. Config import validates all records before applying changes.
10. WebUI build is served by Rust backend.

### 38.11 Milestone 10 acceptance

Required:

1. Rate limit by downstream key works.
2. Rate limit by IP works.
3. Concurrency limit works.
4. Body size limit works.
5. Stream idle timeout works.
6. Max stream duration works.
7. Janitor deletes expired state safely.
8. Janitor never deletes referenced non-expired chain state.
9. Archive writer rotates and compresses segments.
10. Archive manifest checksums validate.
11. Archive index can be rebuilt from files.
12. Docker image starts with config.
13. Public deployment guide documents TLS/reverse proxy/auth.
14. Mandatory security tests in section 28.1 pass for implemented features.
15. Public bind without downstream auth fails startup.
16. Logs, config export, and archive files are verified not to contain raw API
    keys in automated tests.
17. Provider URL SSRF protection tests pass.
18. Admin CSRF/XSS tests pass if WebUI/admin cookie auth is implemented.

### 38.12 Public alpha acceptance

The public alpha is accepted only when all of these pass:

1. A Codex client can use ModelWire as `wire_api = "responses"`.
2. Codex can complete a simple text turn through native Responses upstream.
3. Codex can complete a simple text turn through Chat upstream.
4. Codex can stream text through at least one upstream.
5. Codex can run a tool loop through at least one upstream.
6. Model mapping works.
7. Multi-target fallback works before commit.
8. No fallback occurs after commit.
9. Same-upstream `previous_response_id` works.
10. Cross-upstream replay works.
11. Process restart does not break non-expired continuation.
12. Logs and archives do not contain raw API keys.
13. Raw hidden reasoning is not exposed as assistant text.
14. Conversation archive contains full upstream lineage.
15. All required automated tests pass.
16. All mandatory security tests for enabled features pass.
17. No known high-risk security gap remains in auth, secret storage, SSRF,
    admin CSRF/XSS, archive protection, or open proxy prevention.

## 39. Handoff rules for low-capability implementer models

The project may be implemented by small models around 10B active parameters.
Assume the implementer will follow literal instructions and may not infer
missing behavior. Therefore:

1. Every task must name exact files to edit.
2. Every task must include expected behavior.
3. Every task must include tests to add.
4. Every task must state what not to change.
5. Do not ask the implementer to "make it robust" without listing cases.
6. Do not ask the implementer to "support Responses API" without naming the
   exact subset.
7. Do not ask the implementer to "handle errors" without naming status codes and
   response shapes.
8. Do not ask the implementer to "add fallback" without stating pre-commit and
   post-commit behavior.
9. Do not ask the implementer to "persist state" without naming tables and TTL.
10. Do not ask the implementer to "archive conversations" without naming schema,
    file layout, redaction, and lineage fields.

Recommended task template:

```text
Task:
  Implement <one concrete feature>.

Files to edit:
  - <path>

Do not edit:
  - <path>

Behavior:
  1. ...
  2. ...

Edge cases:
  1. ...
  2. ...

Tests:
  1. ...
  2. ...

Acceptance:
  The task is complete only when <commands/tests> pass.
```

If an implementer cannot satisfy an acceptance item, they must document:

```text
blocked item
why it is blocked
what was implemented instead
what exact follow-up is needed
```

Do not accept vague completion reports such as "implemented fallback" or
"should work". Require test names, observed results, and any remaining gaps.

## 40. Current relay framework handoff

As of the current scaffold, `modelwire-server/src/relay.rs` is the main
data-plane seam for `/v1/responses`. Keep the route handler thin and add
behavior inside this pipeline:

```text
create_response
  -> relay_non_streaming_response
  -> snapshot_route
  -> parse_canonical_request
  -> try_target
  -> adapter.build_request
  -> upstream HTTP call
  -> adapter.parse_response
  -> normalize_downstream_response
  -> persist_response_shell
```

Framework behavior currently in place:

1. Request parsing supports non-streaming Responses text/message input,
   `instructions`, basic function tool definitions, generation parameters, and
   explicit target protocols.
2. Route and target config are snapshotted at request start.
3. The first explicit target protocol can be native Responses, OpenAI Chat, or
   Anthropic. `wire_api = "auto"` now resolves lazily with cache key
   `provider_id + credential_hash + upstream_model`, probe order
   `responses -> anthropic -> openai_chat`, and explicit stop behavior for
   auth failures (`401/403`).
4. Upstream responses are normalized to ModelWire-owned downstream response,
   message, and tool-call IDs. Upstream IDs remain private.
5. `previous_response_id` continuation now resolves persisted ModelWire-owned
   state, supports safe same-upstream handle reuse, and falls back to canonical
   replay when direct handle reuse is not safe or fails before commit.
6. Response shell persistence now stores route/target/provider/upstream metadata
   plus usage and stores upstream response handles in the private
   `upstream_handles` operational table. Upstream response IDs remain hidden
   from downstream JSON.
7. Non-streaming `previous_response_id` continuation now resolves local
   response state, returns `404 state_not_found` for missing chain state, uses
   same-target upstream handle continuation when safe, and falls back to
   visible canonical replay when the upstream handle is rejected.
8. `stream = true` now uses a dedicated relay path that emits downstream
   Responses-style SSE events, allows pre-commit fallback to later targets, does
   not fallback after the first semantic downstream event has been emitted, and
   enforces `stream_idle_timeout_secs` plus `max_stream_duration_secs` with
   post-commit failures mapped to downstream `response.failed` events.
9. Continuation tool-result validation now rejects unknown `call_id` values with
   `400 tool_mapping_failed` before any upstream call is attempted.
10. Context guard now runs before upstream calls. It estimates request/token
    budget conservatively per target and enforces `context_overflow_policy`
    (`reject` or `fallback`) before any upstream HTTP attempt.
11. Additional context-guard slice coverage now verifies:
    `context_guard_does_not_mark_protocol_unsupported`,
    `materialized_replay_budget_includes_history`, and
    `tool_schema_budget_counts_against_context`.
12. `/v1/models` now reports conservative context metadata per downstream route
    (minimum safe context/max-output across enabled targets) and context
    overflow paths are verified with `no_silent_truncation`.
13. `POST /v1/responses/compact` now routes through a capability-gated relay
    path: only targets resolving to native Responses are eligible, Chat and
    Anthropic targets are skipped, and missing support returns a stable
    `400 protocol_not_supported`.
14. Compact source lineage guard now prevents cross-provider/cross-`state_scope`
    replay: when compact requests reference local response state
    (`response_id`/`previous_response_id`), ModelWire requires provider +
    `state_scope` compatibility before forwarding upstream.
15. Added compact-focused slice coverage:
    `native_compact_not_sent_to_chat_or_anthropic`,
    `native_compact_not_replayed_across_state_scope`,
    `native_compact_forwarded_only_to_compatible_responses_target`,
    `missing_compact_support_falls_back_to_context_policy`,
    route-level forwarding success for compatible native targets, and
    route-level rejection when only non-compatible targets are configured.
16. Added configurable compaction modes in server config:
    `none`, `native_responses`, `local_summary`, `hybrid`.
    `hybrid` now prefers native Responses compaction and falls back to explicit
    local-summary compaction.
17. Added `compaction_lineage` operational persistence and local-summary
    lineage coverage (`local_summary_marks_lineage`) including source response
    IDs, summarizer model, prompt version, and token counts.
18. Added streaming timeout slice coverage:
    `streaming_idle_timeout_before_commit_falls_back_to_second_target` and
    `streaming_max_duration_after_commit_emits_failed_without_fallback`.
19. Added scoped relay-key authorization for downstream API:
    `security.relay_keys` now supports per-key model + provider scopes.
    Requests return strict `403` when the key lacks model permission or when
    no route target provider is allowed for that key.
20. Added trusted passthrough hard gate enforcement:
    `trusted_passthrough` now requires configured extra control header/value
    and is covered by `trusted_passthrough_requires_extra_gate`.
21. Added enforced per-key throttling:
    `security.relay_keys.requests_per_minute` and `max_concurrency` now enforce
    runtime limits and return `429 rate_limited` when exceeded; covered by
    strict security tests `rate_limit_by_key_returns_429` and
    `concurrency_limit_by_key_returns_429`.
22. Added enforced per-IP throttling:
    `security.ip_requests_per_minute` now enforces runtime per-minute limits
    keyed by client IP identity (`x-forwarded-for` first hop, then `x-real-ip`,
    else `unknown`) and returns `429 rate_limited` when exceeded; covered by
    strict security test `rate_limit_by_ip_returns_429`.
23. Added managed-key-missing guard:
    targets configured with `auth_mode = "managed"` now fail fast with
    `500 internal_error` if the provider key is missing, emit redacted
    `provider_key_missing` audit logs, and do not attempt upstream calls.
24. Added strict admin API auth + CSRF middleware:
    `/admin/api/*` now requires `admin_auth = "local_password"` credentials and
    rejects missing/invalid admin auth with `401`; state-changing methods
    (`POST`/`PUT`/`PATCH`/`DELETE`) require matching `admin_csrf` cookie and
    `x-csrf-token` header (plus presence of `admin_session`) and return `403`
    when the CSRF check fails.
25. Added admin security slice coverage hardening:
    `admin_api_requires_auth` now asserts strict unauthorized behavior for
    missing/invalid credentials and success for valid credentials;
    `admin_post_without_csrf_rejected` now asserts strict `403` without CSRF
    and success when cookie/header CSRF tokens match.
26. Added real startup/public-auth hardening:
    server startup now validates public bind + auth combinations and rejects
    `downstream_auth = "none"` on public bind/public deployment; public
    deployment with passthrough auth now requires
    `allow_passthrough_keys = true`.
27. Added admin same-origin guard and strict origin tests:
    `/admin/api/*` now rejects untrusted browser `Origin` values by default and
    allows only configured `server.public_base_url` origin (or localhost
    fallback when base URL is unset); `admin_cors_rejects_untrusted_origin`
    now asserts real router behavior instead of simulation.
28. Replaced simulation-style security checks with runtime assertions:
    `public_bind_without_auth_fails_startup` now exercises real `serve(...)`
    startup validation and `passthrough_disabled_rejects_public_request` now
    asserts strict runtime rejection behavior (`403`) through middleware.
29. Added real admin config-import validation path:
    `POST /admin/api/config/import` now parses full config payload, rejects
    partial/invalid records (`400 request_invalid`), validates provider/route
    referential integrity, and enforces provider URL SSRF checks before
    reporting import success.
30. Hardened config-import security test to runtime API behavior:
    `config_import_rejects_partial_invalid_payload` now performs authenticated
    admin requests and asserts server-side rejection for missing provider IDs,
    duplicate provider IDs, and SSRF-blocked provider URLs, plus success for a
    fully valid payload.
31. Replaced provider admin stubs with validated runtime behavior:
    `/admin/api/providers` create/read/update/delete now validates payloads,
    returns `404 state_not_found` for unknown provider IDs, and enforces SSRF
    validation for provider base URLs (unless explicit `skip_ssrf_validation`).
32. Added runtime admin-provider SSRF tests:
    authenticated admin provider-creation requests now assert `400` for
    localhost/private URLs and `201` for valid HTTPS provider URLs.
33. Replaced route/target admin stubs with validated runtime behavior:
    `/admin/api/routes` and `/admin/api/targets/*` now parse typed payloads,
    enforce provider reference validation, validate `wire_api` and
    `context_overflow_policy`, produce deterministic route/target IDs, and
    return `404 state_not_found` for unknown route/target IDs.
34. Added runtime admin route/target security coverage:
    authenticated admin CRUD tests now assert real status behavior for route
    and target validation failures, unknown IDs (`404`), and successful create/
    update/delete flows under auth + CSRF + same-origin middleware.
35. Added probe single-flight for concurrent identical auto-detection:
    protocol resolution now uses per-cache-key probe locks so concurrent
    requests sharing `(provider_id, credential_hash, upstream_model)` collapse
    to one upstream probe sequence while waiters reuse the resulting cache/db
    record.
36. Added runtime single-flight probe test coverage:
    `probe_concurrent_identical_requests_single_flight` verifies concurrent
    identical `WireApi::Auto` resolution triggers exactly one `/responses`
    probe and one fallback `/messages` probe, with both callers receiving the
    same resolved protocol.
37. Hardened admin probe refresh behavior:
    `POST /admin/api/probes/refresh` now clears both in-memory probe cache and
    probe-lock state, deletes persisted `probe_results` rows, and returns
    deterministic JSON with `persisted_cleared` row count.
38. Added runtime admin probe-refresh persistence test:
    `admin_refresh_probes_clears_cache_and_persisted_rows` verifies authenticated
    admin refresh clears cache entries, lock entries, and persisted probe rows
    in SQLite-backed operational state.
39. Implemented transactional admin config-apply for import:
    validated config import now replaces operational `providers`, `routes`, and
    `route_targets` tables in a single DB transaction and returns applied row
    counts (`providers`, `routes`, `targets`) in import response JSON.
40. Strengthened config-import runtime verification:
    `config_import_rejects_partial_invalid_payload` now asserts successful import
    response includes applied counts and verifies imported provider records are
    persisted and queryable from operational DB state.
41. Migrated admin route and target CRUD endpoints to DB-backed operational state:
    `/admin/api/routes`, `/admin/api/routes/{id}`, `/admin/api/routes/{id}/targets`,
    and `/admin/api/targets/{id}` now read/write `routes` and `route_targets`
    through repository methods, validate provider references against persisted
    providers, and return persisted route/target projections (including target
    metadata decoded from persisted `config_json`).
42. Added DB-seeded admin security fixture bootstrap:
    `build_public_state` now applies config into operational tables via
    transactional `replace_admin_config` so admin CRUD/security tests execute
    against persisted providers/routes/targets rather than only in-memory config.
43. Strengthened runtime admin route/target persistence checks:
    `admin_route_crud_enforces_validation_and_not_found` and
    `admin_target_crud_enforces_validation_and_not_found` now assert DB read-back
    after create/update/delete, and
    `admin_target_priority_update_changes_db_order` verifies target priority
    updates persist and reorder route target retrieval deterministically.
44. Implemented DB-backed admin request log listing:
    `/admin/api/logs` now returns persisted `request_logs` rows (newest first,
    paginated by `limit`) and total count from operational state instead of stub
    output, preserving redacted-by-default fields (hashes/metadata only).
45. Added runtime admin logs redaction coverage:
    `admin_logs_endpoint_returns_redacted_request_logs` verifies authenticated
    admin log retrieval returns persisted rows with hashed key material and no
    raw bearer/relay key leakage in the response body.
46. Implemented persisted probe status listing for admin API:
    `/admin/api/probes` now merges non-expired persisted `probe_results` rows
    (provider, credential hash, upstream model, detected wire API, capability
    flags, last success/failure, failure metadata, expiry) with cache-only
    entries, so probe status screens are no longer limited to in-memory cache.
47. Added runtime probe-list status coverage:
    `admin_list_probes_includes_persisted_status_fields` verifies authenticated
    admin probe listing returns persisted probe records including protocol and
    capability/status fields needed by the probe status UI.
48. Hardened upstream redirect handling in relay data plane:
    all upstream HTTP clients used by non-streaming, streaming, compact, and
    probe paths now disable automatic redirect following (`Policy::none`) so
    ModelWire never silently follows provider-issued redirects.
49. Added runtime SSRF redirect coverage:
    `upstream_redirect_to_private_ip_rejected` now uses two mock upstream
    servers and verifies a `302` Location redirect is not followed and returns
    `502 upstream_unavailable` instead of reaching the redirected host.
50. Implemented production archive segment sealing in `modelwire-archive`:
    conversation segments are finalized as `conversations-*.jsonl.zst`,
    checksums are computed from compressed bytes, and `manifest.json` is
    persisted on every segment close (not only final shutdown).
51. Hardened archive path safety in writer:
    relative archive segment paths are validated to reject traversal/absolute
    forms before write, and traversal coverage is enforced by
    `validate_archive_relative_path_rejects_traversal`.
52. Wired non-streaming relay success path to best-effort archive capture:
    `archive_successful_response` now applies configured capture mode
    (`off`, `metadata_only`, `visible_only`, `full_visible`, `debug_raw`),
    redacts archived visible content, hashes upstream IDs/base URLs by default,
    blocks `debug_raw` on public bind, and logs archive failures without
    failing downstream response delivery.
53. Added runtime archive relay coverage:
    `archive_capture_mode_off_writes_no_archive_files` verifies `off` writes
    nothing, and
    `archive_capture_visible_only_writes_redacted_visible_record` verifies
    `visible_only` writes redacted `.jsonl.zst` records with lineage fields.
54. Fixed archive lineage hashing to use upstream-private response handle:
    archive `models.upstream_response_id_hash` now derives from the captured
    upstream response ID handle (when present), not the downstream
    ModelWire-owned response ID.
55. Implemented scoped archive capture-mode override from relay key auth:
    `security.relay_keys.archive_capture_mode` now flows from downstream auth
    context into non-stream relay archive writes and can override global
    `archive.capture_mode` for that request.
56. Added archive capture-mode edge coverage for required modes:
    `archive_capture_metadata_only_excludes_visible_messages`,
    `archive_capture_full_visible_keeps_full_tool_result`, and
    `archive_debug_raw_public_bind_is_best_effort_non_blocking` verify mode
    semantics and non-blocking archive-failure behavior.
57. Added runtime auth-to-archive override coverage:
    `relay_key_auth_context_includes_archive_capture_mode_override` verifies
    relay-key scoped capture policy reaches runtime archive output behavior.
58. Upgraded logging/archive security tests from intent checks to runtime checks:
    `archive_capture_disabled_by_default`,
    `debug_raw_fails_on_public_bind_without_unsafe_flag`, and
    `probe_request_not_archived` now execute real router + upstream + archive
    flows and assert concrete archive filesystem outputs.
59. Added runtime archive checks for reasoning exclusion and checksum integrity:
    `hidden_reasoning_not_archived` now verifies visible archive records exclude
    reasoning output items/content, and
    `archive_manifest_checksum_validates` now verifies manifest checksum fields
    against the finalized compressed segment bytes.
60. Implemented filesystem-based archive index rebuild capability in
    `modelwire-archive` and runtime coverage:
    `rebuild_archive_index_from_files` now scans archive directories,
    validates manifest schema + segment checksums, and reconstructs deterministic
    index metadata; security test `archive_index_rebuild_from_files` now calls
    this real rebuild path against generated archive artifacts.
61. Persisted and surfaced full probe capability metadata:
    probe result storage now carries `supports_parallel_tool_calls` and
    `supports_reasoning_summary` end-to-end through probe persistence, admin
    probe listings, and runtime probe cache hydration; covered by
    `persisted_probe_roundtrip_keeps_parallel_and_reasoning_summary_flags` and
    `admin_list_probes_includes_persisted_status_fields`.
62. Added tool-bearing request target-eligibility guard for auto-detected
    targets:
    routing now skips targets when probe metadata indicates tool support is
    unknown/unsupported for a request that includes tools (instead of silently
    stripping tools), preserving fallback behavior and returning
    `protocol_not_supported` when no eligible target remains; covered by
    `tool_request_skips_auto_target_with_unknown_tool_support`.
63. Added synthetic probe visibility for forced `wire_api` targets:
    forced protocol resolution now skips network probing but records a
    synthetic success probe record in cache + DB for admin visibility, keyed by
    provider + credential hash + upstream model; covered by
    `forced_wire_api_records_synthetic_probe_visibility`.
64. Added fallback-attempt lineage capture in non-stream relay archiving:
    archive records now include all attempted targets (failed + winner) with
    per-attempt status/error/latency, set `routing.had_fallback` and
    `quality.had_fallback` correctly, and record winner attempt index via
    `request.fallback_attempt`; covered by runtime security slice
    `archive_lineage_records_all_attempts_and_winner_on_fallback`.
65. Added runtime request-log fallback-attempt coverage:
    pre-commit fallback requests are now explicitly verified to persist both
    failed-first-target and successful-fallback-target `request_logs` rows
    under one request ID, covering Milestone 6 attempt-log acceptance with
    `request_logs_record_all_fallback_attempts_before_commit`.
66. Added runtime route-snapshot stability coverage for in-flight requests:
    while a delayed `/v1/responses` request is executing, an authenticated admin
    route edit is applied; the in-flight request still completes via the
    originally selected target, proving running-request snapshot stability and
    no mid-flight route mutation effect. Covered by
    `running_request_keeps_route_snapshot_when_admin_edits_route`.
67. Added runtime multi-tool order preservation coverage for Chat adapter:
    non-stream Chat responses containing multiple `tool_calls` are now verified
    to produce downstream Responses `function_call` output items in the same
    received order, covering Milestone 4 parallel-tool-order acceptance with
    `chat_parallel_tool_calls_preserve_order_received`.
68. Added runtime Anthropic streaming tool-input delta coverage:
    Anthropic `content_block_delta` events carrying `input_json_delta` are now
    verified end-to-end to emit downstream Responses
    `response.function_call_arguments.delta` SSE events with JSON fragments,
    covering Milestone 8 streaming tool-input mapping acceptance via
    `anthropic_streaming_tool_input_maps_to_argument_deltas`.
69. Added runtime Anthropic usage-mapping coverage:
    non-stream Anthropic responses with usage fields (including
    `thinking_tokens`) are now verified end-to-end to populate downstream
    Responses usage (`input_tokens`, `output_tokens`, `total_tokens`,
    `reasoning_tokens`), covering Milestone 8 usage-mapping acceptance via
    `anthropic_usage_maps_to_downstream_usage`.
70. Added runtime WebUI backend-serving coverage:
    `webui_root_redirects_to_admin_login` verifies `/` redirects to
    `/admin/login`, and `webui_dist_index_served_by_backend` verifies the Rust
    backend serves `modelwire-webui/dist/index.html` for admin WebUI routes,
    covering Milestone 9 acceptance item 10.
71. Added real Docker and deployment-guide coverage:
    `docker_starts_with_config` now reads the checked-in Dockerfile and
    `.dockerignore` to verify the container starts ModelWire with an explicit
    config path while keeping WebUI dist assets in the build context, and the
    new public deployment guide documents TLS, reverse proxy, auth, rate
    limits, backup sensitivity, and archive sensitivity.
72. Added runtime CLI parsing coverage for container startup arguments:
    `cli_accepts_config_before_serve_subcommand` and
    `cli_defaults_to_modelwire_toml` now verify the actual Clap parser accepts
    `--config ... serve` (the Docker entrypoint shape) and defaults to
    `modelwire.toml` when config is omitted.
73. Added runtime hidden-reasoning non-exposure coverage for downstream output:
    `hidden_reasoning_not_exposed_as_assistant_text` verifies reasoning-summary
    text returned by upstream is not emitted as downstream assistant
    `output_text`, strengthening Public alpha acceptance item 13 with a direct
    response-payload assertion.
74. Replaced simulated managed-key-at-rest check with runtime DB/API assertions:
    `managed_upstream_key_encrypted_at_rest` now creates a managed provider via
    authenticated admin API, then verifies persisted provider rows do not store
    plaintext upstream keys (only `api_key_set` marker metadata) and provider
    read responses do not expose raw key material.
75. Replaced simulated health/readiness/metrics secret checks with runtime API
    assertions:
    `healthz_does_not_expose_config`, `readyz_does_not_expose_config`, and
    `metrics_do_not_include_raw_key_or_prompt` now execute the real router and
    assert emitted payloads do not leak secret/config markers, strengthening
    Public alpha acceptance around operational endpoint safety.
76. Enforced `413 request_too_large` for oversized downstream JSON bodies:
    `/v1/responses` and `/v1/responses/compact` now map body length-limit read
    failures to `request_too_large` (instead of generic `request_invalid`), and
    runtime route slice `create_response_rejects_oversized_body_with_413`
    verifies no upstream attempt occurs and downstream error code/status match
    section 37.1 request parsing requirements.
77. Hardened runtime SSRF validation + provider-override behavior:
    core SSRF host parsing now correctly handles bracketed IPv6 hosts and
    metadata hostnames (for example `metadata.google.internal`), and
    `validate_provider_url_for_provider(..., allow_private_ips = true)` now
    reuses parsed host data correctly so explicit private-IP opt-in works as
    configured. Runtime/security coverage is now enforced by
    `ssrf::tests::*` and `security_tests::ssrf_protection::*`, including
    `provider_url_rejects_localhost_by_default`,
    `provider_url_rejects_private_ip_by_default`,
    `provider_url_rejects_metadata_ip_by_default`,
    `provider_url_allows_private_ip_with_explicit_allow_flag`, and
    `upstream_redirect_to_private_ip_rejected`.

Next small-model implementation tasks should target these exact seams:

```text
Task:
  Add lazy probe resolution for explicit RouteSnapshot/TargetSnapshot.

Files to edit:
  - modelwire-server/src/relay.rs
  - modelwire-db/src/repo/probes.rs

Do not edit:
  - modelwire-server/src/routes/responses.rs

Behavior:
  1. Replace the `WireApi::Auto` error in `try_target` with probe lookup.
  2. Probe key must be provider ID + credential hash + upstream model.
  3. Probe order must be responses, anthropic, openai_chat.

Tests:
  1. Probe cache hit does not call upstream.
  2. 404 for responses advances to anthropic/chat.
  3. 401 stops probing.

Acceptance:
  The task is complete only when these commands pass:
  - cargo fmt --check
  - cargo test --workspace
```

```text
Task:
  Persist complete response metadata and upstream handles.

Files to edit:
  - modelwire-server/src/relay.rs
  - modelwire-db/src/repo/responses.rs

Do not edit:
  - modelwire-adapters/src/*.rs

Behavior:
  1. Store route_id, target_id, provider_id, upstream_model, wire_api,
     upstream_response_id, state_scope, previous_response_id, status, and usage.
  2. Store visible output items in response_items in order.
  3. Store upstream response IDs only in operational state, never downstream JSON.

Tests:
  1. Native Responses upstream ID is absent downstream but present in SQL.
  2. Response items are persisted in output order.

Acceptance:
  The task is complete only when these commands pass:
  - cargo fmt --check
  - cargo test --workspace
```

```text
Task:
  Add streaming framework using the same route snapshot and target attempt
  types.

Files to edit:
  - modelwire-server/src/relay.rs
  - modelwire-server/src/routes/responses.rs
  - modelwire-adapters/src/sse.rs

Do not edit:
  - modelwire-core/src/config.rs

Behavior:
  1. `stream = true` must call a new streaming relay path.
  2. Buffer upstream SSE until the first semantic event before downstream
     commit.
  3. Do not fallback after the first downstream SSE event.

Tests:
  1. Upstream failure before commit falls back.
  2. Upstream failure after commit emits failure and does not fallback.
  3. UTF-8 split across chunks is parsed correctly.

Acceptance:
  The task is complete only when these commands pass:
  - cargo fmt --check
  - cargo test --workspace
```
