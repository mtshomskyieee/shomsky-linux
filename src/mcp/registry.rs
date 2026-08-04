use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::framing::{read_message, write_message};
use crate::config::McpServerConfig;

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub fn qualify_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicU64,
}

pub struct McpRegistry {
    /// Kept so a dead sidecar can be respawned mid-session.
    configs: HashMap<String, McpServerConfig>,
    sessions: HashMap<String, Mutex<McpSession>>,
    tools: Vec<McpToolInfo>,
    /// Avoid spamming the TTY when a sidecar has died and cannot restart.
    record_warned: AtomicBool,
}

impl McpRegistry {
    /// Spawn configured stdio MCP servers. Network/URL transports are not supported.
    pub async fn connect(servers: &[McpServerConfig]) -> Self {
        let mut configs = HashMap::new();
        let mut sessions = HashMap::new();
        let mut tools = Vec::new();

        for cfg in servers {
            if cfg.command.trim().is_empty() {
                eprintln!(
                    "warning: mcp server '{}': empty command — skipped",
                    cfg.name
                );
                continue;
            }
            // Refuse anything that looks like a network URL transport.
            if cfg.command.contains("://") {
                eprintln!(
                    "warning: mcp server '{}': network transports are disabled — skipped ({})",
                    cfg.name, cfg.command
                );
                continue;
            }
            configs.insert(cfg.name.clone(), cfg.clone());
            match spawn_session(cfg).await {
                Ok((session, listed)) => {
                    for t in listed {
                        tools.push(McpToolInfo {
                            server: cfg.name.clone(),
                            name: t.name,
                            description: t.description,
                            input_schema: t.input_schema,
                        });
                    }
                    eprintln!(
                        "mcp: connected '{}' ({})",
                        cfg.name,
                        qualify_preview(&cfg.name, &tools)
                    );
                    sessions.insert(cfg.name.clone(), Mutex::new(session));
                }
                Err(e) => {
                    eprintln!(
                        "warning: mcp server '{}': {e:#} — continuing without it",
                        cfg.name
                    );
                }
            }
        }

        Self {
            configs,
            sessions,
            tools,
            record_warned: AtomicBool::new(false),
        }
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|t| qualify_tool_name(&t.server, &t.name))
            .collect()
    }

    pub fn openai_schemas(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                let name = qualify_tool_name(&t.server, &t.name);
                let params = if t.input_schema.is_null() {
                    json!({"type": "object", "properties": {}})
                } else {
                    t.input_schema.clone()
                };
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.description,
                        "parameters": params
                    }
                })
            })
            .collect()
    }

    pub fn is_mcp_tool(name: &str) -> bool {
        name.starts_with("mcp__")
    }

    pub async fn call_tool(&self, qualified: &str, args: &Value) -> Result<String> {
        let (server, tool) = parse_qualified(qualified)?;
        match self.call_tool_once(server, tool, args).await {
            Ok(out) => Ok(out),
            Err(e) if is_transport_dead(&e) => {
                self.reconnect(server)
                    .await
                    .with_context(|| format!("mcp server '{server}' died ({e:#}); reconnect failed"))?;
                eprintln!("mcp: reconnected '{server}'");
                self.record_warned.store(false, Ordering::Relaxed);
                self.call_tool_once(server, tool, args).await
            }
            Err(e) => Err(e),
        }
    }

    async fn call_tool_once(&self, server: &str, tool: &str, args: &Value) -> Result<String> {
        let session = self
            .sessions
            .get(server)
            .with_context(|| format!("mcp server '{server}' is not connected"))?;
        let mut guard = session.lock().await;
        let id = guard.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": args
            }
        });
        if let Err(e) = write_message(&mut guard.stdin, &req).await {
            let _ = guard.child.try_wait();
            return Err(e).context(format!("mcp write to '{server}' failed"));
        }
        let resp = match read_message(&mut guard.stdout).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                let _ = guard.child.try_wait();
                bail!("mcp server '{server}' closed while calling tool");
            }
            Err(e) => {
                let _ = guard.child.try_wait();
                return Err(e).context(format!("mcp read from '{server}' failed"));
            }
        };
        if let Some(err) = resp.get("error") {
            bail!("mcp error from {server}/{tool}: {err}");
        }
        let result = resp.get("result").cloned().unwrap_or(json!({}));
        Ok(flatten_tool_result(&result))
    }

    /// Kill a dead/live sidecar and spawn a fresh one in the same session slot.
    async fn reconnect(&self, server: &str) -> Result<()> {
        let cfg = self
            .configs
            .get(server)
            .with_context(|| format!("no config retained for mcp server '{server}'"))?;
        let slot = self
            .sessions
            .get(server)
            .with_context(|| format!("mcp server '{server}' has no session slot to replace"))?;
        let mut guard = slot.lock().await;
        let _ = guard.stdin.shutdown().await;
        let _ = guard.child.kill().await;
        let _ = guard.child.wait().await;
        let (new_session, _listed) = spawn_session(cfg)
            .await
            .with_context(|| format!("respawning mcp server '{server}'"))?;
        *guard = new_session;
        Ok(())
    }

    /// Record via MCP (with auto-reconnect). Returns `true` on success.
    pub async fn record_command_best_effort(&self, command: &str) -> bool {
        let cmd = command.trim();
        if cmd.is_empty() {
            return true;
        }
        let qualified = "mcp__cmdfreq__record_command";
        if !self.tool_names().iter().any(|n| n == qualified) {
            return false;
        }
        let args = json!({ "command": cmd });
        match self.call_tool(qualified, &args).await {
            Ok(_) => true,
            Err(e) => {
                if !self.record_warned.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "warning: cmdfreq MCP unavailable ({e:#}); using local store"
                    );
                }
                false
            }
        }
    }

    pub async fn shutdown(mut self) {
        for (name, session) in self.sessions.drain() {
            let mut guard = session.into_inner();
            let _ = guard.stdin.shutdown().await;
            let _ = guard.child.kill().await;
            let _ = name;
        }
    }
}

