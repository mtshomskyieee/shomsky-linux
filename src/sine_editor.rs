//! SINE file editor: numbered buffer + AI conversation overlay.
//!
//! Opened from the files pane (`e`) or the read-only viewer (`e`). Esc returns
//! to the four-pane SINE screen. See ADR-0014 / docs/sine.md.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::llm::{ChatMessage, LlmClient};
use crate::modes::Mode;
use crate::sine_tui::{default_sine_dir, wrap_numbered_lines, ChatTurn};
use crate::split_frame::{self, Key};
use crate::term::RawGuard;
use crate::ui;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);
const AUTO_SUGGEST_QUIET: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);
const RING_CAP: usize = 16;
const FILE_PROMPT_CHARS: usize = 12_000;
const EDITOR_MAX_TOKENS: u32 = 2048;
const SPLIT_RATIO: f32 = 0.55;

const EDITOR_SYSTEM_MANUAL: &str = "\
You are SINE's file editor assistant. The operator is editing one file.
Reply with ONE JSON object only (no prose outside JSON).

The numbered file in the user message is the ONLY source of truth. Prior chat
claims like \"implemented\" or \"rewrote\" may be wrong — look at the file.

When the operator asks to implement or fix something, rewrite the COMPLETE file
in \"content\" (every line of the result). Do not leave stubs. Do not paste the
old file and append; do not duplicate functions. \"content\" replaces the whole
buffer.

