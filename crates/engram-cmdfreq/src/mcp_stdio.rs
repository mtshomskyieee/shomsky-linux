//! Minimal MCP JSON-RPC over stdio (Content-Length framing).
//!
//! Speaks initialize / tools/list / tools/call only. No listen sockets.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read as _, Write};
use std::sync::{Arc, Mutex};

use crate::store::{format_top, FrequencyStore, MAX_TOP};
use crate::DEFAULT_TOP_LIMIT;

pub fn run_stdio_server(store_path: std::path::PathBuf) -> Result<()> {
    let store = Arc::new(Mutex::new(FrequencyStore::load(&store_path)?));
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    loop {
        let msg = match read_message(&mut reader)? {
            Some(m) => m,
            None => break,
        };

        // Notifications have no id — acknowledge by ignoring (except initialized).
        let is_notification = msg.get("id").is_none();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        if is_notification {
            // notifications/initialized and others: no response
            let _ = method;
            continue;
        }

        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "engram-cmdfreq",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": "Command frequency tracker. Use top_commands with limit=10 for the user's most-used commands. Use record_command to increment counts."
                }
            }),
            "ping" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),
            "tools/list" => {
                let tools = tool_defs();
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": tools }
                })
            }
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match handle_tool(name, &args, &store, &store_path) {
                    Ok(text) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": text }],
                            "isError": false
                        }
                    }),
                    Err(e) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": format!("error: {e:#}") }],
                            "isError": true
                        }
                    }),
                }
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {other}")
                }
            }),
        };

        write_message(&mut out, &response)?;
    }
    Ok(())
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "record_command",
            "description": "Record that a shell command was executed (increments frequency count).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command line to record"
                    }
                },
                "required": ["command"]
            }
        },
        {
            "name": "top_commands",
            "description": "Return the most frequently used commands. Default limit is 10 (top 10).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "How many commands to return (default 10, max 100)"
                    },
                    "prefix": {
                        "type": "string",
                        "description": "Optional prefix filter (e.g. \"git\")"
                    }
                }
            }
        }
    ])
}

fn handle_tool(
    name: &str,
    args: &Value,
    store: &Arc<Mutex<FrequencyStore>>,
    path: &std::path::Path,
) -> Result<String> {
    match name {
        "record_command" => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if command.trim().is_empty() {
                bail!("command is required");
            }
            let mut guard = store.lock().expect("cmdfreq store lock");
            guard.record(&command);
            guard.save(path)?;
            let count = guard.commands.get(command.trim()).copied().unwrap_or(0);
            Ok(format!("recorded '{}' (count={count})", command.trim()))
        }
        "top_commands" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_TOP_LIMIT)
                .clamp(1, MAX_TOP);
            let prefix = args
                .get("prefix")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Reload from disk so Completer / other writers are visible.
            let mut guard = store.lock().expect("cmdfreq store lock");
            *guard = FrequencyStore::load(path).unwrap_or_default();
            let items = guard.top(limit, &prefix);
            Ok(format_top(&items))
        }
        other => bail!("unknown tool '{other}'"),
    }
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                rest.trim()
                    .parse()
                    .context("invalid Content-Length")?,
            );
        }
        // Ignore other headers (Content-Type, …).
    }

    let len = match content_length {
        Some(l) => l,
        None => {
            // Fallback: try reading a single NDJSON line (already consumed headers wrongly).
            // Without Content-Length we cannot recover; treat as EOF/protocol error.
            bail!("MCP message missing Content-Length header");
        }
    };

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    let v: Value = serde_json::from_slice(&buf).context("parsing MCP JSON body")?;
    Ok(Some(v))
}

fn write_message<W: Write>(out: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()?;
    Ok(())
}
