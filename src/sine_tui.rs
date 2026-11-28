//! SINE — semi-persistent plan-then-approve assistant (`:sine` / bare `sine`).
//!
//! Dependency surface is ADR-0014. Layout chrome is [`crate::split_frame`].

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use engram_cmdfreq::{format_top, FrequencyStore};
use serde::{Deserialize, Serialize};
use shomagent::daemon::rpc;
use shomagent::protocol::{Request, Response};

use crate::chrome::{self, Avatar};
use crate::config::Config;
use crate::llm::{ChatMessage, LlmClient};
use crate::modes::Mode;
use crate::split_frame::{self, Key, PaneFocus, Slot, SplitFrame};
use crate::term::RawGuard;
use crate::ui;

/// Classic braille bunny (docs/sine_image.txt → assets/).
const SINE_IMAGE: &str = include_str!("../assets/sine_image.txt");
const BRAND_NAVI: &str = include_str!("../assets/brand/navi.txt");
const BRAND_COFFEE_REST: &str = include_str!("../assets/brand/coffee-rest.txt");
const BRAND_COFFEE_BUSY: &str = include_str!("../assets/brand/coffee-busy.txt");
const BRAND_LAPTOP_REST: &str = include_str!("../assets/brand/laptop-rest.txt");
const BRAND_LAPTOP_BUSY: &str = include_str!("../assets/brand/laptop-busy.txt");
const BRAND_MANA_REST: &str = include_str!("../assets/brand/mana-rest.txt");
const BRAND_MANA_BUSY: &str = include_str!("../assets/brand/mana-busy.txt");
const BRAND_LEAF_REST: &str = include_str!("../assets/brand/leaf-rest.txt");
const BRAND_LEAF_BUSY: &str = include_str!("../assets/brand/leaf-busy.txt");

const PLAN_SYSTEM: &str = "\
You are SINE, a planning assistant inside Shomsky Linux (BusyBox ash, Alpine).
Prefer first-party Shomsky tools over inventing curl/grep scrapers.

CLOSED verbs — the \"verb\" field must be EXACTLY one of these five strings:
  shell | shomagent | top_commands | say | quickyn
WRONG (never use these as verb): \"shomagent web\", \"shomagent ask\", \"shomagent pause\",
\"shomagent read\", \"curl\", \"web\", \"schedule\". Those are CLI words inside a \
shell command, not verbs.

