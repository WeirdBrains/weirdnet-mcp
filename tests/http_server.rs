//! The streamable HTTP endpoint (axum route) over real sockets: auth, Host and Origin checks,
//! both protocol eras and the modern header rules.

use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use weirdnet_mcp::http::{Auth, Guard, respond, router};
use weirdnet_mcp::{CallToolResult, Content, Server, ServerInfo, ToolError, ToolSpec, Tools, meta};

const SPECS: &[ToolSpec] = &[
    ToolSpec { name: "picture", description: "An image.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ToolSpec { name: "whoami", description: "The token.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ToolSpec { name: "naïve", description: "A non-ASCII name.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
];

struct App;

impl Tools for App {
    fn specs(&self) -> &[ToolSpec] {
        SPECS
    }

    async fn call_async<'a>(&'a self, tool: &'a ToolSpec, _a: &'a Value, token: Option<&'a str>) -> Result<CallToolResult, ToolError> {
        tokio::task::yield_now().await;
        Ok(match tool.name {
            "picture" => CallToolResult::new(vec![Content::text("a pixel"), Content::image("R0lGODlh", "image/gif")]),
            "whoami" => CallToolResult::json(&json!({"token": token})),
            _ => CallToolResult::new(vec![Content::text("ok")]),
        })
    }
}

fn app_server() -> Arc<Server<App>> {
    Arc::new(Server::new(ServerInfo { name: "h".into(), title: "H".into(), version: "1".into(), instructions: String::new() }, App))
}

async fn start(guard: Guard) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router("/mcp", app_server(), guard);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

/// One raw HTTP/1.1 request. `host` is sent as given (or not at all for `None`).
async fn send(addr: SocketAddr, method: &str, host: Option<&str>, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} /mcp HTTP/1.1\r\n");
    if let Some(h) = host {
        req.push_str(&format!("host: {h}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8(out).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Reply { status, head: head.to_ascii_lowercase(), body: body.to_string() }
}

const TOKEN: &str = "s3cret-token";
const JSON: (&str, &str) = ("content-type", "application/json");
const AUTH: (&str, &str) = ("authorization", "Bearer s3cret-token");

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn modern_params(mut params: Value) -> Value {
    params["_meta"] = json!({meta::PROTOCOL_VERSION: "2026-07-28", meta::CLIENT_CAPABILITIES: {}});
    params
}

async fn post(addr: SocketAddr, headers: &[(&str, &str)], body: &str) -> Reply {
    send(addr, "POST", Some(&addr.to_string()), headers, body).await
}

#[tokio::test]
async fn bearer_token_is_required() {
    let addr = start(Guard::loopback(Auth::token(TOKEN)).resource_metadata("https://auth.example.com/.well-known/oauth-protected-resource")).await;
    let body = rpc(1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    let ok = post(addr, &[JSON, AUTH], &body).await;
    assert_eq!(ok.status, 200);
    assert!(ok.head.contains("content-type: application/json"));
    assert_eq!(ok.json()["result"]["serverInfo"]["name"], "h");
    assert!(!ok.head.contains("mcp-session-id"));
    for headers in [
        vec![JSON],
        vec![JSON, ("authorization", "Bearer wrong")],
        vec![JSON, ("authorization", "Basic czNjcmV0")],
        vec![JSON, ("authorization", "Bearer ")],
    ] {
        let r = post(addr, &headers, &body).await;
        assert_eq!(r.status, 401, "{headers:?}");
        assert!(r.head.contains("www-authenticate: bearer resource_metadata=\"https://auth.example.com/"), "{}", r.head);
        assert!(r.json().get("id").is_none());
        assert!(!r.body.contains(TOKEN));
    }
}

#[tokio::test]
async fn host_and_origin_are_checked_before_auth() {
    let addr = start(Guard::loopback(Auth::token(TOKEN))).await;
    let body = rpc(1, "tools/list", json!({}));
    let port = addr.port();
    for host in [format!("localhost:{port}"), format!("127.0.0.1:{port}"), format!("[::1]:{port}"), "LOCALHOST".to_string()] {
        assert_eq!(send(addr, "POST", Some(&host), &[JSON, AUTH], &body).await.status, 200, "{host}");
    }
    for host in
        ["evil.example", "evil.example:80", "localhost.evil.example", "127.0.0.1.evil.example", "localhost:abc", "user@localhost", "192.0.2.1"]
    {
        let r = send(addr, "POST", Some(host), &[JSON, AUTH], &body).await;
        assert_eq!(r.status, 403, "{host}");
        assert_eq!(r.json()["error"]["message"], "forbidden: Host is not allowed");
    }
    // A rebinding page has no token either, but the Host check comes first.
    assert_eq!(send(addr, "POST", Some("evil.example"), &[JSON], &body).await.status, 403);
    for origin in ["http://localhost:3000", "https://127.0.0.1", "http://[::1]:9"] {
        assert_eq!(post(addr, &[JSON, AUTH, ("origin", origin)], &body).await.status, 200, "{origin}");
    }
    for origin in ["http://evil.example", "null", "http://localhost.evil.example", "file://", "http://evil.example:8485"] {
        let r = post(addr, &[JSON, AUTH, ("origin", origin)], &body).await;
        assert_eq!(r.status, 403, "{origin}");
        assert_eq!(r.json()["error"]["message"], "forbidden: Origin is not allowed");
    }
}

#[tokio::test]
async fn configured_hosts_and_origins() {
    let addr = start(
        Guard::new(Auth::token(TOKEN)).allow_host("mcp.example.com").allow_host("alt.example.com:8443").allow_origin("https://app.example.com/"),
    )
    .await;
    let body = rpc(1, "tools/list", json!({}));
    assert_eq!(send(addr, "POST", Some("mcp.example.com"), &[JSON, AUTH], &body).await.status, 200);
    assert_eq!(send(addr, "POST", Some("MCP.example.com:443"), &[JSON, AUTH], &body).await.status, 200);
    assert_eq!(send(addr, "POST", Some("alt.example.com:8443"), &[JSON, AUTH], &body).await.status, 200);
    assert_eq!(send(addr, "POST", Some("alt.example.com:9999"), &[JSON, AUTH], &body).await.status, 403);
    // Loopback is not allowed unless asked for.
    assert_eq!(post(addr, &[JSON, AUTH], &body).await.status, 403);
    let h = Some("mcp.example.com");
    assert_eq!(send(addr, "POST", h, &[JSON, AUTH, ("origin", "https://app.example.com")], &body).await.status, 200);
    assert_eq!(send(addr, "POST", h, &[JSON, AUTH, ("origin", "http://app.example.com")], &body).await.status, 403);
    assert_eq!(send(addr, "POST", h, &[JSON, AUTH, ("origin", "http://localhost:3000")], &body).await.status, 403);
}

#[tokio::test]
async fn methods_notifications_and_bad_bodies() {
    let addr = start(Guard::loopback(Auth::token(TOKEN)).max_body(4096)).await;
    for method in ["GET", "DELETE", "PUT"] {
        let r = send(addr, method, Some(&addr.to_string()), &[AUTH], "").await;
        assert_eq!(r.status, 405, "{method}");
        assert!(r.head.contains("allow: post"));
    }
    let r = post(addr, &[JSON, AUTH], &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()).await;
    assert_eq!((r.status, r.body.as_str()), (202, ""));
    let r = post(addr, &[JSON, AUTH], "{not json").await;
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (400, json!(-32700)));
    let r = post(addr, &[("content-type", "text/plain"), AUTH], &rpc(1, "tools/list", json!({}))).await;
    assert_eq!(r.status, 415);
    assert_eq!(post(addr, &[("content-type", "application/json; charset=utf-8"), AUTH], &rpc(1, "tools/list", json!({}))).await.status, 200);
    let big = rpc(1, "tools/call", json!({"name": "picture", "arguments": {"pad": "x".repeat(5000)}}));
    assert_eq!(post(addr, &[JSON, AUTH], &big).await.status, 413);
    // Legacy batches still work.
    let batch = json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}]).to_string();
    let r = post(addr, &[JSON, AUTH], &batch).await;
    assert_eq!(r.json().as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn legacy_protocol_version_header() {
    let addr = start(Guard::loopback(Auth::token(TOKEN))).await;
    let body = rpc(1, "tools/call", json!({"name": "picture"}));
    let r = post(addr, &[JSON, AUTH, ("mcp-protocol-version", "2025-06-18")], &body).await;
    assert_eq!(r.status, 200);
    let result = &r.json()["result"];
    assert_eq!(result["content"][1], json!({"type": "image", "data": "R0lGODlh", "mimeType": "image/gif"}));
    assert!(result.get("resultType").is_none());
    assert_eq!(post(addr, &[JSON, AUTH], &body).await.status, 200);
    let r = post(addr, &[JSON, AUTH, ("mcp-protocol-version", "1999-01-01")], &body).await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["id"], 1);
    let r = post(addr, &[JSON, AUTH, ("mcp-protocol-version", "2026-07-28")], &batch_of_one()).await;
    assert_eq!(r.status, 400);
}

fn batch_of_one() -> String {
    json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]).to_string()
}

