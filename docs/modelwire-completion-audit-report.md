# ModelWire 严格完成度审查整改报告

审查日期：2026-05-19

审查对象：

- `AGENTS.md`
- `docs/modelwire-implementation-plan.md`
- Rust workspace 中的核心、服务端、数据库、适配器、归档和测试代码
- `modelwire-webui`
- 当前仓库内的部署、配置和测试证据

## 总结结论

结论：**不能接受“已完成”的说法；不能视为 public-ready；不能按当前状态对外暴露。**

这个仓库已经有大量实现和不少有价值的 slice tests，但严格按
`AGENTS.md` 和 `docs/modelwire-implementation-plan.md` 的验收纪律看，当前完成度存在明显断层：

- 有些安全能力只写成了“设计型测试”，没有跑真实 runtime 路径。
- WebUI/Admin 控制面和 `/v1/responses` 数据面不是同一个配置事实源。
- 公开部署相关的认证、密钥、日志、流式、Postgres、归档等关键路径仍有阻断问题。
- `cargo fmt --check` 与 WebUI lint 当前不通过，基础门禁本身就没过。

最严重的问题是：

1. public bind 下 `relay_key` 默认配置可以接受任意 `mw_` 前缀 key，`managed` downstream auth 分支也未实现真实授权。
2. WebUI 调用的登录、登出、会话检查 API 在后端不存在。
3. Admin API 写 SQL，数据面仍读内存里的 `state.config`，两条路径彼此脱节。
4. Admin 创建的 managed upstream key 没有加密保存，也无法供数据面使用，创建/更新响应还可能回显明文 key。
5. SSE streaming 被完整缓冲后一次性返回，不是真正的下游流式。
6. `wire_api = "auto"` 的 protocol probe 对 Chat/Anthropic 使用了 Responses 形状的 body。
7. Debug 日志可以输出完整 upstream request body，包含 prompt、tool schema 和 tool output。
8. Postgres migration 只建了部分表，无法支撑计划要求的数据库后端。
9. 工具调用的 call ID 映射和跨上游 replay 语义不可靠。
10. 当前测试存在“名字像验收测试，实际只断言本地 JSON/辅助函数”的情况。

## 实际执行的验证命令

| 命令 | 结果 | 说明 |
| --- | --- | --- |
| `git status --short` | 通过 | 审查报告创建前工作区干净；报告文件创建后只有报告文件未跟踪。 |
| `cargo fmt --check` | **失败** | `modelwire-server/tests/integration_slices.rs`、`modelwire-server/tests/security_tests.rs` 有格式 diff；rustfmt 还提示若干稳定版不支持的配置项。 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 | 当前 Rust 代码无 clippy warning。 |
| `cargo test --workspace` | 通过 | Rust 测试通过，但不能证明下面列出的 runtime 路径已经满足产品要求。 |
| `npm run build`（在 `modelwire-webui`） | 通过 | TypeScript 与 Vite build 通过。 |
| `npm run lint`（在 `modelwire-webui`） | **失败** | `AuthContext.tsx` 存在 `checkAuth` 使用早于声明，以及 fast-refresh export 规则问题。 |
| `cargo deny check` | **未能执行** | 当前环境未安装 `cargo-deny`。 |
| `npm audit --audit-level=high` | **未能完成审计** | 当前 npm registry mirror 返回 `[NOT_IMPLEMENTED] /-/npm/v1/security/*`。 |

## 严重级别定义

- **P0 阻断**：修复前不能称为完成，不能 public-ready。
- **P1 重大问题**：违反计划或架构要求，可能不立即造成公开泄露，但会影响正确性、可靠性或可运营性。
- **P2 质量缺口**：门禁、证据、可维护性或运营体验问题，需要在正式交付前解决。

## P0 阻断问题

### P0-01：public downstream auth 仍然不安全

证据：

- `modelwire-server/src/middleware/auth.rs` 在 `security.relay_keys` 为空时，只要 header 是 `Authorization: Bearer mw_*` 就放行。
- `modelwire-server/src/middleware/auth.rs` 的 `downstream_auth = "managed"` 分支只有注释，没有任何授权校验。
- `modelwire-server/src/server.rs` 的 `validate_startup_security` 允许 public bind + `relay_key`，即使没有配置任何 relay key。
- `modelwire-server/src/server.rs` 只有在 `public_deployment = true` 时才拒绝 passthrough；`0.0.0.0` + `public_deployment = false` + passthrough 仍可能启动。

