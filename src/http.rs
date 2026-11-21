//! Streamable HTTP for the server (cargo feature `http-server`; `axum` adds [`route`] and
//! [`router`]).
//!
//! The endpoint is stateless with JSON replies: one POST per JSON-RPC message, `202 Accepted` for
//! notifications, no sessions, and `405` for GET and DELETE. It serves both protocol eras:
//!
//! - modern (2026-07-28) requests must send `MCP-Protocol-Version`, `Mcp-Method` and, for
//!   `tools/call`, `resources/read` and `prompts/get`, `Mcp-Name` (plain or `=?base64?...?=`),
//!   matching the body; otherwise `400` with `HeaderMismatch` (`-32020`). Unsupported versions
//!   get `400` with `UnsupportedProtocolVersion` (`-32022`), unknown methods `404`.
//! - legacy (2025-11-25 and earlier) requests may send `MCP-Protocol-Version` (an unsupported one
//!   gets `400`); without it the request is treated as 2025-03-26. Batches are accepted.
//!
//! Before the body is read, [`Guard::check`] enforces, in order:
//!
//! 1. `Host` and `Origin` (DNS-rebinding protection, `403`): the `Host` must be an allowed host,
//!    and an `Origin`, when sent, an allowed origin. [`Guard::loopback`] allows `localhost`,
//!    `127.0.0.1` and `[::1]` on any port; add public names with [`Guard::allow_host`] and
//!    [`Guard::allow_origin`].
//! 2. The method: only POST (`405`, `Allow: POST`).
//! 3. The bearer token ([`Auth`], `401` with `WWW-Authenticate: Bearer`).
//!
//! ```no_run
//! # #[cfg(feature = "axum")]
//! # async fn demo<T: weirdnet_mcp::Tools + Send + Sync + 'static>(server: weirdnet_mcp::Server<T>) {
//! use std::sync::Arc;
//! use weirdnet_mcp::http::{Auth, Guard};
//!
//! let guard = Guard::loopback(Auth::token("a long random secret"));
//! let app = weirdnet_mcp::http::router("/mcp", Arc::new(server), guard);
//! // axum::serve(tokio::net::TcpListener::bind("127.0.0.1:8485").await?, app).await?;
//! # let _ = app;
//! # }
//! ```

use std::fmt;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN, WWW_AUTHENTICATE};
use http::request::Parts;
use http::{HeaderMap, Method, Response, StatusCode};
use serde_json::{Value, json};

use crate::server::{Era, request_era};
use crate::{MODERN_PROTOCOL_VERSIONS, PROTOCOL_VERSIONS, Server, Tools, codes, meta};

/// The protocol version header.
pub const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
/// The method header (2026-07-28).
pub const METHOD_HEADER: &str = "mcp-method";
/// The name header (2026-07-28): `params.name` or `params.uri`.
pub const NAME_HEADER: &str = "mcp-name";

/// The largest request body accepted by default (32 MiB, room for images and audio in arguments).
pub const DEFAULT_MAX_BODY: usize = 32 * 1024 * 1024;

/// How requests authenticate.
#[derive(Clone)]
#[non_exhaustive]
pub enum Auth {
    /// Every request needs `Authorization: Bearer <this token>` (compared in constant time).
    /// Tools see the token.
    Token(String),
    /// Every request needs a bearer token this function accepts. Tools see the token.
    Check(Arc<dyn Fn(&str) -> bool + Send + Sync>),
    /// A token is optional and passed to the tools, which decide (TAUR-style per-tool auth).
    PassThrough,
}

impl Auth {
    /// [`Auth::Token`].
    pub fn token(token: impl Into<String>) -> Self {
        Self::Token(token.into())
    }

    /// [`Auth::Check`].
    pub fn check(f: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        Self::Check(Arc::new(f))
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Token(_) => "Token(<redacted>)",
            Self::Check(_) => "Check(<fn>)",
            Self::PassThrough => "PassThrough",
        })
    }
}

/// Who may reach the endpoint: hosts, origins, the token and the body limit.
#[derive(Clone, Debug)]
pub struct Guard {
    auth: Auth,
    hosts: Vec<String>,
    origins: Vec<String>,
    loopback: bool,
    max_body: usize,
    resource_metadata: Option<String>,
}