#[tokio::test]
async fn modern_requests_and_header_rules() {
    let addr = start(Guard::loopback(Auth::token(TOKEN))).await;
    let call = rpc(5, "tools/call", modern_params(json!({"name": "picture"})));
    let v = ("mcp-protocol-version", "2026-07-28");
    let m = ("mcp-method", "tools/call");
    let r = post(addr, &[JSON, AUTH, v, m, ("mcp-name", "picture")], &call).await;
    assert_eq!(r.status, 200);
    let result = r.json()["result"].clone();
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["content"][1]["mimeType"], "image/gif");
    assert_eq!(result["_meta"][meta::SERVER_INFO], json!({"name": "h", "title": "H", "version": "1"}));
    // Mcp-Name may be base64-encoded, and must be for a non-ASCII name.
    let r = post(addr, &[JSON, AUTH, v, m, ("mcp-name", "=?base64?cGljdHVyZQ==?=")], &call).await;
    assert_eq!(r.status, 200);
    let naive = rpc(6, "tools/call", modern_params(json!({"name": "naïve"})));
    assert_eq!(post(addr, &[JSON, AUTH, v, m, ("mcp-name", "=?base64?bmHDr3Zl?=")], &naive).await.status, 200);
    // Missing or mismatched headers are HeaderMismatch errors.
    let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
        (vec![m, ("mcp-name", "picture")], "missing MCP-Protocol-Version"),
        (vec![("mcp-protocol-version", "2025-06-18"), m, ("mcp-name", "picture")], "MCP-Protocol-Version header does not match"),
        (vec![v, ("mcp-name", "picture")], "missing Mcp-Method"),
        (vec![v, ("mcp-method", "tools/list"), ("mcp-name", "picture")], "Mcp-Method header does not match"),
        (vec![v, m], "missing Mcp-Name"),
        (vec![v, m, ("mcp-name", "whoami")], "Mcp-Name header does not match"),
        (vec![v, m, ("mcp-name", "=?base64?!!!?=")], "Mcp-Name header does not match"),
    ];
    for (extra, want) in cases {
        let mut headers = vec![JSON, AUTH];
        headers.extend(extra);
        let r = post(addr, &headers, &call).await;
        assert_eq!(r.status, 400, "{want}");
        let err = r.json();
        assert_eq!((err["id"].clone(), err["error"]["code"].clone()), (json!(5), json!(-32020)), "{want}");
        assert!(err["error"]["message"].as_str().unwrap().contains(want), "{want}: {err}");
    }
    // A modern header with a body that has no _meta.
    let r = post(addr, &[JSON, AUTH, v, ("mcp-method", "tools/list")], &rpc(7, "tools/list", json!({}))).await;
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (400, json!(-32020)));
    // Unsupported versions list what is supported.
    let mut old = modern_params(json!({}));
    old["_meta"][meta::PROTOCOL_VERSION] = json!("3000-01-01");
    let r = post(addr, &[JSON, AUTH, ("mcp-protocol-version", "3000-01-01"), ("mcp-method", "tools/list")], &rpc(8, "tools/list", old)).await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"]["data"], json!({"supported": ["2026-07-28"], "requested": "3000-01-01"}));
    // Missing client capabilities.
    let mut no_caps = modern_params(json!({}));
    no_caps["_meta"].as_object_mut().unwrap().remove(meta::CLIENT_CAPABILITIES);
    let r = post(addr, &[JSON, AUTH, v, ("mcp-method", "tools/list")], &rpc(9, "tools/list", no_caps)).await;
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (400, json!(-32602)));
    // Unknown methods are 404 with a JSON-RPC error body.
    let r = post(addr, &[JSON, AUTH, v, ("mcp-method", "prompts/list")], &rpc(10, "prompts/list", modern_params(json!({})))).await;
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (404, json!(-32601)));
    // Discovery.
    let r = post(addr, &[JSON, AUTH, v, ("mcp-method", "server/discover")], &rpc(11, "server/discover", modern_params(json!({})))).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["result"]["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(r.json()["result"]["cacheScope"], "private");
}

