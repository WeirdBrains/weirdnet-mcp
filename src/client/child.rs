//! The stdio transport: newline-delimited JSON-RPC over a child's stdin and stdout.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::{ClientError, Secret, Transport};

/// A local command that speaks MCP on stdio.
#[derive(Clone, Debug)]
pub struct StdioCommand {
    /// Program to run.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Extra environment variables (values are secrets).
    pub env: Vec<(String, Secret)>,
    /// Working directory.
    pub cwd: Option<PathBuf>,
}

impl StdioCommand {
    /// A command with no arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self { program: program.into(), args: Vec::new(), env: Vec::new(), cwd: None }
    }

    /// Adds an argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Adds an environment variable.
    pub fn env(mut self, key: impl Into<String>, value: Secret) -> Self {
        self.env.push((key.into(), value));
        self
    }

    /// Sets the working directory.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// The command line for display (program and arguments, never environment values).
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str()).chain(self.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
    }

    /// The warning to show the user before this server is started.
    pub fn warning(&self) -> String {
        format!("This MCP server is a local command and runs with your permissions: {}", self.display())
    }
}

/// The longest line (one JSON message) the stdio transport accepts by default: 4 MiB.
pub const DEFAULT_MAX_LINE: usize = 4 * 1024 * 1024;

/// One line from the server, or a note that it was too long and was discarded.
enum Line {
    Text(Vec<u8>),
    TooLong,
}

/// Reads up to the next `\n`, never holding more than `max` bytes: a longer line is skipped to its
/// end and reported as [`Line::TooLong`]. `None` at end of input.
async fn read_line_capped<R: AsyncBufReadExt + Unpin>(reader: &mut R, max: usize) -> std::io::Result<Option<Line>> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok((too_long || !line.is_empty()).then_some(if too_long { Line::TooLong } else { Line::Text(line) }));
        }
        let (chunk, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (buf.len(), false),
        };
        if !too_long {
            if line.len() + chunk > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(&buf[..chunk]);
            }
        }
        reader.consume(chunk + usize::from(done));
        if done {
            return Ok(Some(if too_long { Line::TooLong } else { Line::Text(line) }));
        }
    }
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, ClientError>>>>;
type Writer = tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>;

struct Shared {
    pending: Pending,
    closed: AtomicBool,
    writer: Writer,
}

impl Shared {
    async fn send(&self, msg: &Value) -> Result<(), ClientError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(line.as_bytes()).await.map_err(|e| ClientError::Io(e.to_string()))?;
        w.flush().await.map_err(|e| ClientError::Io(e.to_string()))
    }

    fn fail_all(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let drained: Vec<_> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        for (_, tx) in drained {
            let _ = tx.send(Err(ClientError::Closed));
        }
    }

    async fn dispatch(&self, msg: Value) {
        if let Value::Array(batch) = msg {
            for m in batch {
                Box::pin(self.dispatch(m)).await;
            }
            return;
        }
        let id = msg.get("id").cloned();
        match (msg.get("method").and_then(Value::as_str), id) {
            // A response to one of our requests.
            (None, Some(id)) => {
                if let Some(tx) = id.as_u64().and_then(|n| self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&n)) {
                    let _ = tx.send(Ok(msg));
                }
            }
            // A request from the server. We offer no client capabilities, so only ping works.
            (Some(method), Some(id)) => {
                let reply = if method == "ping" {
                    json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": crate::codes::METHOD_NOT_FOUND, "message": "method not found"}})
                };
                let _ = self.send(&reply).await;
            }
            // Notifications (progress, log, list_changed) are ignored.
            _ => {}
        }
    }
}

/// MCP over a byte stream, one JSON message per line.
pub struct StdioTransport {
    shared: Arc<Shared>,
    reader: JoinHandle<()>,
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioTransport").finish_non_exhaustive()
    }
}

impl StdioTransport {
    /// Starts `command` and talks to its stdin and stdout. Its stderr is discarded. The process
    /// is killed when the transport is dropped.
    pub fn spawn(command: &StdioCommand) -> Result<Self, ClientError> {
        Self::spawn_limited(command, DEFAULT_MAX_LINE)
    }

    /// Like [`spawn`](Self::spawn) with a custom longest-line limit in bytes.
    pub fn spawn_limited(command: &StdioCommand, max_line: usize) -> Result<Self, ClientError> {
        let mut cmd = tokio::process::Command::new(&command.program);
        cmd.args(&command.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        for (k, v) in &command.env {
            cmd.env(k, v.expose());
        }
        if let Some(dir) = &command.cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| ClientError::Spawn(format!("{}: {e}", command.program)))?;
        let stdin = child.stdin.take().ok_or_else(|| ClientError::Spawn("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| ClientError::Spawn("no stdout".into()))?;
        let mut t = Self::from_io_limited(stdout, stdin, max_line);
        t.child = tokio::sync::Mutex::new(Some(child));
        Ok(t)
    }

    /// MCP over any reader and writer (in-memory pipes in tests, or a socket).
    pub fn from_io(reader: impl AsyncRead + Send + Unpin + 'static, writer: impl AsyncWrite + Send + Unpin + 'static) -> Self {
        Self::from_io_limited(reader, writer, DEFAULT_MAX_LINE)
    }

    /// Like [`from_io`](Self::from_io) with a custom longest-line limit. A server line longer than
    /// `max_line` bytes is discarded without being buffered (a request waiting for it then times
    /// out), so a misbehaving server cannot exhaust memory.
    pub fn from_io_limited(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        max_line: usize,
    ) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            writer: tokio::sync::Mutex::new(Box::new(writer)),
        });
        let task = shared.clone();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            while let Ok(Some(line)) = read_line_capped(&mut reader, max_line).await {
                if let Line::Text(bytes) = line
                    && let Ok(msg) = serde_json::from_slice::<Value>(bytes.trim_ascii())
                {
                    task.dispatch(msg).await;
                }
            }
            task.fail_all();
        });
        Self { shared, reader, child: tokio::sync::Mutex::new(None) }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn notify(&self, msg: Value) -> Result<(), ClientError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Closed);
        }
        self.shared.send(&msg).await
    }

    async fn request(&self, id: u64, msg: Value) -> Result<Value, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        if self.shared.closed.load(Ordering::SeqCst) {
            self.forget(id);
            return Err(ClientError::Closed);
        }
        if let Err(e) = self.shared.send(&msg).await {
            self.forget(id);
            return Err(e);
        }
        rx.await.unwrap_or(Err(ClientError::Closed))
    }

    fn forget(&self, id: u64) {
        self.shared.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }

    async fn close(&self) {
        let _ = self.shared.writer.lock().await.shutdown().await;
        if let Some(mut child) = self.child.lock().await.take()
            && tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_err()
        {
            let _ = child.kill().await;
        }
        self.shared.fail_all();
    }
}
