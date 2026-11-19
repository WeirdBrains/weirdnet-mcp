use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;
use crate::{Param, ParamType, Server, ServerInfo, ToolError, ToolSpec, Tools, args};

const SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "echo",
        description: "Echo.",
        params: &[Param::required("text", ParamType::String, "Text")],
        auth: false,
        read_only: true,
        destructive: false,
        open_world: false,
    },
    ToolSpec { name: "delete", description: "Delete.", params: &[], auth: false, read_only: false, destructive: true, open_world: false },
    ToolSpec { name: "hang", description: "Never answers.", params: &[], auth: false, read_only: true, destructive: false, open_world: true },
];

struct App;

impl Tools for App {
    fn specs(&self) -> &[ToolSpec] {
        SPECS
    }

    fn call(&self, tool: &ToolSpec, a: &Value, _token: Option<&str>) -> Result<Value, ToolError> {
        match tool.name {
            "echo" => Ok(json!({"echo": args::string(a, "text")?})),
            "delete" => Err(ToolError("refused".into())),
            _ => unreachable!(),
        }
    }
}

/// The in-process mock: the crate's own `Server` plus a `hang` tool that never answers, a
/// record of `notifications/cancelled`, and optional paging of `tools/list`.
#[derive(Clone, Default)]
pub(crate) struct Mock {
    pub(crate) cancelled: Arc<Mutex<Vec<Value>>>,
    seen: Arc<Mutex<Vec<String>>>,
    pub(crate) page_size: Option<usize>,
}

impl Mock {
    fn reply(&self, server: &Server<App>, msg: Value) -> Option<Value> {
        let method = msg["method"].as_str().unwrap_or("").to_string();
        self.seen.lock().unwrap().push(method.clone());
        if method == "notifications/cancelled" {
            self.cancelled.lock().unwrap().push(msg["params"]["requestId"].clone());
            return None;
        }
        if method == "tools/call" && msg["params"]["name"] == "hang" {
            return None;
        }
        if method == "tools/list"
            && let Some(n) = self.page_size
        {
            let start = msg["params"]["cursor"].as_str().map_or(0, |c| c.parse::<usize>().unwrap());
            let all: Vec<Value> = SPECS.iter().map(ToolSpec::to_json).collect();
            let end = (start + n).min(all.len());
            let mut result = json!({"tools": all[start..end]});
            if end < all.len() {
                result["nextCursor"] = json!(end.to_string());
            }
            return Some(crate::rpc_ok(msg["id"].clone(), result));
        }
        server.handle(None, msg)
    }

    pub(crate) fn transport(&self) -> StdioTransport {
        let (client_end, server_end) = tokio::io::duplex(64 * 1024);
        let (client_r, client_w) = tokio::io::split(client_end);
        let (server_r, mut server_w) = tokio::io::split(server_end);
        let mock = self.clone();
        tokio::spawn(async move {
            let server =
                Server::new(ServerInfo { name: "mock".into(), title: "Mock".into(), version: "9".into(), instructions: "Be nice.".into() }, App);
            let mut lines = BufReader::new(server_r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(out) = mock.reply(&server, msg) {
                    let _ = server_w.write_all(format!("{out}\n").as_bytes()).await;
                }
            }
        });
        StdioTransport::from_io(client_r, client_w)
    }
}

pub(crate) fn options(timeout_ms: u64) -> ClientOptions {
    ClientOptions { timeout: Duration::from_millis(timeout_ms), ..ClientOptions::default() }
}

#[tokio::test]
async fn initialize_reports_the_server() {
    let mock = Mock::default();
    let client = Client::connect(mock.transport(), options(2000)).await.unwrap();
    let h = client.hello();
    assert_eq!((h.name.as_str(), h.version.as_str()), ("mock", "9"));
    assert_eq!(h.protocol_version, PROTOCOL_VERSIONS[0]);
    assert_eq!(h.instructions.as_deref(), Some("Be nice."));
    assert!(h.has_tools);
    client.ping().await.unwrap();
    // `notifications/initialized` follows `initialize` before anything else.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(mock.seen.lock().unwrap()[..2], ["initialize", "notifications/initialized"]);
}

