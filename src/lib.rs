//! A small Model Context Protocol tools server, with resources and prompts.
//!
//! The crate speaks MCP's JSON-RPC 2.0 messages. A front end passes each decoded message to
//! [`Server::handle`] (sync tools) or [`Server::handle_async`] (async tools) with the caller's
//! bearer token, and the app implements [`Tools`]. The server is stateless and dual-era: it
//! answers legacy clients that open with `initialize` (2024-11-05 to 2025-11-25) and modern
//! clients that send the protocol version in each request's `_meta` (2026-07-28), including
//! `server/discover`.
//!
//! - [`ToolSpec`] / [`Param`] / [`ParamType`]: static tool tables with `readOnlyHint`,
//!   `destructiveHint` and `openWorldHint` annotations, nested objects, enums, arrays and `anyOf`,
//!   and an auth flag that adds "Requires a bearer token." to the description.
//! - [`Tools`]: the app side. `call` (sync) or `call_async` (async), plus optional resources
//!   (`list_resources`, `resource_templates`, `read_resource`) and prompts (`prompts`,
//!   `get_prompt`).
//! - [`PromptSpec`] / [`PromptArg`] / [`GetPromptResult`] / [`PromptMessage`]: prompt tables and
//!   filled-in prompts for `prompts/list` and `prompts/get`.
//! - [`CallToolResult`] / [`Content`]: tool results with text, image, audio, resource links and
//!   embedded resources, built by servers and parsed back by clients ([`CallToolResult::from_json`]).
//! - [`args`]: typed argument getters with uniform error messages.
//! - [`stdio`]: `stdio::serve_blocking` (sync tools), `stdio::serve` (cargo feature
//!   `stdio-server`: concurrent requests and cancellation), and `stdio::bridge`, the `<app>ctl mcp`
//!   loop that forwards stdio messages to a remote server.
//! - `http` (cargo feature `http-server`): streamable HTTP with bearer auth and `Host` / `Origin`
//!   checks against DNS rebinding; `axum` adds a ready route.
//! - `client` (cargo feature, off by default): an MCP *client* for stdio and streamable HTTP servers.
//! - `agent` (cargo feature, implies `client`): `weirdnet_agent::ToolSource` for an MCP server.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "agent")]
pub mod agent;
pub mod args;
#[cfg(feature = "client")]
pub mod client;
mod content;
#[cfg(feature = "http-server")]
pub mod http;
mod prompts;
mod server;
pub mod stdio;

pub use content::{CallToolResult, Content, Resource, ResourceBody, ResourceContents, ResourceError, ResourceTemplate};
pub use prompts::{GetPromptResult, PromptArg, PromptError, PromptMessage, PromptSpec, Role};
pub use server::{CacheHint, CacheScope, Server, ServerInfo, ToolError, Tools};

use serde_json::{Value, json};

/// Legacy protocol versions this server negotiates in `initialize`, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Modern (per-request `_meta`, no `initialize`) protocol versions this server accepts, newest first.
pub const MODERN_PROTOCOL_VERSIONS: &[&str] = &["2026-07-28"];

/// `_meta` keys defined by MCP 2026-07-28.
pub mod meta {
    /// The request's protocol version (required on modern requests).
    pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
    /// The client's capabilities for this request (required on modern requests).
    pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
    /// The client's name and version (optional).
    pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
    /// The server's name and version, in each modern result.
    pub const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
}

/// JSON-RPC error codes used here.
pub mod codes {
    /// Parse error (the message is not JSON).
    pub const PARSE_ERROR: i64 = -32700;
    /// Invalid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid params (unknown tool or prompt, missing prompt arguments, missing `_meta` fields,
    /// and resource not found on modern requests).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL: i64 = -32603;
    /// Resource not found, for legacy (2025-11-25 and earlier) requests.
    pub const RESOURCE_NOT_FOUND_LEGACY: i64 = -32002;
    /// HTTP headers do not match the body, or a required header is missing (2026-07-28).
    pub const HEADER_MISMATCH: i64 = -32020;
    /// The request needs a client capability the client did not declare (2026-07-28).
    pub const MISSING_CLIENT_CAPABILITY: i64 = -32021;
    /// The requested protocol version is not supported (2026-07-28).
    pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
}