影响：

- 默认 public relay key 配置可以变成开放代理。
- `managed` downstream auth 实际等同未授权。
- 操作者忘记设置 `public_deployment = true` 时，公开监听上的 passthrough 模式风险过高。

整改要求：

- public bind 下必须 fail closed：除非认证模式已完整实现且配置安全，否则启动失败。
- `relay_key` 模式必须要求至少一个 enabled key，或者显式 unsafe dev flag。
- `managed` 模式在未实现前必须拒绝启动或拒绝请求。
- public bind 下 passthrough/trusted passthrough 必须默认拒绝，不能依赖操作者手动标记 `public_deployment`。

必须补测：

- `public_bind_relay_key_without_configured_keys_fails_startup`
- `relay_key_empty_key_list_does_not_accept_any_mw_prefix`
- `managed_downstream_auth_requires_real_validation`
- `public_bind_passthrough_fails_without_unsafe_override`

### P0-02：Admin WebUI 登录/会话流不存在

证据：

- `modelwire-webui/src/api/client.ts` 调用 `/admin/api/auth/login`、`/admin/api/auth/logout`、`/admin/api/auth/check`。
- `modelwire-server/src/admin/mod.rs` 没有定义 `/auth/*` routes。
- `modelwire-server/src/server.rs` 把全部 `/admin/api` routes 都包在 admin auth 和 origin/CSRF middleware 下面。
- `modelwire-server/src/middleware/admin_auth.rs` 支持 bearer admin password 或 cookie+CSRF，但后端没有会话签发 endpoint。
- WebUI 的 `fetchJson` 没有为普通 Admin API 请求发送 bearer admin auth，也没有发送 CSRF header。

影响：

- WebUI 无法通过当前后端完成登录。
- 即使前端保存了某种登录状态，也无法拿到后端签发的 `admin_session`/`admin_csrf`。
- state-changing Admin API 在 cookie auth 设计下无法被当前 WebUI 正常调用。

整改要求：

- 要么实现真实的 Admin login/logout/check/session/CSRF endpoint；
- 要么删掉 cookie/session 假设，让 WebUI 明确使用当前 bearer admin auth 合约；
- state-changing 请求必须带 CSRF header，并且后端必须有真实会话、过期、登出、限速测试。

必须补测：

- `admin_login_sets_secure_session_and_csrf_cookies`
- `admin_auth_check_uses_real_session`
- `admin_logout_invalidates_session`
- `webui_login_can_fetch_providers_after_login`
- `webui_state_change_sends_csrf_header`

### P0-03：Admin/数据库控制面没有驱动数据面

证据：

- `/v1/responses` routing 在 `modelwire-server/src/relay.rs` 的 `snapshot_route` 中读取 `state.config`。
- `/v1/models` 在 `modelwire-server/src/routes/models.rs` 中读取 `state.config`。
- Admin provider/route/target CRUD 在 `modelwire-server/src/admin/mod.rs` 中写 `providers`、`routes`、`route_targets` SQL 表。
- Admin config import 调用 `replace_admin_config` 写 DB，但没有刷新 `state.config`。
- 启动过程加载 TOML 到内存并迁移数据库，但没有把 TOML seed 到 Admin DB，也没有把 DB 当成数据面配置源。

影响：

- WebUI/Admin 修改 provider、route、target 后，下一个 `/v1/responses` 请求仍按旧内存配置转发。
- `/v1/models` 也不会反映 Admin DB 里的模型变化。
- “operational state/database-backed” 的要求在 route/provider 配置层面不成立。

整改要求：

- 定义唯一的 runtime config source of truth。
- 启动时要么 seed TOML 到 DB，要么清晰声明 TOML 是 bootstrap-only。
- 数据面必须读取数据库-backed 的 immutable snapshot。
- Admin 修改后要原子刷新/swap runtime snapshot，同时保证进行中的请求继续使用旧 snapshot。

必须补测：

