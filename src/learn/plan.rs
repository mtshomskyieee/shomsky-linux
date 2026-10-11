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
Keep the command short: one pipeline, no repeated stage. Never emit the same
`|` stage more than twice.
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
    let e1 = match parse_plan_json(&raw) {
        Ok(plan) => return Ok(plan),
        Err(e) => e,
    };
    eprintln!("  ⟡ plan was not valid JSON; trying again…");
    let repair = vec![
        ChatMessage::system(PLAN_SYSTEM),
        ChatMessage::user(intent),
        ChatMessage::assistant_text(&raw),
        ChatMessage::user(format!(
            "Your previous reply was not valid JSON ({e1}). Reply with ONLY the JSON object. \
             Use single quotes for globs and words inside command. Escape every backslash as \\\\."
        )),
    ];
    let raw2 = llm
        .chat_text_max(&repair, 512)
        .await
        .context("learn plan LLM retry")?;
    if let Ok(plan) = parse_plan_json(&raw2) {
        return Ok(plan);
    }
    // Fresh shot: the repair turn often copies the broken reply.
    eprintln!("  ⟡ plan still invalid; trying a fresh reply…");
    let fresh = vec![
        ChatMessage::system(PLAN_SYSTEM),
        ChatMessage::user(format!(
            "{intent}\n\nReply with ONLY one JSON object. In command, wrap globs and words in single quotes, not double quotes."
        )),
    ];
    let raw3 = llm
        .chat_text_max(&fresh, 512)
        .await
        .context("learn plan LLM fresh retry")?;
    parse_plan_json(&raw3).with_context(|| format!("plan parse failed after retry; last={raw3}"))
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
    let collapsed = collapse_repetition(raw);
    let json_str = match extract_json_object(&collapsed) {
        Some(s) => s.to_string(),
        None => close_truncated_json(&collapsed),
    };
    let plan = deserialize_plan(&json_str).or_else(|_| {
        let closed = close_truncated_json(&collapsed);
        deserialize_plan(&closed)
    })?;
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
            if let Ok(p) = serde_json::from_str(&repaired) {
                return Ok(p);
            }
            if let Some(quoted) = repair_embedded_quotes(json_str) {
                let quoted = repair_invalid_json_escapes(&quoted);
                if let Ok(p) = serde_json::from_str(&quoted) {
                    return Ok(p);
                }
            }
            let msg = format!("deserialize plan (after escape repair); first error: {e}");
            Err(e).context(msg)
        }
    }
}

/// Escape raw `"` characters inside the command value. Models write
/// `"command": "find -name "*.txt""` and the inner quotes end the string.
fn repair_embedded_quotes(json: &str) -> Option<String> {
    let cmd_at = json.find("\"command\"")?;
    let after_key = &json[cmd_at + "\"command\"".len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let open_rel = after_colon.find('"')?;
    let open = cmd_at + "\"command\"".len() + colon + 1 + open_rel;
    let rat = json.rfind("\"rationale\"")?;
    if rat <= open {
        return None;
    }
    let before = json[..rat].trim_end().trim_end_matches(',').trim_end();
    if !before.ends_with('"') {
        return None;
    }
    let close = before.len() - 1;
    if close <= open {
        return None;
    }
    let interior = &json[open + 1..close];
    let mut escaped = String::with_capacity(interior.len() + 4);
    let mut chars = interior.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            escaped.push('\\');
            if let Some(n) = chars.next() {
                escaped.push(n);
            }
            continue;
        }
        if c == '"' {
            escaped.push_str("\\\"");
        } else {
            escaped.push(c);
        }
    }
    let mut out = String::new();
    out.push_str(&json[..=open]);
    out.push_str(&escaped);
    out.push_str(&json[close..]);
    Some(out)
}