/// JSON Schema type of a parameter.
///
/// Nested types are `&'static` so tool tables stay `const`:
///
/// ```
/// use weirdnet_mcp::{Param, ParamType};
///
/// const MESSAGE: &[Param] = &[
///     Param::required("role", ParamType::Enum(&["system", "user", "assistant"]), "Who speaks"),
///     Param::required("content", ParamType::Any, "A string or a list of content parts"),
/// ];
/// const MESSAGES: Param = Param::optional("messages", ParamType::Array(&ParamType::Object(MESSAGE)), "The chat so far");
/// const INPUT: Param =
///     Param::required("input", ParamType::AnyOf(&[ParamType::String, ParamType::StringArray]), "Text or texts to embed");
/// # let _ = (MESSAGES, INPUT);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParamType {
    /// `string`
    String,
    /// `integer`
    Integer,
    /// `number`
    Number,
    /// `boolean`
    Boolean,
    /// `array` of strings
    StringArray,
    /// Any JSON value (`{}`).
    Any,
    /// A string from a fixed list (`{"type": "string", "enum": [...]}`).
    Enum(&'static [&'static str]),
    /// An `array` whose items have this type.
    Array(&'static ParamType),
    /// An `object` with these properties (`required` lists the required ones).
    Object(&'static [Param]),
    /// One of these types (`anyOf`).
    AnyOf(&'static [ParamType]),
    /// A raw JSON Schema, as JSON text, for anything the other variants cannot say. Text that is
    /// not a JSON object becomes `{}`; [`ToolSpec::check`] reports it.
    Json(&'static str),
}

impl ParamType {
    /// The JSON Schema for this type.
    pub fn schema(self) -> Value {
        match self {
            Self::String => json!({"type": "string"}),
            Self::Integer => json!({"type": "integer"}),
            Self::Number => json!({"type": "number"}),
            Self::Boolean => json!({"type": "boolean"}),
            Self::StringArray => json!({"type": "array", "items": {"type": "string"}}),
            Self::Any => json!({}),
            Self::Enum(values) => json!({"type": "string", "enum": values}),
            Self::Array(items) => json!({"type": "array", "items": items.schema()}),
            Self::Object(params) => object_schema(params, false),
            Self::AnyOf(types) => json!({"anyOf": types.iter().map(|t| t.schema()).collect::<Vec<_>>()}),
            Self::Json(text) => serde_json::from_str::<Value>(text).ok().filter(Value::is_object).unwrap_or_else(|| json!({})),
        }
    }

    fn check(self, path: &str, problems: &mut Vec<String>) {
        match self {
            Self::Enum([]) => problems.push(format!("{path}: an enum needs at least one value")),
            Self::AnyOf([]) => problems.push(format!("{path}: anyOf needs at least one type")),
            Self::Json(text) if !serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object()) => {
                problems.push(format!("{path}: the raw schema is not a JSON object"))
            }
            Self::Array(items) => items.check(&format!("{path}[]"), problems),
            Self::AnyOf(types) => types.iter().enumerate().for_each(|(i, t)| t.check(&format!("{path}|{i}"), problems)),
            Self::Object(params) => check_params(params, path, problems),
            _ => {}
        }
    }
}

/// `{"type": "object", "properties": ..., "required": [...]}`. Top-level parameters always carry a
/// description (as they always have); nested ones only when it is not empty.
fn object_schema(params: &[Param], top: bool) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = vec![];
    for p in params {
        let mut schema = p.ty.schema();
        if top || !p.description.is_empty() {
            schema["description"] = json!(p.description);
        }
        props.insert(p.name.to_string(), schema);
        if p.required {
            required.push(p.name);
        }
    }
    json!({"type": "object", "properties": props, "required": required})
}

fn check_params(params: &[Param], path: &str, problems: &mut Vec<String>) {
    for (i, p) in params.iter().enumerate() {
        let here = if path.is_empty() { p.name.to_string() } else { format!("{path}.{}", p.name) };
        if p.name.is_empty() {
            problems.push(format!("{path}: a parameter has no name"));
        }
        if params[..i].iter().any(|q| q.name == p.name) {
            problems.push(format!("{here}: duplicate parameter"));
        }
        p.ty.check(&here, problems);
    }
}

/// One tool parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    /// Name.
    pub name: &'static str,
    /// Type.
    pub ty: ParamType,
    /// Description.
    pub description: &'static str,
    /// Required.
    pub required: bool,
}

impl Param {
    /// A required parameter.
    pub const fn required(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self { name, ty, description, required: true }
    }

    /// An optional parameter.
    pub const fn optional(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self { name, ty, description, required: false }
    }
}

/// A tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    /// Name.
    pub name: &'static str,
    /// Description.
    pub description: &'static str,
    /// Parameters.
    pub params: &'static [Param],
    /// Needs the caller's bearer token.
    pub auth: bool,
    /// `readOnlyHint`.
    pub read_only: bool,
    /// `destructiveHint`.
    pub destructive: bool,
    /// `openWorldHint` (reaches outside the app's own data).
    pub open_world: bool,
}

impl ToolSpec {
    /// The `tools/list` entry.
    pub fn to_json(&self) -> Value {
        let description = if self.auth { format!("{} Requires a bearer token.", self.description) } else { self.description.to_string() };
        json!({
            "name": self.name,
            "description": description,
            "inputSchema": object_schema(self.params, true),
            "annotations": {"readOnlyHint": self.read_only, "destructiveHint": self.destructive, "openWorldHint": self.open_world},
        })
    }

    /// Problems with a tool table, for a unit test: empty or duplicate names, empty enums and
    /// `anyOf`s, raw schemas that are not JSON objects.
    pub fn check(specs: &[ToolSpec]) -> Result<(), Vec<String>> {
        let mut problems = vec![];
        for (i, t) in specs.iter().enumerate() {
            if t.name.is_empty() {
                problems.push("a tool has no name".to_string());
            }
            if specs[..i].iter().any(|u| u.name == t.name) {
                problems.push(format!("{}: duplicate tool", t.name));
            }
            check_params(t.params, t.name, &mut problems);
        }
        if problems.is_empty() { Ok(()) } else { Err(problems) }
    }
}

/// A JSON-RPC success.
pub fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A JSON-RPC error.
pub fn rpc_err(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

/// A JSON-RPC error with `data`.
pub fn rpc_err_data(id: Value, code: i64, msg: &str, data: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg, "data": data}})
}

/// A successful tool result: pretty JSON text plus `structuredContent`
/// (the same as `CallToolResult::json(v).to_json()`).
pub fn tool_ok(v: &Value) -> Value {
    CallToolResult::json(v).to_json()
}

/// A failed tool result.
pub fn tool_error(text: &str) -> Value {
    CallToolResult::error(text).to_json()
}

#[cfg(test)]
mod tests;