- `admin_created_route_is_used_by_next_data_plane_request`
- `admin_updated_target_changes_next_upstream_request`
- `admin_deleted_route_removes_model_from_v1_models`
- `startup_seeds_admin_db_from_config`
- `in_flight_request_keeps_old_route_snapshot`

### P0-04：managed upstream key 没有加密保存，也无法使用

证据：

- `modelwire-server/src/admin/mod.rs` 创建/更新 provider 时只在 `config_json` 中保存 `api_key_set` 标记，不保存加密密文。
- `provider_record_to_config` 重建 `ProviderConfig` 时把 `api_key` 设为 `None`。
- 数据面 `resolve_upstream_key` 只从 `TargetSnapshot.provider_api_key` 取 managed key，而该 snapshot 来源于内存 `state.config`。
- Admin create/update 响应返回提交的 provider/candidate 对象，可能包含明文 `api_key`。
- 名为 `managed_upstream_key_encrypted_at_rest` 的测试只验证 DB 中没有明文和 GET 响应不回显；没有验证密文存在、能解密、能用于真实 upstream request，也没有检查 create/update response。

影响：

- 通过 Admin API 创建的 managed provider 不能真正调用上游。
- 明文 upstream key 可能在 Admin 创建/更新响应中泄露。
- “managed upstream keys are encrypted at rest” 的安全声明没有实现证据。

整改要求：

- 增加 secrets table 或接入外部 secret store。
- 用认证加密保存 managed provider key，包含 key version/rotation metadata。
- create/update/read/export 响应都必须默认 redacted。
- 数据面必须从 secret store 取回并只在发送 upstream request 时短暂使用 key。

必须补测：

- `admin_create_provider_response_redacts_api_key`
- `admin_update_provider_response_redacts_api_key`
- `managed_provider_key_is_encrypted_at_rest`
- `managed_provider_key_decrypts_for_upstream_call`
- `managed_provider_key_rotation_keeps_routes_working`

### P0-05：SSE streaming 实际被完整缓冲

证据：

- `modelwire-server/src/routes/responses.rs` 等待 `relay_streaming_response_scoped` 返回后，把 `result.sse_frames` 拼成一个 `Vec<u8>` 再返回。
- `modelwire-server/src/relay.rs` 的 streaming relay 从 upstream stream 读取所有 chunk，收集进 `SseWriter`，结束后返回 `StreamingRelayResult { sse_frames }`。
- 内部的 `committed` 变量基于“解析到上游语义事件”，不是“已经向下游写出第一个 SSE event”。

影响：

- Codex 无法收到增量 token。
- 大响应会在服务端内存中累积，首字节延迟到 upstream 完成。
- 下游断开无法在真实 HTTP runtime 路径中及时取消 upstream。
- “首个下游 SSE event 后禁止 fallback”的规则当前只是内部模拟，不是实际 socket commit 语义。

整改要求：

- route 层返回真正的 `Body::from_stream`/SSE stream。
- 只在 pre-commit fallback 判定需要的最小窗口内缓冲。
- 第一条下游 SSE 写出后，后续 upstream 失败只能发 downstream failure event，不能 fallback。
- 下游断开必须取消 upstream request。

必须补测：

- `streaming_first_sse_arrives_before_upstream_completion`
- `streaming_downstream_disconnect_aborts_upstream`
- `streaming_pre_commit_failure_falls_back_before_any_downstream_bytes`
- `streaming_post_commit_failure_does_not_call_fallback_target`
- `streaming_large_response_does_not_buffer_entire_body`

### P0-06：lazy protocol probing 对 Chat/Anthropic 使用错误请求形状

证据：

- `modelwire-server/src/relay.rs` 的 `probe_wire_api` 构造统一 body：`model`、`input`、`max_output_tokens`、`stream`。
- `probe_candidate_once` 把这个同一个 Responses 形状 body 发往 `/responses`、`/messages`、`/chat/completions`。
- Anthropic Messages 需要 `messages` 和 `max_tokens`。
- OpenAI Chat Completions 需要 `messages`，不是 `input`。

影响：

- `wire_api = "auto"` 对真实 Chat/Anthropic provider 很可能误判为 protocol unsupported。
- 按 provider + credential hash + upstream model 的 lazy detection 要求无法可靠成立。

整改要求：

