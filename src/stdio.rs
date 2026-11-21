//! Newline-delimited JSON-RPC over stdio (or any byte stream).
//!
//! - [`serve_blocking`]: a sequential server loop for sync tools, no runtime needed.
//! - `serve` / `serve_with` (cargo feature `stdio-server`): a tokio server loop that runs requests
//!   concurrently (a slow tool does not hold up the next request) and honours
//!   `notifications/cancelled`.
//! - [`bridge`]: the `<app>ctl mcp` loop that forwards each line to a remote server. Transport
//!   failures become a JSON-RPC internal error for requests that have an id; notifications get
//!   nothing.
//!
//! The servers stop when the input ends (the client's shutdown signal), after answering what is
//! in flight. Lines longer than the limit ([`DEFAULT_MAX_MESSAGE`], 32 MiB) are discarded
//! without being buffered and answered with a parse error.

use crate::{Server, Tools, codes, rpc_err};
use serde_json::{Value, json};
use std::fmt::Display;
use std::io::{BufRead, Write};

/// The longest message the stdio servers accept, in bytes.
pub const DEFAULT_MAX_MESSAGE: usize = 32 * 1024 * 1024;

/// Runs the loop until `input` ends. `forward` sends one raw message and returns the raw reply
/// (`None` when the server sends none, e.g. for notifications).
pub fn bridge<R, W, F, E>(input: R, mut output: W, mut forward: F) -> std::io::Result<()>
where
    R: BufRead,
    W: Write,
    F: FnMut(&str) -> Result<Option<String>, E>,
    E: Display,
{
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match forward(&line) {
            Ok(r) => r,
            Err(e) => serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": codes::INTERNAL, "message": e.to_string()}}).to_string()),
        };
        if let Some(r) = reply {
            writeln!(output, "{}", r.trim()).ok();
            output.flush().ok();
        }
    }
    Ok(())
}

fn too_long(max: usize) -> Value {
    rpc_err(Value::Null, codes::PARSE_ERROR, &format!("message longer than {max} bytes"))
}

/// One line from a capped reader: `Ok(None)` at the end, `Ok(Some(None))` for a line over the cap.
fn read_capped_blocking<R: BufRead>(r: &mut R, max: usize) -> std::io::Result<Option<Option<Vec<u8>>>> {
    let (mut buf, mut over, mut any) = (Vec::new(), false, false);
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(any.then(|| (!over).then_some(buf)));
        }
        any = true;
        let (take, done) = chunk.iter().position(|b| *b == b'\n').map_or((chunk.len(), false), |i| (i, true));
        if !over {
            if buf.len() + take > max {
                over = true;
                buf = Vec::new();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        r.consume(if done { take + 1 } else { take });
        if done {
            return Ok(Some((!over).then_some(buf)));
        }
    }
}

