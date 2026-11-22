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

// ---- 0.6: legacy output is byte-for-byte what 0.5.3 produced ----

const GOLDEN_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "search",
        description: "Search.",
        params: &[
            Param::required("query", ParamType::String, "Term"),
            Param::optional("n", ParamType::Integer, "N"),
            Param::optional("names", ParamType::StringArray, "Names"),
        ],
        auth: false,
        read_only: true,
        destructive: false,
        open_world: false,
    },
    ToolSpec {
        name: "delete",
        description: "Delete.",
        params: &[Param::required("pkgbase", ParamType::String, "Base")],
        auth: true,
        read_only: false,
        destructive: true,
        open_world: true,
    },
];

struct Golden;

impl Tools for Golden {
    fn specs(&self) -> &[ToolSpec] {
        GOLDEN_SPECS
    }

    fn call(&self, tool: &ToolSpec, a: &Value, token: Option<&str>) -> Result<Value, ToolError> {
        if tool.auth && token != Some("good") {
            return Err(ToolError("invalid credentials or session".into()));
        }
        Ok(match tool.name {
            "search" => json!([args::string(a, "query")?, {"k": 1}]),
            _ => json!({"deleted": args::string(a, "pkgbase")?, "n": 2}),
        })
    }
}

/// Captured from weirdnet-mcp 0.5.3 with the same table and messages.
const GOLDEN: &[&str] = &[
    r#"{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"tools":{"listChanged":false}},"instructions":"Hi.","protocolVersion":"2025-06-18","serverInfo":{"name":"taur","title":"TAUR","version":"1.0.0"}}}"#,
    r#"{"id":2,"jsonrpc":"2.0","result":{"capabilities":{"tools":{"listChanged":false}},"instructions":"Hi.","protocolVersion":"2024-11-05","serverInfo":{"name":"taur","title":"TAUR","version":"1.0.0"}}}"#,
    r#"{"id":3,"jsonrpc":"2.0","result":{}}"#,
    r#"{"id":4,"jsonrpc":"2.0","result":{"tools":[{"annotations":{"destructiveHint":false,"openWorldHint":false,"readOnlyHint":true},"description":"Search.","inputSchema":{"properties":{"n":{"description":"N","type":"integer"},"names":{"description":"Names","items":{"type":"string"},"type":"array"},"query":{"description":"Term","type":"string"}},"required":["query"],"type":"object"},"name":"search"},{"annotations":{"destructiveHint":true,"openWorldHint":true,"readOnlyHint":false},"description":"Delete. Requires a bearer token.","inputSchema":{"properties":{"pkgbase":{"description":"Base","type":"string"}},"required":["pkgbase"],"type":"object"},"name":"delete"}]}}"#,
    r#"{"id":5,"jsonrpc":"2.0","result":{"content":[{"text":"[\n  \"x\",\n  {\n    \"k\": 1\n  }\n]","type":"text"}],"isError":false,"structuredContent":{"result":["x",{"k":1}]}}}"#,
    r#"{"id":"s","jsonrpc":"2.0","result":{"content":[{"text":"{\n  \"deleted\": \"p\",\n  \"n\": 2\n}","type":"text"}],"isError":false,"structuredContent":{"deleted":"p","n":2}}}"#,
    r#"{"id":6,"jsonrpc":"2.0","result":{"content":[{"text":"missing string argument \"query\"","type":"text"}],"isError":true}}"#,
    r#"{"error":{"code":-32602,"message":"unknown tool: nope"},"id":7,"jsonrpc":"2.0"}"#,
    r#"{"error":{"code":-32601,"message":"method not found: resources/list"},"id":8,"jsonrpc":"2.0"}"#,
    r#"{"error":{"code":-32600,"message":"invalid request"},"id":null,"jsonrpc":"2.0"}"#,
    r#"[{"id":9,"jsonrpc":"2.0","result":{}}]"#,
];

