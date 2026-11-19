# Changelog

## [Unreleased]

## [0.2.0] - 2026-11-18

### Added

- `client` cargo feature (off by default): an MCP client with stdio (local command) and streamable HTTP transports, `initialize`, `tools/list` (with paging), `tools/call`, per-request timeouts and cancellation (`notifications/cancelled`). Secrets are redacted `Secret`s; local commands have a `warning()`.
- `agent` cargo feature (implies `client`): `agent::McpSource` implements `weirdnet_agent::ToolSource` (weirdnet-agent `=0.1.0`), mapping tool annotations to tiers with a configurable floor.
- The server API and behaviour are unchanged.

## [0.1.0] - 2026-11-12

### Added

- Stateless MCP tools server, tool specs with annotations, argument helpers and a stdio bridge (from TAUR).
