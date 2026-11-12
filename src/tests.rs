use crate::args;
use crate::*;
use serde_json::{Value, json};

const P: Param = Param::required("pkgbase", ParamType::String, "Package base name");

const SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "search",
        description: "Search.",
        params: &[Param::required("query", ParamType::String, "Term"), Param::optional("names", ParamType::StringArray, "Names")],
        auth: false,
        read_only: true,
        destructive: false,
        open_world: false,
    },
    ToolSpec { name: "delete", description: "Delete.", params: &[P], auth: true, read_only: false, destructive: true, open_world: false },
];

struct App;

impl Tools for App {
    fn specs(&self) -> &[ToolSpec] {
        SPECS
    }

    fn call(&self, tool: &ToolSpec, a: &Value, token: Option<&str>) -> Result<Value, ToolError> {
        if tool.auth && token != Some("good") {
            return Err(ToolError("invalid credentials or session".into()));
        }
        Ok(match tool.name {
            "search" => json!([args::string(a, "query")?]),
            "delete" => json!({"deleted": args::string(a, "pkgbase")?}),
            _ => unreachable!(),
        })
    }
}

fn server() -> Server<App> {
    Server::new(ServerInfo { name: "taur".into(), title: "TAUR".into(), version: "1.0.0".into(), instructions: "Hi.".into() }, App)
}

fn req(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})
}

#[test]
fn initialize_negotiates_versions() {
    let s = server();
    let r = s.handle(None, req("initialize", json!({"protocolVersion": "2025-03-26"}))).unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r["result"]["serverInfo"], json!({"name": "taur", "title": "TAUR", "version": "1.0.0"}));
    assert_eq!(r["result"]["capabilities"], json!({"tools": {"listChanged": false}}));
    let r = s.handle(None, req("initialize", json!({"protocolVersion": "1999-01-01"}))).unwrap();
    assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    assert_eq!(s.handle(None, req("ping", json!({}))).unwrap()["result"], json!({}));
}

#[test]
fn tools_list_has_schemas_and_annotations() {
    let r = server().handle(None, req("tools/list", json!({}))).unwrap();
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
    assert_eq!(tools[0]["inputSchema"]["properties"]["names"], json!({"type": "array", "items": {"type": "string"}, "description": "Names"}));
    assert_eq!(tools[1]["description"], "Delete. Requires a bearer token.");
    assert_eq!(tools[1]["annotations"], json!({"readOnlyHint": false, "destructiveHint": true, "openWorldHint": false}));
}

#[test]
fn tools_call_results_and_errors() {
    let s = server();
    let r = s.handle(None, req("tools/call", json!({"name": "search", "arguments": {"query": "x"}}))).unwrap();
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(r["result"]["structuredContent"], json!({"result": ["x"]}));
    let r = s.handle(Some("good"), req("tools/call", json!({"name": "delete", "arguments": {"pkgbase": "p"}}))).unwrap();
    assert_eq!(r["result"]["structuredContent"], json!({"deleted": "p"}));
    assert_eq!(r["result"]["content"][0]["text"], "{\n  \"deleted\": \"p\"\n}");
    let r = s.handle(None, req("tools/call", json!({"name": "delete", "arguments": {"pkgbase": "p"}}))).unwrap();
    assert_eq!(r["result"], json!({"content": [{"type": "text", "text": "invalid credentials or session"}], "isError": true}));
    let r = s.handle(None, req("tools/call", json!({"name": "search"}))).unwrap();
    assert_eq!(r["result"]["content"][0]["text"], "missing string argument \"query\"");
    let r = s.handle(None, req("tools/call", json!({"name": "nope"}))).unwrap();
    assert_eq!(r["error"], json!({"code": -32602, "message": "unknown tool: nope"}));
    let r = s.handle(None, req("frobnicate", json!({}))).unwrap();
    assert_eq!(r["error"]["code"], -32601);
}

#[test]
fn notifications_batches_and_invalid_requests() {
    let s = server();
    assert!(s.handle(None, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    assert!(s.handle(None, json!({"jsonrpc": "2.0", "id": 1, "result": {}})).is_none());
    assert_eq!(s.handle(None, json!({"jsonrpc": "2.0"})).unwrap(), rpc_err(Value::Null, -32600, "invalid request"));
    let batch = json!([req("ping", json!({})), {"jsonrpc": "2.0", "method": "notifications/initialized"}]);
    assert_eq!(s.handle(None, batch).unwrap().as_array().unwrap().len(), 1);
    assert!(s.handle(None, json!([{"jsonrpc": "2.0", "method": "x"}])).is_none());
}

#[test]
fn argument_helpers() {
    let a = json!({"s": "v", "i": 3, "b": false, "l": ["a", "b"], "bad": [1]});
    assert_eq!(args::string(&a, "s").unwrap(), "v");
    assert_eq!(args::integer(&a, "i").unwrap(), 3);
    assert_eq!(args::integer(&a, "s").unwrap_err(), "missing integer argument \"s\"");
    assert!(!args::boolean(&a, "b", true));
    assert!(args::boolean(&a, "missing", true));
    assert_eq!(args::strings(&a, "l").unwrap(), vec!["a", "b"]);
    assert_eq!(args::strings(&a, "bad").unwrap_err(), "\"bad\" must contain strings");
    assert_eq!(args::strings(&a, "x").unwrap_err(), "missing array argument \"x\"");
    assert_eq!(args::opt_string(&a, "x"), None);
}

#[test]
fn stdio_bridge_forwards_and_reports_transport_errors() {
    let input = b"{\"id\":1,\"method\":\"ping\"}\n\n{\"method\":\"n\"}\n{\"id\":2,\"method\":\"fail\"}\n".as_slice();
    let mut out = Vec::new();
    stdio::bridge(input, &mut out, |line: &str| {
        if line.contains("fail") {
            Err("connection refused")
        } else if line.contains("\"id\"") {
            Ok(Some(" {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}} \n".to_string()))
        } else {
            Ok(None)
        }
    })
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}");
    let err: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(err["error"], json!({"code": -32603, "message": "connection refused"}));
}