First-party CLI (only via verb shell, args.command):
  shomagent web <url>     headless Chromium; use for any webpage / JS / headlines
  shomagent / shomagent list / shomagent roster
                          list live members and ids (REQUIRED after web, before ask)
  shomagent ask <id> \"…\"  question a member (web page, watch, document)
  shomagent stop <id>
  shomagent click|type|navigate <id> …
  shomagent watch|spawn|read|readwrite|dream|refine …
  shomloop \"<frequency>\" <command>
                          ONLY via verb shell, and ONLY when the user asked for
                          a schedule / loop / daily / cron. Frequency phrases
                          match shomloop (e.g. \"daily at 4am\"). Prefer pointing
                          shomloop at a job script file under /engram/user/ \
(not a giant inline sh -c).

Verb args:
  shell: {\"command\": \"...\"}
  shomagent: {\"action\": \"ask|pause|resume|stop|roster\", \"id\": <u32 optional>, \"message\": \"... optional\"}
  top_commands: {\"limit\": <n optional>}
  say: {\"text\": \"...\"}
  quickyn: {\"question\": \"...\", \"file\": \"... optional path for context\"}

Member ids (critical):
- NEVER invent or reuse numeric ids from chat history or earlier sessions.
- Interactive (in-plan) web→ask: after every `shomagent web`, list members \
(roster/`list`), then ask/stop with id 0. SINE binds 0 (or stale ids) to the \
live roster member. Do not guess 1, 2, 5, etc.
- Job scripts for shomloop/cron: parse the live id from attach output \
(`shomagent#N`) or `shomagent list` inside the script. Never hardcode ids. \
SINE id-0 binding does NOT apply inside crond.

ONE-SHOT webpage (no schedule asked) — copy this shape (3–6 steps):
{\"summary\":\"Fetch HN top 10 headlines to CSV once\",\"steps\":[\
{\"verb\":\"shell\",\"args\":{\"command\":\"shomagent web https://news.ycombinator.com\"}},\
{\"verb\":\"shomagent\",\"args\":{\"action\":\"roster\"}},\
{\"verb\":\"shell\",\"args\":{\"command\":\"shomagent ask 0 \\\"list top 10 story titles and urls as CSV with header rank,title,url\\\"\"}},\
{\"verb\":\"shell\",\"args\":{\"command\":\"sh -c 'ts=$(date -u +%Y%m%dT%H%M%SZ); shomagent ask 0 \\\"emit only the CSV\\\" > /engram/user/yc_headlines_$ts.csv'\"}},\
{\"verb\":\"shell\",\"args\":{\"command\":\"shomagent stop 0\"}}\
]}

SCHEDULED / daily / loop (REQUIRED shape when the user asks for a schedule):
Write a self-contained job script under /engram/user/, chmod +x, run it once \
if they also asked to test now, then register shomloop on that script path. \
Do NOT emit only an interactive web→ask plan when a schedule was requested.
{\"summary\":\"HN top 10 daily at 4am + test now\",\"steps\":[\
{\"verb\":\"shell\",\"args\":{\"command\":\"cat > /engram/user/yc_headlines_job.sh <<'EOF'\\n#!/bin/sh\\nset -e\\nout=$(shomagent web https://news.ycombinator.com)\\nid=$(printf '%s\\\\n' \\\"$out\\\" | sed -n 's/.*shomagent#\\\\([0-9][0-9]*\\\\).*/\\\\1/p' | tail -n 1)\\ntest -n \\\"$id\\\"\\nts=$(date -u +%Y%m%dT%H%M%SZ)\\nshomagent ask \\\"$id\\\" \\\"list top 10 story titles and urls as CSV with header rank,title,url\\\" > /engram/user/yc_headlines_$ts.csv\\nshomagent stop \\\"$id\\\"\\nEOF\"}},\
{\"verb\":\"shell\",\"args\":{\"command\":\"chmod +x /engram/user/yc_headlines_job.sh\"}},\
{\"verb\":\"shell\",\"args\":{\"command\":\"/engram/user/yc_headlines_job.sh\"}},\
{\"verb\":\"shell\",\"args\":{\"command\":\"shomloop \\\"daily at 4am\\\" /engram/user/yc_headlines_job.sh\"}}\
]}

Rules:
- Opening a URL is always verb shell with command shomagent web <url> (or inside a job script).
- Do NOT use curl|wget for HTML/JS pages when shomagent web fits.
- Large sites (cnn.com, etc.): ask for ONE short fact (\"most recent headline title and url only\"). \
Never ask the model to dump the whole page. Prefer few short asks over one huge ask.
- Do NOT invent pause/resume loops or duplicate web/ask pairs.
- Do NOT invent shomloop unless the user asked for a schedule / daily / loop.
- When the user asks for a schedule (daily / loop / cron / \"at 4am\") — even if they \
also say \"right now\" / \"test\": you MUST include (1) write job script, (2) chmod, \
(3) run script once if they asked to test now, (4) shell `shomloop \"<frequency>\" \
/engram/user/<job>.sh`. Omitting shomloop when a schedule was requested is WRONG.
- Prefer a durable script path under /engram/user/ (survives for cron); avoid \
relying on /tmp for the scheduled job body.
- For yes/no use verb quickyn (not shell).
- Local file edits: prefer a heredoc write (`cat > path <<'EOF'…EOF`), not \
grep -v rewrites or shomagent ask on a file path.
- Inside JSON string values, escape every double-quote as \\\" (shell may still \
use single quotes).
- Output ONLY one JSON object. No markdown fences. Prefer few steps.";

/// Max completion tokens for a SINE plan. 512 truncates multi-step web plans
/// mid-JSON (`EOF while parsing a list`).
const PLAN_MAX_TOKENS: u32 = 1536;

const IDLE_SYSTEM: &str = "\
You are SINE idle mode. Given the operator's most-used shell commands, write a
short playbook (markdown, under 20 lines) on using first-party tools better:
shomagent (web, watch, ask, read/readwrite), shomloop, quickyn, :sine, :top.
Prefer shomagent web over curl for pages. Be concrete. No preamble.";

const STUB_SUMMARY: &str = "stub: list top commands then say hello";
/// Quiet time after the last fielded request (or keystroke) before idle playbook.
/// Does not count while planning/running — the timer restarts when that work ends.
const IDLE_QUIET: Duration = Duration::from_secs(60);
/// Periodic roster refresh when idle (twice a minute). Also refreshed on
/// plan step boundaries when commands kick off / return.
const ROSTER_TICK: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// Same glyphs as `chat_tui` / `spinner.rs` (Alpine ASCII-only).
const SPIN_FRAMES: &[char] = &['-', '\\', '|', '/'];
const SPIN_TICK: Duration = Duration::from_millis(120);
/// How long each bunny eye pose is held while planning/running.
const EYE_POSE_TICK: Duration = Duration::from_millis(350);
/// Resting eyeball cells on the face line (0-based row/col into `SINE_IMAGE`).
const EYE_ROW: usize = 8;
const EYE_LEFT_COL: usize = 10;
const EYE_RIGHT_COL: usize = 13;
/// Face-interior blank (same as surrounding braille voids in the art).
const BRAILLE_BLANK: char = '\u{2800}';
/// Busy eye offsets: (d_row, d_col) applied to both resting eyes.
/// Cycle returns to center between glances so the bounce reads clearly.
const EYE_POSES: &[(isize, isize)] = &[
    (0, 0),  // center
    (-1, 0), // up
    (0, 0),  // center
    (0, -1), // left
    (0, 0),  // center
    (0, 1),  // right
    (0, 0),  // center
    (1, 0),  // down
];

// ── Plan ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    pub verb: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SinePlan {
    pub summary: String,
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

impl SinePlan {
    pub fn format_for_pane(&self) -> String {
        let mut out = format!("PLAN: {}\n", self.summary);
        for (i, step) in self.steps.iter().enumerate() {
            out.push_str(&format!("  {}. {} {}\n", i + 1, step.verb, step.args));
        }
        out.push_str("approve/a · edit/e · cancel/q");
        out
    }

    /// Text loaded into the command line for `edit` / `e` (single line).
    pub fn format_for_edit(&self) -> String {
        let mut parts = Vec::new();
        parts.push(self.summary.clone());
        for step in &self.steps {
            match step.verb.as_str() {
                "shell" => {
                    if let Some(cmd) = step.args.get("command").and_then(|v| v.as_str()) {
                        parts.push(cmd.to_string());
                    } else {
                        parts.push(format!("shell {}", step.args));
                    }
                }
                "say" => {
                    let text = step
                        .args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    parts.push(format!("say: {text}"));
                }
                other => parts.push(format!("{other} {}", step.args)),
            }
        }
        parts.join(" ; ")
    }
}

/// Estimate tokens for SINE **session** working memory (chat, agent log,
/// pending plan, in-memory playbook). Same chars/4 heuristic as `:chat`.
///
/// The fixed plan system prompt is **not** counted here: `^K` / `clear` must
/// leave the agent/chat meters at `ctx 0 tok` when the session is empty.
pub fn estimate_sine_tokens(session: &SineSession) -> usize {
    let mut n = 0usize;
    for turn in &session.chat {
        let role = if turn.user { "user" } else { "assistant" };
        n += role.chars().count() + turn.text.chars().count();
    }
    for line in &session.agent_log {
        n += line.chars().count();
    }
    if let Some(plan) = &session.pending_plan {
        n += "plan".chars().count() + plan.summary.chars().count();
        for step in &plan.steps {
            n += step.verb.chars().count() + step.args.to_string().chars().count();
        }
    }
    if !session.playbook.is_empty() {
        n += "playbook".chars().count() + session.playbook.chars().count();
    }
    n / 4
}

pub fn parse_plan_json(raw: &str) -> Result<SinePlan> {
    let json_str = extract_json_object(raw).unwrap_or(raw).trim();
    let mut plan = deserialize_sine_plan(json_str).with_context(|| {
        format!(
            "deserialize sine plan; raw={}",
            truncate_for_err(raw, 240)
        )
    })?;
    if plan.steps.is_empty() && plan.summary.trim().is_empty() {
        bail!("empty plan");
    }
    let mut steps = Vec::with_capacity(plan.steps.len());
    for step in plan.steps.drain(..) {
        steps.push(normalize_plan_step(step)?);
    }
    plan.steps = steps;
    Ok(plan)
}

fn truncate_for_err(s: &str, max: usize) -> String {
    let t = s.trim();
    let count = t.chars().count();
    if count <= max {
        return t.to_string();
    }
    format!("{}…", t.chars().take(max).collect::<String>())
}

fn deserialize_sine_plan(json_str: &str) -> Result<SinePlan> {
    match serde_json::from_str::<SinePlan>(json_str) {
        Ok(p) => Ok(p),
        Err(e) => {
            // Models emit shell-ish `\;` and bare `"` inside "command" values.
            let repaired = repair_invalid_json_escapes(json_str);
            let repaired = repair_command_field_quotes(&repaired);
            let repaired = sanitize_trailing_commas(&repaired);
            match serde_json::from_str::<SinePlan>(&repaired) {
                Ok(p) => Ok(p),
                Err(_) => Err(e).context("after escape/quote repair"),
            }
        }
    }
}

/// Escape raw `"` inside `"command":"..."` values. Models write
/// `"command":"grep -v "print""` and the inner quotes end the string early.
fn repair_command_field_quotes(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut out = String::with_capacity(json.len() + 8);
    let mut i = 0;
    while i < bytes.len() {
        // Match "command" key (with surrounding quotes already in input).
        if i + 9 <= bytes.len() && &bytes[i..i + 9] == b"\"command\"" {
            out.push_str("\"command\"");
            i += 9;
            // Copy whitespace + colon + whitespace up to the opening quote.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                out.push(bytes[i] as char);
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b':' {
                out.push(':');
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                out.push(bytes[i] as char);
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'"' {
                continue;
            }
            out.push('"');
            i += 1;
            // Escape bare quotes until a quote that really ends the value
            // (followed by whitespace then `,` or `}`).
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'\\' {
                    out.push('\\');
                    if i + 1 < bytes.len() {
                        out.push(bytes[i + 1] as char);
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                if c == b'"' {
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let ends = j >= bytes.len()
                        || bytes[j] == b','
                        || bytes[j] == b'}';
                    if ends {
                        out.push('"');
                        i += 1;
                        break;
                    }
                    out.push_str("\\\"");
                    i += 1;
                    continue;
                }
                out.push(c as char);
                i += 1;
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
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
                        && bytes[i + 2..=i + 5].iter().all(|b| b.is_ascii_hexdigit());
                    if hex_ok {
                        out.push_str(&s[i..i + 6]);
                        i += 6;
                    } else {
                        out.push_str("\\\\");
                        i += 1;
                    }
                }
                _ => {
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

/// Drop trailing commas before `}` / `]`, outside of strings.
fn sanitize_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut cleaned = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_str = false;
    let mut esc = false;
    while i < chars.len() {
        let ch = chars[i];
        if esc {
            cleaned.push(ch);
            esc = false;
            i += 1;
            continue;
        }
        if in_str {
            if ch == '\\' {
                cleaned.push(ch);
                esc = true;
            } else if ch == '"' {
                cleaned.push(ch);
                in_str = false;
            } else {
                cleaned.push(ch);
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            cleaned.push(ch);
            in_str = true;
            i += 1;
            continue;
        }
        if ch == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        cleaned.push(ch);
        i += 1;
    }
    cleaned
}

/// Rewrite common LLM mistakes (`"verb":"shomagent web"`, `"shomagent ask"`, …)
/// into the closed ADR-0014 verb set.
fn normalize_plan_step(step: PlanStep) -> Result<PlanStep> {
    let verb = step.verb.trim();
    match verb {
        "shell" | "shomagent" | "top_commands" | "say" | "quickyn" => Ok(PlanStep {
            verb: verb.to_string(),
            args: step.args,
        }),
        "web" => Ok(shell_shomagent_web(&step.args)),
        other if other.starts_with("shomagent ") => {
            let action = other["shomagent ".len()..].trim();
            normalize_shomagent_compound(action, &step.args)
        }
        other => bail!("unknown verb '{other}'"),
    }
}

fn shell_shomagent_web(args: &serde_json::Value) -> PlanStep {
    let url = args
        .get("url")
        .or_else(|| args.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let command = if url.is_empty() {
        "shomagent web".to_string()
    } else if url.starts_with("shomagent ") {
        url.to_string()
    } else {
        format!("shomagent web {url}")
    };
    PlanStep {
        verb: "shell".into(),
        args: serde_json::json!({ "command": command }),
    }
}

fn normalize_shomagent_compound(action: &str, args: &serde_json::Value) -> Result<PlanStep> {
    match action {
        "web" => Ok(shell_shomagent_web(args)),
        "ask" | "pause" | "resume" | "stop" | "roster" | "list" => {
            let action = if action == "list" { "roster" } else { action };
            let mut map = serde_json::Map::new();
            map.insert("action".into(), serde_json::json!(action));
            if let Some(id) = args.get("id") {
                map.insert("id".into(), id.clone());
            }
            if let Some(msg) = args
                .get("message")
                .or_else(|| args.get("text"))
                .or_else(|| args.get("question"))
            {
                map.insert("message".into(), msg.clone());
            }
            Ok(PlanStep {
                verb: "shomagent".into(),
                args: serde_json::Value::Object(map),
            })
        }
        // CLI-only surfaces → shell so the real binary runs.
        "read" | "readwrite" | "watch" | "spawn" | "dream" | "refine" | "click" | "type"
        | "navigate" | "write" => {
            let command = shomagent_shell_command(action, args);
            Ok(PlanStep {
                verb: "shell".into(),
                args: serde_json::json!({ "command": command }),
            })
        }
        other => bail!("unknown verb 'shomagent {other}'"),
    }
}

fn shomagent_shell_command(action: &str, args: &serde_json::Value) -> String {
    let mut parts = vec!["shomagent".to_string(), action.to_string()];
    if let Some(id) = args.get("id").and_then(|v| v.as_u64()) {
        parts.push(id.to_string());
    }
    if let Some(path) = args
        .get("file")
        .or_else(|| args.get("path"))
        .and_then(|v| v.as_str())
    {
        parts.push(path.to_string());
    }
    if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
        parts.push(url.to_string());
    }
    if let Some(text) = args
        .get("text")
        .or_else(|| args.get("message"))
        .and_then(|v| v.as_str())
    {
        parts.push(format!("\"{}\"", text.replace('"', "\\\"")));
    }
    if let Some(limit) = args.get("limit").and_then(|v| v.as_u64()) {
        parts.push(limit.to_string());
    }
    parts.join(" ")
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

fn stub_plan(intent: &str) -> SinePlan {
    SinePlan {
        summary: format!("{STUB_SUMMARY} ({intent})"),
        steps: vec![
            PlanStep {
                verb: "top_commands".into(),
                args: serde_json::json!({"limit": 5}),
            },
            PlanStep {
                verb: "say".into(),
                args: serde_json::json!({"text": "stub hello from SINE"}),
            },
        ],
    }
}

fn sine_stub() -> bool {
    matches!(
        std::env::var("ENGRAM_SINE_STUB").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

// ── Session ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatTurn {
    pub user: bool,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SineSession {
    pub chat: Vec<ChatTurn>,
    pub agent_log: Vec<String>,
    pub input_history: Vec<String>,
    pub vertical_ratio: f32,
    pub left_h_ratio: f32,
    pub right_h_ratio: f32,
    pub auto: bool,
    pub pending_plan: Option<SinePlan>,
    pub playbook: String,
    /// Paths this SINE session has touched (shell writes, session files, …).
    #[serde(default)]
    pub touched_files: Vec<String>,
}

impl SineSession {
    pub fn defaults() -> Self {
        Self {
            vertical_ratio: 0.35,
            left_h_ratio: 0.45,
            right_h_ratio: 0.55,
            ..Default::default()
        }
    }
}

pub fn default_sine_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ENGRAM_SINE_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    Mode::Agent.home_dir().join(".sine")
}

pub fn load_session(dir: &Path) -> SineSession {
    let path = dir.join("session.json");
    let mut session = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| SineSession::defaults()),
        Err(_) => SineSession::defaults(),
    };
    if session.vertical_ratio == 0.0 {
        session = SineSession::defaults();
    }
    if let Ok(pb) = std::fs::read_to_string(dir.join("playbook.md")) {
        session.playbook = pb;
    }
    session
}

pub fn save_session(dir: &Path, session: &SineSession) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let path = dir.join("session.json");
    let tmp = dir.join("session.json.tmp");
    let text = serde_json::to_string_pretty(session)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    if !session.playbook.is_empty() {
        std::fs::write(dir.join("playbook.md"), &session.playbook)?;
    }
    Ok(())
}

fn plan_md_path(dir: &Path) -> PathBuf {
    dir.join("plan.md")
}

/// Human-readable plan snapshot for small-model / low-RAM recovery after
/// context clear. Embeds JSON so a later turn can re-load the same steps.
pub fn format_plan_markdown(plan: &SinePlan, status: &str) -> String {
    let mut out = String::new();
    out.push_str("# SINE plan\n\n");
    out.push_str(&format!("- status: {status}\n"));
    out.push_str(&format!("- summary: {}\n\n", plan.summary.trim()));
    out.push_str("## Steps\n\n");
    if plan.steps.is_empty() {
        out.push_str("(none)\n\n");
    } else {
        for (i, step) in plan.steps.iter().enumerate() {
            let detail = match step.verb.as_str() {
                "shell" => step
                    .args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|c| format!("`{c}`"))
                    .unwrap_or_else(|| step.args.to_string()),
                "say" => step
                    .args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(|t| format!("\"{t}\""))
                    .unwrap_or_else(|| step.args.to_string()),
                _ => step.args.to_string(),
            };
            out.push_str(&format!("{}. **{}** — {detail}\n", i + 1, step.verb));
        }
        out.push('\n');
    }
    out.push_str("## Machine\n\n");
    out.push_str("```json\n");
    match serde_json::to_string_pretty(plan) {
        Ok(j) => {
            out.push_str(&j);
            out.push('\n');
        }
        Err(_) => out.push_str("{}\n"),
    }
    out.push_str("```\n");
    out
}

fn write_plan_md(state: &mut SineState, plan: &SinePlan, status: &str) {
    if !state.allow_persist {
        return;
    }
    let path = plan_md_path(&state.sine_dir);
    if let Err(e) = std::fs::create_dir_all(&state.sine_dir) {
        state.agent_note(&format!("plan.md mkdir failed: {e:#}"));
        return;
    }
    let text = format_plan_markdown(plan, status);
    if let Err(e) = std::fs::write(&path, text) {
        state.agent_note(&format!("plan.md write failed: {e:#}"));
        return;
    }
    state.touch_file(&path);
}

// ── Runner state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    AwaitingApproval,
    Running,
    Planning,
}

struct PendingJob {
    handle: tokio::task::JoinHandle<JobResult>,
}

/// Wall time and output size for one SINE LLM call (est. tok/s = chars/4 / secs).
#[derive(Debug, Clone, Copy)]
struct LlmTiming {
    elapsed: Duration,
    out_chars: usize,
}

impl LlmTiming {
    fn from_output(elapsed: Duration, text: &str) -> Self {
        Self {
            elapsed,
            out_chars: text.chars().count(),
        }
    }

    fn toks_per_sec(self) -> f64 {
        let toks = (self.out_chars / 4).max(1) as f64;
        let secs = self.elapsed.as_secs_f64().max(0.001);
        toks / secs
    }
}

enum JobResult {
    Plan(Result<(SinePlan, Option<LlmTiming>)>),
    Idle(Result<(String, Option<LlmTiming>)>),
}

struct SineState {
    frame: SplitFrame,
    session: SineSession,
    phase: Phase,
    sine_dir: PathBuf,
    allow_persist: bool,
    cmdfreq_path: PathBuf,
    busybox: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    mode: Mode,
    /// Start of the current quiet stretch. Reset when the user types and when
    /// an active plan/run is fielded or finishes (slow local models).
    quiet_since: Instant,
    last_roster: Instant,
    idle_done: bool,
    status_extra: Option<String>,
    /// When Planning/Running started (for brand-pane spinner / eye bounce).
    busy_since: Option<Instant>,
    /// Left-top bunny shows `x` eyes after a plan/step failure (UI-only).
    brand_error: bool,
    /// Most recent shomagent member attached in this run (web/doc/…).
    /// Used to bind planned ask/stop id `0` (or stale ids) to the live roster.
    last_bound_member: Option<u32>,
    /// Last completed LLM sample (chars/4 / wall secs) for the status line.
    last_tok_per_sec: Option<f64>,
    /// Index into selectable file rows in the left-bottom list (0 = first file).
    file_select: usize,
    /// Full-screen file viewer overlay (Esc closes).
    viewer: Option<FileViewer>,
}

/// How the bunny eyes are drawn for one brand-pane paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrandEyes {
    /// Resting `o` at the packaged positions.
    Rest,
    /// Bounce `o` around the face while work is in flight.
    Busy(Duration),
    /// Resting cells become `x` after a plan or step error.
    Error,
}

/// Outcome of running an approved plan (halt still returns Ok for chat/`plan.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunOutcome {
    Done,
    Halted,
}

/// Numbered, wrapped file view that replaces the split UI until Esc.
#[derive(Debug, Clone)]
struct FileViewer {
    path: PathBuf,
    /// Full file text (re-wrapped when the terminal width changes).
    text: String,
    /// Display rows already wrapped to the terminal width (with line numbers).
    rows: Vec<String>,
    scroll: usize,
    /// Columns used for the last wrap (detect resize).
    wrap_cols: usize,
}

impl SineState {
    fn from_session(
        session: SineSession,
        cfg: &Config,
        cwd: PathBuf,
        sine_dir: PathBuf,
        allow_persist: bool,
    ) -> Self {
        let mut frame = SplitFrame::sine_layout();
        frame.set_ratios(
            session.vertical_ratio,
            session.left_h_ratio,
            session.right_h_ratio,
        );
        frame.input_history = session.input_history.clone();
        frame.set_slot_lines(
            Slot::LeftTop,
            render_brand_lines(chrome::active_avatar(), BrandEyes::Rest),
        );
        // restore logs
        if !session.agent_log.is_empty() {
            frame.set_slot_lines(Slot::RightTop, session.agent_log.clone());
        }
        let mut chat_lines = Vec::new();
        for turn in &session.chat {
            if turn.user {
                chat_lines.push(format!("you: {}", turn.text));
            } else {
                chat_lines.push(format!("sine: {}", turn.text));
            }
        }
        if !chat_lines.is_empty() {
            frame.set_slot_lines(Slot::RightBottom, chat_lines);
        }
        if session.pending_plan.is_some() {
            if let Some(plan) = &session.pending_plan {
                frame.publish(Slot::RightTop, &plan.format_for_pane());
            }
        }
        Self {
            frame,
            phase: if session.pending_plan.is_some() {
                Phase::AwaitingApproval
            } else {
                Phase::Idle
            },
            session,
            sine_dir,
            allow_persist,
            cmdfreq_path: cfg.cmdfreq_path.clone(),
            busybox: cfg.busybox_ash.clone(),
            home: cfg.mode.home_dir(),
            cwd,
            mode: cfg.mode,
            quiet_since: Instant::now(),
            last_roster: Instant::now()
                .checked_sub(ROSTER_TICK)
                .unwrap_or_else(Instant::now),
            idle_done: false,
            status_extra: None,
            busy_since: None,
            brand_error: false,
            last_bound_member: None,
            last_tok_per_sec: None,
            file_select: 0,
            viewer: None,
        }
    }

    fn touch_file(&mut self, path: impl AsRef<Path>) {
        let p = path.as_ref();
        let display = p.display().to_string();
        if display.is_empty() {
            return;
        }
        if !self
            .session
            .touched_files
            .iter()
            .any(|x| x == &display)
        {
            self.session.touched_files.push(display);
        }
    }

    fn selected_file(&self) -> Option<&str> {
        if self.session.touched_files.is_empty() {
            return None;
        }
        let i = self
            .file_select
            .min(self.session.touched_files.len().saturating_sub(1));
        self.session.touched_files.get(i).map(String::as_str)
    }

    fn move_file_select(&mut self, delta: isize) {
        let n = self.session.touched_files.len();
        if n == 0 {
            return;
        }
        let cur = self.file_select.min(n - 1) as isize;
        let next = (cur + delta).clamp(0, (n as isize) - 1) as usize;
        self.file_select = next;
    }

    fn open_selected_file(&mut self) {
        let Some(path) = self.selected_file().map(PathBuf::from) else {
            self.agent_note("(no file selected)");
            return;
        };
        match FileViewer::load(&path) {
            Ok(v) => {
                self.viewer = Some(v);
            }
            Err(e) => {
                self.agent_note(&format!("cannot open {}: {e:#}", path.display()));
            }
        }
    }

    /// Path for the editor overlay (selected touched file), if any.
    fn selected_edit_path(&self) -> Option<PathBuf> {
        self.selected_file().map(PathBuf::from)
    }

    /// Restart the idle countdown (user activity or end of a fielded request).
    fn note_activity(&mut self) {
        self.quiet_since = Instant::now();
        self.idle_done = false;
    }

    fn set_phase(&mut self, phase: Phase) {
        let was_busy = matches!(self.phase, Phase::Planning | Phase::Running);
        let busy = matches!(phase, Phase::Planning | Phase::Running);
        match (busy, self.busy_since) {
            (true, None) => self.busy_since = Some(Instant::now()),
            (false, _) => self.busy_since = None,
            _ => {}
        }
        self.phase = phase;
        // Countdown starts only after active work is fielded/finished — not
        // while a slow local model is still planning or executing.
        if busy || was_busy {
            self.note_activity();
        }
    }

    fn refresh_status(&mut self) {
        let auto = if self.session.auto { "on" } else { "off" };
        let phase = match self.phase {
            Phase::Idle => "ready",
            Phase::AwaitingApproval => "awaiting approve",
            Phase::Running => "running",
            Phase::Planning => "planning",
        };
        let extra = self
            .status_extra
            .as_deref()
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();
        let ctx = self.context_label();
        self.frame.status = Some(format!(
            "SINE · {} · auto:{auto} · {phase} · {ctx}{extra}",
            self.mode.as_str()
        ));
        // Bottom menu: keys + current auto state (Ctrl-A toggles).
        self.frame.footer = Some(format!(
            "Esc exit · ^S send · Enter newline/send/open · ^A auto:{auto} · ^K clear · ^R files · Tab focus · ↑↓ scroll · a/e/q"
        ));
        // Live meter on the agent pane header (same estimate as :chat).
        self.frame
            .set_leaf_header("right_top", format!("agent ── {ctx}"));
        self.frame
            .set_leaf_header("right_bottom", format!("chat ── {ctx}"));
        self.paint_brand();
    }

    /// ASCII art plus a spinner line while planning or running.
    /// Busy work wins over `brand_error` so a retry bounces immediately.
    fn paint_brand(&mut self) {
        let busy = matches!(self.phase, Phase::Planning | Phase::Running);
        let eyes = if busy {
            BrandEyes::Busy(self.busy_since.map(|s| s.elapsed()).unwrap_or_default())
        } else if self.brand_error {
            BrandEyes::Error
        } else {
            BrandEyes::Rest
        };
        let mut lines = render_brand_lines(chrome::active_avatar(), eyes);
        if busy {
            let label = match self.phase {
                Phase::Planning => "planning",
                Phase::Running => "running",
                _ => "busy",
            };
            let elapsed = self.busy_since.map(|s| s.elapsed()).unwrap_or_default();
            let g = spinner_glyph(elapsed);
            lines.push(String::new());
            lines.push(format!("[{g}] {label}…"));
        }
        self.frame.set_slot_lines(Slot::LeftTop, lines);
    }

    fn toggle_auto(&mut self) {
        self.session.auto = !self.session.auto;
        let state = if self.session.auto { "on" } else { "off" };
        self.push_chat(true, &format!("auto {state}"));
        self.refresh_status();
        self.save_if_allowed();
    }

    /// Drop the accumulated files: list so the next shell step's mtime
    /// snapshot is a fresh baseline. Session/playbook/plan are re-seeded on
    /// the following roster refresh when they exist on disk.
    fn clear_touched_files(&mut self) {
        self.session.touched_files.clear();
        self.file_select = 0;
        self.save_if_allowed();
    }

    /// Clear session working memory so agent/chat meters read `ctx 0 tok`.
    /// `plan.md` / `playbook.md` on disk are kept for recovery; in-memory
    /// playbook is dropped from the meter until idle regenerates it.
    /// Command-line draft and ↑↓ input history are left alone — they are not
    /// in the context window until Enter submits a line into chat.
    fn clear_context(&mut self) {
        self.session.chat.clear();
        self.session.pending_plan = None;
        self.session.agent_log.clear();
        self.session.playbook.clear();
        self.last_tok_per_sec = None;
        self.busy_since = None;
        self.brand_error = false;
        self.last_bound_member = None;
        self.idle_done = false;
        self.set_phase(Phase::Idle);
        self.status_extra = None;
        self.sync_panes_from_session();
        // UI-only placeholders — not stored in session, so the meter stays 0.
        self.frame.set_slot_lines(
            Slot::RightTop,
            vec!["(cleared — ctx 0)".into()],
        );
        self.frame.set_slot_lines(
            Slot::RightBottom,
            vec!["(context cleared — ctx 0)".into()],
        );
        self.refresh_status();
        self.save_if_allowed();
    }

    /// Rebuild agent + chat panes from `session`.
    fn sync_panes_from_session(&mut self) {
        self.frame
            .set_slot_lines(Slot::RightTop, self.session.agent_log.clone());
        let chat_lines: Vec<String> = self
            .session
            .chat
            .iter()
            .map(|t| {
                let label = if t.user { "you" } else { "sine" };
                format!("{label}: {}", t.text)
            })
            .collect();
        self.frame.set_slot_lines(Slot::RightBottom, chat_lines);
    }

    /// Working-memory estimate for SINE (chars/4), same window as `:chat`.
    fn context_tokens(&self) -> usize {
        estimate_sine_tokens(&self.session)
    }

    fn context_percent(&self) -> usize {
        crate::chat_tui::percent(self.context_tokens(), crate::chat_tui::context_window_tokens())
    }

    fn rate_label(&self) -> String {
        match self.last_tok_per_sec {
            Some(r) => format!("{r:.1} tok/s"),
            None => "— tok/s".into(),
        }
    }

    fn context_label(&self) -> String {
        format!(
            "ctx {} tok · est {}% · {}",
            self.context_tokens(),
            self.context_percent(),
            self.rate_label()
        )
    }

    fn record_llm_timing(&mut self, timing: Option<LlmTiming>) {
        if let Some(t) = timing {
            self.last_tok_per_sec = Some(t.toks_per_sec());
        }
    }

    fn persist_ratios(&mut self) {
        self.session.vertical_ratio = self.frame.vertical_ratio();
        self.session.left_h_ratio = self.frame.left_horizontal_ratio();
        self.session.right_h_ratio = self.frame.right_horizontal_ratio();
        self.session.input_history = self.frame.input_history.clone();
        // agent_log is owned by session (agent_note / clear_context). Do not
        // copy UI placeholders from the leaf back into the meter.
    }

    fn save_if_allowed(&mut self) {
        if !self.allow_persist {
            return;
        }
        self.persist_ratios();
        let _ = save_session(&self.sine_dir, &self.session);
    }

    fn push_chat(&mut self, user: bool, text: &str) {
        self.session.chat.push(ChatTurn {
            user,
            text: text.to_string(),
        });
        let label = if user { "you" } else { "sine" };
        self.frame
            .publish_line(Slot::RightBottom, format!("{label}: {text}"));
    }

    fn agent_note(&mut self, text: &str) {
        for line in text.lines() {
            self.session.agent_log.push(line.to_string());
        }
        if text.ends_with('\n') {
            self.session.agent_log.push(String::new());
        } else if !text.contains('\n') {
            // single-line notes get a blank spacer (same as before)
            self.session.agent_log.push(String::new());
        }
        self.frame.publish(Slot::RightTop, text);
        if !text.ends_with('\n') {
            self.frame.publish_line(Slot::RightTop, "");
        }
    }
}

async fn refresh_roster(state: &mut SineState) {
    let mut lines = Vec::new();
    lines.push("shomagents:".into());
    match rpc(&Request::Roster).await {
        Ok(Response::Ok { roster, .. }) => {
            if roster.is_empty() {
                lines.push("  (none)".into());
            } else {
                for m in roster {
                    lines.push(format!(
                        "  #{} {} [{}] {}",
                        m.id, m.name, m.state, m.command
                    ));
                }
            }
        }
        Ok(Response::Err { message }) => {
            lines.push(format!("  (daemon: {message})"));
        }
        Err(e) => {
            lines.push(format!("  (daemon unavailable: {e:#})"));
        }
    }
    lines.push("shomloops:".into());
    match shomloop::list_runs() {
        Ok(runs) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mut upcoming: Vec<(i64, String)> = Vec::new();
            for r in runs.into_iter().filter(|r| r.enabled) {
                let Some(fire) = shomloop::next_fire(&r.cron, now) else {
                    continue;
                };
                if fire - now > shomloop::SOON_HORIZON_SECS {
                    continue;
                }
                let until = shomloop::format_until(fire, now);
                upcoming.push((
                    fire,
                    format!("  #{} {}  {}  {}", r.id, until, r.frequency, r.command),
                ));
            }
            upcoming.sort_by_key(|(fire, _)| *fire);
            if upcoming.is_empty() {
                lines.push("  (none in the next 24h)".into());
            } else {
                for (_, line) in upcoming {
                    lines.push(line);
                }
            }
        }
        Err(_) => lines.push("  (ledger unavailable)".into()),
    }
    // Session + playbook + last plan always appear when present; then touched files.
    if state.allow_persist {
        let session_path = state.sine_dir.join("session.json");
        if session_path.exists() {
            state.touch_file(&session_path);
        }
        let playbook_path = state.sine_dir.join("playbook.md");
        if playbook_path.exists() || !state.session.playbook.is_empty() {
            state.touch_file(&playbook_path);
        }
        let plan_path = plan_md_path(&state.sine_dir);
        if plan_path.exists() {
            state.touch_file(&plan_path);
        }
    }
    lines.push("files:".into());
    if state.session.touched_files.is_empty() {
        lines.push("  (none — Tab files, ↑↓ select, Enter open · ^R clear)".into());
    } else {
        if state.file_select >= state.session.touched_files.len() {
            state.file_select = state.session.touched_files.len() - 1;
        }
        for (i, path) in state.session.touched_files.iter().enumerate() {
            let mark = if i == state.file_select { ">" } else { " " };
            lines.push(format!(" {mark}{path}"));
        }
    }
    state
        .frame
        .set_leaf_header("left_bottom", "agents / loops / files");
    // Keep scroll across refreshes so shomloops stay visible (do not snap to files).
    state
        .frame
        .set_slot_lines_preserve_scroll(Slot::LeftBottom, lines);
    state.last_roster = Instant::now();
}

/// Snapshot of live members for the planner (ids change; do not hardcode from chat).
async fn roster_for_plan() -> String {
    match rpc(&Request::Roster).await {
        Ok(Response::Ok { roster, .. }) if roster.is_empty() => {
            "(no members yet — after shomagent web, list with action roster, then ask id 0)".into()
        }
        Ok(Response::Ok { roster, .. }) => {
            let mut lines = Vec::with_capacity(roster.len());
            for m in roster {
                lines.push(format!(
                    "  #{} {} [{}] {}",
                    m.id, m.name, m.state, m.command
                ));
            }
            format!(
                "Live now (may change after a new web attach; still use id 0 for members \
                 this plan creates):\n{}",
                lines.join("\n")
            )
        }
        Ok(Response::Err { message }) => format!("(roster error: {message})"),
        Err(e) => format!("(roster unavailable: {e:#})"),
    }
}

async fn make_plan(cfg: Config, intent: String) -> Result<(SinePlan, Option<LlmTiming>)> {
    if sine_stub() {
        let plan = stub_plan(&intent);
        let text = plan.format_for_pane();
        return Ok((
            plan,
            Some(LlmTiming::from_output(Duration::from_millis(1), &text)),
        ));
    }
    if cfg.mode == Mode::Root {
        // Root skips the model: wrap the line as a shell step.
        return Ok((
            SinePlan {
                summary: format!("run as shell: {intent}"),
                steps: vec![PlanStep {
                    verb: "shell".into(),
                    args: serde_json::json!({"command": intent}),
                }],
            },
            None,
        ));
    }
    let started = Instant::now();
    let roster_note = roster_for_plan().await;
    let llm = LlmClient::new(&cfg)?;
    let user = format!(
        "Live shomagent roster:\n{roster_note}\n\nOperator request:\n{intent}"
    );
    let messages = vec![
        ChatMessage::system(PLAN_SYSTEM),
        ChatMessage::user(&user),
    ];
    let raw = llm
        .chat_text_max(&messages, PLAN_MAX_TOKENS)
        .await
        .context("sine plan LLM")?;
    match parse_plan_json(&raw) {
        Ok(plan) => {
            let timing = LlmTiming::from_output(started.elapsed(), &raw);
            Ok((plan, Some(timing)))
        }
        Err(e1) => {
            let repair = vec![
                ChatMessage::system(PLAN_SYSTEM),
                ChatMessage::user(&user),
                ChatMessage::assistant_text(&raw),
                ChatMessage::user(format!(
                    "Previous reply failed ({e1}). Reply with ONLY a short JSON plan. \
                     verb must be exactly shell|shomagent|top_commands|say|quickyn — \
                     never \"shomagent web\". If the operator asked for daily/loop/schedule: \
                     write /engram/user/<job>.sh (web; parse shomagent#id; ask; write CSV; stop), \
                     chmod +x, run once if they asked to test now, then \
                     shomloop \"<frequency>\" /engram/user/<job>.sh. \
                     One-shot only (no schedule): web → roster → ask/stop id 0. \
                     At most 6 steps."
                )),
            ];
            let raw2 = llm
                .chat_text_max(&repair, PLAN_MAX_TOKENS)
                .await
                .context("sine plan LLM retry")?;
            let plan = parse_plan_json(&raw2)?;
            // Count both replies against total wall time for a honest tok/s.
            let combined = format!("{raw}\n{raw2}");
            let timing = LlmTiming::from_output(started.elapsed(), &combined);
            Ok((plan, Some(timing)))
        }
    }
}

async fn make_idle_playbook(cfg: Config, tops: String) -> Result<(String, Option<LlmTiming>)> {
    if sine_stub() {
        let text = format!(
            "# SINE playbook (stub)\n\nTop commands:\n\n{tops}\n\nPrefer short pipelines and `:sine` for plans.\n"
        );
        return Ok((
            text.clone(),
            Some(LlmTiming::from_output(Duration::from_millis(1), &text)),
        ));
    }
    let started = Instant::now();
    let llm = LlmClient::new(&cfg)?;
    let messages = vec![
        ChatMessage::system(IDLE_SYSTEM),
        ChatMessage::user(format!("Most-used commands:\n{tops}")),
    ];
    let text = llm
        .chat_text_max(&messages, 512)
        .await
        .context("sine idle LLM")?;
    let timing = LlmTiming::from_output(started.elapsed(), &text);
    Ok((text, Some(timing)))
}

async fn execute_plan(
    plan: &SinePlan,
    state: &mut SineState,
) -> Result<RunOutcome> {
    if state.mode == Mode::Hallucination {
        state.agent_note("(hallucination mode: plan shown, not executed)");
        return Ok(RunOutcome::Done);
    }
    refresh_roster(state).await;
    state.refresh_status();
    state.frame.draw();
    for (idx, step) in plan.steps.iter().enumerate() {
        // Kick-off: refresh roster so new web/watch members appear promptly.
        refresh_roster(state).await;
        state.refresh_status();
        state.frame.draw();
        let note = execute_step(step, state).await?;
        if !note.is_empty() {
            state.agent_note(&note);
        }
        // Return: refresh again after the step (shomagent web attach, etc.).
        refresh_roster(state).await;
        state.refresh_status();
        state.frame.draw();
        if step_output_failed(&note) {
            let left = plan.steps.len().saturating_sub(idx + 1);
            let unknown_member = note.contains("unknown member");
            let recovery = if unknown_member {
                format!(
                    "--- halted: step {} failed; skipped {left} remaining ---\n\
                     That was a bad/stale member id, not a context-window problem.\n\
                     Recourse (check the left roster for the live id):\n\
                     1. shomagent list   (or bare shomagent) — note the #id for the URL\n\
                     2. shomagent ask <id> \"…\" then write the file; shomagent stop <id>\n\
                     3. Or re-plan: web → roster → ask/stop with id 0 (SINE binds the live id)",
                    idx + 1
                )
            } else {
                format!(
                    "--- halted: step {} failed; skipped {left} remaining ---\n\
                     If ask failed on a large page: SINE's ctx % is not the shomagent ask \
                     budget — page text can still overwhelm the local model.\n\
                     Recourse (member may still be in the left list):\n\
                     1. Retry a tiny ask: shomagent ask <id> \"one headline title and url only\"\n\
                     2. Write the reply to the file yourself, then shomagent stop <id>\n\
                     3. Or re-open plan.md after ^K and type a narrower request\n\
                     Do not re-run the full plan until ask succeeds.",
                    idx + 1
                )
            };
            state.agent_note(&recovery);
            let chat = if unknown_member {
                format!(
                    "halted after failed step {} — wrong member id; list the roster and ask the live #id (see agent pane)",
                    idx + 1
                )
            } else {
                format!(
                    "halted after failed step {} — retry a shorter shomagent ask (see agent pane)",
                    idx + 1
                )
            };
            state.push_chat(false, &chat);
            write_plan_md(state, plan, &format!("halted at step {}", idx + 1));
            return Ok(RunOutcome::Halted);
        }
    }
    Ok(RunOutcome::Done)
}

fn step_output_failed(note: &str) -> bool {
    note.contains("(exit failed)")
        || note.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("Error:") || t.starts_with("error:")
        })
}

/// Run a shell command without freezing the SINE UI: refresh agents/loops/files
/// and redraw on a short tick until the child exits.
async fn shell_exec_live(state: &mut SineState, command: &str) -> Result<crate::tools::ShellResult> {
    let busybox = state.busybox.clone();
    let cwd = state.cwd.clone();
    let home = state.home.clone();
    let cmd = command.to_string();
    let mut handle =
        tokio::spawn(async move { crate::tools::shell_exec(&busybox, &cmd, &cwd, &home).await });
    let tick = Duration::from_millis(400);
    loop {
        tokio::select! {
            join = &mut handle => {
                return join.map_err(|e| anyhow!("shell task join: {e}"))?;
            }
            _ = tokio::time::sleep(tick) => {
                refresh_roster(state).await;
                state.refresh_status();
                state.frame.draw();
            }
        }
    }
}

fn spinner_glyph(elapsed: Duration) -> char {
    let i = (elapsed.as_millis() / SPIN_TICK.as_millis()) as usize;
    SPIN_FRAMES[i % SPIN_FRAMES.len()]
}

/// Resting avatar art for menu previews (`:menu` → Theme).
pub fn preview_avatar_lines(avatar: Avatar) -> Vec<String> {
    render_brand_lines(avatar, BrandEyes::Rest)
}

/// Brand art for the active avatar. Classic keeps bouncing/error eyes; image
/// avatars swap rest/busy pictures (and tint on error / NO_COLOR strips SGR).
fn render_brand_lines(avatar: Avatar, eyes: BrandEyes) -> Vec<String> {
    match avatar {
        Avatar::Classic => render_classic_brand(eyes),
        Avatar::Navi => {
            let mut lines = art_lines(BRAND_NAVI);
            apply_image_eyes(&mut lines, eyes, false);
            lines
        }
        Avatar::Coffee => {
            let src = match eyes {
                BrandEyes::Busy(_) => BRAND_COFFEE_BUSY,
                _ => BRAND_COFFEE_REST,
            };
            let mut lines = art_lines(src);
            apply_image_eyes(&mut lines, eyes, matches!(eyes, BrandEyes::Busy(_)));
            lines
        }
        Avatar::Laptop => {
            let src = match eyes {
                BrandEyes::Busy(_) => BRAND_LAPTOP_BUSY,
                _ => BRAND_LAPTOP_REST,
            };
            let mut lines = art_lines(src);
            apply_image_eyes(&mut lines, eyes, matches!(eyes, BrandEyes::Busy(_)));
            lines
        }
        Avatar::Mana => {
            let src = match eyes {
                BrandEyes::Busy(_) => BRAND_MANA_BUSY,
                _ => BRAND_MANA_REST,
            };
            let mut lines = art_lines(src);
            apply_image_eyes(&mut lines, eyes, matches!(eyes, BrandEyes::Busy(_)));
            lines
        }
        Avatar::Leaf => {
            let src = match eyes {
                BrandEyes::Busy(_) => BRAND_LEAF_BUSY,
                _ => BRAND_LEAF_REST,
            };
            let mut lines = art_lines(src);
            apply_image_eyes(&mut lines, eyes, matches!(eyes, BrandEyes::Busy(_)));
            lines
        }
    }
}

fn art_lines(src: &str) -> Vec<String> {
    let raw: Vec<String> = src.lines().map(|l| l.to_string()).collect();
    if ui::color_enabled() {
        raw
    } else {
        raw.into_iter().map(|l| chrome::strip_ansi(&l)).collect()
    }
}

fn apply_image_eyes(lines: &mut [String], eyes: BrandEyes, busy_swapped: bool) {
    let _ = busy_swapped;
    match eyes {
        BrandEyes::Rest | BrandEyes::Busy(_) => {}
        BrandEyes::Error => {
            // Retint whole picture with danger color when color is on.
            if !ui::color_enabled() {
                return;
            }
            let danger = chrome::palette().danger.sgr();
            let reset = "\x1b[0m";
            for line in lines.iter_mut() {
                let plain = chrome::strip_ansi(line);
                *line = format!("{danger}{plain}{reset}");
            }
        }
    }
}

fn render_classic_brand(eyes: BrandEyes) -> Vec<String> {
    let mut lines: Vec<String> = SINE_IMAGE.lines().map(|l| l.to_string()).collect();
    match eyes {
        BrandEyes::Rest => {}
        BrandEyes::Error => {
            set_char_at(&mut lines, EYE_ROW, EYE_LEFT_COL, 'x');
            set_char_at(&mut lines, EYE_ROW, EYE_RIGHT_COL, 'x');
        }
        BrandEyes::Busy(elapsed) => {
            set_char_at(&mut lines, EYE_ROW, EYE_LEFT_COL, BRAILLE_BLANK);
            set_char_at(&mut lines, EYE_ROW, EYE_RIGHT_COL, BRAILLE_BLANK);
            let (lr, lc, rr, rc) = eye_pose_cells(elapsed, &lines);
            set_char_at(&mut lines, lr, lc, 'o');
            set_char_at(&mut lines, rr, rc, 'o');
        }
    }
    lines
}

/// Pick eye cells for the current busy pose; skip poses that would overwrite
/// non-blank face ink (falls back through the cycle toward center).
fn eye_pose_cells(elapsed: Duration, lines: &[String]) -> (usize, usize, usize, usize) {
    let start = (elapsed.as_millis() / EYE_POSE_TICK.as_millis()) as usize % EYE_POSES.len();
    for i in 0..EYE_POSES.len() {
        let (dr, dc) = EYE_POSES[(start + i) % EYE_POSES.len()];
        let Some((lr, lc)) = offset_eye(EYE_ROW, EYE_LEFT_COL, dr, dc) else {
            continue;
        };
        let Some((rr, rc)) = offset_eye(EYE_ROW, EYE_RIGHT_COL, dr, dc) else {
            continue;
        };
        // Keep at least one blank between pupils.
        if lr == rr && lc.abs_diff(rc) < 2 {
            continue;
        }
        if cell_is_blank(lines, lr, lc) && cell_is_blank(lines, rr, rc) {
            return (lr, lc, rr, rc);
        }
    }
    (EYE_ROW, EYE_LEFT_COL, EYE_ROW, EYE_RIGHT_COL)
}

fn offset_eye(row: usize, col: usize, dr: isize, dc: isize) -> Option<(usize, usize)> {
    let r = row as isize + dr;
    let c = col as isize + dc;
    if r < 0 || c < 0 {
        return None;
    }
    Some((r as usize, c as usize))
}

fn cell_is_blank(lines: &[String], row: usize, col: usize) -> bool {
    let Some(line) = lines.get(row) else {
        return false;
    };
    match line.chars().nth(col) {
        None => false,
        Some(c) => c == BRAILLE_BLANK || c == ' ',
    }
}

fn set_char_at(lines: &mut [String], row: usize, col: usize, ch: char) {
    let Some(line) = lines.get_mut(row) else {
        return;
    };
    let mut chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return;
    }
    chars[col] = ch;
    *line = chars.into_iter().collect();
}

impl FileViewer {
    fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let (_rows, cols) = crate::term::term_size();
        let cols = cols.clamp(40, crate::term::MAX_COLS);
        let rows = wrap_numbered_lines(&text, cols);
        Ok(Self {
            path: path.to_path_buf(),
            text,
            rows,
            scroll: 0,
            wrap_cols: cols,
        })
    }

    fn body_rows(term_rows: usize) -> usize {
        // Row 1 = title, last row = footer; body is everything between.
        term_rows.saturating_sub(2).max(1)
    }

    /// Re-wrap only when the terminal width changes; keep scroll at a real top.
    fn relayout(&mut self) {
        let (term_rows, cols) = crate::term::term_size();
        let cols = cols.clamp(40, crate::term::MAX_COLS);
        if cols != self.wrap_cols {
            self.rows = wrap_numbered_lines(&self.text, cols);
            self.wrap_cols = cols;
        }
        let body = Self::body_rows(term_rows.clamp(8, crate::term::MAX_ROWS));
        let max_scroll = self.rows.len().saturating_sub(body);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn scroll_by(&mut self, delta: isize) {
        let term_rows = crate::term::term_size().0.clamp(8, crate::term::MAX_ROWS);
        let body = Self::body_rows(term_rows);
        let max_scroll = self.rows.len().saturating_sub(body) as isize;
        let next = (self.scroll as isize + delta).clamp(0, max_scroll.max(0));
        self.scroll = next as usize;
    }

    fn draw(&self) {
        let (term_rows, cols) = crate::term::term_size();
        let cols = cols.clamp(40, crate::term::MAX_COLS);
        let term_rows = term_rows.clamp(8, crate::term::MAX_ROWS);
        let body = Self::body_rows(term_rows);
        let color = crate::ui::color_enabled();
        let reset = "\x1b[0m";
        let cyan = if color { "\x1b[36m" } else { "" };
        let dim = if color { "\x1b[2m" } else { "" };
        let bold = if color { "\x1b[1m" } else { "" };

        let mut buf = String::with_capacity(term_rows * cols * 2);
        use std::fmt::Write as _;
        let _ = write!(buf, "\x1b[2J\x1b[H\x1b[?25l");

        // Title on absolute row 1 (truncate path so it never wraps and steals body rows).
        let prefix = " file ── ";
        let max_path = cols.saturating_sub(prefix.chars().count() + 1).max(8);
        let path = split_frame::truncate_visible(&self.path.display().to_string(), max_path);
        let title = format!("{prefix}{path} ");
        let dash = cols.saturating_sub(title.chars().count());
        let title_cell = format!(
            "{cyan}{bold}{}{reset}{cyan}{}{reset}",
            title,
            "─".repeat(dash)
        );
        let _ = write!(
            buf,
            "\x1b[1;1H{}\x1b[K",
            split_frame::truncate_visible(&title_cell, cols.saturating_sub(1).max(1))
        );

        // Body starts at absolute row 2 — never depends on title wrapping.
        let start = self.scroll;
        for i in 0..body {
            let y = i + 2;
            let line = self.rows.get(start + i).cloned().unwrap_or_default();
            // Leave 1 column free so a full-width write can't wrap the row.
            let cell = split_frame::truncate_visible(&line, cols.saturating_sub(1).max(1));
            let _ = write!(buf, "\x1b[{y};1H{cell}\x1b[K");
        }

        let end = (start + body).min(self.rows.len().max(1));
        // Parse the source line number from the first visible gutter (…N│).
        let top_src = first_gutter_lineno(self.rows.get(start).map(String::as_str).unwrap_or(""));
        let footer = format!(
            "{dim} Esc close · e edit · ↑↓/PgUp/PgDn/Home/End · rows {}-{} / {}{}{reset}",
            if self.rows.is_empty() { 0 } else { start + 1 },
            end,
            self.rows.len(),
            top_src
                .map(|n| format!(" · top source line {n}"))
                .unwrap_or_default()
        );
        // Clear-to-EOL instead of full-width pad — avoids terminal wraparound
        // eating the next row when the cell is exactly `cols` wide.
        let _ = write!(
            buf,
            "\x1b[{term_rows};1H{}\x1b[K",
            split_frame::truncate_visible(&footer, cols.saturating_sub(1).max(1))
        );
        let mut out = io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }
}

/// Pull the source line number off a gutter like ` 19│ …` (None for continuations).
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

/// Wrap file text for the viewer / editor.
/// First visual row of each source line: `NNN│ text`
/// Continuations keep a solid gutter: `   │ text`
pub(crate) fn wrap_numbered_lines(text: &str, width: usize) -> Vec<String> {
    let source: Vec<&str> = if text.is_empty() {
        vec![""]
    } else {
        // Keep empty lines; strip a single trailing newline so we don't invent
        // an extra blank source line at EOF.
        let t = text.strip_suffix('\n').unwrap_or(text);
        if t.is_empty() {
            vec![""]
        } else {
            t.split('\n').collect()
        }
    };
    let digits = source.len().max(1).to_string().len().max(3);
    // "NNN│ " — number, rule, space
    let gutter = digits + 2;
    let body_w = width.saturating_sub(gutter).max(4);
    let mut out = Vec::new();
    for (i, line) in source.iter().enumerate() {
        let num = format!("{:>width$}│ ", i + 1, width = digits);
        // Same width as `num`, but spaces where digits were — keeps │ aligned.
        let cont = format!("{:>width$}│ ", "", width = digits);
        let clean: String = line.chars().filter(|c| *c != '\r').collect();
        if clean.is_empty() {
            out.push(num);
            continue;
        }
        let chars: Vec<char> = clean.chars().collect();
        let mut offset = 0;
        let mut first = true;
        while offset < chars.len() {
            let end = (offset + body_w).min(chars.len());
            let chunk: String = chars[offset..end].iter().collect();
            if first {
                out.push(format!("{num}{chunk}"));
                first = false;
            } else {
                out.push(format!("{cont}{chunk}"));
            }
            offset = end;
        }
    }
    out
}

/// Ids mentioned after `shomagent ask|stop|…` in a shell command.
fn planned_member_ids_in_command(command: &str) -> Vec<u32> {
    let re = regex::Regex::new(
        r"(?i)\bshomagent\s+(?:ask|stop|pause|resume|click|type|navigate|refresh|write)\s+(\d+)\b",
    )
    .expect("member-id regex");
    re.captures_iter(command)
        .filter_map(|c| c.get(1)?.as_str().parse().ok())
        .collect()
}

/// Rewrite `shomagent <verb> <from>` → `<to>` (shell and nested `sh -c`).
fn rewrite_member_id_in_command(command: &str, from: u32, to: u32) -> String {
    if from == to {
        return command.to_string();
    }
    let re = regex::Regex::new(&format!(
        r"(?i)(\bshomagent\s+(?:ask|stop|pause|resume|click|type|navigate|refresh|write)\s+){from}\b"
    ))
    .expect("rewrite-id regex");
    re.replace_all(command, format!("${{1}}{to}").as_str())
        .into_owned()
}

/// Parse `⟡ shomagent#N …` (web/doc/dream attach lines).
fn parse_attached_member_id(output: &str) -> Option<u32> {
    let re = regex::Regex::new(r"(?i)shomagent#(\d+)\b").expect("attach-id regex");
    let mut last = None;
    for cap in re.captures_iter(output) {
        if let Some(id) = cap.get(1).and_then(|m| m.as_str().parse().ok()) {
            last = Some(id);
        }
    }
    last
}

async fn fetch_roster_members() -> Vec<shomagent::protocol::MemberStatus> {
    match rpc(&Request::Roster).await {
        Ok(Response::Ok { roster, .. }) => roster,
        _ => Vec::new(),
    }
}

/// Pick a live member for ask/stop: prefer last attach still on roster, else
/// sole web member, else highest-id web member, else highest id overall.
fn pick_bound_member(
    last: Option<u32>,
    roster: &[shomagent::protocol::MemberStatus],
) -> Option<u32> {
    if roster.is_empty() {
        return None;
    }
    if let Some(id) = last {
        if roster.iter().any(|m| m.id == id) {
            return Some(id);
        }
    }
    let webs: Vec<_> = roster.iter().filter(|m| m.state == "web").collect();
    if webs.len() == 1 {
        return Some(webs[0].id);
    }
    if let Some(m) = webs.iter().max_by_key(|m| m.id) {
        return Some(m.id);
    }
    roster.iter().map(|m| m.id).max()
}

/// Resolve a planned id against the live roster. `0` / missing / stale → bound.
async fn resolve_member_id(state: &SineState, planned: Option<u32>) -> Option<(u32, bool)> {
    let roster = fetch_roster_members().await;
    let bound = pick_bound_member(state.last_bound_member, &roster)?;
    match planned {
        None | Some(0) => Some((bound, true)),
        Some(id) if roster.iter().any(|m| m.id == id) => Some((id, false)),
        Some(id) => Some((bound, id != bound)),
    }
}

fn note_bound_member(state: &mut SineState, output: &str) {
    if let Some(id) = parse_attached_member_id(output) {
        state.last_bound_member = Some(id);
    }
}

async fn execute_step(step: &PlanStep, state: &mut SineState) -> Result<String> {
    match step.verb.as_str() {
        "say" => {
            let text = step
                .args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            state.push_chat(false, &text);
            Ok(text)
        }
        "top_commands" => {
            let limit = step
                .args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let store = FrequencyStore::load(&state.cmdfreq_path).unwrap_or_default();
            let items = store.top(limit, "");
            let text = format_top(&items);
            Ok(text)
        }
        "quickyn" => {
            let question = step
                .args
                .get("question")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("quickyn step missing question"))?;
            let file_body = match step.args.get("file").and_then(|v| v.as_str()) {
                Some(path) if !path.is_empty() => {
                    let p = PathBuf::from(path);
                    state.touch_file(&p);
                    Some(
                        std::fs::read_to_string(&p)
                            .with_context(|| format!("reading quickyn file {}", p.display()))?,
                    )
                }
                _ => None,
            };
            let verdict = quickyn::decide(question, file_body.as_deref())
                .await
                .map_err(|e| anyhow!("quickyn: {e}"))?;
            let line = format!("quickyn: {}", verdict.format_line());
            state.push_chat(false, &line);
            Ok(line)
        }
        "shell" => {
            let raw_command = step
                .args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("shell step missing command"))?;
            let mut bind_note = String::new();
            let command = {
                let planned = planned_member_ids_in_command(raw_command);
                let needs_bind = planned.is_empty()
                    && raw_command.to_ascii_lowercase().contains("shomagent ask");
                if !planned.is_empty() || needs_bind {
                    // Bind each distinct planned id (0 / stale) to the live member.
                    let mut cmd = raw_command.to_string();
                    let ids: Vec<u32> = if planned.is_empty() {
                        vec![0]
                    } else {
                        let mut u = planned;
                        u.sort_unstable();
                        u.dedup();
                        u
                    };
                    for from in ids {
                        if let Some((to, rewritten)) = resolve_member_id(state, Some(from)).await {
                            if rewritten || from == 0 {
                                let next = rewrite_member_id_in_command(&cmd, from, to);
                                if next != cmd {
                                    if !bind_note.is_empty() {
                                        bind_note.push('\n');
                                    }
                                    bind_note.push_str(&format!(
                                        "(bound shomagent id {from} → #{to} from roster)"
                                    ));
                                    cmd = next;
                                } else if from == 0 {
                                    // ask without a digit is rare; leave as-is.
                                }
                            }
                        }
                    }
                    cmd
                } else {
                    raw_command.to_string()
                }
            };
            // Snapshot cwd (and /engram/user) so date-expanded redirects like
            // `> yc_headlines_$(date +%Y%m%d_%H%M%S).csv` still land in files:.
            let watch = touch_watch_dirs(state);
            let before = snapshot_dir_files(&watch);
            // Live roster/UI while the shell runs — otherwise a long
            // `shomagent ask` freezes the left pane on "(none)" even though
            // web already attached.
            let result = shell_exec_live(state, &command).await?;
            state.cwd = result.cwd.clone();
            for path in extract_touched_paths(&command, &state.cwd) {
                state.touch_file(path);
            }
            for path in discover_new_or_updated(&before, &watch) {
                state.touch_file(path);
            }
            for path in extract_paths_from_text(&result.output) {
                state.touch_file(path);
            }
            note_bound_member(state, &result.output);
            // Stopping the bound member clears the binding.
            if result.success {
                let stop_re =
                    regex::Regex::new(r"(?i)\bshomagent\s+stop\s+(\d+)\b").expect("stop-id regex");
                for cap in stop_re.captures_iter(&command) {
                    if let Some(id) = cap.get(1).and_then(|m| m.as_str().parse().ok()) {
                        if state.last_bound_member == Some(id) {
                            state.last_bound_member = None;
                        }
                    }
                }
            }
            let mut out = String::new();
            if !bind_note.is_empty() {
                out.push_str(&bind_note);
                out.push('\n');
            }
            out.push_str(&format!("$ {command}\n{}", result.output));
            if !result.success {
                out.push_str("\n(exit failed)");
            }
            Ok(out)
        }
        "shomagent" => {
            let action = step
                .args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("roster");
            let planned_id = step.args.get("id").and_then(|v| v.as_u64()).map(|v| v as u32);
            let mut bind_note = String::new();
            let resolved_id = if matches!(action, "ask" | "pause" | "resume" | "stop") {
                match resolve_member_id(state, planned_id).await {
                    Some((id, rewritten)) => {
                        if rewritten {
                            let from = planned_id.unwrap_or(0);
                            bind_note = format!(
                                "(bound shomagent id {from} → #{id} from roster)\n"
                            );
                        }
                        Some(id)
                    }
                    None => planned_id,
                }
            } else {
                planned_id
            };
            let req = match action {
                "roster" | "list" => Request::Roster,
                "pause" => {
                    let id = resolved_id.ok_or_else(|| anyhow!("pause needs id"))?;
                    Request::Pause { id }
                }
                "resume" => {
                    let id = resolved_id.ok_or_else(|| anyhow!("resume needs id"))?;
                    Request::Resume { id }
                }
                "stop" => {
                    let id = resolved_id.ok_or_else(|| anyhow!("stop needs id"))?;
                    Request::Stop { id }
                }
                "ask" => {
                    let id = resolved_id.ok_or_else(|| anyhow!("ask needs id (use 0 after roster)"))?;
                    let message = step
                        .args
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Request::Talk { id, message }
                }
                other => bail!("shomagent action '{other}' not in ADR-0014 surface"),
            };
            match rpc(&req).await? {
                Response::Ok {
                    message,
                    reply,
                    roster,
                    ..
                } => {
                    let mut out = bind_note;
                    if let Some(m) = message {
                        out.push_str(&m);
                        note_bound_member(state, &m);
                    }
                    if let Some(r) = reply {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&r);
                    }
                    if action == "stop" {
                        if let Some(id) = resolved_id {
                            if state.last_bound_member == Some(id) {
                                state.last_bound_member = None;
                            }
                        }
                    }
                    if !roster.is_empty() {
                        // Prefer highest-id web as soft bind when listing after attach.
                        if state.last_bound_member.is_none() {
                            if let Some(id) = pick_bound_member(None, &roster) {
                                state.last_bound_member = Some(id);
                            }
                        }
                        for m in &roster {
                            out.push_str(&format!(
                                "\n#{} {} [{}] {}",
                                m.id, m.name, m.state, m.command
                            ));
                        }
                    }
                    if out.is_empty() {
                        out.push_str("(ok)");
                    }
                    Ok(out)
                }
                Response::Err { message } => {
                    Ok(format!("{bind_note}shomagent error: {message}"))
                }
            }
        }
        other => bail!("unknown verb '{other}'"),
    }
}

/// Heuristic paths a shell command likely wrote or referenced under cwd / absolute.
fn touch_watch_dirs(state: &SineState) -> Vec<PathBuf> {
    let mut dirs = vec![state.cwd.clone()];
    let engram_user = PathBuf::from("/engram/user");
    if state.cwd != engram_user {
        dirs.push(engram_user);
    }
    dirs
}

/// Non-hidden files in each watch dir → mtime (for create/update detection).
fn snapshot_dir_files(dirs: &[PathBuf]) -> HashMap<PathBuf, SystemTime> {
    let mut files = HashMap::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            if let Ok(mtime) = meta.modified() {
                files.insert(path, mtime);
            }
        }
    }
    files
}

/// Files created or mtime-bumped under the watch dirs since `before`.
fn discover_new_or_updated(
    before: &HashMap<PathBuf, SystemTime>,
    dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let after = snapshot_dir_files(dirs);
    let mut out = Vec::new();
    for (path, mtime) in after {
        match before.get(&path) {
            None => out.push(path),
            Some(old) if mtime > *old => out.push(path),
            _ => {}
        }
    }
    out.sort();
    out
}

fn extract_touched_paths(command: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut parts = command.split_whitespace().peekable();
    while let Some(tok) = parts.next() {
        // Redirect forms: >file >>file > file ; also `tee path`
        let candidate = if let Some(rest) = tok.strip_prefix(">>").or_else(|| tok.strip_prefix('>'))
        {
            if rest.is_empty() {
                parts.next()
            } else {
                Some(rest)
            }
        } else if tok == ">" || tok == ">>" || tok == "tee" {
            parts.next()
        } else if looks_like_path_token(tok) {
            Some(tok)
        } else {
            None
        };
        if let Some(raw) = candidate {
            // Skip shell expansions we can't resolve as a literal path; the
            // directory snapshot after the command will catch the real file.
            if raw.contains('$') || raw.contains('`') || raw.contains('*') || raw.contains('?') {
                continue;
            }
            let cleaned = raw.trim_matches(|c| c == '"' || c == '\'');
            if cleaned.is_empty() || cleaned.starts_with('-') {
                continue;
            }
            let path = if cleaned.starts_with('/') {
                PathBuf::from(cleaned)
            } else {
                cwd.join(cleaned)
            };
            if is_touchable_path(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// Absolute `/engram/…` paths mentioned in command output (e.g. "wrote /engram/user/x.csv").
fn extract_paths_from_text(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let cleaned = tok.trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '`' || c == ',' || c == ';' || c == ')' || c == '('
        });
        if !cleaned.starts_with("/engram/") {
            continue;
        }
        let path = PathBuf::from(cleaned);
        if is_touchable_path(&path) {
            out.push(path);
        }
    }
    out
}

fn is_touchable_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    path.exists()
        || s.contains("/engram/")
        || path
            .extension()
            .map(|e| {
                matches!(
                    e.to_string_lossy().as_ref(),
                    "csv" | "md" | "json" | "txt" | "toml" | "log" | "html"
                )
            })
            .unwrap_or(false)
}

fn looks_like_path_token(tok: &str) -> bool {
    if tok.contains("://")
        || tok.contains('$')
        || tok.contains('`')
        || tok.contains('(')
        || tok.contains(')')
        || tok.contains('|')
        || tok.contains('&')
        || tok.contains(';')
        || tok.starts_with('+')
    {
        return false;
    }
    tok.contains('/')
        || tok.starts_with("./")
        || tok.starts_with("~/")
        || tok.contains('.')
            && tok
                .rsplit_once('.')
                .map(|(_, e)| e.chars().all(|c| c.is_ascii_alphanumeric()) && !e.is_empty())
                .unwrap_or(false)
}

fn present_plan(state: &mut SineState, plan: SinePlan) {
    state.brand_error = false;
    state.agent_note(&plan.format_for_pane());
    state.push_chat(false, &format!("plan ready: {}", plan.summary));
    write_plan_md(state, &plan, "awaiting approve");
    state.session.pending_plan = Some(plan);
    state.set_phase(Phase::AwaitingApproval);
    if state.session.auto {
        // auto still shows the plan; run without waiting for a second confirm
        state.status_extra = Some("auto-run".into());
    }
    state.refresh_status();
    state.save_if_allowed();
}

async fn approve_and_run(state: &mut SineState) {
    let Some(plan) = state.session.pending_plan.clone() else {
        return;
    };
    write_plan_md(state, &plan, "running");
    state.set_phase(Phase::Running);
    state.refresh_status();
    state.frame.draw();
    match execute_plan(&plan, state).await {
        Ok(RunOutcome::Done) => {
            state.brand_error = false;
            state.push_chat(false, "done");
            state.agent_note("--- done ---");
            write_plan_md(state, &plan, "done");
        }
        Ok(RunOutcome::Halted) => {
            state.brand_error = true;
            // Chat / plan.md already updated inside execute_plan.
        }
        Err(e) => {
            state.brand_error = true;
            state.agent_note(&format!("error: {e:#}"));
            state.push_chat(false, &format!("error: {e}"));
            write_plan_md(state, &plan, &format!("error: {e}"));
        }
    }
    state.session.pending_plan = None;
    state.set_phase(Phase::Idle);
    state.status_extra = None;
    refresh_roster(state).await;
    state.refresh_status();
    state.save_if_allowed();
}

async fn handle_line(state: &mut SineState, line: &str, cfg: &Config, pending: &mut Option<PendingJob>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    state.note_activity();

    // Local controls (shell-owned; model cannot invent these as verbs)
    match trimmed.to_ascii_lowercase().as_str() {
        "auto" => {
            state.toggle_auto();
            return;
        }
        "clear" | "clear context" => {
            if let Some(job) = pending.take() {
                job.handle.abort();
            }
            state.clear_context();
            return;
        }
        "files clear" | "clear files" | "refresh files" => {
            state.clear_touched_files();
            refresh_roster(state).await;
            state.agent_note("(files list cleared — baseline reset)");
            state.refresh_status();
            state.frame.draw();
            return;
        }
        "approve" | "a" if state.phase == Phase::AwaitingApproval => {
            state.push_chat(true, trimmed);
            approve_and_run(state).await;
            state.note_activity();
            return;
        }
        "cancel" | "q" if state.phase == Phase::AwaitingApproval => {
            state.push_chat(true, trimmed);
            if let Some(plan) = state.session.pending_plan.take() {
                write_plan_md(state, &plan, "cancelled");
            }
            state.set_phase(Phase::Idle);
            state.status_extra = None;
            state.note_activity();
            state.agent_note("(plan cancelled)");
            state.refresh_status();
            state.save_if_allowed();
            return;
        }
        "edit" | "e" if state.phase == Phase::AwaitingApproval => {
            state.push_chat(true, trimmed);
            if let Some(plan) = state.session.pending_plan.clone() {
                let edit = plan.format_for_edit();
                state.frame.set_input(edit);
                state.push_chat(
                    false,
                    "plan loaded into the command line — edit it, then Enter/^S to re-plan",
                );
            }
            state.status_extra = Some("edit".into());
            state.refresh_status();
            return;
        }
        _ => {}
    }

    // New request while awaiting → treat as revision (edit path or fresh)
    if state.phase == Phase::AwaitingApproval {
        state.session.pending_plan = None;
        state.set_phase(Phase::Idle);
        state.status_extra = None;
    }

    state.push_chat(true, trimmed);
    state.brand_error = false;
    state.set_phase(Phase::Planning);
    state.refresh_status();
    state.frame.draw();

    let cfg = cfg.clone();
    let intent = trimmed.to_string();
    let handle = tokio::spawn(async move {
        JobResult::Plan(make_plan(cfg, intent).await)
    });
    *pending = Some(PendingJob { handle });
}

pub fn available() -> bool {
    io::stdout().is_terminal() && ui::color_enabled()
}

/// Open SINE. `initial` is an optional first request (from `:sine …` / `sine …`).
pub async fn run(cfg: &Config, cwd: PathBuf, initial: Option<String>) {
    if !available() {
        println!("sine: interactive only");
        return;
    }

    let guard = RawGuard::enter();
    if !guard.active() {
        println!("sine: could not enter raw mode — try :sine from a real TTY");
        return;
    }

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049h\x1b[?25h");
        let _ = out.flush();
    }
    split_frame::set_bracketed_paste(true);

    let allow_persist = cfg.mode != Mode::Hallucination;
    let sine_dir = default_sine_dir();
    let session = if allow_persist {
        load_session(&sine_dir)
    } else {
        SineSession::defaults()
    };
    let mut state = SineState::from_session(session, cfg, cwd, sine_dir, allow_persist);
    state.refresh_status();
    refresh_roster(&mut state).await;

    let mut pending: Option<PendingJob> = None;

    if let Some(first) = initial {
        let first = first.trim().to_string();
        if !first.is_empty() {
            handle_line(&mut state, &first, cfg, &mut pending).await;
        }
    }

    loop {
        // Full-screen file viewer takes over until Esc (or `e` → editor).
        if let Some(viewer) = state.viewer.as_mut() {
            viewer.relayout();
            viewer.draw();
            match split_frame::poll_key(POLL) {
                Some(Key::Esc) | Some(Key::CtrlC) => {
                    state.viewer = None;
                    state.note_activity();
                }
                Some(Key::Char('e')) | Some(Key::Char('E')) => {
                    let path = viewer.path.clone();
                    state.viewer = None;
                    state.note_activity();
                    if let Err(e) = crate::sine_editor::run_embedded(cfg, &path).await {
                        state.agent_note(&format!("editor: {e:#}"));
                    }
                    state.touch_file(&path);
                    state.note_activity();
                    state.refresh_status();
                }
                Some(Key::Up) => {
                    if let Some(v) = state.viewer.as_mut() {
                        v.scroll_by(-1);
                    }
                }
                Some(Key::Down) => {
                    if let Some(v) = state.viewer.as_mut() {
                        v.scroll_by(1);
                    }
                }
                Some(Key::PageUp) => {
                    if let Some(v) = state.viewer.as_mut() {
                        let page = FileViewer::body_rows(
                            crate::term::term_size().0.clamp(8, crate::term::MAX_ROWS),
                        ) as isize;
                        v.scroll_by(-page);
                    }
                }
                Some(Key::PageDown) => {
                    if let Some(v) = state.viewer.as_mut() {
                        let page = FileViewer::body_rows(
                            crate::term::term_size().0.clamp(8, crate::term::MAX_ROWS),
                        ) as isize;
                        v.scroll_by(page);
                    }
                }
                Some(Key::Char('g')) | Some(Key::Home) => {
                    if let Some(v) = state.viewer.as_mut() {
                        v.scroll = 0;
                    }
                }
                Some(Key::Char('G')) | Some(Key::End) => {
                    if let Some(v) = state.viewer.as_mut() {
                        let term_rows =
                            crate::term::term_size().0.clamp(8, crate::term::MAX_ROWS);
                        let body = FileViewer::body_rows(term_rows);
                        v.scroll = v.rows.len().saturating_sub(body);
                    }
                }
                _ => {}
            }
            continue;
        }

        state.refresh_status();
        state.frame.draw();

        // finish background jobs
        if let Some(job) = pending.take() {
            if job.handle.is_finished() {
                match job.handle.await {
                    Ok(JobResult::Plan(Ok((plan, timing)))) => {
                        state.record_llm_timing(timing);
                        present_plan(&mut state, plan);
                        state.note_activity();
                        state.save_if_allowed();
                        if state.session.auto {
                            approve_and_run(&mut state).await;
                            state.note_activity();
                        }
                    }
                    Ok(JobResult::Plan(Err(e))) => {
                        state.brand_error = true;
                        state.agent_note(&format!("plan error: {e:#}"));
                        state.push_chat(false, &format!("plan error: {e}"));
                        state.set_phase(Phase::Idle);
                        state.note_activity();
                    }
                    Ok(JobResult::Idle(Ok((playbook, timing)))) => {
                        state.record_llm_timing(timing);
                        state.session.playbook = playbook.clone();
                        state.agent_note("--- idle playbook ---\n");
                        state.agent_note(&playbook);
                        state.idle_done = true;
                        state.quiet_since = Instant::now();
                        state.save_if_allowed();
                    }
                    Ok(JobResult::Idle(Err(e))) => {
                        state.agent_note(&format!("idle error: {e:#}"));
                        state.idle_done = true;
                        state.quiet_since = Instant::now();
                    }
                    Err(e) => {
                        // Only brand-error for plan jobs; idle playbook failures stay soft.
                        if state.phase == Phase::Planning {
                            state.brand_error = true;
                        }
                        state.agent_note(&format!("job join error: {e}"));
                        state.set_phase(Phase::Idle);
                        state.note_activity();
                    }
                }
            } else {
                pending = Some(job);
            }
        }

        // Periodic roster (twice a minute). Step boundaries also refresh.
        if state.last_roster.elapsed() >= ROSTER_TICK {
            refresh_roster(&mut state).await;
        }

        // idle playbook only after IDLE_QUIET of true quiet — not while a
        // request is in flight, and not until quiet_since restarts after it.
        if pending.is_none()
            && state.phase == Phase::Idle
            && !state.idle_done
            && state.mode != Mode::Hallucination
            && state.frame.input.is_empty()
            && state.quiet_since.elapsed() >= IDLE_QUIET
        {
            let store = FrequencyStore::load(&state.cmdfreq_path).unwrap_or_default();
            let tops = format_top(&store.top(10, ""));
            let cfg_c = cfg.clone();
            let handle = tokio::spawn(async move {
                JobResult::Idle(make_idle_playbook(cfg_c, tops).await)
            });
            pending = Some(PendingJob { handle });
            state.agent_note("(idle: reviewing top commands…)");
        }

        match split_frame::poll_key(POLL) {
            None => continue,
            Some(Key::Esc) | Some(Key::CtrlC) => {
                if pending.is_some() && state.phase == Phase::Planning {
                    if let Some(job) = pending.take() {
                        job.handle.abort();
                    }
                    state.set_phase(Phase::Idle);
                    state.agent_note("(cancelled)");
                    continue;
                }
                break;
            }
            Some(key) if state.frame.handle_resize_key(&key) => {
                state.persist_ratios();
                continue;
            }
            Some(Key::Enter) => {
                if state.frame.pane_focus == PaneFocus::Files
                    && !state.session.touched_files.is_empty()
                    && state.frame.input.is_empty()
                {
                    state.open_selected_file();
                    state.note_activity();
                } else if split_frame::enter_should_send(
                    &state.frame.input,
                    state.frame.cursor,
                ) {
                    let line = state.frame.take_input();
                    handle_line(&mut state, &line, cfg, &mut pending).await;
                } else if state.frame.input.is_empty() {
                    // Empty Enter off the composer: no-op (do not insert a stray blank).
                    state.note_activity();
                } else {
                    state.frame.insert_newline();
                    state.note_activity();
                }
            }
            Some(Key::CtrlS) => {
                if !state.frame.input.trim().is_empty() {
                    let line = state.frame.take_input();
                    handle_line(&mut state, &line, cfg, &mut pending).await;
                }
                state.note_activity();
            }
            Some(Key::Paste(text)) => {
                state.frame.insert_str(&text);
                state.note_activity();
            }
            Some(Key::Backspace) => {
                state.frame.backspace();
                state.note_activity();
            }
            Some(Key::CtrlA) => {
                state.toggle_auto();
                state.note_activity();
            }
            Some(Key::CtrlK) => {
                // Abort in-flight plan/idle so a late result can't refill panes.
                // (^K — not ^L: many hosts treat Ctrl-L as terminal form-feed.)
                if let Some(job) = pending.take() {
                    job.handle.abort();
                }
                state.clear_context();
                state.note_activity();
                state.frame.draw();
            }
            Some(Key::CtrlR) => {
                // Reset files: list (drop prior-session paths); next shell step
                // mtime snapshot starts clean. Roster refresh re-seeds sine meta.
                state.clear_touched_files();
                refresh_roster(&mut state).await;
                state.agent_note("(files list cleared — baseline reset)");
                state.note_activity();
                state.refresh_status();
                state.frame.draw();
            }
            Some(Key::CtrlU) => {
                state.frame.clear_input();
                state.note_activity();
            }
            Some(Key::Up) => match state.frame.pane_focus {
                PaneFocus::Files => {
                    state.move_file_select(-1);
                    let _ = refresh_roster(&mut state).await;
                }
                PaneFocus::Agent | PaneFocus::Chat => {
                    state.frame.scroll_focused(-1);
                }
                PaneFocus::Input => state.frame.history_up(),
            },
            Some(Key::Down) => match state.frame.pane_focus {
                PaneFocus::Files => {
                    state.move_file_select(1);
                    let _ = refresh_roster(&mut state).await;
                }
                PaneFocus::Agent | PaneFocus::Chat => {
                    state.frame.scroll_focused(1);
                }
                PaneFocus::Input => state.frame.history_down(),
            },
            Some(Key::Left) => state.frame.move_cursor_left(),
            Some(Key::Right) => state.frame.move_cursor_right(),
            Some(Key::PageUp) => match state.frame.pane_focus {
                PaneFocus::Files => {
                    state.move_file_select(-5);
                    let _ = refresh_roster(&mut state).await;
                }
                PaneFocus::Agent | PaneFocus::Chat => {
                    state.frame.scroll_focused(-8);
                }
                PaneFocus::Input => {}
            },
            Some(Key::PageDown) => match state.frame.pane_focus {
                PaneFocus::Files => {
                    state.move_file_select(5);
                    let _ = refresh_roster(&mut state).await;
                }
                PaneFocus::Agent | PaneFocus::Chat => {
                    state.frame.scroll_focused(8);
                }
                PaneFocus::Input => {}
            },
            Some(Key::Char('e')) | Some(Key::Char('E'))
                if state.frame.pane_focus == PaneFocus::Files
                    && state.frame.input.is_empty()
                    && !state.session.touched_files.is_empty() =>
            {
                if let Some(path) = state.selected_edit_path() {
                    state.note_activity();
                    if let Err(e) = crate::sine_editor::run_embedded(cfg, &path).await {
                        state.agent_note(&format!("editor: {e:#}"));
                    }
                    state.touch_file(&path);
                    state.note_activity();
                    state.refresh_status();
                } else {
                    state.agent_note("(no file selected)");
                }
            }
            Some(Key::Char(c)) => {
                state.frame.insert_char(c);
                state.note_activity();
            }
            Some(_) => {}
        }
    }

    state.save_if_allowed();

    split_frame::set_bracketed_paste(false);
    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049l");
        let _ = out.flush();
    }
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn parse_plan_accepts_closed_verbs() {
        let raw = r#"{"summary":"hi","steps":[{"verb":"say","args":{"text":"yo"}},{"verb":"top_commands","args":{"limit":3}}]}"#;
        let plan = parse_plan_json(raw).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].verb, "say");
    }

    #[test]
    fn parse_plan_rejects_unknown_verb() {
        let raw = r#"{"summary":"x","steps":[{"verb":"dream","args":{}}]}"#;
        assert!(parse_plan_json(raw).is_err());
    }

    #[test]
    fn step_output_failed_detects_exit_and_error() {
        assert!(step_output_failed("$ x\n(exit failed)"));
        assert!(step_output_failed("Error: shomagent chat request failed"));
        assert!(!step_output_failed("$ shomagent web https://cnn.com\n✧ ok"));
    }

    fn char_at(lines: &[String], row: usize, col: usize) -> Option<char> {
        lines.get(row).and_then(|l| l.chars().nth(col))
    }

    #[test]
    fn brand_rest_keeps_packaged_eyes() {
        let lines = render_brand_lines(Avatar::Classic, BrandEyes::Rest);
        assert_eq!(char_at(&lines, EYE_ROW, EYE_LEFT_COL), Some('o'));
        assert_eq!(char_at(&lines, EYE_ROW, EYE_RIGHT_COL), Some('o'));
    }

    #[test]
    fn brand_busy_moves_eyes_onto_blank_cells() {
        // Pose index 1 = up (after one EYE_POSE_TICK).
        let lines = render_brand_lines(Avatar::Classic, BrandEyes::Busy(EYE_POSE_TICK));
        assert_eq!(char_at(&lines, EYE_ROW, EYE_LEFT_COL), Some(BRAILLE_BLANK));
        assert_eq!(char_at(&lines, EYE_ROW, EYE_RIGHT_COL), Some(BRAILLE_BLANK));
        assert_eq!(char_at(&lines, EYE_ROW - 1, EYE_LEFT_COL), Some('o'));
        assert_eq!(char_at(&lines, EYE_ROW - 1, EYE_RIGHT_COL), Some('o'));
        // Both pupils land on blank face interior (no border ink).
        let rest = render_brand_lines(Avatar::Classic, BrandEyes::Rest);
        assert!(cell_is_blank(&rest, EYE_ROW - 1, EYE_LEFT_COL));
        assert!(cell_is_blank(&rest, EYE_ROW - 1, EYE_RIGHT_COL));
    }

    #[test]
    fn brand_error_replaces_eyes_with_x() {
        let lines = render_brand_lines(Avatar::Classic, BrandEyes::Error);
        assert_eq!(char_at(&lines, EYE_ROW, EYE_LEFT_COL), Some('x'));
        assert_eq!(char_at(&lines, EYE_ROW, EYE_RIGHT_COL), Some('x'));
        let face = &lines[EYE_ROW];
        assert!(!face.contains('o'), "error face must not keep resting o: {face}");
    }

    #[test]
    fn brand_navi_is_nonempty_without_eye_row() {
        let lines = render_brand_lines(Avatar::Navi, BrandEyes::Rest);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| chrome::visible_len(l) > 0));
        // Navi art is wider than the bunny eye grid; do not require EYE_ROW.
        assert!(lines.len() >= 8);
    }

    #[test]
    fn brand_coffee_busy_differs_from_rest() {
        let rest = render_brand_lines(Avatar::Coffee, BrandEyes::Rest);
        let busy = render_brand_lines(Avatar::Coffee, BrandEyes::Busy(Duration::from_secs(1)));
        let rest_plain: Vec<_> = rest.iter().map(|l| chrome::strip_ansi(l)).collect();
        let busy_plain: Vec<_> = busy.iter().map(|l| chrome::strip_ansi(l)).collect();
        assert_ne!(rest_plain, busy_plain);
    }

    #[test]
    fn brand_laptop_busy_differs_from_rest() {
        let rest = render_brand_lines(Avatar::Laptop, BrandEyes::Rest);
        let busy = render_brand_lines(Avatar::Laptop, BrandEyes::Busy(Duration::from_secs(1)));
        let rest_plain: Vec<_> = rest.iter().map(|l| chrome::strip_ansi(l)).collect();
        let busy_plain: Vec<_> = busy.iter().map(|l| chrome::strip_ansi(l)).collect();
        assert_ne!(rest_plain, busy_plain);
    }

    #[test]
    fn brand_mana_busy_differs_from_rest() {
        let rest = render_brand_lines(Avatar::Mana, BrandEyes::Rest);
        let busy = render_brand_lines(Avatar::Mana, BrandEyes::Busy(Duration::from_secs(1)));
        let rest_plain: Vec<_> = rest.iter().map(|l| chrome::strip_ansi(l)).collect();
        let busy_plain: Vec<_> = busy.iter().map(|l| chrome::strip_ansi(l)).collect();
        assert!(!rest_plain.is_empty());
        assert_ne!(rest_plain, busy_plain);
    }

    #[test]
    fn brand_leaf_busy_differs_from_rest() {
        let rest = render_brand_lines(Avatar::Leaf, BrandEyes::Rest);
        let busy = render_brand_lines(Avatar::Leaf, BrandEyes::Busy(Duration::from_secs(1)));
        let rest_plain: Vec<_> = rest.iter().map(|l| chrome::strip_ansi(l)).collect();
        let busy_plain: Vec<_> = busy.iter().map(|l| chrome::strip_ansi(l)).collect();
        assert!(!rest_plain.is_empty());
        assert_ne!(rest_plain, busy_plain);
    }

    #[test]
    fn format_plan_markdown_embeds_json() {
        let plan = SinePlan {
            summary: "Fetch HN".into(),
            steps: vec![PlanStep {
                verb: "shell".into(),
                args: serde_json::json!({"command": "shomagent web https://news.ycombinator.com"}),
            }],
        };
        let md = format_plan_markdown(&plan, "awaiting approve");
        assert!(md.contains("# SINE plan"));
        assert!(md.contains("status: awaiting approve"));
        assert!(md.contains("shomagent web https://news.ycombinator.com"));
        assert!(md.contains("```json"));
        assert!(md.contains("\"verb\": \"shell\""));
    }

    #[test]
    fn parse_plan_normalizes_shomagent_web_compound() {
        let raw = r#"{
          "summary":"fetch hn",
          "steps":[
            {"verb":"shomagent web","args":{"url":"https://news.ycombinator.com"}},
            {"verb":"shomagent ask","args":{"id":1,"message":"top 10"}},
            {"verb":"shomagent stop","args":{"id":1}}
          ]
        }"#;
        let plan = parse_plan_json(raw).unwrap();
        assert_eq!(plan.steps[0].verb, "shell");
        assert_eq!(
            plan.steps[0].args["command"].as_str().unwrap(),
            "shomagent web https://news.ycombinator.com"
        );
        assert_eq!(plan.steps[1].verb, "shomagent");
        assert_eq!(plan.steps[1].args["action"].as_str().unwrap(), "ask");
        assert_eq!(plan.steps[1].args["id"], 1);
        assert_eq!(plan.steps[2].verb, "shomagent");
        assert_eq!(plan.steps[2].args["action"].as_str().unwrap(), "stop");
    }

    #[test]
    fn stub_plan_has_allowed_verbs() {
        let plan = stub_plan("hello");
        for step in &plan.steps {
            assert!(matches!(
                step.verb.as_str(),
                "shell" | "shomagent" | "top_commands" | "say" | "quickyn"
            ));
        }
    }

    #[test]
    fn parse_plan_accepts_quickyn() {
        let raw = r#"{"summary":"gate","steps":[{"verb":"quickyn","args":{"question":"ship it?"}}]}"#;
        let plan = parse_plan_json(raw).unwrap();
        assert_eq!(plan.steps[0].verb, "quickyn");
    }

    #[test]
    fn extract_redirect_paths() {
        let cwd = PathBuf::from("/engram/user");
        let paths = extract_touched_paths(
            "echo hi > /engram/user/yc_headlines_test.csv",
            &cwd,
        );
        assert!(
            paths.iter().any(|p| p.ends_with("yc_headlines_test.csv")),
            "got {paths:?}"
        );
        let tee = extract_touched_paths("echo hi | tee notes.csv", &cwd);
        assert!(
            tee.iter().any(|p| p.ends_with("notes.csv")),
            "got {tee:?}"
        );
        // Date expansions are not literal — snapshot handles those after exec.
        let dyn_paths = extract_touched_paths(
            "echo hi > yc_headlines_$(date +%Y%m%d_%H%M%S).csv",
            &cwd,
        );
        assert!(
            dyn_paths.is_empty(),
            "literal extract should skip $() redirects, got {dyn_paths:?}"
        );
    }

    #[test]
    fn discover_new_or_updated_sees_created_csv() {
        let dir = std::env::temp_dir().join(format!(
            "sine-touch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dirs = vec![dir.clone()];
        let before = snapshot_dir_files(&dirs);
        let csv = dir.join("yc_headlines_20261123_012320.csv");
        std::fs::write(&csv, "a,b\n1,2\n").unwrap();
        let found = discover_new_or_updated(&before, &dirs);
        assert!(
            found.iter().any(|p| p == &csv),
            "expected new csv in {found:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_paths_from_output_text() {
        let paths = extract_paths_from_text(
            "wrote /engram/user/yc_headlines_20261123_012320.csv ok",
        );
        assert!(
            paths
                .iter()
                .any(|p| p.ends_with("yc_headlines_20261123_012320.csv")),
            "got {paths:?}"
        );
    }

    #[test]
    fn wrap_numbered_lines_starts_at_one_and_keeps_rule() {
        // width 12, gutter "  1│ " (5) => body_w 7; 20 chars must wrap.
        let rows = wrap_numbered_lines("abcdefghijklmnopqrst\nxy", 12);
        assert!(
            rows[0].trim_start().starts_with("1│"),
            "first display row should be source line 1, got {:?}",
            rows[0]
        );
        assert!(rows.len() >= 3, "expected wraps, got {rows:?}");
        // Continuations keep the solid vertical rule and have no digits.
        let wraps: Vec<_> = rows
            .iter()
            .filter(|r| r.contains('│') && !r.chars().any(|c| c.is_ascii_digit()))
            .collect();
        assert!(
            !wraps.is_empty(),
            "expected a wrap continuation with │, rows={rows:?}"
        );
        for w in &wraps {
            assert!(
                w.trim_start().starts_with('│'),
                "continuation must keep the gutter rule: {w:?}"
            );
        }
    }

    #[test]
    fn wrap_numbered_lines_top_is_always_line_one() {
        let mut text = String::new();
        for i in 1..=40 {
            text.push_str(&format!("line-{i}\n"));
        }
        let rows = wrap_numbered_lines(&text, 80);
        assert!(
            rows[0].contains("1│") && rows[0].contains("line-1"),
            "top of wrap must be source line 1, got {:?}",
            rows[0]
        );
        assert!(
            !rows[0].contains("19│"),
            "must not start at line 19"
        );
    }

    #[test]
    fn session_round_trip() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "sine-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut session = SineSession::defaults();
        session.auto = true;
        session.chat.push(ChatTurn {
            user: true,
            text: "hi".into(),
        });
        session.playbook = "# play\n".into();
        session.pending_plan = Some(stub_plan("x"));
        save_session(&dir, &session).unwrap();
        let loaded = load_session(&dir);
        assert!(loaded.auto);
        assert_eq!(loaded.chat.len(), 1);
        assert!(loaded.pending_plan.is_some());
        assert!(loaded.playbook.contains("play"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_json_from_fences() {
        let raw = "here\n```json\n{\"summary\":\"a\",\"steps\":[]}\n```\n";
        let plan = parse_plan_json(raw).unwrap();
        assert_eq!(plan.summary, "a");
    }

    #[test]
    fn parse_plan_repairs_bare_quotes_in_command() {
        // LLM often leaves "print" unescaped inside the command string.
        let raw = r#"{
  "summary": "Add a hello world print statement in hello_world.py",
  "steps": [
    {
      "verb": "shell",
      "args": {
        "command": "sh -c 'cat hello_world.py | grep -v "print" > hello_world.py'"
      }
    },
    {
      "verb": "shell",
      "args": {
        "command": "shomagent ask 0 \"Add a 'print' statement to hello_world.py\""
      }
    },
    {
      "verb": "shell",
      "args": {
        "command": "shomagent stop 0"
      }
    }
  ]
}"#;
        let plan = parse_plan_json(raw).unwrap();
        assert_eq!(plan.steps.len(), 3);
        let cmd = plan.steps[0].args.get("command").and_then(|v| v.as_str()).unwrap();
        assert!(cmd.contains("grep -v \"print\""), "cmd={cmd}");
    }

    #[test]
    fn parse_plan_repairs_shell_backslash_escapes() {
        let raw = r#"{"summary":"x","steps":[{"verb":"shell","args":{"command":"find . -name \*.py \; "}}]}"#;
        let plan = parse_plan_json(raw).unwrap();
        let cmd = plan.steps[0].args.get("command").and_then(|v| v.as_str()).unwrap();
        assert!(cmd.contains(r"\*.py"), "cmd={cmd}");
    }

    #[test]
    fn plan_system_prioritizes_shomagent_web() {
        assert!(
            PLAN_SYSTEM.contains("shomagent web"),
            "planner must know about shomagent web"
        );
        assert!(
            PLAN_SYSTEM.contains("Do NOT use curl"),
            "planner must deprioritize curl scrapers for pages"
        );
        assert!(
            PLAN_SYSTEM.contains("shomagent list") && PLAN_SYSTEM.contains("id 0"),
            "planner must list roster and use id 0 placeholders"
        );
        assert!(
            PLAN_SYSTEM.contains("shomloop")
                && PLAN_SYSTEM.contains("daily at 4am")
                && PLAN_SYSTEM.contains("yc_headlines_job.sh")
                && PLAN_SYSTEM.contains("Omitting shomloop"),
            "planner must prefer job script + shomloop when a schedule is asked"
        );
    }

    #[test]
    fn parse_plan_normalizes_shomagent_list_to_roster() {
        let raw = r#"{"summary":"list","steps":[{"verb":"shomagent list","args":{}}]}"#;
        let plan = parse_plan_json(raw).unwrap();
        assert_eq!(plan.steps[0].verb, "shomagent");
        assert_eq!(
            plan.steps[0].args.get("action").and_then(|v| v.as_str()),
            Some("roster")
        );
    }

    #[test]
    fn parse_attached_member_id_from_web_line() {
        let out = "⟡ shomagent#2 web (https://www.slashdot.org)\n";
        assert_eq!(parse_attached_member_id(out), Some(2));
    }

    #[test]
    fn rewrite_member_id_rewrites_ask_and_nested_shell() {
        let cmd = r#"sh -c 'shomagent ask 5 "hi"; shomagent stop 5'"#;
        let next = rewrite_member_id_in_command(cmd, 5, 2);
        assert!(next.contains("ask 2"), "{next}");
        assert!(next.contains("stop 2"), "{next}");
        assert!(!next.contains("ask 5"), "{next}");
    }

    #[test]
    fn rewrite_member_id_zero_placeholder() {
        let cmd = r#"shomagent ask 0 "top links""#;
        assert_eq!(
            rewrite_member_id_in_command(cmd, 0, 2),
            r#"shomagent ask 2 "top links""#
        );
    }

    #[test]
    fn pick_bound_member_prefers_last_then_web() {
        use shomagent::protocol::MemberStatus;
        let roster = vec![
            MemberStatus {
                id: 1,
                name: "old".into(),
                state: "web".into(),
                pid: None,
                runtime_secs: 0,
                findings: 0,
                tell_me: String::new(),
                command: "https://example.com".into(),
            },
            MemberStatus {
                id: 2,
                name: "slashdot".into(),
                state: "web".into(),
                pid: None,
                runtime_secs: 0,
                findings: 0,
                tell_me: String::new(),
                command: "https://www.slashdot.org".into(),
            },
        ];
        assert_eq!(pick_bound_member(Some(2), &roster), Some(2));
        assert_eq!(pick_bound_member(Some(9), &roster), Some(2)); // stale → max web
        assert_eq!(pick_bound_member(None, &roster), Some(2));
    }

    #[test]
    fn llm_timing_toks_per_sec() {
        let t = LlmTiming {
            elapsed: Duration::from_secs(2),
            out_chars: 80, // 20 est tokens
        };
        let rate = t.toks_per_sec();
        assert!((rate - 10.0).abs() < 0.01, "got {rate}");
    }

    #[test]
    fn format_for_edit_puts_shell_on_one_line() {
        let plan = SinePlan {
            summary: "fetch hn".into(),
            steps: vec![PlanStep {
                verb: "shell".into(),
                args: serde_json::json!({"command": "shomagent web https://news.ycombinator.com"}),
            }],
        };
        let edit = plan.format_for_edit();
        assert!(edit.contains("fetch hn"));
        assert!(edit.contains("shomagent web"));
        assert!(!edit.contains('\n'));
    }

    #[test]
    fn context_tokens_grow_with_chat() {
        let mut session = SineSession::defaults();
        let base = estimate_sine_tokens(&session);
        assert_eq!(base, 0, "empty session must meter as 0");
        session.chat.push(ChatTurn {
            user: true,
            text: "x".repeat(400),
        });
        let after = estimate_sine_tokens(&session);
        assert!(after > base, "expected growth {base} -> {after}");
        let label = format!(
            "ctx {} tok · est {}%",
            after,
            crate::chat_tui::percent(after, 32_768)
        );
        assert!(label.starts_with("ctx "));
        assert!(label.contains("tok"));
    }

    #[test]
    fn cleared_session_meters_zero() {
        let mut session = SineSession::defaults();
        session.chat.push(ChatTurn {
            user: true,
            text: "hello".into(),
        });
        session.agent_log.push("PLAN: …".into());
        session.playbook = "# notes\n".into();
        session.pending_plan = Some(stub_plan("x"));
        assert!(estimate_sine_tokens(&session) > 0);
        session.chat.clear();
        session.agent_log.clear();
        session.playbook.clear();
        session.pending_plan = None;
        assert_eq!(estimate_sine_tokens(&session), 0);
    }
}