#[tokio::test]
async fn lists_tools_with_annotations_and_defaults() {
    let client = Client::connect(Mock::default().transport(), options(2000)).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["echo", "delete", "hang"]);
    let echo = &tools[0];
    assert_eq!(echo.description, "Echo.");
    assert_eq!(echo.input_schema["required"], json!(["text"]));
    assert_eq!(echo.annotations, RemoteAnnotations { read_only: Some(true), destructive: Some(false), open_world: Some(false) });
    assert!(echo.annotations.read_only() && !echo.annotations.destructive() && !echo.annotations.open_world());
    assert!(tools[1].annotations.destructive());
    // Missing hints take the spec defaults: not read-only, destructive, open world.
    let none = RemoteAnnotations::default();
    assert!(!none.read_only() && none.destructive() && none.open_world());
    // Destructive only applies to tools that are not read-only.
    let ro = RemoteAnnotations { read_only: Some(true), ..none };
    assert!(!ro.destructive());
}

#[tokio::test]
async fn follows_list_cursors() {
    let client = Client::connect(Mock { page_size: Some(1), ..Mock::default() }.transport(), options(2000)).await.unwrap();
    assert_eq!(client.list_tools().await.unwrap().len(), 3);
}

#[tokio::test]
async fn calls_tools_and_reports_failures() {
    let client = Client::connect(Mock::default().transport(), options(2000)).await.unwrap();
    let ok = client.call_tool("echo", json!({"text": "hi"})).await.unwrap();
    assert!(!ok.is_error);
    assert_eq!(ok.structured, Some(json!({"echo": "hi"})));
    assert!(ok.text.contains("\"echo\": \"hi\""));
    let failed = client.call_tool("delete", Value::Null).await.unwrap();
    assert!(failed.is_error);
    assert_eq!(failed.text, "refused");
    // A JSON-RPC error (unknown tool) is an Err, not an isError result.
    match client.call_tool("nope", json!({})).await {
        Err(ClientError::Rpc { code, message }) => {
            assert_eq!(code, crate::codes::INVALID_PARAMS);
            assert!(message.contains("nope"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn concurrent_calls_are_matched_by_id() {
    let client = Arc::new(Client::connect(Mock::default().transport(), options(2000)).await.unwrap());
    let mut tasks = Vec::new();
    for i in 0..20 {
        let c = client.clone();
        tasks.push(tokio::spawn(
            async move { c.call_tool("echo", json!({"text": format!("m{i}")})).await.unwrap().structured.unwrap()["echo"].clone() },
        ));
    }
    for (i, t) in tasks.into_iter().enumerate() {
        assert_eq!(t.await.unwrap(), json!(format!("m{i}")));
    }
}

#[tokio::test]
async fn timeouts_notify_the_server_and_the_client_keeps_working() {
    let mock = Mock::default();
    let client = Client::connect(mock.transport(), options(150)).await.unwrap();
    let started = std::time::Instant::now();
    assert_eq!(client.call_tool("hang", json!({})).await, Err(ClientError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(2));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(mock.cancelled.lock().unwrap().len(), 1);
    // The abandoned request does not break later ones.
    assert!(client.call_tool("echo", json!({"text": "x"})).await.is_ok());
}

#[tokio::test]
async fn cancellation_wins_over_a_slow_call() {
    let mock = Mock::default();
    let client = Client::connect(mock.transport(), options(10_000)).await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = stop.send(());
    });
    let r = client
        .call_tool_until("hang", json!({}), async {
            let _ = stopped.await;
        })
        .await;
    assert_eq!(r, Err(ClientError::Cancelled));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancelled = mock.cancelled.lock().unwrap().clone();
    assert_eq!(cancelled.len(), 1);
    assert!(cancelled[0].as_u64().is_some());
}

#[tokio::test]
async fn a_closed_connection_fails_pending_and_new_requests() {
    let (client_end, server_end) = tokio::io::duplex(1024);
    let (r, w) = tokio::io::split(client_end);
    let t = StdioTransport::from_io(r, w);
    drop(server_end);
    let e = Client::connect(t, options(2000)).await.unwrap_err();
    assert!(matches!(e, ClientError::Closed | ClientError::Io(_)), "{e:?}");
}

#[tokio::test]
async fn rejects_an_unknown_protocol_version() {
    let (client_end, server_end) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(client_end);
    let (sr, mut sw) = tokio::io::split(server_end);
    tokio::spawn(async move {
        let mut lines = BufReader::new(sr).lines();
        let msg: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let out = crate::rpc_ok(
            msg["id"].clone(),
            json!({"protocolVersion": "1999-01-01", "capabilities": {}, "serverInfo": {"name": "x", "version": "1"}}),
        );
        sw.write_all(format!("{out}\n").as_bytes()).await.unwrap();
        // Keep the pipe open.
        while lines.next_line().await.ok().flatten().is_some() {}
    });
    let e = Client::connect(StdioTransport::from_io(r, w), options(2000)).await.unwrap_err();
    assert!(matches!(e, ClientError::Protocol(_)), "{e:?}");
}

#[tokio::test]
async fn answers_server_pings_and_refuses_other_server_requests() {
    let (client_end, server_end) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(client_end);
    let (sr, mut sw) = tokio::io::split(server_end);
    let _t = StdioTransport::from_io(r, w);
    sw.write_all(
        b"{\"jsonrpc\":\"2.0\",\"id\":\"a\",\"method\":\"ping\"}\n{\"jsonrpc\":\"2.0\",\"id\":\"b\",\"method\":\"sampling/createMessage\"}\n",
    )
    .await
    .unwrap();
    let mut lines = BufReader::new(sr).lines();
    let a: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    let b: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!((a["id"].clone(), a["result"].clone()), (json!("a"), json!({})));
    assert_eq!((b["id"].clone(), b["error"]["code"].clone()), (json!("b"), json!(crate::codes::METHOD_NOT_FOUND)));
}

#[tokio::test]
async fn an_oversized_line_is_skipped_without_buffering_and_later_lines_still_work() {
    let (client_end, server_end) = tokio::io::duplex(1024);
    let (r, w) = tokio::io::split(client_end);
    let (sr, mut sw) = tokio::io::split(server_end);
    // Answers the first request (id 1) after sending a 100 KB junk line; limit is 1 KB.
    tokio::spawn(async move {
        let mut lines = BufReader::new(sr).lines();
        let _ = lines.next_line().await;
        let junk = "x".repeat(100_000);
        sw.write_all(format!("{junk}\n").as_bytes()).await.unwrap();
        sw.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"serverInfo\":{\"name\":\"ok\",\"version\":\"1\"}}}\n").await.unwrap();
        while lines.next_line().await.ok().flatten().is_some() {}
    });
    let t = StdioTransport::from_io_limited(r, w, 1024);
    let client = Client::connect(t, options(3000)).await.unwrap();
    assert_eq!(client.hello().name, "ok");
}

#[tokio::test]
async fn an_oversized_response_times_out_instead_of_exhausting_memory() {
    let (client_end, server_end) = tokio::io::duplex(1024);
    let (r, w) = tokio::io::split(client_end);
    let (sr, mut sw) = tokio::io::split(server_end);
    tokio::spawn(async move {
        let mut lines = BufReader::new(sr).lines();
        let _ = lines.next_line().await;
        let big = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"pad\":\"{}\"}}}}\n", "y".repeat(50_000));
        sw.write_all(big.as_bytes()).await.unwrap();
        while lines.next_line().await.ok().flatten().is_some() {}
    });
    let e = Client::connect(StdioTransport::from_io_limited(r, w, 1024), options(300)).await.unwrap_err();
    assert_eq!(e, ClientError::Timeout);
}

#[test]
fn secrets_are_redacted_and_the_command_warns() {
    let cmd = StdioCommand::new("npx").arg("-y").arg("some-server").env("API_KEY", Secret::new("s3cret"));
    assert!(!format!("{cmd:?}").contains("s3cret"));
    assert!(!cmd.display().contains("s3cret"));
    assert_eq!(cmd.display(), "npx -y some-server");
    assert!(cmd.warning().contains("local command") && cmd.warning().contains("npx -y some-server"));
    assert_eq!(Secret::new("x").expose(), "x");
}

#[tokio::test]
async fn spawning_a_missing_command_fails_cleanly() {
    let e = StdioTransport::spawn(&StdioCommand::new("/nonexistent/weirdnet-mcp-test")).unwrap_err();
    assert!(matches!(e, ClientError::Spawn(_)), "{e:?}");
}

#[tokio::test]
async fn a_command_that_exits_reports_closed() {
    let t = StdioTransport::spawn(&StdioCommand::new("true")).unwrap();
    let e = Client::connect(t, options(3000)).await.unwrap_err();
    assert!(matches!(e, ClientError::Closed | ClientError::Io(_)), "{e:?}");
}

#[tokio::test]
async fn talks_to_a_real_child_process_with_secret_env() {
    // A tiny shell "server": answers initialize (id 1), swallows the initialized notification and
    // answers tools/list (id 2) with a tool named after $MCP_NAME.
    let script = r#"read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"1"}}}'
read l; read l; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"'"$MCP_NAME"'","inputSchema":{"type":"object"}}]}}'
cat >/dev/null"#;
    let cmd = StdioCommand::new("sh").arg("-c").arg(script).env("MCP_NAME", Secret::new("from_env"));
    let client = Client::connect(StdioTransport::spawn(&cmd).unwrap(), options(5000)).await.unwrap();
    assert_eq!(client.hello().name, "sh");
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["from_env"]);
    client.close().await;
    assert_eq!(client.ping().await, Err(ClientError::Closed));
}

