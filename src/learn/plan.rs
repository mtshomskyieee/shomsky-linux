use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::llm::{ChatMessage, LlmClient};
use crate::learn::learn_stub;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub command: String,
    #[serde(default)]
    pub rationale: String,
}

const PLAN_SYSTEM: &str = "\
You are a planning assistant for BusyBox ash on Alpine Linux (Shomsky).
Given a natural-language intent, output ONLY a single JSON object with keys:
  \"command\": a single shell command or pipeline (no markdown, no explanation outside JSON)
  \"rationale\": one short sentence
Prefer portable BusyBox applets. Do not use sudo. Do not write destructive
commands unless the intent clearly requires them. No code fences.
In JSON string values, escape backslashes as \\\\ (e.g. find … -exec … \\\\;).";

pub async fn plan_command(llm: &LlmClient, intent: &str) -> Result<Plan> {
    if learn_stub() {
        return Ok(stub_plan(intent));
    }

    let messages = vec![
        ChatMessage::system(PLAN_SYSTEM),
        ChatMessage::user(intent),
    ];
    let raw = llm
        .chat_text_max(&messages, 512)
        .await
        .context("learn plan LLM")?;
    match parse_plan_json(&raw) {
        Ok(p) => Ok(p),
        Err(e1) => {
            let repair = vec![
                ChatMessage::system(PLAN_SYSTEM),
                ChatMessage::user(intent),
                ChatMessage::assistant_text(&raw),
                ChatMessage::user(format!(
                    "Your previous reply was not valid JSON ({e1}). Reply with ONLY the JSON object. \
                     Escape every backslash in command as \\\\."
                )),
            ];
            let raw2 = llm
                .chat_text_max(&repair, 512)
                .await
                .context("learn plan LLM retry")?;
            parse_plan_json(&raw2)
                .with_context(|| format!("plan parse failed after retry; last={raw2}"))
        }
    }
}

fn stub_plan(intent: &str) -> Plan {
    let lower = intent.to_ascii_lowercase();
    if lower.contains("largest") || lower.contains("biggest") || lower.contains("fat") {
        Plan {
            command: "find . -type f -printf '%s\\t%p\\n' 2>/dev/null | sort -rn | head -5"
                .into(),
            rationale: "list largest files".into(),
        }
    } else if lower.contains("hello") || lower.contains("echo") {
        Plan {
            command: "echo hello-from-learn".into(),
            rationale: "simple echo for smoke tests".into(),
        }
    } else {
        Plan {
            command: "echo hello-from-learn".into(),
            rationale: "stub default echo".into(),
        }
    }
}

pub fn parse_plan_json(raw: &str) -> Result<Plan> {
    let json_str = extract_json_object(raw).ok_or_else(|| anyhow!("no JSON object in reply"))?;
    let plan = deserialize_plan(json_str)?;
    if plan.command.trim().is_empty() {
        bail!("empty command in plan");
    }
    Ok(plan)
}

fn deserialize_plan(json_str: &str) -> Result<Plan> {
    match serde_json::from_str::<Plan>(json_str) {
        Ok(p) => Ok(p),
        Err(e) => {
            // Models often emit shell escapes like `\;` which are invalid JSON.
            let repaired = repair_invalid_json_escapes(json_str);
            serde_json::from_str(&repaired).with_context(|| {
                format!("deserialize plan (after escape repair); first error: {e}")
            })
        }
    }
}

/// Inside JSON strings, turn unknown `\` escapes into `\\` so shell-ish
/// commands (`\;`, `\ `, `\(`, …) still deserialize.
fn repair_invalid_json_escapes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if !in_string {
            out.push(c as char);
            if c == b'"' {
                in_string = true;
            }
            i += 1;
            continue;
        }

        if c == b'"' {
            out.push('"');
            in_string = false;
            i += 1;
            continue;
        }

        if c == b'\\' {
            if i + 1 >= bytes.len() {
                out.push_str("\\\\");
                i += 1;
                continue;
            }
            let next = bytes[i + 1];
            match next {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                    out.push('\\');
                    out.push(next as char);
                    i += 2;
                }
                b'u' => {
                    let hex_ok = i + 5 < bytes.len()
                        && bytes[i + 2..=i + 5]
                            .iter()
                            .all(|b| b.is_ascii_hexdigit());
                    if hex_ok {
                        out.push_str(&s[i..i + 6]);
                        i += 6;
                    } else {
                        out.push_str("\\\\");
                        i += 1;
                    }
                }
                _ => {
                    // e.g. `\;` → `\\;`
                    out.push_str("\\\\");
                    i += 1;
                }
            }
            continue;
        }

        out.push(c as char);
        i += 1;
    }
    out
}

fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&s[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let p = parse_plan_json(r#"{"command":"ls -la","rationale":"list"}"#).unwrap();
        assert_eq!(p.command, "ls -la");
    }

    #[test]
    fn parses_fenced() {
        let raw = "Here:\n```json\n{\"command\":\"pwd\",\"rationale\":\"cwd\"}\n```\n";
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.command, "pwd");
    }

    #[test]
    fn parses_shell_backslash_semicolon() {
        // LLM often writes find -exec … \; without JSON-escaping the backslash.
        let raw = r#"{"command":"find /bin -type f -exec ls {} \;","rationale":"list"}"#;
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.command, "find /bin -type f -exec ls {} \\;");
    }

    #[test]
    fn parses_already_escaped_backslash() {
        let raw = r#"{"command":"find /bin -type f -exec ls {} \\;","rationale":"list"}"#;
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.command, "find /bin -type f -exec ls {} \\;");
    }

    #[test]
    fn parses_fenced_invalid_escape() {
        let raw = "```json\n{\n  \"command\": \"find / -name 'bin' -exec sort {} \\; > /tmp/runnable.txt\",\n  \"rationale\": \"sort\"\n}\n```";
        let p = parse_plan_json(raw).unwrap();
        assert!(p.command.contains("\\;"));
    }
}