/// Inside JSON strings, turn unknown `\` escapes into `\\` so shell-ish
/// commands (`\;`, `\ `, `\(`, …) still deserialize.
pub(crate) fn repair_invalid_json_escapes(s: &str) -> String {
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

/// Collapse a phrase that repeats three or more times in a row down to one copy.
/// A truncated extra copy at the end of the string is dropped too.
fn collapse_repetition(s: &str) -> String {
    let mut out = s.to_string();
    for _ in 0..8 {
        let Some((start, len, count)) = longest_repeat_run(&out) else {
            break;
        };
        if count < 3 {
            break;
        }
        let phrase = out[start..start + len].to_string();
        let run_end = start + len * count;
        out.replace_range(start + len..run_end, "");
        let after = start + len;
        let rest = &out[after..];
        if !rest.is_empty() && rest.len() < phrase.len() && phrase.starts_with(rest) {
            out.truncate(after);
        }
    }
    out
}

fn longest_repeat_run(s: &str) -> Option<(usize, usize, usize)> {
    let n = s.len();
    let mut best: Option<(usize, usize, usize)> = None;
    let max_len = 80.min(n / 3);
    if max_len < 4 {
        return None;
    }
    let mut len = 4;
    while len <= max_len {
        if !s.is_char_boundary(len) {
            len += 1;
            continue;
        }
        let mut i = 0;
        while i + len * 3 <= n {
            if !s.is_char_boundary(i) || !s.is_char_boundary(i + len) {
                i += 1;
                continue;
            }
            let phrase = &s[i..i + len];
            let mut count = 1;
            while i + (count + 1) * len <= n
                && s.is_char_boundary(i + count * len)
                && s.is_char_boundary(i + (count + 1) * len)
            {
                let next = &s[i + count * len..i + (count + 1) * len];
                if next != phrase {
                    break;
                }
                count += 1;
            }
            if count >= 3 {
                let score = count * len;
                let better = best
                    .map(|(_, bl, bc)| score > bc * bl)
                    .unwrap_or(true);
                if better {
                    best = Some((i, len, count));
                }
                i += count * len;
            } else {
                i += 1;
            }
        }
        len += 1;
    }
    best
}

/// If the model hit the token limit mid-string, close the JSON object.
fn close_truncated_json(s: &str) -> String {
    let start = s.find('{').unwrap_or(0);
    let mut out = s[start..].trim_end().to_string();
    if string_is_unclosed(&out) {
        out.push('"');
    }
    if !out.trim_end().ends_with('}') {
        out.push('}');
    }
    out
}

fn string_is_unclosed(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && in_string {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = !in_string;
        }
        i += 1;
    }
    in_string
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
    fn collapses_repeated_awk_and_closes_truncated_json() {
        let stage = " | awk '{print $1}'";
        let raw = format!(
            "```json\n{{\n  \"command\": \"ls -1{stage} | sort -n{stage}{stage}{stage} | awk '{{print"
        );
        let p = parse_plan_json(&raw).unwrap();
        assert_eq!(p.command.matches("awk").count(), 2);
        assert!(p.command.starts_with("ls -1"));
        assert!(p.command.contains("sort -n"));
        assert!(!p.command.contains("{{print"));
    }

    #[test]
    fn parses_unescaped_quotes_inside_command() {
        let raw = r#"```json
{
  "command": "find -type f -name "*.txt" -exec ls -lh {} \;",
  "rationale": "list text files"
}
```"#;
        let p = parse_plan_json(raw).unwrap();
        assert!(p.command.contains("*.txt"));
        assert!(p.command.contains("find -type f"));
    }

    #[test]
    fn leaves_two_repeats_alone() {
        let raw = r#"{"command":"ls | awk '{print $1}' | awk '{print $1}'","rationale":"ok"}"#;
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.command.matches("awk").count(), 2);
    }

    #[test]
    fn parses_fenced_invalid_escape() {
        let raw = "```json\n{\n  \"command\": \"find / -name 'bin' -exec sort {} \\; > /tmp/runnable.txt\",\n  \"rationale\": \"sort\"\n}\n```";
        let p = parse_plan_json(raw).unwrap();
        assert!(p.command.contains("\\;"));
    }
}