#[test]
fn legacy_output_is_unchanged_from_0_5_3() {
    let s = Server::new(ServerInfo { name: "taur".into(), title: "TAUR".into(), version: "1.0.0".into(), instructions: "Hi.".into() }, Golden);
    let msgs = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search","arguments":{"query":"x"}}}),
        json!({"jsonrpc":"2.0","id":"s","method":"tools/call","params":{"name":"delete","arguments":{"pkgbase":"p"}}}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"search"}}),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"nope"}}),
        json!({"jsonrpc":"2.0","id":8,"method":"resources/list"}),
        json!({"jsonrpc":"2.0"}),
        json!([{"jsonrpc":"2.0","id":9,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"}]),
    ];
    assert_eq!(msgs.len(), GOLDEN.len());
    for (i, (m, want)) in msgs.into_iter().zip(GOLDEN).enumerate() {
        let tok = if i == 5 { Some("good") } else { None };
        assert_eq!(s.handle(tok, m.clone()).map(|v| v.to_string()).unwrap_or_default(), *want, "message {i}");
        let async_reply = poll_once(s.handle_async(tok, m));
        assert_eq!(async_reply.map(|v| v.to_string()).unwrap_or_default(), *want, "async message {i}");
    }
}

fn poll_once<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => panic!("pending"),
    }
}

#[test]
fn newest_legacy_version_is_negotiated() {
    let r = server().handle(None, req("initialize", json!({"protocolVersion": "2025-11-25"}))).unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(PROTOCOL_VERSIONS[0], "2025-11-25");
}

// ---- rich input schemas ----

const MESSAGE: &[Param] = &[
    Param::required("role", ParamType::Enum(&["system", "user", "assistant"]), ""),
    Param::required("content", ParamType::Any, "A string or content parts"),
];