- probe body 必须通过各 adapter 的 request builder 或协议专用构造器生成。
- 每个 protocol candidate 都要用 mock upstream 捕获并断言实际请求形状。

必须补测：

- `probe_responses_sends_responses_shape`
- `probe_openai_chat_sends_chat_completions_shape`
- `probe_anthropic_sends_messages_shape`
- `probe_404_responses_then_valid_chat_detects_chat`

### P0-07：auto-probed target 的工具支持不可用

证据：

- text probe 成功后，`ProbeResult.supports_tools = false` 且 `tool_support_known = false`。
- tool-bearing request 到来时，`should_skip_target_for_tool_support` 会跳过 `tool_support_known = false` 的 target。
- 没有发现“请求带 tools 时触发轻量工具 probe”的 runtime 路径。

影响：

- `wire_api = "auto"` 的 target 在 text probe 成功后，仍不能可靠处理工具调用请求。
- 这不符合“Function/tool calling is required. Do not silently strip tools.” 的验收精神。

整改要求：

- 当真实请求包含 tools 且 cached probe 的 tool support unknown 时，执行协议专用 tool probe。
- 将基础 protocol support 和 tool support 分开缓存。
- tool probe 失败时按 fallback/error 规则处理，但不得剥离 tools 后继续。

必须补测：

- `tool_request_runs_second_tool_probe_for_auto_target`
- `tool_probe_success_allows_tool_request`
- `tool_probe_failure_falls_back_without_stripping_tools`
- `tool_probe_auth_error_stops_without_fallback`

### P0-08：Debug 日志会泄露 prompt 和 tool output

证据：

- `modelwire-adapters/src/responses.rs`
- `modelwire-adapters/src/openai_chat.rs`
- `modelwire-adapters/src/anthropic.rs`

这些 adapter 都会在 debug 级别记录完整 upstream request body。body 中包含 prompt、instructions、tool schema、tool output 等敏感内容。

影响：

- 默认配置要求 API keys、prompts、tool outputs 在日志中 redacted。
- 当前只要开启 debug 级别日志，就可能泄露完整用户内容或工具输出。

整改要求：

- 默认移除完整 request body debug logging。
- 如果确实需要调试，必须走显式 unsafe debug flag，并且默认 redacted。
- adapter 层要有 tracing capture 测试，覆盖 prompt/tool output 不进日志。

必须补测：

- `adapter_debug_logs_do_not_include_prompt_by_default`
- `adapter_debug_logs_do_not_include_tool_output_by_default`
- `unsafe_prompt_logging_requires_explicit_flag`

### P0-09：Postgres migration 不完整

证据：

- `modelwire-db/src/lib.rs` 的 Postgres migration path 只创建 `providers` 与 `compaction_lineage` 等少量表，并留有 `// ... more Postgres tables` 注释。
- repository/runtime 路径会访问 `routes`、`route_targets`、`responses`、`response_items`、`upstream_handles`、`request_logs`、`probe_results` 等表。
- `modelwire-db/src/schema.rs` 有更完整 schema，但 `run_migrations` 没有执行完整 Postgres schema。

影响：

- 配置为 Postgres 后，Admin 或 data-plane 路径触发缺表错误。
- “SQLite or Postgres database-backed operational state” 的要求不能成立。

整改要求：

- 使用真实、版本化、可重复执行的 migration。
- SQLite/Postgres 都要覆盖完整 operational schema。
- 加入 Postgres migration 与核心 repo roundtrip 测试。

必须补测：

- `postgres_migrations_create_all_operational_tables`
- `postgres_response_state_roundtrip`
- `postgres_admin_config_crud_roundtrip`
- `postgres_probe_cache_roundtrip`

### P0-10：CLI config export 默认泄露 secrets

证据：

- `modelwire-server/src/main.rs` 的 `ExportConfig` 直接 `serde_json::to_string_pretty(&state.config)`。
- `state.config` 可包含 provider `api_key`、`admin_password`、`log_secret`、`trusted_passthrough_value`、relay key hash 等敏感字段。

影响：

- `modelwire export-config` 可能把 secrets 打到终端、日志或 CI artifacts。
- 违反“config export redacts secrets by default”的安全要求。

整改要求：

