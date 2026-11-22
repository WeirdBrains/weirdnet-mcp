//! [`Server`] and the app-side [`Tools`] trait.

use std::future::{Future, ready};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};

use crate::{
    CallToolResult, GetPromptResult, MODERN_PROTOCOL_VERSIONS, PROTOCOL_VERSIONS, PromptError, PromptSpec, Resource, ResourceContents, ResourceError,
    ResourceTemplate, ToolSpec, codes, meta, rpc_err, rpc_err_data, rpc_ok, tool_error,
};

/// What `initialize` and `server/discover` report.
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

/// Who may cache a result (`cacheScope`, MCP 2026-07-28).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheScope {
    /// Shared intermediaries may cache it.
    Public,
    /// Only the requesting client may cache it.
    #[default]
    Private,
}

/// Caching hints (`ttlMs`, `cacheScope`) on modern `server/discover`, list and read results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheHint {
    /// How long, in milliseconds, the result may be considered fresh. 0 is immediately stale.
    pub ttl_ms: u64,
    /// Who may cache it.
    pub scope: CacheScope,
}

impl CacheHint {
    /// A hint.
    pub const fn new(ttl_ms: u64, scope: CacheScope) -> Self {
        Self { ttl_ms, scope }
    }
}

/// The app side: runs tools and, optionally, serves resources.
///
/// Implement [`call`](Tools::call) for sync tools or [`call_async`](Tools::call_async) for async
/// ones (and rich results: images, audio, resource links). [`Server::handle`] serves both as long
/// as `call_async` finishes without waiting; [`Server::handle_async`] (and the stdio and HTTP
/// servers) serves anything.
///
/// ```
/// use serde_json::{Value, json};
/// use weirdnet_mcp::{CallToolResult, Content, ToolError, ToolSpec, Tools};
///
/// struct App;
///
/// impl Tools for App {
///     fn specs(&self) -> &[ToolSpec] {
///         &[]
///     }
///
///     async fn call_async<'a>(&'a self, tool: &'a ToolSpec, args: &'a Value, token: Option<&'a str>) -> Result<CallToolResult, ToolError> {
///         let _ = (tool, args, token);
///         Ok(CallToolResult::new(vec![Content::text("a red pixel"), Content::image_bytes(b"\x89PNG", "image/png")]))
///     }
/// }
/// ```
pub trait Tools {
    /// The tool table, in a stable order.
    fn specs(&self) -> &[ToolSpec];

    /// Runs `tool` with `args` for a caller presenting `token`. Auth, permission checks and
    /// hiding internal errors are the app's. The default refuses, for apps that implement only
    /// [`call_async`](Tools::call_async).
    fn call(&self, tool: &ToolSpec, args: &Value, token: Option<&str>) -> Result<Value, ToolError> {
        let _ = (args, token);
        Err(ToolError(format!("{} only runs asynchronously (Server::handle_async)", tool.name)))
    }

    /// Runs `tool` asynchronously, with full control of the result's content. The default runs
    /// [`call`](Tools::call) and turns its value into pretty text plus `structuredContent`.
    fn call_async<'a>(
        &'a self,
        tool: &'a ToolSpec,
        args: &'a Value,
        token: Option<&'a str>,
    ) -> impl Future<Output = Result<CallToolResult, ToolError>> + Send + 'a
    where
        Self: Sized,
    {
        ready(self.call(tool, args, token).map(|v| CallToolResult::json(&v)))
    }

    /// Whether the server offers resources (the `resources` capability and `resources/*`
    /// methods). Default `false`.
    fn has_resources(&self) -> bool {
        false
    }

    /// `resources/list`: the resources this caller may see.
    fn list_resources<'a>(&'a self, token: Option<&'a str>) -> impl Future<Output = Result<Vec<Resource>, ResourceError>> + Send + 'a
    where
        Self: Sized,
    {
        let _ = token;
        ready(Ok(vec![]))
    }

    /// `resources/templates/list`.
    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![]
    }

    /// `resources/read`. Return [`ResourceError::NotFound`] for an unknown URI (never an empty list).
    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        token: Option<&'a str>,
    ) -> impl Future<Output = Result<Vec<ResourceContents>, ResourceError>> + Send + 'a
    where
        Self: Sized,
    {
        let _ = (uri, token);
        ready(Err(ResourceError::NotFound))
    }

    /// Caching hints for a modern result of `method` (`server/discover`, `tools/list`,
    /// `prompts/list`, `resources/list`, `resources/templates/list`, `resources/read`).
    /// Default: 0 ms, private.
    fn cache_hint(&self, method: &str) -> CacheHint {
        let _ = method;
        CacheHint::default()
    }

    /// The prompt table for `prompts/list`, in a stable order. Default: none. The `prompts`
    /// capability and `prompts/*` methods are offered only when this is not empty.
    fn prompts(&self) -> &[PromptSpec] {
        &[]
    }

    /// `prompts/get`: fills in `prompt` with `args` (a JSON object of strings) for a caller
    /// presenting `token`. The server has already checked that `prompt` is in
    /// [`prompts`](Tools::prompts) and that its required arguments are present; other checks
    /// (permissions, argument values) are the app's. The default refuses.
    fn get_prompt<'a>(
        &'a self,
        prompt: &'a PromptSpec,
        args: &'a Value,
        token: Option<&'a str>,
    ) -> impl Future<Output = Result<GetPromptResult, PromptError>> + Send + 'a
    where
        Self: Sized,
    {
        let _ = (args, token);
        ready(Err(PromptError::Failed(format!("prompt {} is not implemented", prompt.name))))
    }
}

