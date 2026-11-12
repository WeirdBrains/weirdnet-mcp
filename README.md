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

## License

BSD-3-Clause.