const LOOPBACK: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// Compares two byte strings in time that depends only on their lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Splits `host[:port]` (or `[v6][:port]`) into the lowercased host and the whole value.
fn split_host(value: &str) -> Option<(String, String)> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || value.contains(['/', '@', ' ', '\\']) {
        return None;
    }
    let host = if value.starts_with('[') {
        let end = value.find(']')?;
        let rest = &value[end + 1..];
        if !(rest.is_empty() || rest.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))) {
            return None;
        }
        value[..=end].to_string()
    } else {
        match value.split_once(':') {
            Some((h, p)) if !h.is_empty() && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h.to_string(),
            Some(_) => return None,
            None => value.clone(),
        }
    };
    Some((host, value))
}

fn json_response(status: StatusCode, body: &Value) -> Response<String> {
    let mut r = Response::new(body.to_string());
    *r.status_mut() = status;
    r.headers_mut().insert(CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
    r
}

fn error_response(status: StatusCode, id: Option<&Value>, code: i64, message: &str, data: Option<Value>) -> Response<String> {
    let mut err = json!({"code": code, "message": message});
    if let Some(d) = data {
        err["data"] = d;
    }
    let mut body = json!({"jsonrpc": "2.0", "error": err});
    if let Some(id) = id {
        body["id"] = id.clone();
    }
    json_response(status, &body)
}

impl Guard {
    /// Loopback hosts and origins (`localhost`, `127.0.0.1`, `[::1]`, any port) with this auth.
    pub fn loopback(auth: Auth) -> Self {
        Self { auth, hosts: vec![], origins: vec![], loopback: true, max_body: DEFAULT_MAX_BODY, resource_metadata: None }
    }

    /// Only the hosts and origins added with [`allow_host`](Guard::allow_host) and
    /// [`allow_origin`](Guard::allow_origin), with this auth.
    pub fn new(auth: Auth) -> Self {
        Self { loopback: false, ..Self::loopback(auth) }
    }

    /// Allows a `Host`: a name (any port) or `name:port` (that port only).
    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        self.hosts.push(host.into().trim().to_ascii_lowercase());
        self
    }

    /// Allows an `Origin`, such as `https://app.example.com` (scheme, host and port as sent).
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.origins.push(origin.into().trim().trim_end_matches('/').to_ascii_lowercase());
        self
    }

    /// Sets the largest accepted body.
    pub fn max_body(mut self, bytes: usize) -> Self {
        self.max_body = bytes;
        self
    }

    /// The largest accepted body.
    pub fn body_limit(&self) -> usize {
        self.max_body
    }

    /// Adds `resource_metadata="<url>"` (OAuth protected resource metadata) to `401` replies.
    pub fn resource_metadata(mut self, url: impl Into<String>) -> Self {
        self.resource_metadata = Some(url.into());
        self
    }

    fn host_allowed(&self, value: &str) -> bool {
        let Some((host, full)) = split_host(value) else { return false };
        (self.loopback && LOOPBACK.contains(&host.as_str())) || self.hosts.iter().any(|h| *h == host || *h == full)
    }

    fn origin_allowed(&self, value: &str) -> bool {
        let origin = value.trim().trim_end_matches('/').to_ascii_lowercase();
        if self.origins.contains(&origin) {
            return true;
        }
        let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
        self.loopback && split_host(rest).is_some_and(|(host, _)| LOOPBACK.contains(&host.as_str()))
    }

    fn unauthorized(&self, message: &str) -> Response<String> {
        let mut r = error_response(StatusCode::UNAUTHORIZED, None, codes::INVALID_REQUEST, message, None);
        let challenge = match &self.resource_metadata {
            Some(url) => format!("Bearer resource_metadata=\"{url}\""),
            None => "Bearer".to_string(),
        };
        if let Ok(v) = http::HeaderValue::from_str(&challenge) {
            r.headers_mut().insert(WWW_AUTHENTICATE, v);
        }
        r
    }

    /// Checks `Host`, `Origin`, the method and the bearer token, before the body is read.
    /// `Ok` carries the token the tools should see; `Err` is the reply to send instead.
    pub fn check(&self, parts: &Parts) -> Result<Option<String>, Box<Response<String>>> {
        self.check_inner(parts).map_err(Box::new)
    }

    // Internal callers return the reply as it is; only the public signature boxes it.
    #[allow(clippy::result_large_err)]
    fn check_inner(&self, parts: &Parts) -> Result<Option<String>, Response<String>> {
        let host = parts.headers.get(HOST).and_then(|v| v.to_str().ok()).map(str::to_string).or_else(|| parts.uri.authority().map(|a| a.to_string()));
        if !host.as_deref().is_some_and(|h| self.host_allowed(h)) {
            return Err(error_response(StatusCode::FORBIDDEN, None, codes::INVALID_REQUEST, "forbidden: Host is not allowed", None));
        }
        if let Some(origin) = parts.headers.get(ORIGIN)
            && !origin.to_str().is_ok_and(|o| self.origin_allowed(o))
        {
            return Err(error_response(StatusCode::FORBIDDEN, None, codes::INVALID_REQUEST, "forbidden: Origin is not allowed", None));
        }
        if parts.method != Method::POST {
            let mut r = error_response(StatusCode::METHOD_NOT_ALLOWED, None, codes::INVALID_REQUEST, "only POST is supported", None);
            r.headers_mut().insert(ALLOW, http::HeaderValue::from_static("POST"));
            return Err(r);
        }
        let presented = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let ok = match (&self.auth, presented) {
            (Auth::PassThrough, _) => true,
            (_, None) => return Err(self.unauthorized("missing bearer token")),
            (Auth::Token(want), Some(t)) => same(t.as_bytes(), want.as_bytes()),
            (Auth::Check(f), Some(t)) => f(t),
        };
        if !ok {
            return Err(self.unauthorized("invalid bearer token"));
        }
        Ok(presented.map(str::to_string))
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<Result<&'a str, ()>> {
    headers.get(name).map(|v| v.to_str().map_err(|_| ()))
}