struct ListedTool {
    name: String,
    description: String,
    input_schema: Value,
}

async fn spawn_session(cfg: &McpServerConfig) -> Result<(McpSession, Vec<ListedTool>)> {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    // Stay out of the shell's foreground process group so a TTY SIGINT
    // (Ctrl-C / rustyline interrupt) does not kill long-lived MCP sidecars.
    #[cfg(unix)]
    cmd.process_group(0);
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning mcp server '{}'", cfg.name))?;
    let stdin = child.stdin.take().context("mcp child stdin")?;
    let stdout = child.stdout.take().context("mcp child stdout")?;
    let mut session = McpSession {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        next_id: AtomicU64::new(1),
    };

    // initialize
    let init_id = session.next_id.fetch_add(1, Ordering::Relaxed);
    let init = json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "engram-shell",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    write_message(&mut session.stdin, &init).await?;
    let init_resp = read_message(&mut session.stdout)
        .await?
        .context("mcp server closed during initialize")?;
    if init_resp.get("error").is_some() {
        bail!("initialize failed: {init_resp}");
    }

    // notifications/initialized
    let notify = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    write_message(&mut session.stdin, &notify).await?;

    // tools/list
    let list_id = session.next_id.fetch_add(1, Ordering::Relaxed);
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": list_id,
        "method": "tools/list",
        "params": {}
    });
    write_message(&mut session.stdin, &list_req).await?;
    let list_resp = read_message(&mut session.stdout)
        .await?
        .context("mcp server closed during tools/list")?;
    if let Some(err) = list_resp.get("error") {
        bail!("tools/list failed: {err}");
    }
    let mut listed = Vec::new();
    if let Some(arr) = list_resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
    {
        for t in arr {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            listed.push(ListedTool {
                name,
                description,
                input_schema,
            });
        }
    }

    Ok((session, listed))
}

fn is_transport_dead(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(ioe) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                ioe.kind(),
                ErrorKind::BrokenPipe
                    | ErrorKind::ConnectionReset
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionAborted
            ) {
                return true;
            }
        }
    }
    let msg = format!("{err:#}");
    msg.contains("closed while calling") || msg.contains("closed during")
}

fn parse_qualified(qualified: &str) -> Result<(&str, &str)> {
    let rest = qualified
        .strip_prefix("mcp__")
        .context("not an mcp tool name")?;
    let (server, tool) = rest
        .split_once("__")
        .context("malformed mcp tool name (expected mcp__server__tool)")?;
    if server.is_empty() || tool.is_empty() {
        bail!("malformed mcp tool name '{qualified}'");
    }
    Ok((server, tool))
}

fn flatten_tool_result(result: &Value) -> String {
    if let Some(arr) = result.get("content").and_then(|c| c.as_array()) {
        let mut parts = Vec::new();
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            } else {
                parts.push(item.to_string());
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    result.to_string()
}

fn qualify_preview(server: &str, tools: &[McpToolInfo]) -> String {
    let names: Vec<&str> = tools
        .iter()
        .filter(|t| t.server == server)
        .map(|t| t.name.as_str())
        .collect();
    if names.is_empty() {
        "0 tools".into()
    } else {
        format!("{} tools: {}", names.len(), names.join(", "))
    }
}
