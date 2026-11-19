//! [`McpSource`]: an MCP server as a [`weirdnet_agent::ToolSource`] (cargo feature `agent`).
//!
//! MCP annotations map to tiers with the spec's defaults (a tool is not read-only, is
//! destructive when it is not read-only, and is open-world unless it says otherwise):
//!
//! | annotations | tier |
//! |---|---|
//! | destructive (including "not declared") | `Interact` |
//! | read-only, closed world | `Read` |
//! | read-only, open world (including "not declared") | `Research` |
//! | not read-only, not destructive | `Navigate` |
//!
//! A server's hints are untrusted, so each tier is raised to at least
//! [`SourceOptions::tier_floor`] (default `Research`): an outside tool never counts as "reads only
//! what the user attached". The registry can only make tiers stricter still.
//!
//! Local-command servers run with the user's permissions. [`McpSource::warning`] returns the text
//! to show before the server is started; secrets in [`StdioCommand`] and [`HttpConfig`] are
//! [`Secret`](crate::client::Secret)s that belong in a per-machine store.

use async_trait::async_trait;
use serde_json::Value;
use weirdnet_agent::{CallTarget, SourceKind, Tier, ToolAnnotations, ToolContext, ToolOutput, ToolSource};

use crate::client::{Client, ClientError, ClientOptions, HttpConfig, HttpTransport, RemoteAnnotations, RemoteTool, StdioCommand, StdioTransport};

/// How an MCP server's tools are exposed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceOptions {
    /// The lowest tier any of this server's tools can have.
    pub tier_floor: Tier,
    /// Prefix added to every tool name, to keep names unique across sources.
    pub name_prefix: Option<String>,
    /// Tool output longer than this many bytes is cut (at a character boundary) with a note.
    pub max_output_bytes: usize,
}

impl Default for SourceOptions {
    fn default() -> Self {
        Self { tier_floor: Tier::Research, name_prefix: None, max_output_bytes: 256 * 1024 }
    }
}

/// The tier for a tool's annotations, never below `floor`.
pub fn tier_for(annotations: &RemoteAnnotations, floor: Tier) -> Tier {
    Tier::from_annotations(annotations.read_only(), annotations.destructive(), annotations.open_world()).max(floor)
}

/// An MCP server whose tools an agent can use.
#[derive(Debug)]
pub struct McpSource {
    id: String,
    client: Client,
    options: SourceOptions,
    warning: Option<String>,
}

impl McpSource {
    /// Wraps a connected client. `id` names the source (it appears in traces and approvals).
    pub fn new(id: impl Into<String>, client: Client) -> Self {
        Self { id: id.into(), client, options: SourceOptions::default(), warning: None }
    }

    /// Starts a local command and connects to it. Show [`StdioCommand::warning`] to the user
    /// before calling this.
    pub async fn connect_stdio(id: impl Into<String>, command: &StdioCommand, options: ClientOptions) -> Result<Self, ClientError> {
        let client = Client::connect(StdioTransport::spawn(command)?, options).await?;
        let mut source = Self::new(id, client);
        source.warning = Some(command.warning());
        Ok(source)
    }

    /// Connects to a streamable HTTP server.
    pub async fn connect_http(id: impl Into<String>, config: &HttpConfig, options: ClientOptions) -> Result<Self, ClientError> {
        Ok(Self::new(id, Client::connect(HttpTransport::new(config)?, options).await?))
    }

    /// Sets how tools are exposed.
    pub fn with_options(mut self, options: SourceOptions) -> Self {
        self.options = options;
        self
    }

    /// The warning for a local-command server (`None` for HTTP and for wrapped clients).
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// The underlying client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn exposed_name(&self, name: &str) -> String {
        format!("{}{name}", self.options.name_prefix.as_deref().unwrap_or(""))
    }

    fn remote_name<'a>(&self, tool: &'a str) -> Option<&'a str> {
        tool.strip_prefix(self.options.name_prefix.as_deref().unwrap_or(""))
    }

    fn spec(&self, t: &RemoteTool) -> weirdnet_agent::ToolSpec {
        let description = match (&t.title, t.description.is_empty()) {
            (Some(title), true) => title.clone(),
            (Some(title), false) => format!("{title}. {}", t.description),
            (None, _) => t.description.clone(),
        };
        let a = &t.annotations;
        weirdnet_agent::ToolSpec::new(self.exposed_name(&t.name), description)
            .schema(t.input_schema.clone())
            .annotations(ToolAnnotations { read_only: a.read_only(), destructive: a.destructive(), open_world: a.open_world() })
            .tier(tier_for(a, self.options.tier_floor))
    }
}

fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[output truncated]");
    }
    text
}

fn tool_error(e: ClientError) -> weirdnet_agent::ToolError {
    use weirdnet_agent::ToolError;
    match e {
        ClientError::Timeout => ToolError::Timeout,
        ClientError::Cancelled => ToolError::Cancelled,
        ClientError::Rpc { code, message } if code == crate::codes::INVALID_PARAMS => ToolError::InvalidArgs(message),
        other => ToolError::Failed(other.to_string()),
    }
}

#[async_trait]
impl ToolSource for McpSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Mcp
    }

    async fn list(&self) -> Result<Vec<weirdnet_agent::ToolSpec>, weirdnet_agent::ToolError> {
        let tools = self.client.list_tools().await.map_err(tool_error)?;
        Ok(tools.iter().map(|t| self.spec(t)).collect())
    }

    /// Names this server as the provider, so a standing "Always allow" can be limited to it.
    fn target(&self, _tool: &str, _args: &Value) -> CallTarget {
        CallTarget::provider(&self.id)
    }

    async fn call(&self, tool: &str, ctx: &ToolContext, args: Value) -> Result<ToolOutput, weirdnet_agent::ToolError> {
        let name = self.remote_name(tool).ok_or_else(|| weirdnet_agent::ToolError::NotFound(tool.to_string()))?;
        let r = self.client.call_tool_until(name, args, ctx.cancel.cancelled()).await.map_err(tool_error)?;
        let content = truncate(r.text, self.options.max_output_bytes);
        Ok(if r.is_error { ToolOutput::error(content) } else { ToolOutput::text(content) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use weirdnet_agent::{AgentId, AgentIdentity, AgentKind, CancelToken, SessionMark, ToolError, ToolRegistry};

    use super::*;
    use crate::client::tests::{Mock, options};

    fn ctx(cancel: CancelToken) -> ToolContext {
        ToolContext {
            agent: AgentIdentity { id: AgentId::new("atlas"), name: "Atlas".into(), kind: AgentKind::Local, node: None },
            task_id: "t1".into(),
            session: SessionMark::LoggedOut,
            cancel,
        }
    }

    async fn source(mock: &Mock, opts: SourceOptions) -> McpSource {
        McpSource::new("mock-server", Client::connect(mock.transport(), options(2000)).await.unwrap()).with_options(opts)
    }

    #[test]
    fn tiers_follow_the_spec_defaults_and_the_floor() {
        let a = |ro, de, ow| RemoteAnnotations { read_only: ro, destructive: de, open_world: ow };
        let floor = Tier::Read;
        assert_eq!(tier_for(&a(Some(true), Some(false), Some(false)), floor), Tier::Read);
        assert_eq!(tier_for(&a(Some(true), None, Some(true)), floor), Tier::Research);
        assert_eq!(tier_for(&a(Some(true), None, None), floor), Tier::Research);
        assert_eq!(tier_for(&a(Some(false), Some(false), Some(false)), floor), Tier::Navigate);
        assert_eq!(tier_for(&a(Some(false), Some(true), Some(false)), floor), Tier::Interact);
        assert_eq!(tier_for(&RemoteAnnotations::default(), floor), Tier::Interact);
        // A read-only tool never counts as destructive, whatever the server also says.
        assert_eq!(tier_for(&a(Some(true), Some(true), Some(false)), floor), Tier::Read);
        // The default floor lifts everything below Research; it never lowers a tier.
        assert_eq!(tier_for(&a(Some(true), Some(false), Some(false)), Tier::Research), Tier::Research);
        assert_eq!(tier_for(&a(Some(false), Some(false), Some(false)), Tier::Research), Tier::Research);
        assert_eq!(tier_for(&RemoteAnnotations::default(), Tier::Research), Tier::Interact);
    }

    #[tokio::test]
    async fn lists_specs_with_tiers_schema_and_kind() {
        let s = source(&Mock::default(), SourceOptions::default()).await;
        assert_eq!((s.id(), s.kind()), ("mock-server", SourceKind::Mcp));
        assert_eq!(s.warning(), None);
        let specs = s.list().await.unwrap();
        let by = |n: &str| specs.iter().find(|t| t.name == n).unwrap();
        assert_eq!(by("echo").tier, Tier::Research);
        assert_eq!(by("echo").annotations, ToolAnnotations { read_only: true, destructive: false, open_world: false });
        assert_eq!(by("echo").input_schema["required"], json!(["text"]));
        assert_eq!(by("delete").tier, Tier::Interact);
        assert_eq!(by("hang").tier, Tier::Research);
        let strict = source(&Mock::default(), SourceOptions { tier_floor: Tier::Read, ..SourceOptions::default() }).await;
        assert_eq!(strict.list().await.unwrap().iter().find(|t| t.name == "echo").unwrap().tier, Tier::Read);
    }

    #[tokio::test]
    async fn calls_map_results_and_errors() {
        let s = source(&Mock::default(), SourceOptions::default()).await;
        let c = ctx(CancelToken::new());
        let ok = s.call("echo", &c, json!({"text": "hello"})).await.unwrap();
        assert!(!ok.is_error && ok.content.contains("hello"));
        let failed = s.call("delete", &c, json!({})).await.unwrap();
        assert!(failed.is_error);
        assert_eq!(failed.content, "refused");
        assert!(matches!(s.call("nope", &c, json!({})).await, Err(ToolError::InvalidArgs(m)) if m.contains("nope")));
        assert_eq!(s.target("echo", &json!({})), CallTarget::provider("mock-server"));
    }

    #[tokio::test]
    async fn prefix_is_added_to_names_and_stripped_for_calls() {
        let s = source(&Mock::default(), SourceOptions { name_prefix: Some("m_".into()), ..SourceOptions::default() }).await;
        assert!(s.list().await.unwrap().iter().all(|t| t.name.starts_with("m_")));
        let c = ctx(CancelToken::new());
        assert!(s.call("m_echo", &c, json!({"text": "x"})).await.is_ok());
        assert_eq!(s.call("echo", &c, json!({"text": "x"})).await.unwrap_err(), ToolError::NotFound("echo".into()));
    }

    #[tokio::test]
    async fn the_agents_cancel_token_stops_a_running_call() {
        let mock = Mock::default();
        let s = source(&mock, SourceOptions::default()).await;
        let cancel = CancelToken::new();
        let c = ctx(cancel.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        assert_eq!(s.call("hang", &c, json!({})).await.unwrap_err(), ToolError::Cancelled);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mock.cancelled.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_slow_call_times_out() {
        let client = Client::connect(Mock::default().transport(), options(100)).await.unwrap();
        let s = McpSource::new("m", client);
        assert_eq!(s.call("hang", &ctx(CancelToken::new()), json!({})).await.unwrap_err(), ToolError::Timeout);
    }

    #[tokio::test]
    async fn works_in_a_registry() {
        let mut reg = ToolRegistry::new();
        reg.add_source(Arc::new(source(&Mock::default(), SourceOptions::default()).await));
        reg.refresh().await.unwrap();
        assert_eq!(reg.specs().len(), 3);
        let (src, spec) = reg.lookup("delete").unwrap();
        assert_eq!((src.id(), spec.tier), ("mock-server", Tier::Interact));
        assert!(reg.set_tier_override("echo", Tier::Interact).is_ok());
    }

    #[test]
    fn truncates_on_a_character_boundary() {
        assert_eq!(truncate("héllo".into(), 2), "h\n[output truncated]");
        assert_eq!(truncate("short".into(), 100), "short");
    }

    #[tokio::test]
    async fn local_command_sources_carry_the_warning() {
        let script = r#"read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"1"}}}'
cat >/dev/null"#;
        let cmd = StdioCommand::new("sh").arg("-c").arg(script);
        let s = McpSource::connect_stdio("local-sh", &cmd, options(5000)).await.unwrap();
        assert!(s.warning().unwrap().contains("local command"));
        assert_eq!(s.client().hello().name, "sh");
    }
}
