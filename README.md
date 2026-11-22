# weirdnet-mcp

A stateless [Model Context Protocol](https://modelcontextprotocol.io) server for tools,
resources and prompts, plus an MCP client. Extracted from TAUR and shared by
[Weird Brains](https://weirdbrains.com) apps (Salty Gull, Ymir, Bragi).

The server is dual-era. It answers legacy clients that open with `initialize` (protocol
versions 2024-11-05 to 2025-11-25) and modern clients that send the version in each request's
`_meta` ([2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)), including
`server/discover`, `resultType` and caching hints. Legacy output is byte-for-byte what 0.5
produced.

## Server

- `Server::handle` (sync) or `Server::handle_async` takes one decoded JSON-RPC 2.0 message (or a
  batch) plus the caller's bearer token and returns the reply, or nothing for notifications.
  Methods: `initialize`, `server/discover`, `ping`, `tools/list`, `tools/call`, with resources
  `resources/list`, `resources/templates/list`, `resources/read`, and with prompts
  `prompts/list`, `prompts/get`.
- `ToolSpec` / `Param` / `ParamType`: `const` tool tables with input schemas and `readOnlyHint`,
  `destructiveHint`, `openWorldHint`. Tools that need auth say "Requires a bearer token.".
  Parameter types: string, integer, number, boolean, string arrays, `Any`, `Enum`, `Array`,
  nested `Object`, `AnyOf`, and `Json` (a raw schema). `ToolSpec::check` finds mistakes in a table.
- `Tools` is the app side. Implement `call` (sync, returns JSON) or `call_async` (async, returns a
  `CallToolResult`). Failures become `isError` tool results. Optional: `has_resources`,
  `list_resources`, `resource_templates`, `read_resource`, `cache_hint`, `prompts`, `get_prompt`.
- Prompts ([MCP prompts](https://modelcontextprotocol.io/specification/2026-07-28/server/prompts)):
  `PromptSpec` / `PromptArg` are `const` prompt tables (`PromptSpec::check` finds mistakes), and
  `Tools::get_prompt` returns a `GetPromptResult` of `PromptMessage`s (user or assistant, any
  `Content`). The `prompts` capability is advertised only when `Tools::prompts` is not empty.
  The server answers unknown prompts and missing or non-string arguments with `-32602` before
  the app runs; `PromptError::NotFound` (hide a prompt from this caller), `InvalidArguments` and
  `Failed` (`-32603`) cover the rest.
- `CallToolResult` / `Content`: text, `image` and `audio` (base64 `data` plus `mimeType`),
  `resource_link`, embedded resources, and `structuredContent`. `Content::from_data_url`,
  `data_url`, `audio_format` and `format` convert to and from the `data:` URLs and audio formats
  that [weirdnet-agent](https://crates.io/crates/weirdnet-agent) 0.5 `ContentPart`s use.
- `args`: argument getters with uniform messages.

### Transports

- `stdio::serve_blocking`: a sequential stdio loop for sync tools.
- `stdio::serve` / `serve_with` (feature `stdio-server`): a tokio loop that runs requests
  concurrently, honours `notifications/cancelled` and caps message size (32 MiB).
- `http` (feature `http-server`): streamable HTTP for any framework built on the `http` crate.
  `Guard` checks `Host` and `Origin` (DNS-rebinding protection, `403`), allows only POST (`405`),
  then the bearer token (`Auth::Token` in constant time, `Auth::Check`, or `Auth::PassThrough`
  for per-tool auth; `401` with `WWW-Authenticate: Bearer`). `Guard::loopback` allows
  `localhost`, `127.0.0.1` and `[::1]`; add public names with `allow_host` and `allow_origin`.
  Modern requests must carry matching `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name`
  headers (`400` with `HeaderMismatch` otherwise). Notifications get `202`, and no sessions are
  minted.
- `http::route` / `http::router` (feature `axum`): the same as an axum 0.8 route.

```rust,ignore
let server = Arc::new(Server::new(info, MyTools));
let app = weirdnet_mcp::http::router("/mcp", server.clone(), Guard::loopback(Auth::token(secret)));
axum::serve(TcpListener::bind("127.0.0.1:8485").await?, app).await?;
// or, over stdio:
weirdnet_mcp::stdio::serve(server, tokio::io::stdin(), tokio::io::stdout()).await?;
```

- `stdio::bridge`: the `<app>ctl mcp` loop that forwards stdio lines to a remote `/mcp`.

## Client (cargo features `client` and `agent`, off by default)

The server API above needs neither feature.

- `client`: `Client::connect(transport, ClientOptions)` runs `initialize`, then `list_tools`
  (follows cursors), `call_tool` and `call_tool_until(name, args, cancel)`. Every request has a
  timeout; a timeout or cancel sends `notifications/cancelled`. Transports: `StdioTransport`
  (`spawn(&StdioCommand)` or `from_io`) and `HttpTransport` (`HttpConfig`; JSON or SSE responses,
  `Mcp-Session-Id` and `MCP-Protocol-Version` headers, no redirects). The client speaks the
  legacy (`initialize`) era.
- Local-command servers run with the user's permissions: show `StdioCommand::warning()` first.
  Environment values and bearer tokens are `Secret`s (redacted in `Debug`); keep them in a
  per-machine store, never in synced settings.
- `agent` (implies `client`): `agent::McpSource` implements `weirdnet_agent::ToolSource`.
  Annotations map to tiers with the MCP defaults (undeclared = not read-only, destructive,
  open world): destructive is `Interact`, read-only and closed-world `Read`, read-only and
  open-world `Research`, otherwise `Navigate`, each raised to `SourceOptions::tier_floor`
  (default `Research`, because a server's hints are untrusted).

Resource subscriptions (`subscriptions/listen`) and the 2025 session-based HTTP features (GET
streams, `Mcp-Session-Id`) are not served: the server is stateless.

See [CHANGELOG.md](CHANGELOG.md) for changes and [HANDOFF.md](HANDOFF.md) for work in progress.

## License

BSD-3-Clause, copyright [Weird Brains](https://weirdbrains.com) LLC. See [LICENSE](LICENSE).
