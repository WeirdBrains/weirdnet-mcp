# Changelog

## [Unreleased]

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
