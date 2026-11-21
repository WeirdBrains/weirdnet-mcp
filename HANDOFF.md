# Handoff

Running notes for whoever picks this repo up next, newest first. Rules for this file are in the
collective [AGENTS.md](https://github.com/WeirdBrains/weirdnet-agents/blob/main/AGENTS.md).

## 2026-11-21: MCP server gaps for Ymir and Bragi (branch `mcp-gaps`, unreleased)

**What changed** (commit `4258a5d`, on branch [`mcp-gaps`](https://github.com/WeirdBrains/weirdnet-mcp/tree/mcp-gaps), not on main and not released):

- Async tools: `Tools::call_async` (returns `CallToolResult`), `Server::handle_async` and
  `handle_raw_async`. `Tools::call` now has a default, so sync apps compile unchanged and
  `Server::handle` still serves them (it polls `call_async` once).
- Image, audio, resource-link and embedded-resource content (`CallToolResult`, `Content`), with
  `data:` URL and audio-format helpers that match [weirdnet-agent](https://crates.io/crates/weirdnet-agent) 0.5 content parts.
- `ParamType::{Any, Enum, Array, Object, AnyOf, Json}` (now `#[non_exhaustive]`) and `ToolSpec::check`.
- Resources: optional `Tools::{has_resources, list_resources, resource_templates, read_resource}`.
- [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) alongside the
  `initialize` era: per-request `_meta`, `server/discover`, `resultType`, server identity,
  `ttlMs` / `cacheScope` (`Tools::cache_hint`), `-32022` / `-32602` / `-32020` errors.
  `initialize` also negotiates 2025-11-25.
- `stdio::serve_blocking`; `stdio::serve` / `serve_with` (feature `stdio-server`): concurrent
  requests, `notifications/cancelled`, 32 MiB message cap.
- `http` (feature `http-server`): `Guard` (Host, Origin, POST only, bearer `Auth`), `respond`,
  `respond_checked`, modern header validation; `http::route` / `router` for axum 0.8 (feature `axum`).

**Checks run:** `cargo fmt --check`; `cargo clippy --all-targets -D warnings` with default
features, `--all-features`, and each of `stdio-server`, `http-server`, `axum`, `client`, `agent`
alone; `cargo test` default, `--all-features`, `--no-default-features`, `stdio-server` only,
`axum` only (unit tests, a golden test that the legacy output matches 0.5.3 byte for byte,
stdio integration tests over in-memory pipes, HTTP integration tests over real loopback sockets
covering token, Host, Origin, method, body size, legacy and modern header rejections, and the
crate's own client against both servers); `cargo doc` with `-D warnings`; `cargo package`;
`cargo semver-checks` against 0.5.3 (only `ParamType` is breaking).

**Not tested:** real MCP hosts (Claude, Cursor, Inspector) against these servers; the axum route
under HTTP/2 (`Host` falls back to the URI authority); SSE responses (the server always replies
with JSON); Windows.

**What's left:** resource subscriptions (`subscriptions/listen`) and list-changed notifications
are not served (the server is stateless); prompts; `outputSchema` and tool `title`/`icons`;
`x-mcp-header`; the client still speaks only the `initialize` era.

**What's next:** Michael picks the version (proposed 0.6.0, because `ParamType` gains variants),
then merge to main and release with `scripts/weirdnet_release.py`; Ymir and Bragi drop their
local `rpc.rs` workaround.
