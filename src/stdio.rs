//! The stdio bridge behind `<app>ctl mcp`: read one JSON-RPC message per line, forward it,
//! write the reply on its own line. Transport failures become a JSON-RPC internal error for
//! requests that have an id; notifications get nothing.

use crate::codes;
use serde_json::json;
use std::fmt::Display;
use std::io::{BufRead, Write};

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