- CLI export 必须复用 hardened redaction path。
- 如果需要明文 backup，必须使用显式 unsafe flag，并有审计提示。

必须补测：

- `cli_export_config_redacts_provider_api_key`
- `cli_export_config_redacts_admin_password`
- `cli_export_config_redacts_log_secret`
- `cli_export_with_secrets_requires_explicit_flag`

## P1 重大问题

### P1-01：工具调用 call ID 映射缺失，会破坏同上游 continuation

证据：

- `normalize_output_item` 处理 `CanonicalOutputItem::FunctionCall` 时丢弃 canonical/upstream `call_id`，重新生成 downstream `call_id`。
- 持久化 `response_items` 保存的是重新生成的 downstream call id。
- core 中存在 `ToolCallIdMap` 类型，但未看到实际 DB 表和 runtime 映射路径。

影响：

- 对 native Responses 同上游 `previous_response_id` continuation 来说，下游回传的 tool result 使用 ModelWire 生成的 call id；如果上游期望原始 call id，第二轮可能失败。
- 跨上游 replay 时也缺少“下游 call id 到上游 tool_call/tool_use id”的可审计映射。

整改要求：

- 为每个工具调用持久化 downstream call id、canonical call id、upstream tool id/call id 的映射。
- 同上游 continuation 使用已知 upstream handle 时，tool result 必须转换回兼容上游的 id。
- 跨上游 replay 必须重新 materialize assistant tool call 与 tool result，并生成目标协议合法 id。

必须补测：

- `same_upstream_tool_result_uses_mapped_upstream_call_id`
- `cross_upstream_replay_remaps_tool_call_ids`
- `unknown_tool_result_id_returns_codex_style_error`

### P1-02：tool-loop materialized replay 语义错误

证据：

- canonical input model 缺少“历史 assistant function_call/tool_use”输入项。
- `to_replay_input_items` 将持久化的 `function_call` output item 转成 `FunctionCallOutput`，且把原始 arguments 当作 output。
- Chat/Anthropic adapter replay 需要 assistant tool call 再接 tool result，而不是只有 tool-result-shaped item。

影响：

- 跨上游 fallback/replay 的工具循环历史可能无效。
- Codex-style tool loop 可能只在 same-upstream stateful continuation 下看起来可用。

整改要求：

- Canonical transcript 要区分 assistant tool call 和 tool result。
- 按顺序持久化并 replay message、assistant function_call、tool output。
- 每个 adapter 都必须把 replay history 转成协议合法格式。

必须补测：

- `cross_upstream_tool_loop_replay_includes_assistant_call_then_tool_result`
- `chat_replay_tool_history_has_assistant_tool_calls_before_tool_messages`
- `anthropic_replay_tool_history_has_tool_use_before_tool_result`

### P1-03：跨 provider/target 的 upstream handle 复用规则过窄

证据：

- `can_send_upstream_previous_response_id` 要求 `target.provider_id == previous_provider_id`。
- 计划允许在两个 provider 共享相同配置 `state_scope` 时尝试 cross-provider upstream response ID reuse；失败且未 commit 时再 replay。

影响：

- 同 `state_scope` 的兼容 Responses targets 无法复用已有 upstream handle。
- 当前实现比计划要求少一个优化/兼容路径。

整改要求：

- 将复用条件调整为“协议兼容 + state_scope 兼容 + credential/model/handle 约束满足”。
- 对 cross-provider same-state-scope 复用失败 before commit 的 replay fallback 加 slice test。

必须补测：

- `cross_provider_same_state_scope_attempts_handle_reuse`
- `cross_provider_handle_reuse_failure_replays_before_commit`
- `cross_state_scope_never_reuses_handle`

### P1-04：`pass_authorization` 可能把 ModelWire relay key 当 upstream key 转发

证据：

- `resolve_upstream_key` 在 provider auth mode 为 `pass_authorization` 时，会从 downstream `Authorization` 中 strip bearer 并转发。
- 如果 downstream auth 使用 `relay_key`，这个 bearer 是 ModelWire relay key，不是 upstream provider key。

影响：

- relay key 可能被发往第三方 upstream provider。
- 这既是 secret exposure，也是认证语义混乱。

整改要求：