// ---- streamable HTTP, against a loopback mock (127.0.0.1, in this process) ----

mod http {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Json,
        Sse,
        Unauthorized,
    }

    #[derive(Clone, Default)]
    struct Seen {
        /// (method, lower-cased headers) of every request.
        reqs: Arc<Mutex<Vec<(String, String, Value)>>>,
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String, Value)> {
        let mut data = Vec::new();
        let mut tmp = [0u8; 4096];
        let head_end = loop {
            if let Some(p) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break p;
            }
            let n = sock.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            data.extend_from_slice(&tmp[..n]);
        };
        let head = String::from_utf8_lossy(&data[..head_end]).to_lowercase();
        let len = head.lines().find_map(|l| l.strip_prefix("content-length: ")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        while data.len() < head_end + 4 + len {
            let n = sock.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            data.extend_from_slice(&tmp[..n]);
        }
        let body: Value = serde_json::from_slice(&data[head_end + 4..head_end + 4 + len]).unwrap_or(Value::Null);
        let method = head.split_whitespace().next()?.to_string();
        Some((method, head, body))
    }

    async fn serve(mode: Mode, seen: Seen) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let server = Arc::new(Server::new(
                ServerInfo { name: "http-mock".into(), title: "M".into(), version: "1".into(), instructions: String::new() },
                App,
            ));
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (server, seen) = (server.clone(), seen.clone());
                tokio::spawn(async move {
                    let Some((method, head, body)) = read_request(&mut sock).await else { return };
                    seen.reqs.lock().unwrap().push((method.clone(), head.clone(), body.clone()));
                    let respond = |status: &str, extra: &str, ctype: &str, payload: String| {
                        format!(
                            "HTTP/1.1 {status}\r\n{extra}content-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                            payload.len()
                        )
                    };
                    let out = if mode == Mode::Unauthorized {
                        respond("401 Unauthorized", "", "text/plain", "nope".into())
                    } else if method == "delete" {
                        respond("200 OK", "", "text/plain", String::new())
                    } else {
                        match server.handle(None, body.clone()) {
                            None => respond("202 Accepted", "", "text/plain", String::new()),
                            Some(reply) => {
                                let session = if body["method"] == "initialize" { "mcp-session-id: sess-1\r\n" } else { "" };
                                if mode == Mode::Sse {
                                    let ev = format!(
                                        "event: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\n\nevent: message\ndata: {reply}\n\n"
                                    );
                                    respond("200 OK", session, "text/event-stream", ev)
                                } else {
                                    respond("200 OK", session, "application/json", reply.to_string())
                                }
                            }
                        }
                    };
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn json_responses_session_headers_and_close() {
        let seen = Seen::default();
        let url = serve(Mode::Json, seen.clone()).await;
        let cfg = HttpConfig::new(url).bearer(Secret::new("tok-123")).header("x-extra", Secret::new("e1"));
        let client = Client::connect(HttpTransport::new(&cfg).unwrap(), options(5000)).await.unwrap();
        assert_eq!(client.hello().name, "http-mock");
        assert_eq!(client.list_tools().await.unwrap().len(), 3);
        let r = client.call_tool("echo", json!({"text": "over http"})).await.unwrap();
        assert_eq!(r.structured, Some(json!({"echo": "over http"})));
        client.close().await;
        let reqs = seen.reqs.lock().unwrap().clone();
        assert!(reqs.iter().all(|(_, h, _)| h.contains("authorization: bearer tok-123") && h.contains("x-extra: e1")));
        assert!(reqs.iter().filter(|(m, _, _)| m == "post").all(|(_, h, _)| h.contains("application/json, text/event-stream")));
        // The first request has no session; later ones carry it plus the protocol version.
        assert!(!reqs[0].1.contains("mcp-session-id"));
        assert!(reqs[1..].iter().all(|(_, h, _)| h.contains("mcp-session-id: sess-1")));
        assert!(reqs[1..].iter().all(|(_, h, _)| h.contains("mcp-protocol-version: 2025-06-18")));
        assert_eq!(reqs.last().unwrap().0, "delete");
        assert!(!format!("{cfg:?}").contains("tok-123"));
    }

    #[tokio::test]
    async fn sse_responses_skip_other_events() {
        let url = serve(Mode::Sse, Seen::default()).await;
        let client = Client::connect(HttpTransport::new(&HttpConfig::new(url)).unwrap(), options(5000)).await.unwrap();
        assert_eq!(client.list_tools().await.unwrap().len(), 3);
        assert!(!client.call_tool("echo", json!({"text": "s"})).await.unwrap().is_error);
    }

    #[tokio::test]
    async fn http_errors_are_reported_without_leaking_the_token() {
        let url = serve(Mode::Unauthorized, Seen::default()).await;
        let t = HttpTransport::new(&HttpConfig::new(url).bearer(Secret::new("tok-123"))).unwrap();
        let e = Client::connect(t, options(5000)).await.unwrap_err();
        assert_eq!(e, ClientError::Http { status: 401, body: "nope".into() });
        assert!(!e.to_string().contains("tok-123"));
    }

    #[tokio::test]
    async fn an_unreachable_server_is_an_io_error_and_bad_headers_are_rejected() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", l.local_addr().unwrap());
        drop(l);
        let e = Client::connect(HttpTransport::new(&HttpConfig::new(url)).unwrap(), options(5000)).await.unwrap_err();
        assert!(matches!(e, ClientError::Io(_)), "{e:?}");
        assert!(HttpTransport::new(&HttpConfig::new("http://x").header("bad name", Secret::new("v"))).is_err());
        assert!(HttpTransport::new(&HttpConfig::new("http://x").bearer(Secret::new("a\nb"))).is_err());
    }
}