const RICH: &[ToolSpec] = &[ToolSpec {
    name: "chat",
    description: "Chat.",
    params: &[
        Param::optional("messages", ParamType::Array(&ParamType::Object(MESSAGE)), "Messages"),
        Param::optional("route", ParamType::Enum(&["bridge", "weirdnet"]), "Route"),
        Param::required("input", ParamType::AnyOf(&[ParamType::String, ParamType::StringArray]), "Input"),
        Param::optional("opts", ParamType::Object(&[Param::optional("temperature", ParamType::Number, "")]), "Options"),
        Param::optional("raw", ParamType::Json(r#"{"type": "integer", "minimum": 1}"#), "Raw"),
    ],
    auth: false,
    read_only: true,
    destructive: false,
    open_world: false,
}];

#[test]
fn object_enum_anyof_and_raw_schemas() {
    let t = RICH[0].to_json();
    let p = &t["inputSchema"]["properties"];
    assert_eq!(
        p["messages"],
        json!({
            "type": "array",
            "description": "Messages",
            "items": {
                "type": "object",
                "properties": {"role": {"type": "string", "enum": ["system", "user", "assistant"]}, "content": {"description": "A string or content parts"}},
                "required": ["role", "content"],
            },
        })
    );
    assert_eq!(p["route"], json!({"type": "string", "enum": ["bridge", "weirdnet"], "description": "Route"}));
    assert_eq!(p["input"], json!({"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}], "description": "Input"}));
    assert_eq!(p["opts"]["properties"]["temperature"], json!({"type": "number"}));
    assert_eq!(p["opts"]["required"], json!([]));
    assert_eq!(p["raw"], json!({"type": "integer", "minimum": 1, "description": "Raw"}));
    assert_eq!(t["inputSchema"]["required"], json!(["input"]));
    assert!(ToolSpec::check(RICH).is_ok());
    assert!(ToolSpec::check(SPECS).is_ok());
}

#[test]
fn tool_table_check_reports_problems() {
    const BAD: &[ToolSpec] = &[
        ToolSpec {
            name: "a",
            description: "",
            params: &[
                Param::required("x", ParamType::Enum(&[]), ""),
                Param::required("x", ParamType::String, ""),
                Param::required("y", ParamType::Array(&ParamType::AnyOf(&[])), ""),
                Param::required("z", ParamType::Json("[1]"), ""),
            ],
            auth: false,
            read_only: true,
            destructive: false,
            open_world: false,
        },
        ToolSpec { name: "a", description: "", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ];
    let problems = ToolSpec::check(BAD).unwrap_err();
    assert_eq!(
        problems,
        vec![
            "a.x: an enum needs at least one value",
            "a.x: duplicate parameter",
            "a.y[]: anyOf needs at least one type",
            "a.z: the raw schema is not a JSON object",
            "a: duplicate tool",
        ]
    );
    assert_eq!(ParamType::Json("not json").schema(), json!({}));
}

// ---- content ----

#[test]
fn image_audio_and_resource_content() {
    let img = Content::image_bytes(b"\x89PNG", "image/png");
    assert_eq!(img.to_json(), json!({"type": "image", "data": "iVBORw==", "mimeType": "image/png"}));
    assert_eq!(img.data_url().unwrap(), "data:image/png;base64,iVBORw==");
    assert_eq!(Content::from_data_url("data:image/png;base64,iVBORw==").unwrap(), img);
    assert_eq!(Content::from_data_url("data:audio/wav;base64,UklG").unwrap(), Content::audio("UklG", "audio/wav"));
    assert!(Content::from_data_url("data:text/plain;base64,AAAA").is_none());
    assert!(Content::from_data_url("data:image/png,raw").is_none());
    assert!(Content::from_data_url("https://example.com/a.png").is_none());
    let mp3 = Content::audio_format("SUQz", "mp3");
    assert_eq!(mp3.to_json(), json!({"type": "audio", "data": "SUQz", "mimeType": "audio/mpeg"}));
    assert_eq!(mp3.format().as_deref(), Some("mp3"));
    assert_eq!(Content::audio_format("UklG", "wav").format().as_deref(), Some("wav"));
    assert_eq!(Content::audio("x", "audio/x-wav").format().as_deref(), Some("wav"));
    assert_eq!(img.format(), None);
    let link = Content::ResourceLink(Resource::new("ymir://models/a", "a").with_mime_type("application/json"));
    assert_eq!(link.to_json(), json!({"type": "resource_link", "uri": "ymir://models/a", "name": "a", "mimeType": "application/json"}));
    let embedded = Content::Resource(ResourceContents::bytes("ymir://x", Some("image/png"), b"\x89PNG"));
    assert_eq!(embedded.to_json(), json!({"type": "resource", "resource": {"uri": "ymir://x", "mimeType": "image/png", "blob": "iVBORw=="}}));
}

#[test]
fn call_tool_result_json_matches_tool_ok() {
    for v in [json!({"a": 1}), json!([1, 2]), json!("s"), json!(null)] {
        assert_eq!(CallToolResult::json(&v).to_json().to_string(), tool_ok(&v).to_string());
        assert_eq!(CallToolResult::from(v.clone()), CallToolResult::json(&v));
    }
    assert_eq!(CallToolResult::error("no").to_json(), json!({"content": [{"type": "text", "text": "no"}], "isError": true}));
    let r = CallToolResult::new(vec![Content::text("hi")]).with(Content::image("AA==", "image/gif")).with_structured(json!({"n": 1}));
    assert_eq!(
        r.to_json(),
        json!({"content": [{"type": "text", "text": "hi"}, {"type": "image", "data": "AA==", "mimeType": "image/gif"}], "structuredContent": {"n": 1}, "isError": false})
    );
}

// ---- async tools and resources ----

struct Async {
    gate: std::sync::Arc<tokio::sync::Notify>,
}

const ASYNC_SPECS: &[ToolSpec] = &[
    ToolSpec { name: "picture", description: "A picture.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ToolSpec { name: "wait", description: "Waits.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
    ToolSpec { name: "fail", description: "Fails.", params: &[], auth: false, read_only: true, destructive: false, open_world: false },
];

impl Tools for Async {
    fn specs(&self) -> &[ToolSpec] {
        ASYNC_SPECS
    }

    async fn call_async<'a>(&'a self, tool: &'a ToolSpec, _args: &'a Value, token: Option<&'a str>) -> Result<CallToolResult, ToolError> {
        match tool.name {
            "picture" => Ok(CallToolResult::new(vec![Content::text(format!("token {token:?}")), Content::image("AA==", "image/png")])),
            "wait" => {
                self.gate.notified().await;
                Ok(CallToolResult::new(vec![Content::text("done")]))
            }
            _ => Err(ToolError("broken".into())),
        }
    }

    fn has_resources(&self) -> bool {
        true
    }

    async fn list_resources<'a>(&'a self, token: Option<&'a str>) -> Result<Vec<Resource>, ResourceError> {
        if token == Some("bad") {
            return Err(ResourceError::Failed("no store".into()));
        }
        Ok(vec![Resource::new("app://models/a", "a").with_title("Model A").with_description("d").with_mime_type("application/json").with_size(3)])
    }

    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate::new("app://models/{name}", "model").with_title("A model").with_description("d").with_mime_type("application/json")]
    }

    async fn read_resource<'a>(&'a self, uri: &'a str, _token: Option<&'a str>) -> Result<Vec<ResourceContents>, ResourceError> {
        match uri {
            "app://models/a" => Ok(vec![ResourceContents::json(uri, &json!({"a": 1}))]),
            "app://text" => Ok(vec![ResourceContents::text(uri, Some("text/plain"), "hi"), ResourceContents::blob(uri, None, "AA==")]),
            _ => Err(ResourceError::NotFound),
        }
    }

    fn cache_hint(&self, method: &str) -> CacheHint {
        if method == "resources/read" { CacheHint::new(60_000, CacheScope::Private) } else { CacheHint::new(300_000, CacheScope::Public) }
    }
}

fn async_server() -> (Server<Async>, std::sync::Arc<tokio::sync::Notify>) {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    (
        Server::new(
            ServerInfo { name: "app".into(), title: String::new(), version: "2".into(), instructions: String::new() },
            Async { gate: gate.clone() },
        ),
        gate,
    )
}

fn modern(method: &str, params: Value) -> Value {
    let mut params = params;
    params["_meta"] = json!({meta::PROTOCOL_VERSION: "2026-07-28", meta::CLIENT_CAPABILITIES: {}, meta::CLIENT_INFO: {"name": "t", "version": "1"}});
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

#[tokio::test]
async fn async_tools_return_images_and_errors() {
    let (s, gate) = async_server();
    let r = s.handle_async(Some("t"), req("tools/call", json!({"name": "picture"}))).await.unwrap();
    assert_eq!(
        r["result"],
        json!({"content": [{"type": "text", "text": "token Some(\"t\")"}, {"type": "image", "data": "AA==", "mimeType": "image/png"}], "isError": false})
    );
    let r = s.handle_async(None, req("tools/call", json!({"name": "fail"}))).await.unwrap();
    assert_eq!(r["result"]["isError"], true);
    // The sync entry point serves async tools that finish without waiting...
    assert_eq!(s.handle(None, req("tools/call", json!({"name": "picture"}))).unwrap()["result"]["content"][1]["type"], "image");
    // ...and says so for one that has to wait.
    let r = s.handle(None, req("tools/call", json!({"name": "wait"}))).unwrap();
    assert_eq!(r["error"]["code"], codes::INTERNAL);
    let (s, gate2) = (std::sync::Arc::new(s), gate);
    let s2 = s.clone();
    let task = tokio::spawn(async move { s2.handle_async(None, req("tools/call", json!({"name": "wait"}))).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    gate2.notify_one();
    assert_eq!(task.await.unwrap().unwrap()["result"]["content"][0]["text"], "done");
}

#[test]
fn a_tool_with_only_call_async_refuses_the_sync_call() {
    struct OnlyAsync;
    impl Tools for OnlyAsync {
        fn specs(&self) -> &[ToolSpec] {
            SPECS
        }
    }
    let e = OnlyAsync.call(&SPECS[0], &json!({}), None).unwrap_err();
    assert_eq!(e.0, "search only runs asynchronously (Server::handle_async)");
}

#[tokio::test]
async fn legacy_resources() {
    let (s, _) = async_server();
    let r = s.handle_async(None, req("initialize", json!({"protocolVersion": "2025-06-18"}))).await.unwrap();
    assert_eq!(r["result"]["capabilities"], json!({"tools": {"listChanged": false}, "resources": {"subscribe": false, "listChanged": false}}));
    assert_eq!(r["result"]["serverInfo"], json!({"name": "app", "title": "", "version": "2"}));
    let r = s.handle_async(None, req("resources/list", json!({}))).await.unwrap();
    assert_eq!(
        r["result"],
        json!({"resources": [{"uri": "app://models/a", "name": "a", "title": "Model A", "description": "d", "mimeType": "application/json", "size": 3}]})
    );
    let r = s.handle_async(Some("bad"), req("resources/list", json!({}))).await.unwrap();
    assert_eq!(r["error"], json!({"code": -32603, "message": "no store"}));
    let r = s.handle_async(None, req("resources/templates/list", json!({}))).await.unwrap();
    assert_eq!(
        r["result"],
        json!({"resourceTemplates": [{"uriTemplate": "app://models/{name}", "name": "model", "title": "A model", "description": "d", "mimeType": "application/json"}]})
    );
    let r = s.handle_async(None, req("resources/read", json!({"uri": "app://models/a"}))).await.unwrap();
    assert_eq!(r["result"], json!({"contents": [{"uri": "app://models/a", "mimeType": "application/json", "text": "{\n  \"a\": 1\n}"}]}));
    let r = s.handle_async(None, req("resources/read", json!({"uri": "app://text"}))).await.unwrap();
    assert_eq!(r["result"]["contents"][1], json!({"uri": "app://text", "blob": "AA=="}));
    let r = s.handle_async(None, req("resources/read", json!({"uri": "app://nope"}))).await.unwrap();
    assert_eq!(r["error"], json!({"code": -32002, "message": "Resource not found", "data": {"uri": "app://nope"}}));
    let r = s.handle_async(None, req("resources/read", json!({}))).await.unwrap();
    assert_eq!(r["error"]["code"], -32602);
    let r = s.handle_async(None, req("resources/subscribe", json!({"uri": "app://models/a"}))).await.unwrap();
    assert_eq!(r["error"]["code"], -32601);
}

// ---- modern (2026-07-28) requests ----

#[tokio::test]
async fn modern_discover_results_and_cache_hints() {
    let (s, _) = async_server();
    let r = s.handle_async(None, modern("server/discover", json!({}))).await.unwrap();
    assert_eq!(
        r["result"],
        json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {"listChanged": false}, "resources": {"subscribe": false, "listChanged": false}},
            "_meta": {meta::SERVER_INFO: {"name": "app", "version": "2"}},
            "ttlMs": 300000,
            "cacheScope": "public",
        })
    );
    // A probe without _meta (stdio era detection) is still answered.
    let r = s.handle_async(None, req("server/discover", json!({}))).await.unwrap();
    assert_eq!(r["result"]["supportedVersions"], json!(["2026-07-28"]));
    let r = s.handle_async(None, modern("tools/list", json!({}))).await.unwrap();
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(r["result"]["ttlMs"], 300000);
    let r = s.handle_async(None, modern("tools/call", json!({"name": "picture"}))).await.unwrap();
    assert_eq!(r["result"]["resultType"], "complete");
    assert!(r["result"].get("ttlMs").is_none());
    assert_eq!(r["result"]["_meta"][meta::SERVER_INFO]["name"], "app");
    let r = s.handle_async(None, modern("resources/read", json!({"uri": "app://models/a"}))).await.unwrap();
    assert_eq!((r["result"]["ttlMs"].clone(), r["result"]["cacheScope"].clone()), (json!(60000), json!("private")));
    let r = s.handle_async(None, modern("resources/read", json!({"uri": "app://nope"}))).await.unwrap();
    assert_eq!(r["error"], json!({"code": -32602, "message": "Resource not found", "data": {"uri": "app://nope"}}));
    // Notifications get nothing; the old ping still answers.
    let mut n = modern("notifications/whatever", json!({}));
    n.as_object_mut().unwrap().remove("id");
    assert!(s.handle_async(None, n).await.is_none());
    assert_eq!(s.handle_async(None, modern("ping", json!({}))).await.unwrap()["result"]["resultType"], "complete");
}

#[test]
fn modern_version_and_meta_errors() {
    let s = server();
    let mut m = modern("tools/list", json!({}));
    m["params"]["_meta"][meta::PROTOCOL_VERSION] = json!("1900-01-01");
    let r = s.handle(None, m).unwrap();
    assert_eq!(
        r["error"],
        json!({"code": -32022, "message": "Unsupported protocol version", "data": {"supported": ["2026-07-28"], "requested": "1900-01-01"}})
    );
    let mut m = modern("tools/list", json!({}));
    m["params"]["_meta"][meta::PROTOCOL_VERSION] = json!("2025-06-18");
    assert_eq!(s.handle(None, m).unwrap()["error"]["code"], -32022);
    let mut m = modern("tools/list", json!({}));
    m["params"]["_meta"].as_object_mut().unwrap().remove(meta::CLIENT_CAPABILITIES);
    let r = s.handle(None, m).unwrap();
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["id"], 1);
    // A server without resources does not offer them in either era.
    assert_eq!(s.handle(None, modern("resources/list", json!({}))).unwrap()["error"]["code"], -32601);
    let r = s.handle(None, modern("server/discover", json!({}))).unwrap();
    assert_eq!(r["result"]["capabilities"], json!({"tools": {"listChanged": false}}));
    assert_eq!(r["result"]["instructions"], "Hi.");
    assert_eq!(r["result"]["_meta"][meta::SERVER_INFO], json!({"name": "taur", "title": "TAUR", "version": "1.0.0"}));
}

#[test]
fn raw_messages_and_blocking_stdio_server() {
    let s = server();
    assert_eq!(s.handle_raw(None, "{nope").unwrap()["error"]["code"], -32700);
    assert_eq!(poll_once(s.handle_raw_async(None, "{nope")).unwrap()["error"]["code"], -32700);
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\nnot json\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"query\":\"q\"}}}".as_slice();
    let mut out = Vec::new();
    stdio::serve_blocking(&s, input, &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[2]["result"]["structuredContent"], json!({"result": ["q"]}));
}

#[test]
fn tools_stays_dyn_compatible() {
    let tools: &dyn Tools = &App;
    assert_eq!(tools.specs().len(), 2);
    assert!(!tools.has_resources());
    assert_eq!(tools.cache_hint("tools/list"), CacheHint::default());
}

// ---- prompts ----

const PROMPTS: &[PromptSpec] = &[
    PromptSpec::new("review", "Reviews code.", &[PromptArg::required("code", "The code"), PromptArg::optional("focus", "")])
        .with_title("Code review"),
    PromptSpec::new("hello", "", &[]),
    PromptSpec::new("secret", "Needs a token.", &[]),
];

struct WithPrompts;

impl Tools for WithPrompts {
    fn specs(&self) -> &[ToolSpec] {
        SPECS
    }

    fn prompts(&self) -> &[PromptSpec] {
        PROMPTS
    }

    async fn get_prompt<'a>(&'a self, prompt: &'a PromptSpec, args: &'a Value, token: Option<&'a str>) -> Result<GetPromptResult, PromptError> {
        match prompt.name {
            "review" => {
                let code = args::string(args, "code").map_err(PromptError::InvalidArguments)?;
                if code == "boom" {
                    return Err(PromptError::Failed("broken".into()));
                }
                Ok(GetPromptResult::new(vec![PromptMessage::user(format!("Review this:\n{code}"))])
                    .with_description("Code review")
                    .with(PromptMessage::assistant(Content::image("AA==", "image/png"))))
            }
            "secret" if token != Some("good") => Err(PromptError::NotFound),
            "secret" => Ok(GetPromptResult::new(vec![PromptMessage::user("s")])),
            _ => Ok(GetPromptResult::default()),
        }
    }
}

