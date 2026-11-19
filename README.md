# weirdnet-mcp

A transport-agnostic Model Context Protocol tools server, extracted from TAUR and shared with
Salty Gull.

- `Server::handle` takes one decoded JSON-RPC 2.0 message (or batch) plus the caller's bearer
  token and returns the reply (or nothing for notifications): `initialize` with version
  negotiation (2025-06-18, 2025-03-26, 2024-11-05), `ping`, `tools/list`, `tools/call`.
- `ToolSpec` / `Param`: static tool tables with input schemas and `readOnlyHint`,
  `destructiveHint`, `openWorldHint`; tools that need auth say "Requires a bearer token.".
- The app implements `Tools::call`; failures become `isError` tool results.
- `args`: argument getters with uniform messages.
- `stdio::bridge`: the `<app>ctl mcp` loop that forwards stdio lines to a remote `/mcp`.

The HTTP route (stateless streamable HTTP, JSON responses) stays in the app's server.

## Client (cargo features `client` and `agent`, off by default)

The server API above is unchanged and needs neither feature.

- `client`: `Client::connect(transport, ClientOptions)` runs `initialize`, then `list_tools`
  (follows cursors), `call_tool` and `call_tool_until(name, args, cancel)`. Every request has a
  timeout; a timeout or cancel sends `notifications/cancelled`. Transports: `StdioTransport`
  (`spawn(&StdioCommand)` or `from_io`) and `HttpTransport` (`HttpConfig`; JSON or SSE responses,
  `Mcp-Session-Id` and `MCP-Protocol-Version` headers, no redirects).
- Local-command servers run with the user's permissions: show `StdioCommand::warning()` first.
  Environment values and bearer tokens are `Secret`s (redacted in `Debug`); keep them in a
  per-machine store, never in synced settings.
- `agent` (implies `client`): `agent::McpSource` implements `weirdnet_agent::ToolSource`.
  Annotations map to tiers with the MCP defaults (undeclared = not read-only, destructive,
  open world): destructive is `Interact`, read-only and closed-world `Read`, read-only and
  open-world `Research`, otherwise `Navigate`, each raised to `SourceOptions::tier_floor`
  (default `Research`, because a server's hints are untrusted).

## License

BSD-3-Clause.
