# Handoff

Running notes for whoever picks this repo up next, newest first. Rules for this file are in the
collective [AGENTS.md](https://github.com/WeirdBrains/weirdnet-agents/blob/main/AGENTS.md).

## 2026-11-26: parsing image and audio tool results (main, unreleased)

**What changed** (this commit; parent `6caf470`): the client side of image and audio content
blocks in tool results, for video frame tools (`video_frames` returns frames as images).
Servers already sent `{"type": "image" | "audio", "data": <base64>, "mimeType": ...}`;
clients can now read them back. `CallToolResult::from_json`, `Content::from_json`,
`Resource::from_json` and `ResourceContents::from_json` parse the wire JSON (optional
`annotations` and `_meta` are ignored; unknown or malformed blocks are skipped).
`Content::decoded_bytes`, `mime_type`, `is_image`, `is_audio`, `as_text`,
`CallToolResult::text` and `ResourceContents::decoded_bytes` read it.
`Client::call_tool_content` / `call_tool_content_until` return the full `CallToolResult`;
`call_tool` and `McpSource` are unchanged. Server output is unchanged.

**Checks run:** `cargo fmt --check`, `cargo clippy --all-targets -D warnings` (default and all
features), `cargo test` (default, all features, no default features), `cargo doc -D warnings`
(default and all features), `cargo package --locked`, and
[cargo-semver-checks](https://crates.io/crates/cargo-semver-checks) against 0.6.1 (no semver
update required, so the next release is 0.6.2).

**Not tested:** real MCP hosts sending images or audio to the client; large (multi-MB) media
blocks over stdio or HTTP.

**What's left:** release 0.6.2 once CI is green.

**What's next:** `ymirctl mcp` and `bragi mcp` return `video_frames` as image blocks.

## 2026-11-22: release 0.6.1

**What changed:** `a777432` (CI green) released as 0.6.1 with `scripts/weirdnet_release.py` (commit
`release: v0.6.1`, annotated tag [v0.6.1](https://github.com/WeirdBrains/weirdnet-mcp/releases/tag/v0.6.1)),
published to [crates.io](https://crates.io/crates/weirdnet-mcp/0.6.1); docs at
[docs.rs](https://docs.rs/weirdnet-mcp/0.6.1). Additive (prompts), so a patch release; see [CHANGELOG.md](CHANGELOG.md).

**Checks run:** the release script's checks from `release.toml`, then the crates.io upload.

**Not tested:** as in the entry below.

**What's left:** nothing for this release.

**What's next:** Bragi's MCP server serves its prompts with 0.6.1.

## 2026-11-22: prompts (main, unreleased)

**What changed** (`db17faa`): MCP [prompts](https://modelcontextprotocol.io/specification/2026-07-28/server/prompts).
`Tools::prompts` (a `const` `PromptSpec` / `PromptArg` table, default empty) and
`Tools::get_prompt` (async, default refuses) serve `prompts/list` and `prompts/get` in both the
`initialize` era and 2026-07-28. The `prompts` capability is advertised only when the table is
not empty. The server answers a missing or unknown name and missing or non-string arguments
with `-32602` before the app runs; `PromptError` covers hidden prompts (`-32602`), bad argument
values (`-32602`) and failures (`-32603`). Modern `prompts/list` results carry caching hints.
`Content` converts from `&str` and `String`. Servers without prompts answer exactly as in 0.6.0.

**Checks run:** `cargo fmt --check`, `cargo clippy --all-targets -D warnings` (default and all
features), `cargo test` (default, all features, no default features), `cargo doc -D warnings`,
`cargo package --locked`. [cargo-semver-checks](https://crates.io/crates/cargo-semver-checks)
against 0.6.0: no semver update required, so the next release can be 0.6.1.

**Not tested:** real MCP hosts listing or running prompts; `prompts/get` over the HTTP route
(the `Mcp-Name` header check for it predates this change and is covered by its own tests).

**What's left:** release 0.6.1 once m3talsmith approves the version; update the mcp guide on
[net.weirdbrains.com](https://net.weirdbrains.com) with the release.

**What's next:** Bragi's MCP server lists its prompts through `Tools::prompts`.

## 2026-11-21: release 0.6.0

**What changed:** the `mcp-gaps` work (`4258a5d`, `7a5e2de`, `6428ed4`) was fast-forwarded onto
main (CI green on `6428ed4`) and released as 0.6.0 with `scripts/weirdnet_release.py` (commit
`release: v0.6.0`, annotated tag `v0.6.0`, published to [crates.io](https://crates.io/crates/weirdnet-mcp/0.6.0);
docs at [docs.rs](https://docs.rs/weirdnet-mcp/0.6.0)). 0.6.0 is breaking only because `ParamType`
gained variants and is `#[non_exhaustive]`; see [CHANGELOG.md](CHANGELOG.md).

**Checks run:** the release script's checks from `release.toml` (fmt, clippy `--all-features
-D warnings`, `cargo test --all-features`, `cargo doc -D warnings`), then the crates.io upload.

**Not tested:** as in the entry below (real MCP hosts, HTTP/2, Windows).

**What's left:** as in the entry below.

**What's next:** Ymir and Bragi move to 0.6 and drop their local MCP front end.

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

**What's next:** m3talsmith picks the version (proposed 0.6.0, because `ParamType` gains variants),
then merge to main and release with `scripts/weirdnet_release.py`; Ymir and Bragi drop their
local `rpc.rs` workaround.