fn prompt_server() -> Server<WithPrompts> {
    Server::new(ServerInfo { name: "p".into(), title: String::new(), version: "1".into(), instructions: String::new() }, WithPrompts)
}

#[test]
fn prompt_table_check() {
    assert!(PromptSpec::check(PROMPTS).is_ok());
    const BAD: &[PromptSpec] = &[
        PromptSpec::new("a", "", &[PromptArg::required("x", ""), PromptArg::optional("x", "")]),
        PromptSpec::new("a", "", &[PromptArg::required("", "")]),
        PromptSpec::new("", "", &[]),
    ];
    assert_eq!(
        PromptSpec::check(BAD).unwrap_err(),
        vec!["a.x: duplicate argument", "a: duplicate prompt", "a: an argument has no name", "a prompt has no name"]
    );
}

#[test]
fn prompts_capability_only_when_offered() {
    // Without prompts: no capability and no methods, in either era (unchanged from 0.6.0).
    let s = server();
    let r = s.handle(None, req("initialize", json!({"protocolVersion": "2025-11-25"}))).unwrap();
    assert!(r["result"]["capabilities"].get("prompts").is_none());
    assert_eq!(s.handle(None, req("prompts/list", json!({}))).unwrap()["error"]["code"], -32601);
    assert_eq!(s.handle(None, modern("prompts/get", json!({"name": "x"}))).unwrap()["error"]["code"], -32601);
    // With prompts: advertised in initialize and server/discover.
    let s = prompt_server();
    let r = s.handle(None, req("initialize", json!({"protocolVersion": "2025-06-18"}))).unwrap();
    assert_eq!(r["result"]["capabilities"], json!({"tools": {"listChanged": false}, "prompts": {"listChanged": false}}));
    let r = s.handle(None, modern("server/discover", json!({}))).unwrap();
    assert_eq!(r["result"]["capabilities"]["prompts"], json!({"listChanged": false}));
    let tools: &dyn Tools = &WithPrompts;
    assert_eq!(tools.prompts().len(), 3);
}

