//! An MCP client (cargo feature `client`, off by default).
//!
//! [`Client::connect`] runs `initialize` over a [`Transport`] and then offers `tools/list`
//! ([`Client::list_tools`]) and `tools/call` ([`Client::call_tool`] for text, or
//! [`Client::call_tool_content`] to keep image, audio and resource blocks). Every request has a timeout
//! and can be cancelled; both send MCP's `notifications/cancelled` to the server and free the
//! pending request.
//!
//! Transports: [`StdioTransport`] (a local command, or any async reader/writer pair) and, with
//! the same feature, [`HttpTransport`] (streamable HTTP, JSON or SSE responses).
//!
//! Security notes:
//! - A local-command server runs with the user's permissions. [`StdioCommand::warning`] gives
//!   the text to show before starting one.
//! - Secrets (environment values, bearer tokens) are [`Secret`]s: redacted in `Debug`, never
//!   serialised by this crate. Keep them in a per-machine store, never in synced settings.
//! - A server's annotations are hints from an untrusted party; callers decide what to trust.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

mod child;
mod http;

pub use child::{DEFAULT_MAX_LINE, StdioCommand, StdioTransport};
pub use http::{HttpConfig, HttpTransport};

use crate::{CallToolResult, PROTOCOL_VERSIONS};

/// A secret string (token, environment value). `Debug` is redacted; there is no `Display`
/// or serialisation.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself. Use only to hand it to the transport.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// A client failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// The server command could not be started.
    Spawn(String),
    /// Reading or writing the transport failed.
    Io(String),
    /// The connection is closed (the process exited, or the session ended).
    Closed,
    /// The server sent something that is not valid MCP.
    Protocol(String),
    /// The server answered with a JSON-RPC error.
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// Message.
        message: String,
    },
    /// An HTTP status other than success.
    Http {
        /// Status code.
        status: u16,
        /// Start of the body (at most 200 characters).
        body: String,
    },
    /// No answer within the timeout.
    Timeout,
    /// The caller cancelled the request.
    Cancelled,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start the server: {e}"),
            Self::Io(e) => write!(f, "transport error: {e}"),
            Self::Closed => f.write_str("the server connection is closed"),
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
            Self::Rpc { code, message } => write!(f, "server error {code}: {message}"),
            Self::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Self::Timeout => f.write_str("timed out"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ClientError {}

/// A way to exchange JSON-RPC messages with one server.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Sends a notification (no response expected).
    async fn notify(&self, msg: Value) -> Result<(), ClientError>;

    /// Sends a request whose `id` is `id` and returns the matching JSON-RPC response message
    /// (result or error). Dropping the future abandons the request.
    async fn request(&self, id: u64, msg: Value) -> Result<Value, ClientError>;

    /// The request `id` timed out or was cancelled; drop any state kept for it.
    fn forget(&self, _id: u64) {}

    /// The negotiated protocol version (HTTP sends it as a header).
    fn set_protocol_version(&self, _version: &str) {}

    /// Ends the session and releases the connection.
    async fn close(&self) {}
}

/// Client settings.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// Per-request timeout.
    pub timeout: Duration,
    /// `clientInfo.name`.
    pub client_name: String,
    /// `clientInfo.version`.
    pub client_version: String,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(30), client_name: "weirdnet-mcp".into(), client_version: env!("CARGO_PKG_VERSION").into() }
    }
}

/// What the server said in `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerHello {
    /// Negotiated protocol version.
    pub protocol_version: String,
    /// `serverInfo.name`.
    pub name: String,
    /// `serverInfo.version`.
    pub version: String,
    /// Instructions for the model, if any.
    pub instructions: Option<String>,
    /// Whether the server declared the `tools` capability.
    pub has_tools: bool,
}

/// A tool's annotations as the server declared them. `None` means "not declared".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteAnnotations {
    /// `readOnlyHint`.
    pub read_only: Option<bool>,
    /// `destructiveHint`.
    pub destructive: Option<bool>,
    /// `openWorldHint`.
    pub open_world: Option<bool>,
}

impl RemoteAnnotations {
    /// `readOnlyHint` with the spec default (`false`).
    pub fn read_only(&self) -> bool {
        self.read_only.unwrap_or(false)
    }

