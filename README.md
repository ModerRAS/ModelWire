# ModelWire

A Codex-first Responses API relay with lazy per-model protocol probing, model
mapping, multi-upstream routing, and fallback.

ModelWire presents the OpenAI Responses-compatible downstream API Codex expects,
then routes each model to native Responses, Anthropic Messages, or OpenAI Chat
Completions upstreams. Other Responses-compatible clients may work, but Codex
compatibility is the v1 priority.

Start here:

- [Implementation plan](docs/modelwire-implementation-plan.md)
- [Public deployment guide](docs/public-deployment-guide.md)
- [Agent instructions](AGENTS.md)

Local verification:

```bash
cargo fmt --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
npm --prefix modelwire-webui ci
npm --prefix modelwire-webui run lint
npm --prefix modelwire-webui run build
npm --prefix modelwire-webui audit --audit-level=high --registry=https://registry.npmjs.org
```