- 禁止 `downstream_auth = relay_key` 与 provider `pass_authorization` 组合，除非有显式、安全、测试覆盖的 key mapping。
- passthrough upstream auth 必须只在 passthrough/trusted_passthrough downstream auth 模式下允许。

必须补测：

- `relay_key_is_never_forwarded_as_upstream_authorization`
- `pass_authorization_requires_passthrough_downstream_auth`

### P1-05：上下文窗口 guard 不够保守

证据：

- `estimate_request_tokens` 使用近似 `(chars + 3) / 4`。
- 非英文文本、JSON-heavy tool schema、模型 tokenizer overhead 都可能被低估。
- 如果 target 没有 `context_window_tokens`，guard 基本不执行。

影响：

- ModelWire 可能允许会让真实 upstream overflow 的请求。
- 与“不要把 mapped model 表现得比真实 upstream target 上下文更大”的要求不完全一致。

整改要求：

- public-ready route target 必须有明确 context metadata。
- 估算器应按目标模型更保守，或使用可配置 tokenizer。
- `max_output_tokens` 必须受 target `max_output_tokens` 约束。
- 未知 context metadata 在 public mode 下应拒绝或要求显式 unsafe。

必须补测：

- `context_guard_cjk_text_is_not_underestimated`
- `route_without_context_metadata_rejected_in_public_mode`
- `requested_max_output_above_target_limit_rejected_or_explicitly_clamped`

### P1-06：Archive layout 与计划不一致，writer cache 也可能混用 capture mode

证据：

- `modelwire-archive/src/writer.rs` 创建 `root/arch_<uuid>/...`，不是计划中的 `archives/<yyyy-mm>/<archive-id>/...`。
- 当前写 `conversations-000001.jsonl.zst`，未看到单独 `items-000001.jsonl.zst`。
- `ServerState.archive_writer` 是单个 `Option<ArchiveWriter>`；第一次 non-off capture mode 初始化后，后续不同 relay key 的 capture-mode override 可能复用同一个 writer。
- 计划提到 archive 文件可由 SQL index 重建，但 runtime archive 写入没有明显持久化 `archive_files` rows。

影响：

- 归档文件与计划的 parseable/rebuildable shape 不一致。
- capture mode metadata 可能与实际写入内容混杂。
- 后续 UI/导出/index 重建缺少稳定 SQL 索引。

整改要求：

- 按月份分区实现计划中的 archive root。
- 要么写独立 items segment，要么修改计划并补足 schema 说明。
- writer cache 需要按 root + capture mode + period 分 key。
- segment seal 时持久化 archive file metadata。

必须补测：

- `archive_path_uses_year_month_partition`
- `archive_writes_items_segment_or_documented_equivalent`
- `archive_capture_mode_override_does_not_mix_manifest_modes`
- `archive_segment_persists_archive_files_row`

### P1-07：多项 security tests 不是 runtime 验收测试

证据：

`modelwire-server/tests/security_tests.rs` 中存在多项测试只构造本地 JSON 或本地 helper，不经过真实 router/middleware/storage/UI 路径，例如：

- `admin_cookie_has_secure_attributes`
- `admin_login_rate_limited`
- `admin_logout_invalidates_session`
- `provider_error_escapes_html`
- `config_change_writes_audit_log`
- `hop_by_hop_headers_not_forwarded_upstream`
- `admin_cookie_not_forwarded_upstream`
- `archive_path_traversal_rejected`
- `archive_symlink_delete_does_not_escape_root`

影响：

- 测试名称给人“已验证产品行为”的错觉。
- 这些测试不能支撑 public-ready security claim。

整改要求：

- 将设计型测试改名为 spec/design tests，不能计入验收。
- 对应增加真实 runtime tests：走 router、middleware、mock upstream、DB、archive writer、WebUI/browser。

必须补测：

- 上述每一项都需要一个真实路径测试，至少覆盖请求、响应、持久化/日志/归档副作用。

### P1-08：Admin config change audit logging 没有真实实现证据

证据：

- `config_change_writes_audit_log` 只断言本地 JSON。
- Admin CRUD handler 未看到持久化 actor、request_id、resource、redacted diff 的专用 audit event。

影响：

- public admin mutation 不可审计。
- Secret redaction in admin change history 没有证明。