    /// `destructiveHint` with the spec default (`true`), which only applies to tools that are
    /// not read-only.
    pub fn destructive(&self) -> bool {
        !self.read_only() && self.destructive.unwrap_or(true)
    }

    /// `openWorldHint` with the spec default (`true`).
    pub fn open_world(&self) -> bool {
        self.open_world.unwrap_or(true)
    }
}

/// One tool from `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTool {
    /// Tool name.
    pub name: String,
    /// Display title, if the server gave one.
    pub title: Option<String>,
    /// Description (empty when missing).
    pub description: String,
    /// JSON Schema for the arguments.
    pub input_schema: Value,
    /// Annotations.
    pub annotations: RemoteAnnotations,
}

/// The result of `tools/call`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    /// Text content joined by newlines. Non-text items appear as `[<type> content omitted]`; use
    /// [`Client::call_tool_content`] to keep images and audio.
    pub text: String,
    /// `structuredContent`, if any.
    pub structured: Option<Value>,
    /// The server flagged the call as failed (`isError`).
    pub is_error: bool,
}

/// A connected MCP client.
pub struct Client {
    transport: Box<dyn Transport>,
    next_id: AtomicU64,
    options: ClientOptions,
    hello: ServerHello,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("hello", &self.hello).field("options", &self.options).finish_non_exhaustive()
    }
}

const MAX_LIST_PAGES: usize = 100;

impl Client {
    /// Runs `initialize` and `notifications/initialized` over `transport`.
    pub async fn connect(transport: impl Transport + 'static, options: ClientOptions) -> Result<Self, ClientError> {
        let mut client = Self {
            transport: Box::new(transport),
            next_id: AtomicU64::new(1),
            options,
            hello: ServerHello { protocol_version: String::new(), name: String::new(), version: String::new(), instructions: None, has_tools: false },
        };
        let params = json!({
            "protocolVersion": PROTOCOL_VERSIONS[0],
            "capabilities": {},
            "clientInfo": {"name": client.options.client_name, "version": client.options.client_version},
        });
        let result = client.rpc("initialize", params, std::future::pending()).await?;
        let version = result["protocolVersion"].as_str().unwrap_or("");
        if !PROTOCOL_VERSIONS.contains(&version) {
            client.transport.close().await;
            return Err(ClientError::Protocol(format!("unsupported protocol version {version:?}")));
        }
        client.hello = ServerHello {
            protocol_version: version.to_string(),
            name: result["serverInfo"]["name"].as_str().unwrap_or("").to_string(),
            version: result["serverInfo"]["version"].as_str().unwrap_or("").to_string(),
            instructions: result["instructions"].as_str().map(str::to_string),
            has_tools: result["capabilities"].get("tools").is_some_and(|t| !t.is_null()),
        };
        client.transport.set_protocol_version(version);
        client.transport.notify(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await?;
        Ok(client)
    }

    /// What the server reported at `initialize`.
    pub fn hello(&self) -> &ServerHello {
        &self.hello
    }

    /// Pings the server.
    pub async fn ping(&self) -> Result<(), ClientError> {
        self.rpc("ping", json!({}), std::future::pending()).await.map(|_| ())
    }

    /// Lists every tool, following `nextCursor`.
    pub async fn list_tools(&self) -> Result<Vec<RemoteTool>, ClientError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = cursor.as_ref().map_or_else(|| json!({}), |c| json!({"cursor": c}));
            let result = self.rpc("tools/list", params, std::future::pending()).await?;
            let page = result["tools"].as_array().ok_or_else(|| ClientError::Protocol("tools/list without a tools array".into()))?;
            for t in page {
                tools.push(parse_tool(t)?);
            }
            match result["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => return Ok(tools),
            }
        }
        Err(ClientError::Protocol("tools/list has too many pages".into()))
    }

    /// Calls a tool.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult, ClientError> {
        self.call_tool_until(name, arguments, std::future::pending()).await
    }

    /// Calls a tool; if `cancel` resolves first the call is abandoned, the server gets
    /// `notifications/cancelled` and the result is [`ClientError::Cancelled`].
    pub async fn call_tool_until(&self, name: &str, arguments: Value, cancel: impl Future<Output = ()>) -> Result<ToolResult, ClientError> {
        let arguments = if arguments.is_null() { json!({}) } else { arguments };
        let result = self.rpc("tools/call", json!({"name": name, "arguments": arguments}), cancel).await?;
        Ok(parse_result(&result))
    }

    /// Calls a tool and keeps every content block: text, images and audio (base64 `data` plus
    /// `mimeType`), resource links and embedded resources, in the server's order. See
    /// [`CallToolResult::from_json`].
    pub async fn call_tool_content(&self, name: &str, arguments: Value) -> Result<CallToolResult, ClientError> {
        self.call_tool_content_until(name, arguments, std::future::pending()).await
    }

    /// [`call_tool_content`](Self::call_tool_content) that is abandoned (with
    /// `notifications/cancelled`) if `cancel` resolves first, as in
    /// [`call_tool_until`](Self::call_tool_until).
    pub async fn call_tool_content_until(
        &self,
        name: &str,
        arguments: Value,
        cancel: impl Future<Output = ()>,
    ) -> Result<CallToolResult, ClientError> {
        let arguments = if arguments.is_null() { json!({}) } else { arguments };
        let result = self.rpc("tools/call", json!({"name": name, "arguments": arguments}), cancel).await?;
        Ok(CallToolResult::from_json(&result))
    }

    /// Ends the session (closes stdin and stops the process, or deletes the HTTP session).
    pub async fn close(&self) {
        self.transport.close().await;
    }

    async fn rpc(&self, method: &str, params: Value, cancel: impl Future<Output = ()>) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let outcome = tokio::select! {
            r = self.transport.request(id, msg) => return into_result(r?),
            () = tokio::time::sleep(self.options.timeout) => ClientError::Timeout,
            () = cancel => ClientError::Cancelled,
        };
        self.transport.forget(id);
        if method != "initialize" {
            let reason = if outcome == ClientError::Timeout { "timed out" } else { "cancelled by the client" };
            let note = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id, "reason": reason}});
            // Best effort, and bounded so a stuck pipe cannot hang the caller.
            let _ = tokio::time::timeout(Duration::from_secs(2), self.transport.notify(note)).await;
        }
        Err(outcome)
    }
}