/// Serves sync tools on newline-delimited JSON-RPC until `input` ends, one request at a time,
/// with no bearer token. Use `serve` (feature `stdio-server`) for async tools.
pub fn serve_blocking<T, R, W>(server: &Server<T>, mut input: R, mut output: W) -> std::io::Result<()>
where
    T: Tools,
    R: BufRead,
    W: Write,
{
    while let Some(line) = read_capped_blocking(&mut input, DEFAULT_MAX_MESSAGE)? {
        let reply = match line {
            None => Some(too_long(DEFAULT_MAX_MESSAGE)),
            Some(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                if text.trim().is_empty() {
                    continue;
                }
                server.handle_raw(None, &text)
            }
        };
        if let Some(r) = reply {
            writeln!(output, "{r}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(feature = "stdio-server")]
pub use tokio_server::{StdioOptions, serve, serve_with};

#[cfg(feature = "stdio-server")]
mod tokio_server {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

    use super::{DEFAULT_MAX_MESSAGE, too_long};
    use crate::server::Server;
    use crate::{Tools, codes, rpc_err};

    /// Options for [`serve_with`].
    #[derive(Clone, Debug)]
    #[non_exhaustive]
    pub struct StdioOptions {
        /// The bearer token tools see (stdio has no headers; usually `None`).
        pub token: Option<String>,
        /// The longest accepted message in bytes.
        pub max_message: usize,
    }

    impl Default for StdioOptions {
        fn default() -> Self {
            Self { token: None, max_message: DEFAULT_MAX_MESSAGE }
        }
    }

    impl StdioOptions {
        /// Sets the token tools see.
        pub fn token(mut self, token: impl Into<String>) -> Self {
            self.token = Some(token.into());
            self
        }

        /// Sets the longest accepted message.
        pub fn max_message(mut self, bytes: usize) -> Self {
            self.max_message = bytes;
            self
        }
    }

    async fn read_capped<R: AsyncBufRead + Unpin>(r: &mut R, max: usize) -> std::io::Result<Option<Option<Vec<u8>>>> {
        let (mut buf, mut over, mut any) = (Vec::new(), false, false);
        loop {
            let chunk = r.fill_buf().await?;
            if chunk.is_empty() {
                return Ok(any.then(|| (!over).then_some(buf)));
            }
            any = true;
            let (take, done) = chunk.iter().position(|b| *b == b'\n').map_or((chunk.len(), false), |i| (i, true));
            if !over {
                if buf.len() + take > max {
                    over = true;
                    buf = Vec::new();
                } else {
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
            r.consume(if done { take + 1 } else { take });
            if done {
                return Ok(Some((!over).then_some(buf)));
            }
        }
    }

    /// [`serve_with`] with default options.
    pub async fn serve<T, R, W>(server: Arc<Server<T>>, input: R, output: W) -> std::io::Result<()>
    where
        T: Tools + Send + Sync + 'static,
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        serve_with(server, StdioOptions::default(), input, output).await
    }

    /// Serves newline-delimited JSON-RPC until `input` ends and every request in flight has been
    /// answered. Each request runs as its own task, so replies can come back out of order (they
    /// carry their ids). `notifications/cancelled` stops the named request; it gets no reply.
    pub async fn serve_with<T, R, W>(server: Arc<Server<T>>, options: StdioOptions, input: R, mut output: W) -> std::io::Result<()>
    where
        T: Tools + Send + Sync + 'static,
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if output.write_all(line.as_bytes()).await.is_err() || output.flush().await.is_err() {
                    break;
                }
            }
        });
        let token: Option<Arc<str>> = options.token.map(Arc::from);
        let in_flight: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>> = Arc::default();
        let mut tasks = tokio::task::JoinSet::new();
        let mut input = BufReader::new(input);
        while let Some(line) = read_capped(&mut input, options.max_message).await? {
            while tasks.try_join_next().is_some() {}
            let Some(bytes) = line else {
                let _ = tx.send(too_long(options.max_message).to_string());
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            if text.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(rpc_err(Value::Null, codes::PARSE_ERROR, &format!("parse error: {e}")).to_string());
                    continue;
                }
            };
            if msg.get("method").and_then(Value::as_str) == Some("notifications/cancelled") && msg.get("id").is_none() {
                if let Some(handle) = in_flight.lock().unwrap_or_else(|e| e.into_inner()).remove(&msg["params"]["requestId"].to_string()) {
                    handle.abort();
                }
                continue;
            }
            let key = msg.get("method").and(msg.get("id")).map(Value::to_string);
            let (server, tx, token, flights, task_key) = (server.clone(), tx.clone(), token.clone(), in_flight.clone(), key.clone());
            // Holding the lock while spawning means the task cannot finish and remove its entry
            // before the entry exists.
            let mut map = in_flight.lock().unwrap_or_else(|e| e.into_inner());
            let handle = tasks.spawn(async move {
                let reply = server.handle_async(token.as_deref(), msg).await;
                if let Some(k) = task_key {
                    flights.lock().unwrap_or_else(|e| e.into_inner()).remove(&k);
                }
                if let Some(r) = reply {
                    let _ = tx.send(r.to_string());
                }
            });
            if let Some(k) = key {
                map.insert(k, handle);
            }
            drop(map);
        }
        while tasks.join_next().await.is_some() {}
        drop(tx);
        writer.await.ok();
        Ok(())
    }
}