#[tokio::test]
async fn check_and_pass_through_auth() {
    let addr = start(Guard::loopback(Auth::check(|t| t.starts_with("ok-")))).await;
    let body = rpc(1, "tools/call", json!({"name": "whoami"}));
    let r = post(addr, &[JSON, ("authorization", "Bearer ok-1")], &body).await;
    assert_eq!(r.json()["result"]["structuredContent"], json!({"token": "ok-1"}));
    assert_eq!(post(addr, &[JSON, ("authorization", "Bearer no-1")], &body).await.status, 401);
    let addr = start(Guard::loopback(Auth::PassThrough)).await;
    assert_eq!(post(addr, &[JSON], &body).await.json()["result"]["structuredContent"], json!({"token": null}));
    assert_eq!(post(addr, &[JSON, ("authorization", "Bearer abc")], &body).await.json()["result"]["structuredContent"], json!({"token": "abc"}));
    assert!(!format!("{:?}", Guard::loopback(Auth::token(TOKEN))).contains(TOKEN));
}

#[tokio::test]
async fn framework_agnostic_respond() {
    let server = app_server();
    let guard = Guard::loopback(Auth::token(TOKEN));
    let build = |host: &str| {
        let (parts, ()) =
            http::Request::post("/mcp").header("host", host).header("authorization", "Bearer s3cret-token").body(()).unwrap().into_parts();
        parts
    };
    let body = rpc(1, "tools/list", json!({}));
    let r = respond(&server, &guard, &build("localhost:1"), body.as_bytes()).await;
    assert_eq!(r.status(), 200);
    assert_eq!(serde_json::from_str::<Value>(r.body()).unwrap()["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(respond(&server, &guard, &build("evil.example"), body.as_bytes()).await.status(), 403);
    assert_eq!(guard.check(&build("evil.example")).unwrap_err().status(), 403);
    assert_eq!(guard.check(&build("localhost")).unwrap().as_deref(), Some(TOKEN));
}

#[cfg(feature = "client")]
#[tokio::test]
async fn the_crate_client_talks_to_the_http_server() {
    use weirdnet_mcp::client::{Client, ClientOptions, HttpConfig, HttpTransport, Secret};
    let addr = start(Guard::loopback(Auth::token(TOKEN))).await;
    let cfg = HttpConfig::new(format!("http://{addr}/mcp")).bearer(Secret::new(TOKEN));
    let client = Client::connect(HttpTransport::new(&cfg).unwrap(), ClientOptions::default()).await.unwrap();
    assert_eq!(client.hello().name, "h");
    assert_eq!(client.list_tools().await.unwrap().len(), 3);
    let r = client.call_tool("picture", json!({})).await.unwrap();
    assert_eq!(r.text, "a pixel\n[image content omitted]");
    let full = client.call_tool_content("picture", json!({})).await.unwrap();
    assert_eq!(full.content, vec![Content::text("a pixel"), Content::image("R0lGODlh", "image/gif")]);
    assert_eq!(full.text(), "a pixel");
    assert_eq!(full.content[1].decoded_bytes().as_deref(), Some(&b"GIF89a"[..]));
    client.close().await;
    let bad = HttpConfig::new(format!("http://{addr}/mcp")).bearer(Secret::new("nope"));
    assert!(Client::connect(HttpTransport::new(&bad).unwrap(), ClientOptions::default()).await.is_err());
}
