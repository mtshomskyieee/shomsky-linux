mod fs;
mod shell_detect;
mod shell_exec;
mod shell_interactive;
pub mod text_parse;

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::config::Config;
use crate::llm::ToolCall;
use crate::mcp::McpRegistry;
use crate::modes::Mode;
use crate::skills::SkillStore;

pub use fs::{list_dir, read_file, write_file};
pub use shell_detect::looks_like_shell_command;
pub use shell_exec::shell_exec;
pub use shell_interactive::{is_ignore_llm_command, shell_exec_interactive};
pub use text_parse::{
    extract_tool_calls, extract_tool_calls_with, strip_tool_markup, text_tool_instructions,
};

/// Built-in OpenAI tool schemas (without MCP).
pub fn builtin_tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "shell_exec",
                "description": "Execute a shell command with BusyBox ash and return stdout/stderr.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to execute"
                        }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute or relative path" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write contents to a file, creating parents as needed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List entries in a directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path (default .)" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full markdown body of a skill that is not always-loaded.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name from the index" }
                    },
                    "required": ["name"]
                }
            }
        }),
    ]
}

/// OpenAI tool schemas advertised to the model (builtins ∪ MCP).
pub fn tool_schemas(mcp: Option<&McpRegistry>) -> Vec<Value> {
    let mut schemas = builtin_tool_schemas();
    if let Some(reg) = mcp {
        schemas.extend(reg.openai_schemas());
    }
    schemas
}

pub async fn execute_tool(
    call: &ToolCall,
    mode: Mode,
    cfg: &Config,
    skills: &SkillStore,
    mcp: Option<&McpRegistry>,
    cwd: &mut PathBuf,
) -> Result<String> {
    let args: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| json!({}));

    if McpRegistry::is_mcp_tool(&call.function.name) {
        let result = match mode {
            Mode::Hallucination => String::new(),
            Mode::Agent | Mode::Root => match mcp {
                Some(reg) => reg
                    .call_tool(&call.function.name, &args)
                    .await
                    .unwrap_or_else(|e| format!("error: {e:#}")),
                None => format!(
                    "error: mcp tool '{}' but no MCP servers are connected",
                    call.function.name
                ),
            },
        };
        return Ok(truncate(&result, cfg.tool_output_limit));
    }

    let result = match call.function.name.as_str() {
        "shell_exec" => {
            let original = text_parse::strip_shell_command_quotes(
                args.get("command").and_then(|v| v.as_str()).unwrap_or(""),
            );
            // Models often emit BusyBox `install git` or `apt-get install git`.
            // Rewrite clear package requests; leave real BusyBox install alone.
            let rewritten = crate::packages::package_install_command(&original);
            let command = rewritten.clone().unwrap_or(original);
            match mode {
                Mode::Agent | Mode::Root => {
                    if is_ignore_llm_command(&command, skills.ignore_llm_commands()) {
                        // Do not capture TUI output into the LLM tool loop.
                        let name = shell_interactive::primary_command_name(&command)
                            .unwrap_or("command");
                        format!(
                            "error: '{name}' is an interactive TUI command listed in \
                             ignore_llm_for_these_commands. Type it at the shell prompt \
                             instead of calling shell_exec."
                        )
                    } else {
                        let home = mode.home_dir();
                        let r = shell_exec(&cfg.busybox_ash, &command, cwd, &home).await?;
                        adopt_cwd(cwd, r.cwd);
                        if r.success {
                            crate::packages::record_from_command(
                                &cfg.packages_path,
                                &command,
                            );
                        }
                        match rewritten {
                            Some(cmd) => format!("→ {cmd}\n{}", r.output),
                            None => r.output,
                        }
                    }
                }
                Mode::Hallucination => {
                    // Quiet no-op: hallucination mode prefers the LLM to invent
                    // shell output. If a tool call still arrives, do not warn.
                    // Real process cwd is intentionally left alone.
                    String::new()
                }
            }
        }
        "read_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            match mode {
                Mode::Hallucination => {
                    // Prefer LLM-invented content; if a tool is used, return empty
                    // so the model can continue without leaking "disabled" warnings.
                    let _ = path;
                    String::new()
                }
                _ => read_file(path).await?,
            }
        }
        "write_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            match mode {
                Mode::Agent | Mode::Root => write_file(path, content).await?,
                Mode::Hallucination => String::new(),
            }
        }
        "list_dir" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match mode {
                Mode::Hallucination => String::new(),
                _ => list_dir(path).await?,
            }
        }
        "load_skill" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match skills.get(name) {
                Some(skill) => {
                    format!("# Skill: {}\n\n{}", skill.meta.name, skill.body)
                }
                None => format!("error: unknown skill '{name}'"),
            }
        }
        other => format!("error: unknown tool '{other}'"),
    };

    Ok(truncate(&result, cfg.tool_output_limit))
}

/// Update tracked cwd and process working directory (agent/root only).
pub fn adopt_cwd(cwd: &mut PathBuf, next: PathBuf) {
    if next.as_os_str().is_empty() || next == *cwd {
        return;
    }
    if next.is_dir() {
        std::env::set_var("OLDPWD", cwd.as_os_str());
        let _ = std::env::set_current_dir(&next);
        *cwd = next;
    }
}

/// If this tool call was a real shell_exec, return the command for cmdfreq.
pub fn shell_exec_command(call: &ToolCall) -> Option<String> {
    if call.function.name != "shell_exec" {
        return None;
    }
    let args: Value = serde_json::from_str(&call.function.arguments).ok()?;
    args.get("command")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!(
        "{}\n\n...[truncated, {} of {} bytes]...",
        &s[..end],
        end,
        s.len()
    )
}