#[test]
fn legacy_prompts_list_and_get() {
    let s = prompt_server();
    let r = s.handle(None, req("prompts/list", json!({}))).unwrap();
    assert_eq!(
        r["result"],
        json!({"prompts": [
            {"name": "review", "title": "Code review", "description": "Reviews code.", "arguments": [
                {"name": "code", "description": "The code", "required": true},
                {"name": "focus", "required": false},
            ]},
            {"name": "hello"},
            {"name": "secret", "description": "Needs a token."},
        ]})
    );
    let r = s.handle(None, req("prompts/get", json!({"name": "review", "arguments": {"code": "fn main() {}"}}))).unwrap();
    assert_eq!(
        r["result"],
        json!({"description": "Code review", "messages": [
            {"role": "user", "content": {"type": "text", "text": "Review this:\nfn main() {}"}},
            {"role": "assistant", "content": {"type": "image", "data": "AA==", "mimeType": "image/png"}},
        ]})
    );
    let r = s.handle(None, req("prompts/get", json!({"name": "hello"}))).unwrap();
    assert_eq!(r["result"], json!({"messages": []}));
}

#[test]
fn prompt_errors() {
    let s = prompt_server();
    let err = |params: Value, token: Option<&str>| s.handle(token, req("prompts/get", params)).unwrap()["error"].clone();
    assert_eq!(err(json!({}), None), json!({"code": -32602, "message": "missing string parameter \"name\""}));
    assert_eq!(err(json!({"name": "nope"}), None), json!({"code": -32602, "message": "unknown prompt: nope"}));
    assert_eq!(err(json!({"name": "review"}), None), json!({"code": -32602, "message": "missing required argument \"code\""}));
    assert_eq!(
        err(json!({"name": "review", "arguments": {"code": 1}}), None),
        json!({"code": -32602, "message": "prompt argument \"code\" must be a string"})
    );
    assert_eq!(err(json!({"name": "review", "arguments": ["x"]}), None)["code"], -32602);
    assert_eq!(err(json!({"name": "review", "arguments": {"code": "boom"}}), None), json!({"code": -32603, "message": "broken"}));
    // The app hides a prompt from a caller without a token.
    assert_eq!(err(json!({"name": "secret"}), None), json!({"code": -32602, "message": "unknown prompt: secret"}));
    assert_eq!(s.handle(Some("good"), req("prompts/get", json!({"name": "secret"}))).unwrap()["result"]["messages"][0]["content"]["text"], "s");
}

