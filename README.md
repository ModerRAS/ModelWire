# ModelWire

A Codex-first Responses API relay with lazy per-model protocol probing, model
mapping, multi-upstream routing, and fallback.

ModelWire presents the OpenAI Responses-compatible downstream API Codex expects,
then routes each model to native Responses, Anthropic Messages, or OpenAI Chat
Completions upstreams. Other Responses-compatible clients may work, but Codex
compatibility is the v1 priority.

Start here:

- [Implementation plan](docs/modelwire-implementation-plan.md)
- [Agent instructions](AGENTS.md)