Preferred:
{\"say\":\"implemented fib\",\"content\":\"def fib(n):\\n    a, b = 0, 1\\n    for _ in range(n):\\n        print(a)\\n        a, b = b, a + b\\n\\nfib(5)\\n\"}

Tiny single-line fixes only (op exactly replace OR delete OR insert_after):
{\"say\":\"typo fix\",\"edits\":[{\"line\":1,\"op\":\"replace\",\"text\":\"fixed line\"}]}

Rules:
- Escape newlines inside JSON strings as \\n (never a raw line break in quotes).
- Never use Python triple-quotes (\"\"\") inside JSON. content is ONLY the file
  body — never wrap it in another {\"say\":...,\"content\":...} object.
- Prefer \"content\" over \"edits\" for any multi-line or feature change.
- Include content/edits ONLY when the operator asked to change the file.
- Never claim you wrote the disk yourself; the shell applies the change.
";

const EDITOR_SYSTEM_AUTO: &str = "\
You are SINE's file editor assistant in suggestion mode.
Reply with ONE JSON object only (no prose outside JSON).
The numbered file in the user message is the source of truth.

Prefer whole-file proposals:
{\"say\":\"suggestion\",\"content\":\"full proposed file with \\n newlines\"}
Tiny: {\"say\":\"suggestion\",\"edits\":[{\"line\":1,\"op\":\"replace\",\"text\":\"line\"}]}
op must be exactly replace, delete, or insert_after — never a pipe-separated list.
Any content/edits are PROPOSALS — do not describe them as already saved.
Empty content/edits is fine if nothing useful. Escape newlines as \\n.
";

const EDITOR_REPAIR: &str = "\
Your previous reply was not valid editor JSON. Reply again with ONLY one JSON object.
Prefer whole-file form with escaped newlines (COMPLETE file, no duplicates):
{\"say\":\"fixed\",\"content\":\"line1\\nline2\\n\"}
Do NOT use op values like replace|delete|insert_after. If you use edits, op is
exactly one word: replace OR delete OR insert_after.
";

const COMPACTIFY_ASK: &str = "\
Summarize this editor conversation so far. Keep the file path, what changed, \
decisions, and open questions. Be concise. Output only the summary — no JSON.";

fn editor_stub() -> bool {
    matches!(
        std::env::var("ENGRAM_SINE_STUB").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

// ── Buffer ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct EditorBuffer {
    /// Source lines without trailing newlines (empty vec → one blank line).
    lines: Vec<String>,
    /// 0-based row.
    row: usize,
    /// 0-based column in characters.
    col: usize,
}

impl EditorBuffer {
    fn from_text(text: &str) -> Self {
        let lines = split_lines(text);
        Self {
            lines,
            row: 0,
            col: 0,
        }
    }

    fn to_text(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.row.min(self.lines.len() - 1);
        let len = self.lines[self.row].chars().count();
        self.col = self.col.min(len);
    }

    fn insert_char(&mut self, ch: char) {
        self.clamp_cursor();
        let line = &mut self.lines[self.row];
        let mut chars: Vec<char> = line.chars().collect();
        chars.insert(self.col, ch);
        *line = chars.into_iter().collect();
        self.col += 1;
    }

    fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.insert_newline();
            } else if ch != '\r' {
                self.insert_char(ch);
            }
        }
    }

    fn insert_newline(&mut self) {
        self.clamp_cursor();
        let line = self.lines[self.row].clone();
        let chars: Vec<char> = line.chars().collect();
        let left: String = chars[..self.col].iter().collect();
        let right: String = chars[self.col..].iter().collect();
        self.lines[self.row] = left;
        self.lines.insert(self.row + 1, right);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        self.clamp_cursor();
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            let mut chars: Vec<char> = line.chars().collect();
            chars.remove(self.col - 1);
            *line = chars.into_iter().collect();
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&cur);
        }
    }

    fn move_left(&mut self) {
        self.clamp_cursor();
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    fn move_right(&mut self) {
        self.clamp_cursor();
        let len = self.lines[self.row].chars().count();
        if self.col < len {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        self.clamp_cursor();
        if self.row > 0 {
            self.row -= 1;
            self.clamp_cursor();
        }
    }

    fn move_down(&mut self) {
        self.clamp_cursor();
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.clamp_cursor();
        }
    }

    fn move_page(&mut self, delta: isize) {
        self.clamp_cursor();
        let next = (self.row as isize + delta).clamp(0, (self.lines.len() as isize) - 1);
        self.row = next as usize;
        self.clamp_cursor();
    }

    fn move_home(&mut self) {
        self.col = 0;
    }

    fn move_end(&mut self) {
        self.clamp_cursor();
        self.col = self.lines[self.row].chars().count();
    }
}

fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let t = text.strip_suffix('\n').unwrap_or(text);
    if t.is_empty() {
        return vec![String::new()];
    }
    t.split('\n').map(|s| s.trim_end_matches('\r').to_string()).collect()
}

// ── Line edits from the model ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LineEdit {
    line: usize,
    op: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EditorReply {
    #[serde(default)]
    say: String,
    /// Whole-file rewrite (preferred for small models / feature asks).
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    edits: Vec<LineEdit>,
}

impl EditorReply {
    fn has_changes(&self) -> bool {
        self.content.as_ref().map(|c| !c.is_empty()).unwrap_or(false) || !self.edits.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyReport {
    applied: usize,
    refused: Vec<String>,
}

/// Map model synonyms / schema mistakes onto the three closed ops.
fn normalize_op(op: &str) -> Option<&'static str> {
    let t = op.trim().to_ascii_lowercase();
    // Models sometimes parrot "replace|delete|insert_after" from a bad schema hint.
    if t.contains('|') {
        return None;
    }
    match t.as_str() {
        "replace" | "set" | "write" => Some("replace"),
        "delete" | "del" | "remove" => Some("delete"),
        "insert_after" | "insert" | "append" | "insert_below" => Some("insert_after"),
        _ => None,
    }
}

fn split_edit_text(text: &str) -> Vec<String> {
    // JSON may give real newlines or literal \n sequences from smaller models.
    let owned;
    let raw = if text.contains('\\') && text.contains('n') && !text.contains('\n') {
        owned = text.replace("\\n", "\n").replace("\\t", "\t");
        owned.as_str()
    } else {
        text
    };
    if raw.is_empty() {
        return vec![String::new()];
    }
    raw.replace('\r', "")
        .split('\n')
        .map(str::to_string)
        .collect()
}

fn apply_edits(buf: &mut EditorBuffer, edits: &[LineEdit]) -> ApplyReport {
    let mut refused = Vec::new();
    let mut applied = 0usize;
    // Apply bottom-up so earlier line numbers stay stable for same-batch ops.
    let mut indexed: Vec<(usize, &LineEdit)> = edits.iter().enumerate().collect();
    indexed.sort_by(|a, b| b.1.line.cmp(&a.1.line).then(b.0.cmp(&a.0)));

    for (_i, edit) in indexed {
        let line = edit.line;
        if line == 0 {
            refused.push(format!("line 0 invalid ({})", edit.op));
            continue;
        }
        let Some(op) = normalize_op(&edit.op) else {
            refused.push(format!(
                "unknown op '{}' on line {line} (want replace, delete, or insert_after)",
                edit.op
            ));
            continue;
        };
        match op {
            "replace" => {
                if line > buf.lines.len() {
                    refused.push(format!("replace line {line} out of range"));
                    continue;
                }
                let Some(text) = edit.text.as_ref() else {
                    refused.push(format!("replace line {line} missing text"));
                    continue;
                };
                let parts = split_edit_text(text);
                buf.lines[line - 1] = parts[0].clone();
                for (i, part) in parts.iter().skip(1).enumerate() {
                    buf.lines.insert(line + i, part.clone());
                }
                applied += 1;
            }
            "delete" => {
                if line > buf.lines.len() {
                    refused.push(format!("delete line {line} out of range"));
                    continue;
                }
                if buf.lines.len() == 1 {
                    buf.lines[0].clear();
                } else {
                    buf.lines.remove(line - 1);
                }
                applied += 1;
            }
            "insert_after" => {
                if line > buf.lines.len() {
                    refused.push(format!("insert_after line {line} out of range"));
                    continue;
                }
                let Some(text) = edit.text.as_ref() else {
                    refused.push(format!("insert_after line {line} missing text"));
                    continue;
                };
                let parts = split_edit_text(text);
                for (i, part) in parts.iter().enumerate() {
                    buf.lines.insert(line + i, part.clone());
                }
                applied += 1;
            }
            _ => unreachable!(),
        }
    }
    buf.clamp_cursor();
    ApplyReport { applied, refused }
}

fn reply_needs_repair(reply: &EditorReply) -> bool {
    if !reply.edits.is_empty() && reply.edits.iter().any(|e| normalize_op(&e.op).is_none()) {
        return true;
    }
    // Schema parrot with empty useful payload.
    reply.say.trim().eq_ignore_ascii_case("short reply") && !reply.has_changes()
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end >= start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

/// Escape raw newlines/tabs that small models leave inside JSON string literals.
fn sanitize_json_strings(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    let mut in_string = false;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            out.push(ch);
            escape = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => {
                    out.push(ch);
                    escape = true;
                }
                '"' => {
                    out.push(ch);
                    in_string = false;
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(ch),
            }
        } else {
            match ch {
                '"' => {
                    out.push(ch);
                    in_string = true;
                }
                _ => out.push(ch),
            }
        }
    }
    // Trailing commas before } or ]
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        cleaned.push(chars[i]);
        i += 1;
    }
    cleaned
}

fn extract_fenced_block(raw: &str) -> Option<(String /*lang*/, String /*body*/)> {
    let start = raw.find("```")?;
    let after = &raw[start + 3..];
    let (lang, body_start) = if let Some(nl) = after.find('\n') {
        (after[..nl].trim().to_ascii_lowercase(), nl + 1)
    } else {
        (String::new(), 0)
    };
    let body = &after[body_start..];
    let end = body.find("```")?;
    let text = body[..end].trim_end_matches('\n').to_string();
    if text.is_empty() {
        None
    } else {
        Some((lang, text))
    }
}

fn looks_like_editor_envelope(s: &str) -> bool {
    let t = s.trim_start();
    if !t.starts_with('{') {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    lower.contains("\"say\"") && (lower.contains("\"content\"") || lower.contains("\"edits\""))
}

/// Pull say/content out of model replies that use Python triple-quotes or
/// other almost-JSON shapes serde cannot parse.
fn extract_loose_editor_reply(raw: &str) -> Option<EditorReply> {
    if !looks_like_editor_envelope(raw) {
        return None;
    }
    let say = extract_simple_json_string(raw, "say").unwrap_or_else(|| "applied".into());
    if let Some(content) = extract_content_field_loose(raw) {
        let content = content.trim_start_matches('\n').to_string();
        if content.trim().is_empty() {
            return None;
        }
        // Nested envelope accidentally stuffed into content — unwrap once.
        let content = if looks_like_editor_envelope(&content) {
            extract_content_field_loose(&content).unwrap_or(content)
        } else {
            content
        };
        return Some(EditorReply {
            say,
            content: Some(content),
            edits: vec![],
        });
    }
    None
}

fn extract_simple_json_string(s: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let idx = s.find(&key)?;
    let after = s[idx + key.len()..].trim_start().strip_prefix(':')?.trim_start();
    let rest = after.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(c) => out.push(c),
                None => break,
            }
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

fn extract_content_field_loose(s: &str) -> Option<String> {
    let key = "\"content\"";
    let idx = s.find(key)?;
    let after = s[idx + key.len()..].trim_start().strip_prefix(':')?.trim_start();
    if after.starts_with("\"\"\"") {
        let rest = &after[3..];
        let end = rest.find("\"\"\"")?;
        return Some(rest[..end].trim_matches('\n').to_string());
    }
    if after.starts_with("'''") {
        let rest = &after[3..];
        let end = rest.find("'''")?;
        return Some(rest[..end].trim_matches('\n').to_string());
    }
    extract_simple_json_string(s, "content")
}

fn parse_editor_reply(raw: &str) -> Result<EditorReply> {
    if let Some(json) = extract_json_object(raw) {
        if let Ok(reply) = serde_json::from_str::<EditorReply>(json) {
            return Ok(normalize_reply_content(reply));
        }
        let sanitized = sanitize_json_strings(json);
        if let Ok(reply) = serde_json::from_str::<EditorReply>(&sanitized) {
            return Ok(normalize_reply_content(reply));
        }
        if let Some(reply) = extract_loose_editor_reply(json) {
            return Ok(normalize_reply_content(reply));
        }
    }
    if let Some(reply) = extract_loose_editor_reply(raw) {
        return Ok(normalize_reply_content(reply));
    }
    // Fenced block: JSON/envelope fences are replies; other fences are file body.
    if let Some((lang, body)) = extract_fenced_block(raw) {
        if lang == "json" || looks_like_editor_envelope(&body) {
            if let Ok(reply) = serde_json::from_str::<EditorReply>(&body) {
                return Ok(normalize_reply_content(reply));
            }
            let sanitized = sanitize_json_strings(&body);
            if let Ok(reply) = serde_json::from_str::<EditorReply>(&sanitized) {
                return Ok(normalize_reply_content(reply));
            }
            if let Some(reply) = extract_loose_editor_reply(&body) {
                return Ok(normalize_reply_content(reply));
            }
            return Err(anyhow!(
                "parse editor reply: fenced JSON/envelope was not usable"
            ));
        }
        return Ok(EditorReply {
            say: "applied fenced content".into(),
            content: Some(body),
            edits: vec![],
        });
    }
    Err(anyhow!("parse editor reply: no usable JSON or fenced content"))
}

fn normalize_reply_content(mut reply: EditorReply) -> EditorReply {
    if let Some(c) = reply.content.as_mut() {
        if c.contains('\\') && c.contains('n') && !c.contains('\n') {
            *c = c.replace("\\n", "\n").replace("\\t", "\t");
        }
        // Safety: never write an editor JSON envelope into the buffer.
        if looks_like_editor_envelope(c) {
            if let Some(inner) = extract_content_field_loose(c) {
                *c = inner;
            }
        }
    }
    reply
}

/// File body must never be our reply schema.
fn unwrap_file_content(content: &str) -> String {
    let mut text = content.to_string();
    for _ in 0..3 {
        if !looks_like_editor_envelope(&text) {
            break;
        }
        if let Some(inner) = extract_content_field_loose(&text) {
            text = inner;
        } else {
            break;
        }
    }
    text
}

/// Strip UI apply notes from a displayed assistant turn for model history.
fn chat_say_for_model(text: &str) -> &str {
    text.lines()
        .next()
        .unwrap_or(text)
        .trim()
}

/// Models often stuff a whole-file rewrite into `replace` on line 1. That path
/// inserts extra lines without removing the rest → duplicated buffer. Coerce
/// those (and prefer `content` over `edits`) before apply.
fn coerce_reply_for_apply(mut reply: EditorReply, line_count: usize) -> EditorReply {
    if reply
        .content
        .as_ref()
        .map(|c| !c.trim().is_empty())
        .unwrap_or(false)
    {
        reply.edits.clear();
        return reply;
    }
    if reply.edits.len() == 1 {
        let e = &reply.edits[0];
        if e.line == 1 && normalize_op(&e.op) == Some("replace") {
            if let Some(text) = e.text.as_ref() {
                let parts = split_edit_text(text);
                // Multi-line replace of line 1 that covers most/all of the file
                // is a whole-file rewrite in disguise.
                if parts.len() > 1 && parts.len() >= line_count.saturating_sub(1).max(2) {
                    return EditorReply {
                        say: reply.say,
                        content: Some(parts.join("\n")),
                        edits: vec![],
                    };
                }
            }
        }
    }
    reply
}

/// Build the model messages: one system + one user payload.
///
/// Prior chat is embedded as prose (not alternating roles) so we do not:
/// - duplicate the latest operator ask (already pushed into `state.chat`)
/// - train the model on UI notes like "rewrote file" as assistant JSON
fn build_editor_user_payload(
    state: &EditorState,
    user_msg: Option<&str>,
    auto_suggest: bool,
) -> String {
    let file_part = state.numbered_for_prompt();
    let mode = if state.auto { "auto" } else { "manual" };

    // History: drop the trailing user turn when it is this send's ask.
    let mut end = state.chat.len();
    if let Some(msg) = user_msg {
        if state
            .chat
            .last()
            .map(|t| t.user && t.text == msg)
            .unwrap_or(false)
        {
            end = end.saturating_sub(1);
        }
    }
    let start = end.saturating_sub(8);
    let mut prior = String::new();
    for turn in &state.chat[start..end] {
        if turn.user {
            prior.push_str("operator: ");
            prior.push_str(turn.text.trim());
            prior.push('\n');
        } else {
            let say = chat_say_for_model(&turn.text);
            if !say.is_empty()
                && !say.starts_with("rewrote ")
                && !say.starts_with("applied ")
                && !say.starts_with("accepted ")
            {
                prior.push_str("assistant: ");
                prior.push_str(say);
                prior.push('\n');
            }
        }
    }

    let mut user = format!(
        "The numbered file below is current on disk. Prior assistant claims may be wrong — \
implement the operator ask against THIS file.\n\
path: {}\ncursor_line: {}\nmode: {}\n\nfile:\n{file_part}\n",
        state.path.display(),
        state.buf.row + 1,
        mode
    );
    if !prior.trim().is_empty() {
        user.push_str("\nprior conversation (context only):\n");
        user.push_str(&prior);
        user.push('\n');
    }
    if let Some(msg) = user_msg {
        user.push_str("\noperator:\n");
        user.push_str(msg);
        user.push('\n');
    } else if auto_suggest {
        user.push_str("\noperator: (quiet — offer one small suggestion if useful)\n");
    }
    user.push_str(
        "\nReply with ONE JSON object. For feature/fix asks, put the COMPLETE new file in \
\"content\" (no duplicates, no leftover stubs).\n",
    );
    user
}

fn format_edits_for_chat(edits: &[LineEdit]) -> String {
    if edits.is_empty() {
        return String::new();
    }
    let mut out = String::from("proposed edits:\n");
    for e in edits {
        match e.op.as_str() {
            "delete" => out.push_str(&format!("  - delete line {}\n", e.line)),
            "replace" => {
                out.push_str(&format!(
                    "  - replace line {} → {}\n",
                    e.line,
                    e.text.as_deref().unwrap_or("")
                ));
            }
            "insert_after" => {
                out.push_str(&format!(
                    "  - insert after line {} → {}\n",
                    e.line,
                    e.text.as_deref().unwrap_or("")
                ));
            }
            other => out.push_str(&format!("  - {other} line {}\n", e.line)),
        }
    }
    out
}

// ── Version ring ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct VersionRing {
    /// Oldest → newest snapshots (distinct consecutive texts).
    snapshots: Vec<String>,
    /// Index of the current view into snapshots (None if empty).
    #[serde(default)]
    cursor: Option<usize>,
}

impl VersionRing {
    fn push(&mut self, text: &str) {
        // Drop redo tail when editing after undo.
        if let Some(cur) = self.cursor {
            if cur + 1 < self.snapshots.len() {
                self.snapshots.truncate(cur + 1);
            }
        }
        if self
            .cursor
            .and_then(|i| self.snapshots.get(i))
            .map(|s| s.as_str() == text)
            .unwrap_or(false)
        {
            return;
        }
        self.snapshots.push(text.to_string());
        while self.snapshots.len() > RING_CAP {
            self.snapshots.remove(0);
        }
        self.cursor = Some(self.snapshots.len() - 1);
    }

    fn undo(&mut self) -> Option<&str> {
        let cur = self.cursor?;
        if cur == 0 {
            return None;
        }
        self.cursor = Some(cur - 1);
        self.snapshots.get(cur - 1).map(String::as_str)
    }

    fn redo(&mut self) -> Option<&str> {
        let cur = self.cursor?;
        if cur + 1 >= self.snapshots.len() {
            return None;
        }
        self.cursor = Some(cur + 1);
        self.snapshots.get(cur + 1).map(String::as_str)
    }

    fn len(&self) -> usize {
        self.snapshots.len()
    }
}

fn encode_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn edit_meta_dir(sine_dir: &Path, path: &Path) -> PathBuf {
    sine_dir.join("edits").join(encode_path_key(path))
}

fn load_ring(dir: &Path) -> VersionRing {
    let path = dir.join("ring.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => VersionRing::default(),
    }
}

fn save_ring(dir: &Path, ring: &VersionRing) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("ring.json");
    let tmp = dir.join("ring.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(ring)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn load_chat(dir: &Path) -> Vec<ChatTurn> {
    let path = dir.join("chat.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_chat(dir: &Path, chat: &[ChatTurn]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("chat.json");
    let tmp = dir.join("chat.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(chat)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ── Editor state ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorFocus {
    File,
    Chat,
}

struct PendingProposal {
    say: String,
    content: Option<String>,
    edits: Vec<LineEdit>,
}

enum EditorJobResult {
    Reply {
        reply: Result<EditorReply>,
        as_suggest: bool,
    },
    Compactify(Result<String>),
}

struct EditorState {
    path: PathBuf,
    buf: EditorBuffer,
    ring: VersionRing,
    chat: Vec<ChatTurn>,
    composer: String,
    focus: EditorFocus,
    auto: bool,
    allow_persist: bool,
    meta_dir: PathBuf,
    dirty: bool,
    last_edit: Instant,
    scroll: usize,
    wrap_cols: usize,
    rows: Vec<String>,
    chat_scroll: usize,
    quiet_since: Instant,
    suggest_done: bool,
    pending: Option<PendingProposal>,
    status: String,
    busy: bool,
}

impl EditorState {
    fn open(path: &Path, allow_persist: bool) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let sine_dir = default_sine_dir();
        let meta_dir = edit_meta_dir(&sine_dir, path);
        let mut ring = if allow_persist {
            load_ring(&meta_dir)
        } else {
            VersionRing::default()
        };
        let chat = if allow_persist {
            load_chat(&meta_dir)
        } else {
            Vec::new()
        };
        // Seed ring with current disk content if empty.
        if ring.snapshots.is_empty() {
            ring.push(&ensure_trailing_newline(&text));
            if allow_persist {
                let _ = save_ring(&meta_dir, &ring);
            }
        }
        let (_rows, cols) = crate::term::term_size();
        let cols = cols.clamp(40, crate::term::MAX_COLS);
        let left_w = left_cols(cols);
        let buf = EditorBuffer::from_text(&text);
        let rows = wrap_numbered_lines(&buf.to_text(), left_w);
        Ok(Self {
            path: path.to_path_buf(),
            buf,
            ring,
            chat,
            composer: String::new(),
            focus: EditorFocus::File,
            auto: false,
            allow_persist,
            meta_dir,
            dirty: false,
            last_edit: Instant::now(),
            scroll: 0,
            wrap_cols: left_w,
            rows,
            chat_scroll: 0,
            quiet_since: Instant::now(),
            suggest_done: false,
            pending: None,
            status: String::new(),
            busy: false,
        })
    }

    fn note_activity(&mut self) {
        self.quiet_since = Instant::now();
        self.suggest_done = false;
    }

    fn mark_edited(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
        self.note_activity();
        self.relayout();
    }

    fn relayout(&mut self) {
        let (term_rows, cols) = crate::term::term_size();
        let cols = cols.clamp(40, crate::term::MAX_COLS);
        let left_w = left_cols(cols);
        if left_w != self.wrap_cols {
            self.wrap_cols = left_w;
        }
        self.rows = wrap_numbered_lines(&self.buf.to_text(), left_w);
        let body = file_body_rows(term_rows.clamp(8, crate::term::MAX_ROWS));
        self.ensure_cursor_visible(body);
        let max_scroll = self.rows.len().saturating_sub(body);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn ensure_cursor_visible(&mut self, body: usize) {
        let target = display_row_for_cursor(&self.rows, self.buf.row + 1, self.buf.col);
        if target < self.scroll {
            self.scroll = target;
        } else if target >= self.scroll + body {
            self.scroll = target + 1 - body;
        }
    }

    fn push_chat(&mut self, user: bool, text: impl Into<String>) {
        self.chat.push(ChatTurn {
            user,
            text: text.into(),
        });
        self.chat_scroll = 0;
        self.persist_chat();
    }

    fn persist_chat(&self) {
        if self.allow_persist {
            let _ = save_chat(&self.meta_dir, &self.chat);
        }
    }

    fn flush_save(&mut self) -> Result<()> {
        if !self.dirty && !self.allow_persist {
            return Ok(());
        }
        let text = self.buf.to_text();
        if self.allow_persist {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = self.path.with_extension("sine-edit.tmp");
            std::fs::write(&tmp, &text)
                .with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, &self.path)
                .with_context(|| format!("renaming onto {}", self.path.display()))?;
            self.ring.push(&text);
            save_ring(&self.meta_dir, &self.ring)?;
            self.persist_chat();
        } else {
            // Hallucination: keep undo in memory only.
            self.ring.push(&text);
        }
        self.dirty = false;
        Ok(())
    }

    fn maybe_autosave(&mut self) {
        if self.dirty && self.last_edit.elapsed() >= AUTOSAVE_DELAY {
            if let Err(e) = self.flush_save() {
                self.status = format!("autosave failed: {e:#}");
            } else {
                self.status = format!(
                    "saved · {} versions",
                    self.ring.len().max(1)
                );
            }
        }
    }

    fn undo(&mut self) {
        // Persist dirty edits as a tip first, then step back.
        if self.dirty {
            let _ = self.flush_save();
        }
        if let Some(text) = self.ring.undo().map(str::to_string) {
            self.buf = EditorBuffer::from_text(&text);
            self.buf.clamp_cursor();
            self.dirty = true;
            self.relayout();
            if let Err(e) = self.write_disk_only(&text) {
                self.status = format!("undo write failed: {e:#}");
            } else {
                self.dirty = false;
                let cur = self.ring.cursor.map(|c| c + 1).unwrap_or(0);
                self.status = format!("undone · v{cur}/{}", self.ring.len().max(1));
            }
            self.note_activity();
        } else {
            self.status = "nothing to undo".into();
        }
    }

    fn redo(&mut self) {
        if let Some(text) = self.ring.redo().map(str::to_string) {
            self.buf = EditorBuffer::from_text(&text);
            self.buf.clamp_cursor();
            self.dirty = true;
            self.relayout();
            if let Err(e) = self.write_disk_only(&text) {
                self.status = format!("redo write failed: {e:#}");
            } else {
                self.dirty = false;
                let cur = self.ring.cursor.map(|c| c + 1).unwrap_or(0);
                self.status = format!("redone · v{cur}/{}", self.ring.len().max(1));
            }
            self.note_activity();
        } else {
            self.status = "nothing to redo".into();
        }
    }

    fn write_disk_only(&self, text: &str) -> Result<()> {
        if !self.allow_persist {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("sine-edit.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        save_ring(&self.meta_dir, &self.ring)?;
        Ok(())
    }

    /// Snapshot the current buffer into the version ring if it is not already
    /// the tip — so the next AI/user change can be undone.
    fn checkpoint(&mut self) {
        let text = self.buf.to_text();
        self.ring.push(&text);
        if self.allow_persist {
            let _ = save_ring(&self.meta_dir, &self.ring);
        }
    }

    fn apply_content(&mut self, content: &str) {
        self.checkpoint();
        let text = ensure_trailing_newline(&unwrap_file_content(content));
        self.buf = EditorBuffer::from_text(&text);
        self.buf.clamp_cursor();
        self.mark_edited();
        if let Err(e) = self.flush_save() {
            self.status = format!("save after rewrite failed: {e:#}");
        }
    }

    fn apply_reply_edits(&mut self, edits: &[LineEdit]) -> ApplyReport {
        self.checkpoint();
        let report = apply_edits(&mut self.buf, edits);
        if report.applied > 0 {
            self.mark_edited();
            if let Err(e) = self.flush_save() {
                self.status = format!("save after edit failed: {e:#}");
            }
        }
        report
    }

    fn apply_reply_changes(&mut self, reply: &EditorReply) -> String {
        let reply = coerce_reply_for_apply(reply.clone(), self.buf.lines.len());
        let mut parts = Vec::new();
        if let Some(content) = reply.content.as_ref().filter(|c| !c.is_empty()) {
            self.apply_content(content);
            parts.push("rewrote file".to_string());
        } else if !reply.edits.is_empty() {
            let report = self.apply_reply_edits(&reply.edits);
            parts.push(format!("applied {} edit(s)", report.applied));
            if !report.refused.is_empty() {
                parts.push(format!("refused: {}", report.refused.join("; ")));
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            parts.join(" · ")
        }
    }

    fn clear_chat(&mut self) {
        self.chat.clear();
        self.composer.clear();
        self.pending = None;
        self.chat_scroll = 0;
        self.persist_chat();
        self.status = "context cleared".into();
        self.note_activity();
    }

    fn apply_compactify(&mut self, summary: String) {
        let text = summary.trim().to_string();
        self.chat = if text.is_empty() {
            Vec::new()
        } else {
            vec![ChatTurn {
                user: true,
                text,
            }]
        };
        self.pending = None;
        self.chat_scroll = 0;
        self.persist_chat();
        self.status = "compactified".into();
        self.note_activity();
    }

    fn accept_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let synthetic = EditorReply {
            say: pending.say.clone(),
            content: pending.content,
            edits: pending.edits,
        };
        let mut note = self.apply_reply_changes(&synthetic);
        if note.is_empty() {
            note = "accepted · no changes".into();
        } else {
            note = format!("accepted · {note}");
        }
        self.push_chat(false, &note);
        self.status = note;
        self.note_activity();
    }

    fn dismiss_pending(&mut self) {
        if self.pending.take().is_some() {
            self.push_chat(false, "(suggestion dismissed)");
            self.status = "suggestion dismissed".into();
            self.note_activity();
        }
    }

    fn numbered_for_prompt(&self) -> String {
        let full = self.buf.to_text();
        if full.chars().count() <= FILE_PROMPT_CHARS {
            return number_plain(&full);
        }
        let start = self.buf.row.saturating_sub(40);
        let end = (self.buf.row + 40).min(self.buf.lines.len().saturating_sub(1));
        let mut out = format!(
            "(showing lines {}-{} of {})\n",
            start + 1,
            end + 1,
            self.buf.lines.len()
        );
        for (i, line) in self.buf.lines[start..=end].iter().enumerate() {
            out.push_str(&format!("{:>4}│ {}\n", start + i + 1, line));
        }
        out
    }

    /// Working-memory estimate for the editor (chars/4), same window as `:chat`.
    /// Counts the active system prompt, full editor chat, and the file slice
    /// that would be attached on the next model call.
    fn context_tokens(&self) -> usize {
        let system = if self.auto {
            EDITOR_SYSTEM_AUTO
        } else {
            EDITOR_SYSTEM_MANUAL
        };
        let mut n = "system".chars().count() + system.chars().count();
        for turn in &self.chat {
            let role = if turn.user { "user" } else { "assistant" };
            n += role.chars().count() + turn.text.chars().count();
        }
        n += self.path.display().to_string().chars().count();
        n += self.numbered_for_prompt().chars().count();
        if let Some(p) = &self.pending {
            n += p.say.chars().count();
            if let Some(c) = &p.content {
                n += c.chars().count();
            }
            for e in &p.edits {
                n += e.op.chars().count() + e.text.as_deref().unwrap_or("").chars().count();
            }
        }
        n / 4
    }

    fn context_percent(&self) -> usize {
        crate::chat_tui::percent(self.context_tokens(), crate::chat_tui::context_window_tokens())
    }

    fn context_label(&self) -> String {
        format!(
            "ctx {} tok · est {}%",
            self.context_tokens(),
            self.context_percent()
        )
    }
}

fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

fn number_plain(text: &str) -> String {
    let lines = split_lines(text);
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(&format!("{:>4}│ {}\n", i + 1, line));
    }
    out
}

fn left_cols(term_cols: usize) -> usize {
    // Reserve 1 column for the vertical divider plus a usable right pane.
    let left = ((term_cols as f32) * SPLIT_RATIO) as usize;
    left.clamp(24, term_cols.saturating_sub(21).max(24))
}

fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let clean: String = raw.chars().filter(|c| *c != '\r').collect();
        if clean.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = clean.chars().collect();
        let mut offset = 0;
        while offset < chars.len() {
            let end = (offset + width).min(chars.len());
            out.push(chars[offset..end].iter().collect());
            offset = end;
        }
    }
    out
}

fn file_body_rows(term_rows: usize) -> usize {
    // title + footer
    term_rows.saturating_sub(2).max(1)
}

/// Find the display row index for a 1-based source line and column.
fn display_row_for_cursor(rows: &[String], src_line: usize, col: usize) -> usize {
    let mut found_first = None;
    for (i, row) in rows.iter().enumerate() {
        if let Some(n) = first_gutter_lineno(row) {
            if n == src_line {
                found_first = Some(i);
                break;
            }
        }
    }
    let Some(start) = found_first else {
        return 0;
    };
    // Continuations after the first row of this source line.
    let mut remaining = col;
    let body_start = rows[start]
        .find('│')
        .map(|p| p + '│'.len_utf8())
        .unwrap_or(0);
    let first_body: String = rows[start][body_start..].trim_start().chars().collect();
    // Gutter is "NNN│ " — body after rule+space.
    let first_body_len = {
        let after = rows[start].get(body_start..).unwrap_or("");
        let trimmed = after.strip_prefix(' ').unwrap_or(after);
        trimmed.chars().count()
    };
    if remaining <= first_body_len {
        let _ = first_body;
        return start;
    }
    remaining = remaining.saturating_sub(first_body_len);
    let mut idx = start + 1;
    while idx < rows.len() {
        if first_gutter_lineno(&rows[idx]).is_some() {
            break;
        }
        let after = rows[idx]
            .find('│')
            .and_then(|p| rows[idx].get(p + '│'.len_utf8()..))
            .unwrap_or("");
        let trimmed = after.strip_prefix(' ').unwrap_or(after);
        let len = trimmed.chars().count();
        if remaining <= len {
            return idx;
        }
        remaining -= len;
        idx += 1;
    }
    start
}

fn first_gutter_lineno(row: &str) -> Option<usize> {
    let s = row.trim_start();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &s[digits.len()..];
    if rest.starts_with('│') {
        digits.parse().ok()
    } else {
        None
    }
}

// ── Draw ─────────────────────────────────────────────────────────────────────

fn draw(state: &EditorState) {
    let (term_rows, cols) = crate::term::term_size();
    let cols = cols.clamp(40, crate::term::MAX_COLS);
    let term_rows = term_rows.clamp(8, crate::term::MAX_ROWS);
    let left_w = left_cols(cols);
    // Column layout: [left_w file][1 divider][right_w chat]
    let right_w = cols.saturating_sub(left_w + 1).max(16);
    let div_x = left_w + 1;
    let right_x = left_w + 2;
    let body = file_body_rows(term_rows);
    let color = ui::color_enabled();
    let reset = "\x1b[0m";
    let cyan = if color { "\x1b[36m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let bold = if color { "\x1b[1m" } else { "" };
    let rev = if color { "\x1b[7m" } else { "" };

    let mut buf = String::with_capacity(term_rows * cols * 2);
    use std::fmt::Write as _;
    let _ = write!(buf, "\x1b[2J\x1b[H\x1b[?25l");

    // Title
    let auto = if state.auto { "on" } else { "off" };
    let focus = match state.focus {
        EditorFocus::File => "file",
        EditorFocus::Chat => "chat",
    };
    let prefix = " editor ── ";
    let max_path = left_w
        .saturating_sub(prefix.chars().count() + 1)
        .max(8);
    let path = split_frame::truncate_visible(&state.path.display().to_string(), max_path);
    let title_left = format!("{prefix}{path} ");
    let dash = left_w.saturating_sub(title_left.chars().count());
    let left_title = format!(
        "{cyan}{bold}{}{reset}{cyan}{}{reset}",
        title_left,
        "─".repeat(dash)
    );
    let ctx = state.context_label();
    let right_head = format!(" chat ── {ctx} · auto:{auto} · focus:{focus} ");
    let right_title = format!(
        "{cyan}{bold}{}{reset}{cyan}{}{reset}",
        right_head,
        "─".repeat(right_w.saturating_sub(right_head.chars().count()))
    );
    let _ = write!(
        buf,
        "\x1b[1;1H{}",
        split_frame::truncate_visible(&left_title, left_w.saturating_sub(1).max(1))
    );
    let _ = write!(buf, "\x1b[1;{div_x}H{cyan}┬{reset}");
    let _ = write!(
        buf,
        "\x1b[1;{right_x}H{}\x1b[K",
        split_frame::truncate_visible(&right_title, right_w.saturating_sub(1).max(1))
    );

    // File body with reverse-video cursor (soft-wrapped via wrap_numbered_lines)
    let cursor_disp = display_row_for_cursor(&state.rows, state.buf.row + 1, state.buf.col);
    let start = state.scroll;
    for i in 0..body {
        let y = i + 2;
        let row_idx = start + i;
        let line = state.rows.get(row_idx).cloned().unwrap_or_default();
        let painted = if row_idx == cursor_disp && state.focus == EditorFocus::File {
            paint_cursor_on_row(&line, state.buf.row + 1, state.buf.col, rev, reset)
        } else {
            line
        };
        let cell = split_frame::truncate_visible(&painted, left_w.saturating_sub(1).max(1));
        let _ = write!(buf, "\x1b[{y};1H{cell}\x1b[K");
        let _ = write!(buf, "\x1b[{y};{div_x}H{cyan}│{reset}");
    }

    // Chat body (leave last body row for composer); wrap to pane width
    let chat_body = body.saturating_sub(1).max(1);
    let chat_lines = render_chat_lines(state, right_w.saturating_sub(1).max(8));
    let chat_max_scroll = chat_lines.len().saturating_sub(chat_body);
    let chat_start = chat_max_scroll.saturating_sub(state.chat_scroll);
    for i in 0..chat_body {
        let y = i + 2;
        let line = chat_lines
            .get(chat_start + i)
            .cloned()
            .unwrap_or_default();
        let cell = split_frame::truncate_visible(&line, right_w.saturating_sub(1).max(1));
        let _ = write!(buf, "\x1b[{y};{right_x}H{cell}\x1b[K");
    }

    // Composer on last body row of the right pane
    let composer_y = 1 + body;
    let prompt = if state.focus == EditorFocus::Chat {
        format!("> {}_", state.composer)
    } else {
        format!("> {}", state.composer)
    };
    let cell = split_frame::truncate_visible(&prompt, right_w.saturating_sub(1).max(1));
    let _ = write!(buf, "\x1b[{composer_y};{div_x}H{cyan}│{reset}");
    let _ = write!(buf, "\x1b[{composer_y};{right_x}H{cell}\x1b[K");

    // Footer
    let pending = if state.pending.is_some() {
        " · Enter accept · ^K dismiss"
    } else {
        ""
    };
    let busy = if state.busy { " · thinking…" } else { "" };
    let ver = {
        let n = state.ring.len().max(1);
        let cur = state.ring.cursor.map(|c| c + 1).unwrap_or(1);
        format!("v{cur}/{n}")
    };
    let footer = format!(
        "{dim} Esc · Tab · ^S send · ^A compact · auto:{auto} · ^K clear · ^Z undo ^Y redo · {ver} · L{} C{}{}{} · {}{reset}",
        state.buf.row + 1,
        state.buf.col + 1,
        pending,
        busy,
        if state.status.is_empty() {
            "—"
        } else {
            &state.status
        }
    );
    let _ = write!(
        buf,
        "\x1b[{term_rows};1H{}\x1b[K",
        split_frame::truncate_visible(&footer, cols.saturating_sub(1).max(1))
    );

    let mut out = io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn paint_cursor_on_row(
    row: &str,
    src_line: usize,
    col: usize,
    rev: &str,
    reset: &str,
) -> String {
    let Some(gutter_n) = first_gutter_lineno(row) else {
        // Continuation row: place cursor in body if this is the wrap holding col.
        return paint_cursor_in_body(row, col, rev, reset, true);
    };
    if gutter_n != src_line {
        return row.to_string();
    }
    paint_cursor_in_body(row, col, rev, reset, false)
}

fn paint_cursor_in_body(
    row: &str,
    col: usize,
    rev: &str,
    reset: &str,
    continuation: bool,
) -> String {
    let Some(rule) = row.find('│') else {
        return row.to_string();
    };
    let after_rule = rule + '│'.len_utf8();
    let (prefix, body) = row.split_at(after_rule);
    let body = body.strip_prefix(' ').unwrap_or(body);
    let space = if row[after_rule..].starts_with(' ') {
        " "
    } else {
        ""
    };
    let chars: Vec<char> = body.chars().collect();
    // For the first visual row of a source line, col maps from the start.
    // Continuations are handled by display_row_for_cursor selecting the right row;
    // we still paint at a relative col within that chunk when continuation=true
    // by treating `col` as absolute and letting the caller pick the row — so for
    // first row we use col directly; for continuation the absolute col was already
    // reduced conceptually by choosing this row, but we only get absolute col.
    // Simpler approach: paint at min(col, len) on first row; on continuation paint
    // at start of body when this is the cursor's display row (caller only paints
    // the cursor row).
    let idx = if continuation {
        // Approximate: reverse-video first body cell on continuation rows when
        // this is the active display row (col already past earlier chunks).
        0.min(chars.len())
    } else {
        col.min(chars.len())
    };
    let mut out = String::from(prefix);
    out.push_str(space);
    for (i, ch) in chars.iter().enumerate() {
        if i == idx {
            out.push_str(rev);
            out.push(*ch);
            out.push_str(reset);
        } else {
            out.push(*ch);
        }
    }
    if idx >= chars.len() {
        out.push_str(rev);
        out.push(' ');
        out.push_str(reset);
    }
    out
}

fn render_chat_lines(state: &EditorState, width: usize) -> Vec<String> {
    let mut raw = Vec::new();
    for turn in &state.chat {
        let prefix = if turn.user { "you: " } else { "sine: " };
        for (i, part) in turn.text.split('\n').enumerate() {
            if i == 0 {
                raw.push(format!("{prefix}{part}"));
            } else {
                raw.push(format!("      {part}"));
            }
        }
    }
        if let Some(p) = &state.pending {
        raw.push("--- suggestion (Enter accept / ^K dismiss) ---".into());
        if !p.say.is_empty() {
            raw.push(format!("sine: {}", p.say));
        }
        if let Some(content) = &p.content {
            raw.push("proposed file:".into());
            for line in content.lines().take(12) {
                raw.push(format!("  {line}"));
            }
            if content.lines().count() > 12 {
                raw.push("  …".into());
            }
        }
        for part in format_edits_for_chat(&p.edits).lines() {
            raw.push(part.to_string());
        }
    }
    let mut lines = Vec::new();
    for line in raw {
        lines.extend(wrap_plain(&line, width));
    }
    lines
}

// ── LLM ──────────────────────────────────────────────────────────────────────

async fn call_editor_model(
    cfg: &Config,
    state: &EditorState,
    user_msg: Option<&str>,
    auto_suggest: bool,
) -> Result<EditorReply> {
    if editor_stub() {
        return Ok(EditorReply {
            say: if auto_suggest {
                "stub suggestion".into()
            } else {
                "stub editor reply".into()
            },
            content: None,
            edits: if auto_suggest {
                vec![LineEdit {
                    line: 1,
                    op: "replace".into(),
                    text: Some("stub-suggested-line".into()),
                }]
            } else if user_msg.map(|m| {
                m.contains("change")
                    || m.contains("edit")
                    || m.contains("replace")
                    || m.contains("fix")
                    || m.contains("delete")
                    || m.contains("insert")
            })
            .unwrap_or(false)
            {
                vec![LineEdit {
                    line: state.buf.row + 1,
                    op: "replace".into(),
                    text: Some("stub-edited-line".into()),
                }]
            } else {
                vec![]
            },
        });
    }

    let system = if state.auto || auto_suggest {
        EDITOR_SYSTEM_AUTO
    } else {
        EDITOR_SYSTEM_MANUAL
    };
    let user = build_editor_user_payload(state, user_msg, auto_suggest);
    let messages = vec![
        ChatMessage::system(system),
        ChatMessage::user(user),
    ];

    let client = LlmClient::new(cfg)?;
    let raw = client
        .chat_text_max(&messages, EDITOR_MAX_TOKENS)
        .await
        .context("editor model call")?;
    let line_count = state.buf.lines.len();
    match parse_editor_reply(&raw) {
        Ok(reply) if !reply_needs_repair(&reply) => {
            Ok(coerce_reply_for_apply(reply, line_count))
        }
        first => {
            // Repair on parse failure or schema-parrot / bad ops.
            let mut repair = messages;
            repair.push(ChatMessage::assistant_text(&raw));
            let why = match &first {
                Ok(_) => "ops/schema invalid".to_string(),
                Err(e) => e.to_string(),
            };
            repair.push(ChatMessage::user(format!(
                "{EDITOR_REPAIR}\nParse note: {why}\nYour previous reply was:\n{raw}"
            )));
            let fixed = client
                .chat_text_max(&repair, EDITOR_MAX_TOKENS)
                .await
                .context("editor repair call")?;
            match parse_editor_reply(&fixed) {
                Ok(reply) => Ok(coerce_reply_for_apply(reply, line_count)),
                Err(e2) => match first {
                    Ok(bad) => Ok(coerce_reply_for_apply(bad, line_count)),
                    Err(e1) => Err(anyhow!("{e1:#}; repair also failed: {e2:#}")),
                },
            }
        }
    }
}

async fn call_compactify(cfg: &Config, chat: &[ChatTurn]) -> Result<String> {
    if editor_stub() {
        return Ok("stub compact summary of the editor chat".into());
    }
    let mut messages = vec![ChatMessage::system(
        "You summarize an editor collaboration chat. Output only the summary.",
    )];
    for turn in chat {
        if turn.user {
            messages.push(ChatMessage::user(&turn.text));
        } else {
            messages.push(ChatMessage::assistant_text(&turn.text));
        }
    }
    messages.push(ChatMessage::user(COMPACTIFY_ASK));
    let client = LlmClient::new(cfg)?;
    let text = client
        .chat_text_max(&messages, 512)
        .await
        .context("editor compactify")?;
    Ok(text)
}

fn handle_model_reply(state: &mut EditorState, reply: EditorReply, as_suggestion: bool) {
    if as_suggestion {
        if !reply.has_changes() && reply.say.trim().is_empty() {
            state.status = "no suggestion".into();
            return;
        }
        let say = if reply.say.is_empty() {
            "suggestion ready".into()
        } else {
            reply.say.clone()
        };
        state.pending = Some(PendingProposal {
            say: say.clone(),
            content: reply.content.clone(),
            edits: reply.edits.clone(),
        });
        state.push_chat(false, &say);
        state.status = "suggestion pending".into();
        return;
    }

    let mut text = reply.say.clone();
    if text.is_empty() && !reply.has_changes() {
        text = "(no reply)".into();
    }
    let applied = state.apply_reply_changes(&reply);
    if !applied.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&applied);
    }
    state.push_chat(false, &text);
    state.status = "replied".into();
}

// ── Public entry ─────────────────────────────────────────────────────────────

/// Ensure `path` exists (create empty file + parents if needed), record recent
/// history (unless hallucination), then open the editor owning the TTY
/// (raw mode + alt screen). Use this from `:edit` / the menu.
pub async fn run_for_path(cfg: &Config, path: &Path) -> Result<()> {
    ensure_file(path)?;
    record_open(cfg, path);
    run_inner(cfg, path, true).await
}

/// Open an existing path from inside SINE (parent already owns raw/alt screen).
pub async fn run_embedded(cfg: &Config, path: &Path) -> Result<()> {
    record_open(cfg, path);
    run_inner(cfg, path, false).await
}

fn record_open(cfg: &Config, path: &Path) {
    if cfg.mode != Mode::Hallucination {
        let hist = crate::edit_history::history_path();
        let _ = crate::edit_history::record(&hist, path);
    }
}

fn ensure_file(path: &Path) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    if path.exists() && !path.is_file() {
        return Err(anyhow!("{} is not a regular file", path.display()));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    if !path.exists() {
        std::fs::write(path, "")
            .with_context(|| format!("creating {}", path.display()))?;
    }
    Ok(())
}

/// Run the editor overlay until Esc. Returns Ok(()) on normal close.
///
/// When `owns_tty` is true (standalone `:edit`), enter raw mode and the alt
/// screen. When false (opened from SINE with `e`), the parent already holds
/// those — only paint and read keys.
async fn run_inner(cfg: &Config, path: &Path, owns_tty: bool) -> Result<()> {
    let _guard = if owns_tty {
        let guard = RawGuard::enter();
        if !guard.active() {
            return Err(anyhow!(
                "could not enter raw mode — try :edit from a real TTY"
            ));
        }
        {
            let mut out = io::stdout().lock();
            let _ = write!(out, "\x1b[?1049h\x1b[?25h");
            let _ = out.flush();
        }
        split_frame::set_bracketed_paste(true);
        Some(guard)
    } else {
        None
    };

    let result = run_loop(cfg, path).await;

    if owns_tty {
        split_frame::set_bracketed_paste(false);
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049l\x1b[?25h\x1b[0m");
        let _ = out.flush();
    }
    result
}

async fn run_loop(cfg: &Config, path: &Path) -> Result<()> {
    let allow_persist = cfg.mode != Mode::Hallucination;
    let mut state = EditorState::open(path, allow_persist)?;
    state.relayout();

    let mut pending_job: Option<tokio::task::JoinHandle<EditorJobResult>> = None;

    loop {
        state.relayout();
        state.maybe_autosave();
        draw(&state);

        // Finish LLM job
        if let Some(handle) = pending_job.take() {
            if handle.is_finished() {
                state.busy = false;
                match handle.await {
                    Ok(EditorJobResult::Reply { reply: Ok(reply), as_suggest }) => {
                        handle_model_reply(&mut state, reply, as_suggest);
                        state.note_activity();
                    }
                    Ok(EditorJobResult::Reply { reply: Err(e), .. }) => {
                        state.push_chat(false, &format!("error: {e:#}"));
                        state.status = "model error".into();
                        state.note_activity();
                    }
                    Ok(EditorJobResult::Compactify(Ok(summary))) => {
                        state.apply_compactify(summary);
                    }
                    Ok(EditorJobResult::Compactify(Err(e))) => {
                        state.push_chat(false, &format!("compactify error: {e:#}"));
                        state.status = "compactify error".into();
                        state.note_activity();
                    }
                    Err(e) => {
                        state.push_chat(false, &format!("job error: {e}"));
                        state.status = "job error".into();
                        state.note_activity();
                    }
                }
            } else {
                pending_job = Some(handle);
            }
        }

        // Auto-suggest when quiet
        if state.auto
            && pending_job.is_none()
            && state.pending.is_none()
            && !state.busy
            && !state.suggest_done
            && state.quiet_since.elapsed() >= AUTO_SUGGEST_QUIET
            && cfg.mode != Mode::Hallucination
        {
            state.busy = true;
            state.suggest_done = true;
            state.status = "suggesting…".into();
            let cfg_c = cfg.clone();
            let snap_path = state.path.clone();
            let snap_auto = state.auto;
            let snap_row = state.buf.row;
            let snap_text = state.buf.to_text();
            let snap_chat = state.chat.clone();
            let handle = tokio::spawn(async move {
                let mut pseudo = EditorState {
                    path: snap_path,
                    buf: EditorBuffer::from_text(&snap_text),
                    ring: VersionRing::default(),
                    chat: snap_chat,
                    composer: String::new(),
                    focus: EditorFocus::File,
                    auto: snap_auto,
                    allow_persist: false,
                    meta_dir: PathBuf::new(),
                    dirty: false,
                    last_edit: Instant::now(),
                    scroll: 0,
                    wrap_cols: 40,
                    rows: vec![],
                    chat_scroll: 0,
                    quiet_since: Instant::now(),
                    suggest_done: true,
                    pending: None,
                    status: String::new(),
                    busy: true,
                };
                pseudo.buf.row = snap_row;
                EditorJobResult::Reply {
                    reply: call_editor_model(&cfg_c, &pseudo, None, true).await,
                    as_suggest: true,
                }
            });
            pending_job = Some(handle);
        }

        match split_frame::poll_key(POLL) {
            None => continue,
            Some(Key::Esc) | Some(Key::CtrlC) => {
                if let Some(h) = pending_job.take() {
                    h.abort();
                }
                let _ = state.flush_save();
                break;
            }
            Some(Key::Tab) => {
                let _ = state.flush_save();
                state.focus = match state.focus {
                    EditorFocus::File => EditorFocus::Chat,
                    EditorFocus::Chat => EditorFocus::File,
                };
                state.note_activity();
            }
            Some(Key::CtrlA) => {
                // Compactify (not auto — auto is typed `auto`; bare `c` must stay a letter).
                start_compactify(cfg, &mut state, &mut pending_job);
            }
            Some(Key::CtrlZ) => state.undo(),
            Some(Key::CtrlY) => state.redo(),
            Some(Key::CtrlK) => {
                if state.pending.is_some() {
                    state.dismiss_pending();
                } else {
                    // Clear editor chat context (file buffer kept).
                    state.clear_chat();
                }
            }
            Some(Key::CtrlS) => {
                if !state.composer.trim().is_empty() {
                    send_composer(cfg, &mut state, &mut pending_job).await;
                } else {
                    let _ = state.flush_save();
                    state.status = "saved".into();
                }
            }
            Some(Key::Enter) => {
                if state.focus == EditorFocus::Chat {
                    if !state.composer.trim().is_empty() {
                        send_composer(cfg, &mut state, &mut pending_job).await;
                    } else if state.pending.is_some() {
                        state.accept_pending();
                    } else {
                        state.note_activity();
                    }
                } else {
                    state.buf.insert_newline();
                    state.mark_edited();
                }
            }
            Some(Key::Paste(text)) => {
                if state.focus == EditorFocus::Chat {
                    state.composer.push_str(&text.replace('\n', " "));
                } else {
                    state.buf.insert_str(&text);
                    state.mark_edited();
                }
                state.note_activity();
            }
            Some(Key::Backspace) => {
                if state.focus == EditorFocus::Chat {
                    state.composer.pop();
                } else {
                    state.buf.backspace();
                    state.mark_edited();
                }
                state.note_activity();
            }
            Some(Key::Up) => {
                if state.focus == EditorFocus::Chat {
                    state.chat_scroll = state.chat_scroll.saturating_add(1);
                } else {
                    state.buf.move_up();
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::Down) => {
                if state.focus == EditorFocus::Chat {
                    state.chat_scroll = state.chat_scroll.saturating_sub(1);
                } else {
                    state.buf.move_down();
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::Left) => {
                if state.focus == EditorFocus::File {
                    state.buf.move_left();
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::Right) => {
                if state.focus == EditorFocus::File {
                    state.buf.move_right();
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::PageUp) => {
                if state.focus == EditorFocus::File {
                    let page = file_body_rows(
                        crate::term::term_size().0.clamp(8, crate::term::MAX_ROWS),
                    ) as isize;
                    state.buf.move_page(-page);
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::PageDown) => {
                if state.focus == EditorFocus::File {
                    let page = file_body_rows(
                        crate::term::term_size().0.clamp(8, crate::term::MAX_ROWS),
                    ) as isize;
                    state.buf.move_page(page);
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::Home) => {
                if state.focus == EditorFocus::File {
                    state.buf.move_home();
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::End) => {
                if state.focus == EditorFocus::File {
                    state.buf.move_end();
                    state.relayout();
                }
                state.note_activity();
            }
            Some(Key::Char(c)) => {
                if state.focus == EditorFocus::Chat {
                    // Always insert letters — compactify is `compact` / Ctrl chord, not bare `c`.
                    if !c.is_control() {
                        state.composer.push(c);
                        state.note_activity();
                    }
                } else if !c.is_control() {
                    state.buf.insert_char(c);
                    state.mark_edited();
                } else {
                    state.note_activity();
                }
            }
            Some(_) => {}
        }
    }

    Ok(())
}

fn start_compactify(
    cfg: &Config,
    state: &mut EditorState,
    pending_job: &mut Option<tokio::task::JoinHandle<EditorJobResult>>,
) {
    if pending_job.is_some() || state.busy {
        state.status = "busy…".into();
        return;
    }
    if state.chat.is_empty() {
        state.status = "nothing to compactify".into();
        return;
    }
    state.busy = true;
    state.status = "compactify…".into();
    state.note_activity();
    let cfg_c = cfg.clone();
    let chat = state.chat.clone();
    let handle = tokio::spawn(async move {
        EditorJobResult::Compactify(call_compactify(&cfg_c, &chat).await)
    });
    *pending_job = Some(handle);
}

async fn send_composer(
    cfg: &Config,
    state: &mut EditorState,
    pending_job: &mut Option<tokio::task::JoinHandle<EditorJobResult>>,
) {
    if pending_job.is_some() {
        state.status = "busy…".into();
        return;
    }
    let text = state.composer.trim().to_string();
    if text.is_empty() {
        return;
    }
    state.composer.clear();

    // Local commands — no model call.
    let lower = text.to_ascii_lowercase();
    if lower == "clear" {
        state.clear_chat();
        return;
    }
    if lower == "auto" {
        state.auto = !state.auto;
        state.status = format!("auto:{}", if state.auto { "on" } else { "off" });
        state.note_activity();
        return;
    }
    if lower == "undo" {
        state.undo();
        return;
    }
    if lower == "redo" {
        state.redo();
        return;
    }
    if lower == "compact" || lower == "compactify" {
        start_compactify(cfg, state, pending_job);
        return;
    }

    state.push_chat(true, &text);
    state.busy = true;
    state.status = "thinking…".into();
    state.note_activity();

    let as_suggest = state.auto;
    let cfg_c = cfg.clone();
    let snap_path = state.path.clone();
    let snap_auto = state.auto;
    let snap_row = state.buf.row;
    let snap_text = state.buf.to_text();
    let snap_chat = state.chat.clone();
    let user_msg = text.clone();
    let handle = tokio::spawn(async move {
        let mut pseudo = EditorState {
            path: snap_path,
            buf: EditorBuffer::from_text(&snap_text),
            ring: VersionRing::default(),
            chat: snap_chat,
            composer: String::new(),
            focus: EditorFocus::Chat,
            auto: snap_auto,
            allow_persist: false,
            meta_dir: PathBuf::new(),
            dirty: false,
            last_edit: Instant::now(),
            scroll: 0,
            wrap_cols: 40,
            rows: vec![],
            chat_scroll: 0,
            quiet_since: Instant::now(),
            suggest_done: true,
            pending: None,
            status: String::new(),
            busy: true,
        };
        pseudo.buf.row = snap_row;
        EditorJobResult::Reply {
            reply: call_editor_model(&cfg_c, &pseudo, Some(&user_msg), as_suggest).await,
            as_suggest,
        }
    });
    *pending_job = Some(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_backspace_newline_clamp() {
        let mut buf = EditorBuffer::from_text("ab\ncd\n");
        buf.row = 0;
        buf.col = 1;
        buf.insert_char('X');
        assert_eq!(buf.lines[0], "aXb");
        buf.backspace();
        assert_eq!(buf.lines[0], "ab");
        buf.col = 2;
        buf.insert_newline();
        assert_eq!(buf.lines, vec!["ab".to_string(), String::new(), "cd".to_string()]);
        buf.row = 99;
        buf.col = 99;
        buf.clamp_cursor();
        assert_eq!(buf.row, 2);
        assert_eq!(buf.col, 2);
    }

    #[test]
    fn apply_edits_and_refuse_bad_lines() {
        let mut buf = EditorBuffer::from_text("one\ntwo\nthree\n");
        let edits = vec![
            LineEdit {
                line: 2,
                op: "replace".into(),
                text: Some("TWO".into()),
            },
            LineEdit {
                line: 99,
                op: "delete".into(),
                text: None,
            },
            LineEdit {
                line: 1,
                op: "insert_after".into(),
                text: Some("1.5".into()),
            },
            LineEdit {
                line: 1,
                op: "explode".into(),
                text: None,
            },
            LineEdit {
                line: 1,
                op: "replace|delete|insert_after".into(),
                text: Some("nope".into()),
            },
        ];
        let report = apply_edits(&mut buf, &edits);
        assert_eq!(report.applied, 2);
        assert_eq!(report.refused.len(), 3);
        assert_eq!(buf.lines, vec!["one".to_string(), "1.5".into(), "TWO".into(), "three".into()]);
    }

    #[test]
    fn replace_multiline_expands_lines() {
        let mut buf = EditorBuffer::from_text("\n");
        let edits = vec![LineEdit {
            line: 1,
            op: "replace".into(),
            text: Some("def fib(n):\n    return n".into()),
        }];
        let report = apply_edits(&mut buf, &edits);
        assert_eq!(report.applied, 1);
        assert_eq!(buf.lines, vec!["def fib(n):".to_string(), "    return n".into()]);
    }

    #[test]
    fn normalize_op_rejects_schema_parrot() {
        assert_eq!(normalize_op("replace"), Some("replace"));
        assert_eq!(normalize_op("insert"), Some("insert_after"));
        assert!(normalize_op("replace|delete|insert_after").is_none());
        assert!(reply_needs_repair(&EditorReply {
            say: "short reply".into(),
            content: None,
            edits: vec![LineEdit {
                line: 1,
                op: "replace|delete|insert_after".into(),
                text: Some("x".into()),
            }],
        }));
    }

    #[test]
    fn suggest_does_not_mutate_until_accept() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let mut state = EditorState::open(&path, true).unwrap();
        let original = state.buf.to_text();
        state.pending = Some(PendingProposal {
            say: "change it".into(),
            content: None,
            edits: vec![LineEdit {
                line: 1,
                op: "replace".into(),
                text: Some("ALPHA".into()),
            }],
        });
        assert_eq!(state.buf.to_text(), original);
        state.accept_pending();
        assert!(state.buf.lines[0].starts_with("ALPHA"));
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(disk.starts_with("ALPHA"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_meter_grows_with_chat_and_drops_on_clear() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-ctx-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "x\n").unwrap();
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_SINE_DIR", dir.join(".sine"));
        let mut state = EditorState::open(&path, true).unwrap();
        let base = state.context_tokens();
        assert!(state.context_label().starts_with("ctx "));
        assert!(state.context_label().contains("est "));
        state.push_chat(true, &"word ".repeat(200));
        let grown = state.context_tokens();
        assert!(grown > base, "expected chat to grow ctx: {base} -> {grown}");
        state.clear_chat();
        assert!(
            state.context_tokens() <= base + 2,
            "clear should drop meter near baseline"
        );
        std::env::remove_var("ENGRAM_SINE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_chat_keeps_file_and_compactify_replaces_transcript() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-clear-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "keep-me\n").unwrap();
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_SINE_DIR", dir.join(".sine"));
        let mut state = EditorState::open(&path, true).unwrap();
        state.push_chat(true, "hello");
        state.push_chat(false, "world");
        assert_eq!(state.chat.len(), 2);
        state.clear_chat();
        assert!(state.chat.is_empty());
        assert_eq!(state.buf.lines[0], "keep-me");
        assert_eq!(state.status, "context cleared");

        state.push_chat(true, "long discussion");
        state.push_chat(false, "many details");
        state.apply_compactify("short summary".into());
        assert_eq!(state.chat.len(), 1);
        assert!(state.chat[0].user);
        assert_eq!(state.chat[0].text, "short summary");
        assert_eq!(state.status, "compactified");
        std::env::remove_var("ENGRAM_SINE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_file_creates_missing_path() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-ensure-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("nested").join("new.py");
        assert!(!path.exists());
        ensure_file(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        // Idempotent on existing file.
        std::fs::write(&path, "keep\n").unwrap();
        ensure_file(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_rewrite_and_sanitized_json_parse() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-content-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.py");
        std::fs::write(&path, "old\n").unwrap();
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_SINE_DIR", dir.join(".sine"));
        let mut state = EditorState::open(&path, true).unwrap();
        let reply = EditorReply {
            say: "fib".into(),
            content: Some("def fib():\n    print(0)\n".into()),
            edits: vec![],
        };
        let note = state.apply_reply_changes(&reply);
        assert!(note.contains("rewrote"));
        assert_eq!(state.buf.lines[0], "def fib():");

        // Raw newline inside a JSON string should still parse after sanitize.
        let messy = "{\"say\":\"ok\",\"content\":\"a\nb\"}";
        let parsed = parse_editor_reply(messy).unwrap();
        assert_eq!(parsed.content.as_deref(), Some("a\nb"));

        let fenced = "here\n```python\nx = 1\n```\n";
        let from_fence = parse_editor_reply(fenced).unwrap();
        assert_eq!(from_fence.content.as_deref(), Some("x = 1"));

        std::env::remove_var("ENGRAM_SINE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ring_undo_redo_and_drop_redo() {
        let mut ring = VersionRing::default();
        ring.push("a\n");
        ring.push("b\n");
        ring.push("c\n");
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.undo(), Some("b\n"));
        assert_eq!(ring.undo(), Some("a\n"));
        assert_eq!(ring.redo(), Some("b\n"));
        ring.push("d\n");
        assert_eq!(ring.redo(), None);
        assert_eq!(ring.snapshots.last().map(String::as_str), Some("d\n"));
    }

    #[test]
    fn autosave_writes_path() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-save-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("n.txt");
        std::fs::write(&path, "old\n").unwrap();
        // Point meta under temp via ENGRAM_SINE_DIR
        let sine = dir.join(".sine");
        std::fs::create_dir_all(&sine).unwrap();
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_SINE_DIR", &sine);
        let mut state = EditorState::open(&path, true).unwrap();
        state.buf.lines[0] = "new".into();
        state.dirty = true;
        state.flush_save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert!(state.ring.len() >= 2);
        std::env::remove_var("ENGRAM_SINE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stub_json_parses() {
        let raw = "```json\n{\"say\":\"hi\",\"edits\":[{\"line\":1,\"op\":\"replace\",\"text\":\"x\"}]}\n```";
        let reply = parse_editor_reply(raw).unwrap();
        assert_eq!(reply.say, "hi");
        assert_eq!(reply.edits.len(), 1);
        assert_eq!(reply.edits[0].op, "replace");
    }

    #[test]
    fn coerce_whole_file_replace_avoids_duplication() {
        let old = "def phi(n): return n\n\ndef fib(n):\n    pass\n";
        let line_count = EditorBuffer::from_text(old).lines.len();
        let new_file = "def phi(n):\n    return 1.618\n\ndef fib(n):\n    a,b=0,1\n    return a\n";
        let reply = coerce_reply_for_apply(
            EditorReply {
                say: "fixed".into(),
                content: None,
                edits: vec![LineEdit {
                    line: 1,
                    op: "replace".into(),
                    text: Some(new_file.into()),
                }],
            },
            line_count,
        );
        assert!(reply.content.is_some());
        assert!(reply.edits.is_empty());

        // Without coerce, multiline replace leaves old lines → duplication.
        let mut bad = EditorBuffer::from_text(old);
        let _ = apply_edits(
            &mut bad,
            &[LineEdit {
                line: 1,
                op: "replace".into(),
                text: Some(new_file.into()),
            }],
        );
        assert!(bad.lines.len() > split_lines(new_file).len());

        let state_lines = EditorBuffer::from_text(reply.content.as_deref().unwrap()).lines;
        assert_eq!(state_lines[0], "def phi(n):");
        assert!(!state_lines.iter().skip(1).any(|l| l == "def phi(n): return n"));
        assert_eq!(state_lines.len(), split_lines(new_file).len());
    }

    #[test]
    fn triple_quoted_envelope_does_not_write_json_to_file() {
        let raw = r#"{
 "say": "implemented fib",
 "content": """
def fib(n):
    a, b = 0, 1
    for _ in range(n):
        print(a)
        a, b = b, a + b

fib(10)
"""
}"#;
        let reply = parse_editor_reply(raw).unwrap();
        assert_eq!(reply.say, "implemented fib");
        let content = reply.content.unwrap();
        assert!(content.starts_with("def fib(n):"), "{content}");
        assert!(!content.trim_start().starts_with('{'));
        assert!(!content.contains("\"say\""));

        let fenced = format!("```\n{raw}\n```");
        let from_fence = parse_editor_reply(&fenced).unwrap();
        let c2 = from_fence.content.unwrap();
        assert!(c2.starts_with("def fib(n):"), "{c2}");
        assert!(!looks_like_editor_envelope(&c2));
    }

    #[test]
    fn unwrap_nested_envelope_on_apply() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-unwrap-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.py");
        std::fs::write(&path, "old\n").unwrap();
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_SINE_DIR", dir.join(".sine"));
        let mut state = EditorState::open(&path, true).unwrap();
        let envelope = "{\n \"say\": \"x\",\n \"content\": \"\"\"\nhello()\n\"\"\"\n}";
        let note = state.apply_reply_changes(&EditorReply {
            say: "x".into(),
            content: Some(envelope.into()),
            edits: vec![],
        });
        assert!(note.contains("rewrote"));
        assert_eq!(state.buf.lines[0], "hello()");
        assert!(!state.buf.lines[0].starts_with('{'));
        // Undo restores the pre-AI buffer.
        state.undo();
        assert_eq!(state.buf.lines[0], "old");
        std::env::remove_var("ENGRAM_SINE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_wins_over_edits() {
        let reply = coerce_reply_for_apply(
            EditorReply {
                say: "x".into(),
                content: Some("only\n".into()),
                edits: vec![LineEdit {
                    line: 1,
                    op: "insert_after".into(),
                    text: Some("dup".into()),
                }],
            },
            3,
        );
        assert_eq!(reply.content.as_deref(), Some("only\n"));
        assert!(reply.edits.is_empty());
    }

    #[test]
    fn build_payload_skips_duplicate_ask_and_ui_notes() {
        let dir = std::env::temp_dir().join(format!(
            "sine-ed-payload-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.py");
        std::fs::write(&path, "def phi(n): return n\n").unwrap();
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_SINE_DIR", dir.join(".sine"));
        let mut state = EditorState::open(&path, true).unwrap();
        state.push_chat(true, "fix phi");
        state.push_chat(false, "implemented phi\nrewrote file");
        let ask = "really fix phi";
        state.push_chat(true, ask);

        let payload = build_editor_user_payload(&state, Some(ask), false);
        // Latest ask appears once (under operator:), not also in prior.
        assert_eq!(payload.matches("really fix phi").count(), 1);
        assert!(payload.contains("prior conversation"));
        assert!(payload.contains("operator: fix phi"));
        assert!(payload.contains("assistant: implemented phi"));
        assert!(!payload.contains("rewrote file"));
        assert!(payload.contains("def phi(n): return n"));
        assert!(payload.contains("source of truth") || payload.contains("current on disk"));

        std::env::remove_var("ENGRAM_SINE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());
}
