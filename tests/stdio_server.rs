//! The tokio stdio server against scripted clients (and the crate's own client).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use weirdnet_mcp::stdio::{StdioOptions, serve_with};
use weirdnet_mcp::{CallToolResult, Content, Param, ParamType, Server, ServerInfo, ToolError, ToolSpec, Tools, args, meta};

const SPECS: &[ToolSpec] = &[
    ToolSpec { name: "slow", description: "Sleeps.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ToolSpec {
        name: "echo",
        description: "Echoes.",
        params: &[Param::required("text", ParamType::String, "Text")],
        auth: false,
        read_only: true,
        destructive: false,
        open_world: false,
    },
    ToolSpec { name: "picture", description: "An image.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ToolSpec { name: "whoami", description: "The token.", params: &[], auth: true, read_only: true, destructive: false, open_world: false },
];

struct App;

impl Tools for App {
    fn specs(&self) -> &[ToolSpec] {
        SPECS
    }

    async fn call_async<'a>(&'a self, tool: &'a ToolSpec, a: &'a Value, token: Option<&'a str>) -> Result<CallToolResult, ToolError> {
        match tool.name {
            "slow" => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(CallToolResult::new(vec![Content::text("slept")]))
            }
            "echo" => Ok(CallToolResult::json(&json!({"echo": args::string(a, "text")?}))),
            "picture" => Ok(CallToolResult::new(vec![Content::image_bytes(b"GIF89a", "image/gif"), Content::audio_format("UklG", "wav")])),
            _ => Ok(CallToolResult::json(&json!({"token": token}))),
        }
    }
}

fn server() -> Arc<Server<App>> {
    Arc::new(Server::new(ServerInfo { name: "t".into(), title: "T".into(), version: "1".into(), instructions: String::new() }, App))
}

struct Harness {
    to_server: DuplexStream,
    from_server: tokio::io::Lines<BufReader<DuplexStream>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

fn start(options: StdioOptions) -> Harness {
    let (client_w, server_r) = tokio::io::duplex(1 << 20);
    let (server_w, client_r) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(serve_with(server(), options, server_r, server_w));
    Harness { to_server: client_w, from_server: BufReader::new(client_r).lines(), task }
}

impl Harness {
    async fn send(&mut self, v: &Value) {
        self.to_server.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.from_server.next_line()).await.unwrap().unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// Closes the input and returns whatever else the server writes before it stops.
    async fn finish(mut self) -> Vec<Value> {
        drop(self.to_server);
        let mut rest = vec![];
        while let Ok(Ok(Some(line))) = tokio::time::timeout(Duration::from_secs(5), self.from_server.next_line()).await {
            rest.push(serde_json::from_str(&line).unwrap());
        }
        tokio::time::timeout(Duration::from_secs(5), self.task).await.unwrap().unwrap().unwrap();
        rest
    }
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

#[tokio::test]
async fn requests_run_concurrently() {
    let mut h = start(StdioOptions::default());
    h.send(&call(1, "slow", json!({}))).await;
    h.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})).await;
    assert_eq!(h.recv().await, json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
    let slow = h.recv().await;
    assert_eq!((slow["id"].clone(), slow["result"]["content"][0]["text"].clone()), (json!(1), json!("slept")));
    assert!(h.finish().await.is_empty());
}

#[tokio::test]
async fn cancelled_requests_get_no_reply() {
    let mut h = start(StdioOptions::default());
    h.send(&call(3, "slow", json!({}))).await;
    h.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 3, "reason": "user"}})).await;
    h.send(&call(4, "echo", json!({"text": "after"}))).await;
    let r = h.recv().await;
    assert_eq!(r["id"], 4);
    assert_eq!(r["result"]["structuredContent"], json!({"echo": "after"}));
    // Input ends: the server stops without ever answering request 3.
    assert!(h.finish().await.is_empty());
}

#[tokio::test]
async fn images_audio_tokens_batches_and_modern_requests() {
    let mut h = start(StdioOptions::default().token("stdio-token"));
    h.send(&call(5, "picture", json!({}))).await;
    assert_eq!(
        h.recv().await["result"]["content"],
        json!([{"type": "image", "data": "R0lGODlh", "mimeType": "image/gif"}, {"type": "audio", "data": "UklG", "mimeType": "audio/wav"}])
    );
    h.send(&call(6, "whoami", json!({}))).await;
    assert_eq!(h.recv().await["result"]["structuredContent"], json!({"token": "stdio-token"}));
    h.send(&json!([{"jsonrpc": "2.0", "id": 7, "method": "ping"}, {"jsonrpc": "2.0", "method": "notifications/initialized"}])).await;
    assert_eq!(h.recv().await, json!([{"jsonrpc": "2.0", "id": 7, "result": {}}]));
    h.send(&json!({"jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": {"_meta": {meta::PROTOCOL_VERSION: "2026-07-28", meta::CLIENT_CAPABILITIES: {}}}}))
        .await;
    let r = h.recv().await;
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 4);
    h.send(&json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {"_meta": {meta::PROTOCOL_VERSION: "3000-01-01", meta::CLIENT_CAPABILITIES: {}}}}))
        .await;
    assert_eq!(h.recv().await["error"]["code"], -32022);
    assert!(h.finish().await.is_empty());
}

#[tokio::test]
async fn bad_json_and_oversized_lines() {
    let mut h = start(StdioOptions::default().max_message(256));
    h.to_server.write_all(b"this is not json\n").await.unwrap();
    assert_eq!(h.recv().await["error"]["code"], -32700);
    h.to_server.write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{}\"}}\n", "x".repeat(1000)).as_bytes()).await.unwrap();
    let r = h.recv().await;
    assert_eq!(r["error"]["code"], -32700);
    assert!(r["error"]["message"].as_str().unwrap().contains("longer than 256 bytes"));
    // The stream is still usable after a discarded line.
    h.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})).await;
    assert_eq!(h.recv().await["id"], 2);
    // A final request without a trailing newline is still answered.
    h.to_server.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}").await.unwrap();
    let rest = h.finish().await;
    assert_eq!(rest, vec![json!({"jsonrpc": "2.0", "id": 3, "result": {}})]);
}

#[cfg(feature = "client")]
#[tokio::test]
async fn the_crate_client_talks_to_the_stdio_server() {
    use weirdnet_mcp::client::{Client, ClientOptions, StdioTransport};
    let (client_w, server_r) = tokio::io::duplex(1 << 20);
    let (server_w, client_r) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(serve_with(server(), StdioOptions::default(), server_r, server_w));
    let client = Client::connect(StdioTransport::from_io(client_r, client_w), ClientOptions::default()).await.unwrap();
    assert_eq!(client.hello().name, "t");
    assert_eq!(client.list_tools().await.unwrap().len(), 4);
    let r = client.call_tool("echo", json!({"text": "hi"})).await.unwrap();
    assert_eq!(r.structured, Some(json!({"echo": "hi"})));
    client.close().await;
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
}