/// Decodes a `=?base64?...?=` header value; other values are returned as they are.
fn decode_sentinel(v: &str) -> Option<String> {
    match v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(b64) => STANDARD.decode(b64).ok().and_then(|b| String::from_utf8(b).ok()),
        None => Some(v.to_string()),
    }
}

/// The modern header rules: `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` must be present
/// and match the body.
fn check_modern_headers(headers: &HeaderMap, method: &str, params: &Value) -> Result<(), String> {
    let version = params["_meta"][meta::PROTOCOL_VERSION].as_str().unwrap_or_default();
    match header(headers, PROTOCOL_VERSION_HEADER) {
        None => return Err("missing MCP-Protocol-Version header".into()),
        Some(Ok(h)) if h.trim() == version => {}
        Some(_) => return Err(format!("MCP-Protocol-Version header does not match body value '{version}'")),
    }
    match header(headers, METHOD_HEADER) {
        None => return Err("missing Mcp-Method header".into()),
        Some(Ok(h)) if h.trim() == method => {}
        Some(_) => return Err(format!("Mcp-Method header does not match body value '{method}'")),
    }
    let name_field = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    if let Some(field) = name_field {
        let body = params[field].as_str().unwrap_or_default();
        match header(headers, NAME_HEADER) {
            None => return Err("missing Mcp-Name header".into()),
            Some(Ok(h)) if decode_sentinel(h.trim()).as_deref() == Some(body) => {}
            Some(_) => return Err(format!("Mcp-Name header does not match body value '{body}'")),
        }
    }
    Ok(())
}