整改要求：

- 增加 admin audit table 或 typed request log event。
- 每个 state-changing Admin API 记录 actor/session/key hash、request id、资源、动作、redacted diff。

必须补测：

- `admin_provider_create_writes_redacted_audit_event`
- `admin_route_update_writes_redacted_audit_event`
- `admin_import_config_writes_redacted_audit_event`

### P1-09：IP rate limiting 信任可伪造 forwarding headers

证据：

- `modelwire-server/src/middleware/auth.rs` 的 `extract_client_identity` 无条件信任 `x-forwarded-for` 和 `x-real-ip`。

影响：

- 公开客户端可以伪造不同 IP 绕过 IP rate limit，除非外部代理正确清洗 header。

整改要求：

- 只有来自 configured trusted proxies 的请求才可使用 forwarding headers。
- 默认使用 peer socket address。

必须补测：

- `untrusted_x_forwarded_for_does_not_bypass_ip_limit`
- `trusted_proxy_x_forwarded_for_sets_client_identity`

### P1-10：Provider URL SSRF 防护缺 DNS resolved IP 校验

证据：

- 当前 validation 主要拒绝 literal localhost/private/metadata host 或不安全 scheme。
- 未看到对 hostname 解析结果的 private/metadata IP 校验。

影响：

- 恶意 DNS 或 DNS rebinding 可让看似公网的 hostname 解析到内网/metadata IP。

整改要求：

- upstream connect 前解析 hostname 并拒绝 private/link-local/metadata/loopback resolved IP。
- 连接时尽可能校验实际 remote address，防止重绑定。

必须补测：

- `provider_hostname_resolving_to_private_ip_rejected`
- `provider_hostname_dns_rebind_to_private_ip_rejected`

### P1-11：Responses 兼容接口面不完整

证据：

- 后端主要实现 `POST /v1`、`POST /v1/responses`、`GET /v1/models`、`POST /v1/responses/compact`。
- 计划中提到 Responses 兼容面还包括 `GET /v1/responses/{response_id}`、`GET /v1/responses/{response_id}/input_items` 等。

影响：

- 使用 Responses retrieval/input-items 的客户端会失败。
- 若 Codex 某些流程使用这些接口，兼容性存在缺口。

整改要求：

- 实现文档列出的 endpoint，或明确收窄 v1 兼容范围并增加 negative tests。

必须补测：

- `get_response_returns_modelwire_owned_response`
- `get_response_input_items_returns_canonical_replayable_items`
- `unsupported_responses_endpoint_returns_codex_style_error`

### P1-12：`docs/modelwire-implementation-plan.md` 自身状态不可信

证据：

- 文档多处标注 milestone/任务完成，但文件末尾仍保留与已完成声明相冲突的 “Next small-model implementation tasks”。
- 部分“已完成”项缺少能证明真实 runtime 行为的测试，例如 Admin auth/session、Postgres、真实 streaming、Admin DB 驱动数据面。

影响：

- 文档作为 source of truth 时会误导后续实现者。
- 小模型按文档接力时可能跳过实际未完成工作。

整改要求：

- 在修复代码前，先把计划文档中的状态改为“verified / implemented but unverified / not implemented / design-test only”。
- 每个完成项必须链接到真实测试名和验收命令结果。

必须补测：

- 不是单个测试能解决的问题；需要文档验收矩阵，把每个 hard requirement 映射到代码路径和 runtime test。

## P2 质量缺口

### P2-01：格式化门禁失败

证据：

- `cargo fmt --check` 失败，涉及：
  - `modelwire-server/tests/integration_slices.rs`
  - `modelwire-server/tests/security_tests.rs`
- rustfmt 警告稳定版不支持：
  - `use_try`
  - `warn_on_unreachable_pub`
  - nightly-only import grouping options

整改要求：

- 运行并提交 `cargo fmt` 结果。
- 移除或按工具链条件化 unsupported rustfmt 配置。

### P2-02：WebUI lint 门禁失败

证据：

- `npm run lint` 报：
  - `AuthContext.tsx` 中 `checkAuth` 使用早于声明。
  - `AuthContext.tsx` 同时导出 component 与 hook，违反 fast-refresh rule。

整改要求：

