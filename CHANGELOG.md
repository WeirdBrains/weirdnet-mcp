# Changelog

## [Unreleased]

## [0.6.2] - 2026-11-26

### Added

- Parsing tool results: `CallToolResult::from_json` and `Content::from_json` (text, image, audio, resource links and embedded resources), plus `Resource::from_json` and `ResourceContents::from_json`.
- Content helpers: `Content::decoded_bytes`, `mime_type`, `is_image`, `is_audio`, `as_text`, `CallToolResult::text` and `ResourceContents::decoded_bytes`.
- `client`: `Client::call_tool_content` and `call_tool_content_until` keep image and audio blocks in tool results.

## [0.6.1] - 2026-11-22

### Added

- Prompts: `prompts/list` and `prompts/get` through optional `Tools::prompts` and `Tools::get_prompt`, with `PromptSpec`, `PromptArg`, `GetPromptResult`, `PromptMessage`, `Role` and `PromptError`. The `prompts` capability is advertised only when a server has prompts.
- `Content` converts from `&str` and `String` (text).

## [0.6.0] - 2026-11-21

### Added

- Async tools: `Tools::call_async` returns a `CallToolResult`; `Server::handle_async` and `handle_raw_async` serve it. Sync `Tools::call` keeps working and now has a default.
- Image, audio, resource-link and embedded-resource tool results (`CallToolResult`, `Content`), with `data:` URL and audio-format helpers matching weirdnet-agent 0.5 content parts.
- Resources: `resources/list`, `resources/templates/list` and `resources/read` through optional `Tools` methods.
- Parameter types `Any`, `Enum`, `Array`, `Object`, `AnyOf` and `Json`; `ToolSpec::check` for tool tables.
- MCP 2026-07-28 requests (per-request `_meta`), `server/discover`, `resultType`, server identity and caching hints (`Tools::cache_hint`), alongside the `initialize`-based versions.
- `stdio::serve_blocking`, and `stdio::serve` / `serve_with` (feature `stdio-server`) with concurrent requests and cancellation.
- Streamable HTTP endpoint (feature `http-server`) with bearer auth and `Host` / `Origin` checks, and an axum 0.8 route (feature `axum`).

### Changed

- **Breaking:** `ParamType` has new variants and is `#[non_exhaustive]`.
- `initialize` also negotiates 2025-11-25, now the newest legacy version (and the one the client asks for). Other legacy output is unchanged.

## [0.5.3] - 2026-11-20

### Changed

- The `agent` feature takes `weirdnet-agent` 0.4 or 0.5 (`">=0.4, <0.6"`); no API changes.

## [0.5.2] - 2026-11-20

### Changed

- Metadata and docs scrub; no API changes.

## [0.5.1] - 2026-11-19

### Changed

- The `agent` feature depends on `weirdnet-agent` `"0.4"` (any 0.4.x, 0.4.1 and later included) instead of exactly `=0.4.0`, so agent patch releases no longer need a weirdnet-mcp release.

## [0.5.0] - 2026-11-19

### Changed

- **Breaking for `agent` users:** the `agent` feature now depends on weirdnet-agent `=0.4.0` (was `=0.3.0`), so `McpSource` implements the 0.4.0 `ToolSource`. No source changes were needed. The server API and the `client` feature are unchanged.

## [0.4.0] - 2026-11-19

### Changed

- **Breaking for `agent` users:** the `agent` feature now depends on weirdnet-agent `=0.3.0` (was `=0.2.0`), so `McpSource` implements the 0.3.0 `ToolSource`. No source changes were needed. The server API and the `client` feature are unchanged.

## [0.3.0] - 2026-11-18

### Changed

- **Breaking for `agent` users:** the `agent` feature now depends on weirdnet-agent `=0.2.0` (was `=0.1.0`), so `McpSource` implements the 0.2.0 `ToolSource` and apps on weirdnet-agent 0.2.0 get a single copy of the trait. No source changes were needed here. The server API and the `client` feature are unchanged.
- `client`: the stdio transport reads server lines with a cap (`DEFAULT_MAX_LINE`, 4 MiB; `StdioTransport::spawn_limited` and `from_io_limited` set it). A longer line is discarded without being buffered, so a misbehaving server cannot exhaust memory; a request waiting for it times out. Not breaking: `spawn` and `from_io` keep their signatures.

## [0.2.0] - 2026-11-18

### Added

- `client` cargo feature (off by default): an MCP client with stdio (local command) and streamable HTTP transports, `initialize`, `tools/list` (with paging), `tools/call`, per-request timeouts and cancellation (`notifications/cancelled`). Secrets are redacted `Secret`s; local commands have a `warning()`.
- `agent` cargo feature (implies `client`): `agent::McpSource` implements `weirdnet_agent::ToolSource` (weirdnet-agent `=0.1.0`), mapping tool annotations to tiers with a configurable floor.
- The server API and behaviour are unchanged.

## [0.1.0] - 2026-11-12

### Added

- Stateless MCP tools server, tool specs with annotations, argument helpers and a stdio bridge (from TAUR).