#[test]
fn modern_prompts_have_result_type_and_cache_hints() {
    let s = prompt_server();
    let r = s.handle(None, modern("prompts/list", json!({}))).unwrap();
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["prompts"].as_array().unwrap().len(), 3);
    assert_eq!((r["result"]["ttlMs"].clone(), r["result"]["cacheScope"].clone()), (json!(0), json!("private")));
    let r = s.handle(None, modern("prompts/get", json!({"name": "review", "arguments": {"code": "x"}}))).unwrap();
    assert_eq!(r["result"]["resultType"], "complete");
    assert!(r["result"].get("ttlMs").is_none());
    assert_eq!(r["result"]["_meta"][meta::SERVER_INFO]["name"], "p");
    assert_eq!(r["result"]["messages"][0]["role"], "user");
}

#[test]
fn a_prompt_table_without_get_prompt_fails_cleanly() {
    struct ListOnly;
    impl Tools for ListOnly {
        fn specs(&self) -> &[ToolSpec] {
            &[]
        }
        fn prompts(&self) -> &[PromptSpec] {
            PROMPTS
        }
    }
    let s = Server::new(ServerInfo { name: "l".into(), title: String::new(), version: "1".into(), instructions: String::new() }, ListOnly);
    let r = s.handle(None, req("prompts/get", json!({"name": "hello"}))).unwrap();
    assert_eq!(r["error"], json!({"code": -32603, "message": "prompt hello is not implemented"}));
}
