//! Text-pattern tool calling for models that cannot use native OpenAI tool_calls.
//!
//! The shell greps the assistant reply for allowed tool invocations, executes
//! them client-side, appends results to the context, and continues.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

use crate::llm::{FunctionCall, ToolCall};

/// Built-in names the executor accepts from text patterns.
pub const BUILTIN_TOOLS: &[&str] = &[
    "shell_exec",
    "read_file",
    "write_file",
    "list_dir",
    "load_skill",
];

pub fn is_allowed_with(name: &str, extra: &[String]) -> bool {
    BUILTIN_TOOLS.iter().any(|t| *t == name)
        || name.starts_with("mcp__")
        || extra.iter().any(|t| t == name)
}

/// Extract tool calls from free-form model text (builtins only).
pub fn extract_tool_calls(text: &str) -> Vec<ToolCall> {
    extract_tool_calls_with(text, &[])
}

/// Extract tool calls, allowing extra MCP-qualified names.
pub fn extract_tool_calls_with(text: &str, extra: &[String]) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    calls.extend(extract_xml_tool_calls(text, extra));
    if calls.is_empty() {
        calls.extend(extract_tool_lines(text, extra));
    }
    if calls.is_empty() {
        calls.extend(extract_json_objects(text, extra));
    }
    calls
}

/// Strip tool-call markup from text so the user sees the prose only.
pub fn strip_tool_markup(text: &str) -> String {
    let xml = xml_re();
    let mut out = xml.replace_all(text, "").to_string();
    let tool_line = tool_line_re();
    out = tool_line
        .replace_all(&out, "")
        .lines()
        .filter(|l| {
            let t = l.trim();
            if t.is_empty() {
                return false;
            }
            // Drop leftover bare JSON tool objects
            if let Ok(v) = serde_json::from_str::<Value>(t) {
                if v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| is_allowed_with(n, &[]))
                {
                    return false;
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n");
    out.trim().to_string()
}

fn xml_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>").expect("xml tool_call regex")
    })
}

fn tool_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^[ \t]*TOOL[ \t]+(\S+)[ \t]*(.*)$").expect("TOOL line regex"))
}

fn extract_xml_tool_calls(text: &str, extra: &[String]) -> Vec<ToolCall> {
    let mut out = Vec::new();
    for cap in xml_re().captures_iter(text) {
        let body = cap.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        if let Some(call) = parse_json_tool(body, extra) {
            out.push(call);
        }
    }
    out
}

fn extract_tool_lines(text: &str, extra: &[String]) -> Vec<ToolCall> {
    let mut out = Vec::new();
    for cap in tool_line_re().captures_iter(text) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if !is_allowed_with(name, extra) {
            continue;
        }
        let rest = cap.get(2).map(|m| m.as_str()).unwrap_or("").trim();
        let args = parse_kv_args(name, rest);
        out.push(make_call(name, args));
    }
    out
}

fn extract_json_objects(text: &str, extra: &[String]) -> Vec<ToolCall> {
    let mut out = Vec::new();
    // Scan for JSON objects that look like tool calls.
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with('{') {
            continue;
        }
        if let Some(call) = parse_json_tool(t, extra) {
            out.push(call);
        }
    }
    out
}

fn parse_json_tool(body: &str, extra: &[String]) -> Option<ToolCall> {
    let v: Value = serde_json::from_str(body).ok()?;
    let name = v.get("name")?.as_str()?;
    if !is_allowed_with(name, extra) {
        return None;
    }
    let args = v.get("arguments").cloned().unwrap_or_else(|| json!({}));
    // Also accept flat {"name","path"} without nested arguments.
    let args = if args.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        let mut flat = v.clone();
        if let Some(obj) = flat.as_object_mut() {
            obj.remove("name");
            obj.remove("tool");
        }
        flat
    } else {
        args
    };
    Some(make_call(name, args))
}

fn parse_kv_args(name: &str, rest: &str) -> Value {
    if rest.is_empty() {
        return default_args(name);
    }
    // Allow a raw JSON object after the tool name.
    if rest.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(rest) {
            return v;
        }
    }
    let mut map = serde_json::Map::new();
    // content= may contain spaces — treat as last key if present.
    if let Some(idx) = rest.find("content=") {
        let before = rest[..idx].trim();
        let content = rest[idx + "content=".len()..].to_string();
        parse_simple_kvs(before, &mut map);
        map.insert("content".into(), Value::String(content));
        return Value::Object(map);
    }
    parse_simple_kvs(rest, &mut map);
    if map.is_empty() {
        // Bare argument: map to the primary parameter for the tool.
        return match name {
            "shell_exec" => json!({"command": rest}),
            "read_file" | "write_file" | "list_dir" => json!({"path": rest}),
            "load_skill" => json!({"name": rest}),
            n if n.starts_with("mcp__") && n.ends_with("__top_commands") => {
                json!({"limit": 10})
            }
            n if n.starts_with("mcp__") && n.ends_with("__record_command") => {
                json!({"command": rest})
            }
            _ => json!({}),
        };
    }
    Value::Object(map)
}

fn parse_simple_kvs(s: &str, map: &mut serde_json::Map<String, Value>) {
    for part in s.split_whitespace() {
        if let Some((k, v)) = part.split_once('=') {
            if !k.is_empty() {
                map.insert(k.to_string(), Value::String(v.to_string()));
            }
        }
    }
}

fn default_args(name: &str) -> Value {
    match name {
        "list_dir" => json!({"path": "."}),
        _ => json!({}),
    }
}

fn make_call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: format!("text_{}", uuid::Uuid::new_v4()),
        kind: "function".into(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: args.to_string(),
        },
    }
}

/// Instructions injected into the system prompt for text-style tool use.
pub fn text_tool_instructions() -> &'static str {
    r#"# Text tool protocol (required)

You do NOT have API function-calling. To use a tool, emit EXACTLY one of these
forms and nothing else invented:

Form A (preferred):
<tool_call>
{"name":"list_dir","arguments":{"path":"/etc/engram"}}
</tool_call>

Form B (one line):
TOOL list_dir path=/etc/engram
TOOL read_file path=/etc/os-release
TOOL shell_exec command=uname -a
TOOL write_file path=/tmp/x content=hello
TOOL load_skill name=shell

Allowed built-in tool names: shell_exec, read_file, write_file, list_dir, load_skill.
MCP tools use names like mcp__cmdfreq__top_commands (when connected).
To list the user's top 10 commands:
TOOL mcp__cmdfreq__top_commands {"limit":10}
After you emit a tool call, stop and wait for a tool result message.
When answering without tools, write plain text with no TOOL / tool_call markup.
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xml() {
        let text = r#"Sure.
<tool_call>
{"name":"list_dir","arguments":{"path":"/etc"}}
</tool_call>
"#;
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "list_dir");
    }

    #[test]
    fn parses_tool_line() {
        let text = "TOOL read_file path=/etc/os-release\n";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        assert!(calls[0].function.arguments.contains("/etc/os-release"));
    }

    #[test]
    fn rejects_unknown_tools() {
        let text = r#"<tool_call>{"name":"egremain","arguments":{}}</tool_call>"#;
        assert!(extract_tool_calls(text).is_empty());
    }

    #[test]
    fn strips_markup() {
        let text = "Hello\nTOOL list_dir path=/\nWorld";
        let stripped = strip_tool_markup(text);
        assert!(stripped.contains("Hello"));
        assert!(stripped.contains("World"));
        assert!(!stripped.contains("TOOL"));
    }
}