- 用 `useCallback` 或调整声明顺序修复 hooks lint。
- 将 `useAuth`/context helper 移到独立模块，或调整 lint 合法结构。

### P2-03：供应链审计命令当前不可执行

证据：

- `cargo deny check` 因未安装 `cargo-deny` 无法执行。
- `npm audit` 因当前 registry mirror 不支持 audit endpoint 无法完成。

整改要求：

- 在 CI 和开发文档中固定安装 `cargo-deny`。
- npm audit 使用支持 advisory endpoint 的 registry，或配置替代审计工具。

### P2-04：成功请求日志缺少 latency

证据：

- non-streaming 成功路径有 `request_start`，但写 `request_logs` 时 latency 可为空。

影响：

- Admin logs/metrics 无法可靠统计成功请求延迟。

整改要求：

- 成功、失败、fallback、streaming 都持久化 latency。
- 增加 request log runtime test。

### P2-05：Admin metrics 太浅，不足以运营 public relay

证据：

- `/admin/api/metrics` 当前主要返回 route count、provider count、probe cache size 等浅层指标。

影响：

- public operation 缺少请求量、错误率、fallback、rate limit、stream failure、upstream status class、latency bucket 等关键观测。

整改要求：

- 增加 redacted operational metrics。
- 指标中不得包含 prompt、tool output、Authorization、provider key。

### P2-06：构建产物未忽略

证据：

- `npm run build` 后生成 `modelwire-webui/dist` 未被 `.gitignore` 忽略，工作区出现 untracked build artifacts。

整改要求：

- 若 `dist` 不应提交，加入 `.gitignore`。
- 若 `dist` 应提交，需明确文档化并纳入构建发布流程。

## 建议整改顺序

不要继续优先打磨非关键 WebUI 页面。建议按下面顺序收敛：

1. 先恢复基础门禁：`cargo fmt --check`、WebUI lint、供应链审计命令。
2. 锁死 public startup 和 downstream auth：拒绝 unsafe auth 组合，拒绝未实现的 `managed`。
3. 实现真实 Admin auth/session/CSRF，或让 WebUI 明确使用 bearer admin auth。
4. 统一 Admin DB 与数据面的 runtime config source of truth。
5. 实现 managed upstream key 的加密存储、解密使用和全响应 redaction。
6. 重写 streaming，使下游真实逐 event 接收，并支持 disconnect cancellation。
7. 重写 protocol probing，通过协议专用请求形状探测。
8. 加 tool-support probing，禁止 tool-bearing request 静默降级。
9. 移除或 redacted adapter request-body debug logs。
10. 补全 Postgres migrations 和 Postgres roundtrip tests。
11. 修复工具调用 call ID 映射和 tool-loop replay。
12. 加强 context guard 和 route context metadata 要求。
13. 对齐 archive layout、capture mode writer cache 和 archive file indexing。
14. 将 design-only security tests 替换为真实 runtime/security tests。
15. 整理 `docs/modelwire-implementation-plan.md` 的状态矩阵，未验证不得标完成。

## 修复后的最低验收命令

完成整改后，至少需要本地和 CI 都通过：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run --prefix modelwire-webui lint
npm run --prefix modelwire-webui build
cargo deny check
npm --prefix modelwire-webui audit --audit-level=high
```

public-ready 还必须增加 deployment-profile 测试：用 public bind 配置启动服务，验证 unsafe auth、admin、provider URL、archive、logging、managed key 配置全部 fail closed。

## 最终审查意见

ModelWire 的核心方向是对的，也已经沉淀了不少有价值的 slice tests。但当前仓库最大的问题不是“还差几个小 bug”，而是若干关键系统边界没有真正闭合：

- 数据面读内存 TOML 配置，Admin/WebUI 写 SQL 配置；
- 安全测试中有一部分并未经过真实 runtime；
- streaming、protocol probing、managed secrets、tool ID mapping、Postgres 等 public-ready 必需能力仍存在结构性缺口。

按 `AGENTS.md` 的规则：**No test, no done. No security test, no public deployment feature is accepted.** 当前项目不能标记为完成。建议把本报告中的 P0 全部作为“完成前阻断项”，P1 作为“计划要求补齐项”，P2 作为“交付门禁项”。