/// Which protocol era a request belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Era {
    /// `initialize`-based (2025-11-25 and earlier).
    Legacy,
    /// Per-request `_meta` (2026-07-28).
    Modern,
}

/// A request-level protocol error: code, message, optional data.
pub(crate) type ProtocolError = (i64, String, Option<Value>);

/// The era of a request, or the error a modern request with bad `_meta` gets.
pub(crate) fn request_era(method: &str, params: &Value) -> Result<Era, ProtocolError> {
    if method == "initialize" {
        return Ok(Era::Legacy);
    }
    let meta = params.get("_meta");
    let Some(version) = meta.and_then(|m| m.get(meta::PROTOCOL_VERSION)) else {
        return Ok(if method == "server/discover" { Era::Modern } else { Era::Legacy });
    };
    let requested = version.as_str().unwrap_or_default();
    if !MODERN_PROTOCOL_VERSIONS.contains(&requested) {
        return Err((
            codes::UNSUPPORTED_PROTOCOL_VERSION,
            "Unsupported protocol version".into(),
            Some(json!({"supported": MODERN_PROTOCOL_VERSIONS, "requested": version})),
        ));
    }
    if !meta.and_then(|m| m.get(meta::CLIENT_CAPABILITIES)).is_some_and(Value::is_object) {
        return Err((codes::INVALID_PARAMS, format!("missing _meta \"{}\"", meta::CLIENT_CAPABILITIES), None));
    }
    Ok(Era::Modern)
}