fn into_result(response: Value) -> Result<Value, ClientError> {
    if let Some(err) = response.get("error") {
        return Err(ClientError::Rpc { code: err["code"].as_i64().unwrap_or(0), message: err["message"].as_str().unwrap_or("").to_string() });
    }
    response.get("result").cloned().ok_or_else(|| ClientError::Protocol("response has neither result nor error".into()))
}

fn parse_tool(t: &Value) -> Result<RemoteTool, ClientError> {
    let name = t["name"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| ClientError::Protocol("a tool has no name".into()))?;
    let a = &t["annotations"];
    Ok(RemoteTool {
        name: name.to_string(),
        title: t["title"].as_str().map(str::to_string),
        description: t["description"].as_str().unwrap_or("").to_string(),
        input_schema: if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({"type": "object", "properties": {}}) },
        annotations: RemoteAnnotations {
            read_only: a["readOnlyHint"].as_bool(),
            destructive: a["destructiveHint"].as_bool(),
            open_world: a["openWorldHint"].as_bool(),
        },
    })
}

fn parse_result(r: &Value) -> ToolResult {
    let mut parts = Vec::new();
    for item in r["content"].as_array().into_iter().flatten() {
        match (item["type"].as_str(), item["text"].as_str()) {
            (Some("text"), Some(text)) => parts.push(text.to_string()),
            (ty, _) => parts.push(format!("[{} content omitted]", ty.unwrap_or("unknown"))),
        }
    }
    let structured = r.get("structuredContent").filter(|v| !v.is_null()).cloned();
    let text = match (&structured, parts.is_empty()) {
        (Some(s), true) => s.to_string(),
        _ => parts.join("\n"),
    };
    ToolResult { text, structured, is_error: r["isError"].as_bool().unwrap_or(false) }
}

#[cfg(test)]
pub(crate) mod tests;
