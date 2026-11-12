//! A small Model Context Protocol tools server.
//!
//! The crate speaks MCP's JSON-RPC 2.0 messages and knows nothing about HTTP or the app:
//! a front end (an axum `POST /mcp` route, a gRPC bridge, a test) passes each decoded message
//! to [`Server::handle`] with the caller's bearer token, and the app implements [`Tools::call`].
//! The server is stateless, so it fits MCP's streamable HTTP transport with JSON responses.
//!
//! - [`ToolSpec`] / [`Param`]: static tool tables with `readOnlyHint`, `destructiveHint` and
//!   `openWorldHint` annotations, and an auth flag that adds "Requires a bearer token." to the
//!   description.
//! - [`args`]: typed argument getters with uniform error messages.
//! - [`stdio`]: the `<app>ctl mcp` loop that forwards stdio messages to a remote server.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod args;
pub mod stdio;

use serde_json::{Value, json};

/// Protocol versions this server negotiates, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes used here.
pub mod codes {
    /// Invalid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid params (unknown tool).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL: i64 = -32603;
}

/// JSON Schema type of a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl ParamType {
    fn schema(self) -> Value {
        match self {
            Self::String => json!({"type": "string"}),
            Self::Integer => json!({"type": "integer"}),
            Self::Number => json!({"type": "number"}),
            Self::Boolean => json!({"type": "boolean"}),
            Self::StringArray => json!({"type": "array", "items": {"type": "string"}}),
        }
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
        let mut props = serde_json::Map::new();
        let mut required = vec![];
        for p in self.params {
            let mut schema = p.ty.schema();
            schema["description"] = json!(p.description);
            props.insert(p.name.to_string(), schema);
            if p.required {
                required.push(p.name);
            }
        }
        let description = if self.auth { format!("{} Requires a bearer token.", self.description) } else { self.description.to_string() };
        json!({
            "name": self.name,
            "description": description,
            "inputSchema": {"type": "object", "properties": props, "required": required},
            "annotations": {"readOnlyHint": self.read_only, "destructiveHint": self.destructive, "openWorldHint": self.open_world},
        })
    }
}

/// What `initialize` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// Machine name, e.g. `taur`.
    pub name: String,
    /// Display name, e.g. `TAUR`.
    pub title: String,
    /// Server version.
    pub version: String,
    /// Instructions shown to the model.
    pub instructions: String,
}

/// A tool failure, reported to the model as an `isError` result (not a JSON-RPC error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError(pub String);

impl<E: std::fmt::Display> From<E> for ToolError {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

/// The app side: runs a tool.
pub trait Tools {
    /// The tool table.
    fn specs(&self) -> &[ToolSpec];

    /// Runs `tool` with `args` for a caller presenting `token`. Auth, permission checks and
    /// hiding internal errors are the app's.
    fn call(&self, tool: &ToolSpec, args: &Value, token: Option<&str>) -> Result<Value, ToolError>;
}

/// A JSON-RPC success.
pub fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A JSON-RPC error.
pub fn rpc_err(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

/// A successful tool result: pretty JSON text plus `structuredContent`.
pub fn tool_ok(v: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default()}],
        "structuredContent": if v.is_object() { v.clone() } else { json!({"result": v}) },
        "isError": false,
    })
}

/// A failed tool result.
pub fn tool_error(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

/// The MCP server.
pub struct Server<T> {
    /// `initialize` data.
    pub info: ServerInfo,
    /// The app's tools.
    pub tools: T,
}

impl<T: Tools> Server<T> {
    /// A server.
    pub fn new(info: ServerInfo, tools: T) -> Self {
        Self { info, tools }
    }

    /// Handles one JSON-RPC message or batch. `None` means no response (notifications,
    /// responses, or a batch of only those).
    pub fn handle(&self, token: Option<&str>, msg: Value) -> Option<Value> {
        if let Value::Array(batch) = msg {
            let out: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(token, m)).collect();
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str);
        let (Some(id), Some(method)) = (id, method) else {
            return if msg.get("method").is_some() || msg.get("result").is_some() {
                None
            } else {
                Some(rpc_err(Value::Null, codes::INVALID_REQUEST, "invalid request"))
            };
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        Some(match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or("");
                let v = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
                rpc_ok(
                    id,
                    json!({
                        "protocolVersion": v,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": self.info.name, "title": self.info.title, "version": self.info.version},
                        "instructions": self.info.instructions,
                    }),
                )
            }
            "ping" => rpc_ok(id, json!({})),
            "tools/list" => rpc_ok(id, json!({"tools": self.tools.specs().iter().map(ToolSpec::to_json).collect::<Vec<_>>()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let Some(tool) = self.tools.specs().iter().find(|t| t.name == name) else {
                    return Some(rpc_err(id, codes::INVALID_PARAMS, &format!("unknown tool: {name}")));
                };
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.tools.call(tool, &args, token) {
                    Ok(v) => rpc_ok(id, tool_ok(&v)),
                    Err(ToolError(text)) => rpc_ok(id, tool_error(&text)),
                }
            }
            _ => rpc_err(id, codes::METHOD_NOT_FOUND, &format!("method not found: {method}")),
        })
    }
}

#[cfg(test)]
mod tests;