/// Answers a request that passed [`Guard::check`]: `token` is what `check` returned and `body` the
/// whole request body (at most [`Guard::body_limit`] bytes).
pub async fn respond_checked<T: Tools + Sync>(server: &Server<T>, parts: &Parts, token: Option<&str>, body: &[u8]) -> Response<String> {
    if let Some(ct) = parts.headers.get(CONTENT_TYPE) {
        let essence = ct.to_str().unwrap_or_default().split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
        if essence != "application/json" {
            return error_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, None, codes::INVALID_REQUEST, "Content-Type must be application/json", None);
        }
    }
    let msg: Value = match std::str::from_utf8(body).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(s).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, None, codes::PARSE_ERROR, &format!("parse error: {e}"), None),
    };
    let header_version = header(&parts.headers, PROTOCOL_VERSION_HEADER);
    let header_is_modern = matches!(header_version, Some(Ok(v)) if MODERN_PROTOCOL_VERSIONS.contains(&v.trim()));
    let mut era = Era::Legacy;
    let id = msg.get("id").cloned();
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        era = match request_era(method, &params) {
            Ok(e) => e,
            Err((code, message, data)) => return error_response(StatusCode::BAD_REQUEST, id.as_ref(), code, &message, data),
        };
        let has_meta_version = params.get("_meta").and_then(|m| m.get(meta::PROTOCOL_VERSION)).is_some();
        if has_meta_version {
            if let Err(e) = check_modern_headers(&parts.headers, method, &params) {
                return error_response(StatusCode::BAD_REQUEST, id.as_ref(), codes::HEADER_MISMATCH, &format!("Header mismatch: {e}"), None);
            }
        } else if header_is_modern {
            return error_response(
                StatusCode::BAD_REQUEST,
                id.as_ref(),
                codes::HEADER_MISMATCH,
                "Header mismatch: MCP-Protocol-Version header names a version the body's _meta does not carry",
                None,
            );
        }
    } else if header_is_modern && msg.is_array() {
        return error_response(StatusCode::BAD_REQUEST, None, codes::INVALID_REQUEST, "batches are not supported in this protocol version", None);
    }
    if era == Era::Legacy
        && !header_is_modern
        && let Some(v) = header_version
        && !v.is_ok_and(|v| PROTOCOL_VERSIONS.contains(&v.trim()))
    {
        return error_response(StatusCode::BAD_REQUEST, id.as_ref(), codes::INVALID_REQUEST, "unsupported MCP-Protocol-Version", None);
    }
    match server.handle_async(token, msg).await {
        None => {
            let mut r = Response::new(String::new());
            *r.status_mut() = StatusCode::ACCEPTED;
            r
        }
        Some(reply) => {
            let status =
                if era == Era::Modern && reply["error"]["code"] == json!(codes::METHOD_NOT_FOUND) { StatusCode::NOT_FOUND } else { StatusCode::OK };
            json_response(status, &reply)
        }
    }
}

/// Checks and answers one request ([`Guard::check`], then [`respond_checked`]), for any framework
/// built on the `http` crate. Prefer [`Guard::check`] before reading a large body.
pub async fn respond<T: Tools + Sync>(server: &Server<T>, guard: &Guard, parts: &Parts, body: &[u8]) -> Response<String> {
    let token = match guard.check_inner(parts) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if body.len() > guard.max_body {
        return error_response(StatusCode::PAYLOAD_TOO_LARGE, None, codes::INVALID_REQUEST, "request body is too large", None);
    }
    respond_checked(server, parts, token.as_deref(), body).await
}

#[cfg(feature = "axum")]
pub use axum_route::{route, router};

#[cfg(feature = "axum")]
mod axum_route {
    use std::sync::Arc;

    use super::{Guard, codes, error_response, respond_checked};
    use crate::{Server, Tools};

    /// An axum route for the MCP endpoint: checks the guard before reading the body, then
    /// answers. Mount it with `Router::route("/mcp", route(server, guard))`.
    pub fn route<T, S>(server: Arc<Server<T>>, guard: Guard) -> axum::routing::MethodRouter<S>
    where
        T: Tools + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        let guard = Arc::new(guard);
        axum::routing::any(move |req: axum::extract::Request| {
            let (server, guard) = (server.clone(), guard.clone());
            async move {
                let (parts, body) = req.into_parts();
                let reply = match guard.check_inner(&parts) {
                    Err(r) => r,
                    Ok(token) => match axum::body::to_bytes(body, guard.body_limit()).await {
                        Ok(bytes) => respond_checked(&server, &parts, token.as_deref(), &bytes).await,
                        Err(_) => {
                            error_response(http::StatusCode::PAYLOAD_TOO_LARGE, None, codes::INVALID_REQUEST, "request body is too large", None)
                        }
                    },
                };
                reply.map(axum::body::Body::from)
            }
        })
    }

    /// A router with only the MCP endpoint at `path` (usually `/mcp`).
    pub fn router<T>(path: &str, server: Arc<Server<T>>, guard: Guard) -> axum::Router
    where
        T: Tools + Send + Sync + 'static,
    {
        axum::Router::new().route(path, route(server, guard))
    }
}