const CACHEABLE: &[&str] = &["server/discover", "tools/list", "prompts/list", "resources/list", "resources/templates/list", "resources/read"];

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
    ///
    /// Tools run through [`Tools::call_async`] polled once: sync tools (the default `call_async`)
    /// always finish; a tool that has to wait gets an internal error telling the app to use
    /// [`handle_async`](Server::handle_async).
    pub fn handle(&self, token: Option<&str>, msg: Value) -> Option<Value> {
        if let Value::Array(batch) = msg {
            let out: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(token, m)).collect();
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        let id = msg.get("id").cloned();
        let fut = pin!(self.handle_one(token, msg));
        match fut.poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(r) => r,
            Poll::Pending => id.map(|id| rpc_err(id, codes::INTERNAL, "this request runs asynchronously; serve it with Server::handle_async")),
        }
    }

    /// Handles one JSON-RPC message or batch, awaiting async tools and resources.
    pub async fn handle_async(&self, token: Option<&str>, msg: Value) -> Option<Value> {
        if let Value::Array(batch) = msg {
            let mut out = vec![];
            for m in batch {
                out.extend(self.handle_one(token, m).await);
            }
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        self.handle_one(token, msg).await
    }

    /// Parses raw JSON and handles it; text that is not JSON gets a parse error.
    pub fn handle_raw(&self, token: Option<&str>, raw: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => self.handle(token, v),
            Err(e) => Some(parse_error(&e)),
        }
    }

    /// Parses raw JSON and handles it asynchronously; text that is not JSON gets a parse error.
    pub async fn handle_raw_async(&self, token: Option<&str>, raw: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => self.handle_async(token, v).await,
            Err(e) => Some(parse_error(&e)),
        }
    }

    async fn handle_one(&self, token: Option<&str>, msg: Value) -> Option<Value> {
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
        let era = match request_era(method, &params) {
            Ok(era) => era,
            Err((code, message, data)) => {
                return Some(match data {
                    Some(d) => rpc_err_data(id, code, &message, d),
                    None => rpc_err(id, code, &message),
                });
            }
        };
        let reply = self.dispatch(token, id, method, &params, era).await;
        Some(self.finish(era, method, reply))
    }

    fn capabilities(&self) -> Value {
        let mut caps = json!({"tools": {"listChanged": false}});
        if self.tools.has_resources() {
            caps["resources"] = json!({"subscribe": false, "listChanged": false});
        }
        if !self.tools.prompts().is_empty() {
            caps["prompts"] = json!({"listChanged": false});
        }
        caps
    }

    async fn dispatch(&self, token: Option<&str>, id: Value, method: &str, params: &Value, era: Era) -> Value {
        let resources = self.tools.has_resources();
        let prompts = !self.tools.prompts().is_empty();
        match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or("");
                let v = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
                rpc_ok(
                    id,
                    json!({
                        "protocolVersion": v,
                        "capabilities": self.capabilities(),
                        "serverInfo": {"name": self.info.name, "title": self.info.title, "version": self.info.version},
                        "instructions": self.info.instructions,
                    }),
                )
            }
            "server/discover" => {
                let mut r = json!({"supportedVersions": MODERN_PROTOCOL_VERSIONS, "capabilities": self.capabilities()});
                if !self.info.instructions.is_empty() {
                    r["instructions"] = json!(self.info.instructions);
                }
                rpc_ok(id, r)
            }
            "ping" => rpc_ok(id, json!({})),
            "tools/list" => rpc_ok(id, json!({"tools": self.tools.specs().iter().map(ToolSpec::to_json).collect::<Vec<_>>()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let Some(tool) = self.tools.specs().iter().find(|t| t.name == name) else {
                    return rpc_err(id, codes::INVALID_PARAMS, &format!("unknown tool: {name}"));
                };
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.tools.call_async(tool, &args, token).await {
                    Ok(r) => rpc_ok(id, r.to_json()),
                    Err(ToolError(text)) => rpc_ok(id, tool_error(&text)),
                }
            }
            "prompts/list" if prompts => rpc_ok(id, json!({"prompts": self.tools.prompts().iter().map(PromptSpec::to_json).collect::<Vec<_>>()})),
            "prompts/get" if prompts => {
                let Some(name) = params["name"].as_str() else {
                    return rpc_err(id, codes::INVALID_PARAMS, "missing string parameter \"name\"");
                };
                let Some(prompt) = self.tools.prompts().iter().find(|p| p.name == name) else {
                    return rpc_err(id, codes::INVALID_PARAMS, &format!("unknown prompt: {name}"));
                };
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if let Err(m) = prompt.check_arguments(&args) {
                    return rpc_err(id, codes::INVALID_PARAMS, &m);
                }
                let args = if args.is_null() { json!({}) } else { args };
                match self.tools.get_prompt(prompt, &args, token).await {
                    Ok(r) => rpc_ok(id, r.to_json()),
                    Err(PromptError::NotFound) => rpc_err(id, codes::INVALID_PARAMS, &format!("unknown prompt: {name}")),
                    Err(PromptError::InvalidArguments(m)) => rpc_err(id, codes::INVALID_PARAMS, &m),
                    Err(PromptError::Failed(m)) => rpc_err(id, codes::INTERNAL, &m),
                }
            }
            "resources/list" if resources => match self.tools.list_resources(token).await {
                Ok(list) => rpc_ok(id, json!({"resources": list.iter().map(Resource::to_json).collect::<Vec<_>>()})),
                Err(e) => resource_error(id, e, era, None),
            },
            "resources/templates/list" if resources => {
                rpc_ok(id, json!({"resourceTemplates": self.tools.resource_templates().iter().map(ResourceTemplate::to_json).collect::<Vec<_>>()}))
            }
            "resources/read" if resources => {
                let Some(uri) = params["uri"].as_str() else {
                    return rpc_err(id, codes::INVALID_PARAMS, "missing string parameter \"uri\"");
                };
                match self.tools.read_resource(uri, token).await {
                    Ok(contents) => rpc_ok(id, json!({"contents": contents.iter().map(ResourceContents::to_json).collect::<Vec<_>>()})),
                    Err(e) => resource_error(id, e, era, Some(uri)),
                }
            }
            _ => rpc_err(id, codes::METHOD_NOT_FOUND, &format!("method not found: {method}")),
        }
    }

    /// Modern results carry `resultType`, the server's identity and (for lists and reads) caching hints.
    fn finish(&self, era: Era, method: &str, mut reply: Value) -> Value {
        if era != Era::Modern {
            return reply;
        }
        let Some(result) = reply.get_mut("result").and_then(Value::as_object_mut) else { return reply };
        result.insert("resultType".into(), json!("complete"));
        let mut implementation = json!({"name": self.info.name, "version": self.info.version});
        if !self.info.title.is_empty() {
            implementation["title"] = json!(self.info.title);
        }
        let meta = result.entry("_meta").or_insert_with(|| json!({}));
        if let Some(m) = meta.as_object_mut() {
            m.insert(meta::SERVER_INFO.into(), implementation);
        }
        if CACHEABLE.contains(&method) {
            let hint = self.tools.cache_hint(method);
            result.insert("ttlMs".into(), json!(hint.ttl_ms));
            result.insert("cacheScope".into(), json!(if hint.scope == CacheScope::Public { "public" } else { "private" }));
        }
        reply
    }
}

fn parse_error(e: &serde_json::Error) -> Value {
    rpc_err(Value::Null, codes::PARSE_ERROR, &format!("parse error: {e}"))
}

fn resource_error(id: Value, e: ResourceError, era: Era, uri: Option<&str>) -> Value {
    match e {
        ResourceError::NotFound => {
            let code = if era == Era::Modern { codes::INVALID_PARAMS } else { codes::RESOURCE_NOT_FOUND_LEGACY };
            rpc_err_data(id, code, "Resource not found", json!({"uri": uri}))
        }
        ResourceError::Failed(m) => rpc_err(id, codes::INTERNAL, &m),
    }
}
