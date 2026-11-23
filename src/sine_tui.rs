//! SINE — semi-persistent plan-then-approve assistant (`:sine` / bare `sine`).
//!
//! Dependency surface is ADR-0014. Layout chrome is [`crate::split_frame`].

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use engram_cmdfreq::{format_top, FrequencyStore};
use serde::{Deserialize, Serialize};
use shomagent::daemon::rpc;
use shomagent::protocol::{Request, Response};

use crate::config::Config;
use crate::llm::{ChatMessage, LlmClient};
use crate::modes::Mode;
use crate::split_frame::{self, Key, Slot, SplitFrame};
use crate::term::RawGuard;
use crate::ui;

/// Brand art for the left-top pane. Packaged copy of `docs/sine_image.txt`
/// (docs/ is dockerignored; the image build copies `assets/`).
const SINE_IMAGE: &str = include_str!("../assets/sine_image.txt");

const PLAN_SYSTEM: &str = "\
You are SINE, a planning assistant inside Shomsky Linux (BusyBox ash, Alpine).
Prefer first-party Shomsky tools over inventing curl/grep scrapers.
First-party CLI (invoke with verb shell when not covered by the shomagent verb):
  shomagent web <url>     headless Chromium; use for any webpage / JS / headlines
  shomagent ask <id> \"…\"  question a member (web page, watch, document)
  shomagent click|type|navigate|stop <id> …
  shomagent watch|spawn|read|readwrite|dream|refine …
  shomloop …              schedules only when the user asks for recurring work
  quickyn \"…\"             yes/no + confidence
For news.ycombinator.com or other sites: plan shell steps that use
  shomagent web https://… then shomagent ask <id> \"…\" (and stop when done).
Do NOT use curl|wget against HTML/RSS for page content when shomagent web fits.
Do NOT invent a schedule (shomloop) unless the user asked for one.
Given the user request and optional context, output ONLY a single JSON object:
  \"summary\": one short sentence describing the plan
  \"steps\": array of objects, each with:
    \"verb\": one of shell | shomagent | top_commands | say
    \"args\": object
Verb args:
  shell: {\"command\": \"...\"}   // BusyBox ash OR first-party CLI lines above
  shomagent: {\"action\": \"ask|pause|resume|stop|roster\", \"id\": <u32 optional>, \"message\": \"... optional\"}
  top_commands: {\"limit\": <n optional>}
  say: {\"text\": \"...\"}
The shomagent verb is only for ask/pause/resume/stop/roster on an existing id.
Opening a URL is always shell with shomagent web. Prefer few steps.
No markdown fences. No verbs outside that list.";

const IDLE_SYSTEM: &str = "\
You are SINE idle mode. Given the operator's most-used shell commands, write a
short playbook (markdown, under 20 lines) on using first-party tools better:
shomagent (web, watch, ask, read/readwrite), shomloop, quickyn, :sine, :top.
Prefer shomagent web over curl for pages. Be concrete. No preamble.";

const STUB_SUMMARY: &str = "stub: list top commands then say hello";
const IDLE_QUIET: Duration = Duration::from_secs(60);
/// Periodic roster refresh when idle (twice a minute). Also refreshed on
/// plan step boundaries when commands kick off / return.
const ROSTER_TICK: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// Same glyphs as `chat_tui` / `spinner.rs` (Alpine ASCII-only).
const SPIN_FRAMES: &[char] = &['-', '\\', '|', '/'];
const SPIN_TICK: Duration = Duration::from_millis(120);

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

/// Estimate tokens for SINE working memory (plan system + chat + pending plan).
/// Same chars/4 heuristic as [`crate::chat_tui::estimate_tokens`].
pub fn estimate_sine_tokens(session: &SineSession) -> usize {
    let mut n = "system".chars().count() + PLAN_SYSTEM.chars().count();
    for turn in &session.chat {
        let role = if turn.user { "user" } else { "assistant" };
        n += role.chars().count() + turn.text.chars().count();
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
    let plan: SinePlan = serde_json::from_str(json_str)
        .with_context(|| format!("deserialize sine plan; raw={raw}"))?;
    if plan.steps.is_empty() && plan.summary.trim().is_empty() {
        bail!("empty plan");
    }
    for step in &plan.steps {
        match step.verb.as_str() {
            "shell" | "shomagent" | "top_commands" | "say" => {}
            other => bail!("unknown verb '{other}'"),
        }
    }
    Ok(plan)
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

enum JobResult {
    Plan(Result<SinePlan>),
    Idle(Result<String>),
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
    last_input: Instant,
    last_roster: Instant,
    idle_done: bool,
    status_extra: Option<String>,
    /// When Planning/Running started (for brand-pane spinner).
    busy_since: Option<Instant>,
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
            SINE_IMAGE.lines().map(|l| l.to_string()).collect(),
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
            last_input: Instant::now(),
            last_roster: Instant::now()
                .checked_sub(ROSTER_TICK)
                .unwrap_or_else(Instant::now),
            idle_done: false,
            status_extra: None,
            busy_since: None,
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        let busy = matches!(phase, Phase::Planning | Phase::Running);
        match (busy, self.busy_since) {
            (true, None) => self.busy_since = Some(Instant::now()),
            (false, _) => self.busy_since = None,
            _ => {}
        }
        self.phase = phase;
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
            "Esc exit · Enter send · ^A auto:{auto} · ^L clear · Tab focus · Ctrl-arrows · a/e/q plan"
        ));
        // Live meter on the agent pane header (same estimate as :chat).
        self.frame
            .set_leaf_header("right_top", format!("agent ── {ctx}"));
        self.frame
            .set_leaf_header("right_bottom", format!("chat ── {ctx}"));
        self.paint_brand();
    }

    /// ASCII art plus a spinner line while planning or running.
    fn paint_brand(&mut self) {
        let mut lines: Vec<String> = SINE_IMAGE.lines().map(|l| l.to_string()).collect();
        if let Some(since) = self.busy_since {
            if matches!(self.phase, Phase::Planning | Phase::Running) {
                let label = match self.phase {
                    Phase::Planning => "planning",
                    Phase::Running => "running",
                    _ => "busy",
                };
                let g = spinner_glyph(since.elapsed());
                lines.push(String::new());
                lines.push(format!("[{g}] {label}…"));
            }
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

    /// Clear chat, agent log, and pending plan (working context).
    fn clear_context(&mut self) {
        self.session.chat.clear();
        self.session.pending_plan = None;
        self.session.agent_log.clear();
        self.frame.clear_slot(Slot::RightTop);
        self.frame.clear_slot(Slot::RightBottom);
        self.frame.clear_input();
        self.set_phase(Phase::Idle);
        self.status_extra = None;
        self.push_chat(false, "(context cleared)");
        self.refresh_status();
        self.save_if_allowed();
    }

    /// Working-memory estimate for SINE (chars/4), same window as `:chat`.
    fn context_tokens(&self) -> usize {
        estimate_sine_tokens(&self.session)
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

    fn persist_ratios(&mut self) {
        self.session.vertical_ratio = self.frame.vertical_ratio();
        self.session.left_h_ratio = self.frame.left_horizontal_ratio();
        self.session.right_h_ratio = self.frame.right_horizontal_ratio();
        self.session.input_history = self.frame.input_history.clone();
        if let Some(leaf) = self.frame.leaf("right_top") {
            self.session.agent_log = leaf.lines.clone();
        }
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
        Err(_) => {
            lines.push("  (daemon unavailable)".into());
        }
    }
    lines.push("shomloops:".into());
    match shomloop::list_runs() {
        Ok(runs) => {
            if runs.is_empty() {
                lines.push("  (none)".into());
            } else {
                for r in runs {
                    let mark = if r.enabled { "+" } else { "-" };
                    lines.push(format!(
                        "  {mark}#{} {} {}",
                        r.id, r.frequency, r.command
                    ));
                }
            }
        }
        Err(_) => lines.push("  (ledger unavailable)".into()),
    }
    state.frame.set_slot_lines(Slot::LeftBottom, lines);
    state.last_roster = Instant::now();
}

async fn make_plan(cfg: Config, intent: String) -> Result<SinePlan> {
    if sine_stub() {
        return Ok(stub_plan(&intent));
    }
    if cfg.mode == Mode::Root {
        // Root skips the model: wrap the line as a shell step.
        return Ok(SinePlan {
            summary: format!("run as shell: {intent}"),
            steps: vec![PlanStep {
                verb: "shell".into(),
                args: serde_json::json!({"command": intent}),
            }],
        });
    }
    let llm = LlmClient::new(&cfg)?;
    let messages = vec![
        ChatMessage::system(PLAN_SYSTEM),
        ChatMessage::user(&intent),
    ];
    let raw = llm
        .chat_text_max(&messages, 512)
        .await
        .context("sine plan LLM")?;
    match parse_plan_json(&raw) {
        Ok(plan) => Ok(plan),
        Err(e1) => {
            let repair = vec![
                ChatMessage::system(PLAN_SYSTEM),
                ChatMessage::user(&intent),
                ChatMessage::assistant_text(&raw),
                ChatMessage::user(format!(
                    "Previous reply was not valid JSON ({e1}). Reply with ONLY the JSON object."
                )),
            ];
            let raw2 = llm
                .chat_text_max(&repair, 512)
                .await
                .context("sine plan LLM retry")?;
            parse_plan_json(&raw2)
        }
    }
}

async fn make_idle_playbook(cfg: Config, tops: String) -> Result<String> {
    if sine_stub() {
        return Ok(format!(
            "# SINE playbook (stub)\n\nTop commands:\n\n{tops}\n\nPrefer short pipelines and `:sine` for plans.\n"
        ));
    }
    let llm = LlmClient::new(&cfg)?;
    let messages = vec![
        ChatMessage::system(IDLE_SYSTEM),
        ChatMessage::user(format!("Most-used commands:\n{tops}")),
    ];
    llm.chat_text_max(&messages, 512)
        .await
        .context("sine idle LLM")
}

async fn execute_plan(
    plan: &SinePlan,
    state: &mut SineState,
) -> Result<()> {
    if state.mode == Mode::Hallucination {
        state.agent_note("(hallucination mode: plan shown, not executed)");
        return Ok(());
    }
    refresh_roster(state).await;
    state.refresh_status();
    state.frame.draw();
    for step in &plan.steps {
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
    }
    Ok(())
}

fn spinner_glyph(elapsed: Duration) -> char {
    let i = (elapsed.as_millis() / SPIN_TICK.as_millis()) as usize;
    SPIN_FRAMES[i % SPIN_FRAMES.len()]
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
        "shell" => {
            let command = step
                .args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("shell step missing command"))?;
            let result =
                crate::tools::shell_exec(&state.busybox, command, &state.cwd, &state.home).await?;
            state.cwd = result.cwd;
            let mut out = format!("$ {command}\n{}", result.output);
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
            let req = match action {
                "roster" => Request::Roster,
                "pause" => {
                    let id = step
                        .args
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("pause needs id"))? as u32;
                    Request::Pause { id }
                }
                "resume" => {
                    let id = step
                        .args
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("resume needs id"))? as u32;
                    Request::Resume { id }
                }
                "stop" => {
                    let id = step
                        .args
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("stop needs id"))? as u32;
                    Request::Stop { id }
                }
                "ask" => {
                    let id = step
                        .args
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("ask needs id"))? as u32;
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
                    let mut out = String::new();
                    if let Some(m) = message {
                        out.push_str(&m);
                    }
                    if let Some(r) = reply {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&r);
                    }
                    if !roster.is_empty() {
                        for m in roster {
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
                Response::Err { message } => Ok(format!("shomagent error: {message}")),
            }
        }
        other => bail!("unknown verb '{other}'"),
    }
}

fn present_plan(state: &mut SineState, plan: SinePlan) {
    state.agent_note(&plan.format_for_pane());
    state.push_chat(false, &format!("plan ready: {}", plan.summary));
    state.session.pending_plan = Some(plan);
    state.set_phase(Phase::AwaitingApproval);
    if state.session.auto {
        // auto still shows the plan; run without waiting for a second confirm
        state.status_extra = Some("auto-run".into());
    }
    state.refresh_status();
}

async fn approve_and_run(state: &mut SineState) {
    let Some(plan) = state.session.pending_plan.clone() else {
        return;
    };
    state.set_phase(Phase::Running);
    state.refresh_status();
    state.frame.draw();
    match execute_plan(&plan, state).await {
        Ok(()) => {
            state.push_chat(false, "done");
            state.agent_note("--- done ---");
        }
        Err(e) => {
            state.agent_note(&format!("error: {e:#}"));
            state.push_chat(false, &format!("error: {e}"));
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
    state.last_input = Instant::now();
    state.idle_done = false;

    // Local controls (shell-owned; model cannot invent these as verbs)
    match trimmed.to_ascii_lowercase().as_str() {
        "auto" => {
            state.toggle_auto();
            return;
        }
        "clear" | "clear context" => {
            state.clear_context();
            return;
        }
        "approve" | "a" if state.phase == Phase::AwaitingApproval => {
            state.push_chat(true, trimmed);
            approve_and_run(state).await;
            return;
        }
        "cancel" | "q" if state.phase == Phase::AwaitingApproval => {
            state.push_chat(true, trimmed);
            state.session.pending_plan = None;
            state.set_phase(Phase::Idle);
            state.status_extra = None;
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
                    "plan loaded into the command line — edit it, then Enter to re-plan",
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
        state.refresh_status();
        state.frame.draw();

        // finish background jobs
        if let Some(job) = pending.take() {
            if job.handle.is_finished() {
                match job.handle.await {
                    Ok(JobResult::Plan(Ok(plan))) => {
                        present_plan(&mut state, plan);
                        state.save_if_allowed();
                        if state.session.auto {
                            approve_and_run(&mut state).await;
                        }
                    }
                    Ok(JobResult::Plan(Err(e))) => {
                        state.agent_note(&format!("plan error: {e:#}"));
                        state.push_chat(false, &format!("plan error: {e}"));
                        state.set_phase(Phase::Idle);
                    }
                    Ok(JobResult::Idle(Ok(playbook))) => {
                        state.session.playbook = playbook.clone();
                        state.agent_note("--- idle playbook ---\n");
                        state.agent_note(&playbook);
                        state.idle_done = true;
                        state.save_if_allowed();
                    }
                    Ok(JobResult::Idle(Err(e))) => {
                        state.agent_note(&format!("idle error: {e:#}"));
                        state.idle_done = true;
                    }
                    Err(e) => {
                        state.agent_note(&format!("job join error: {e}"));
                        state.set_phase(Phase::Idle);
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

        // idle playbook (agent/root only; not while busy or awaiting)
        if pending.is_none()
            && state.phase == Phase::Idle
            && !state.idle_done
            && state.mode != Mode::Hallucination
            && state.frame.input.is_empty()
            && state.last_input.elapsed() >= IDLE_QUIET
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
            Some(key) if state.frame.handle_resize_key(key) => {
                state.persist_ratios();
                continue;
            }
            Some(Key::Enter) => {
                let line = state.frame.take_input();
                handle_line(&mut state, &line, cfg, &mut pending).await;
            }
            Some(Key::Backspace) => {
                state.frame.backspace();
                state.last_input = Instant::now();
                state.idle_done = false;
            }
            Some(Key::CtrlA) => {
                state.toggle_auto();
            }
            Some(Key::CtrlL) => {
                state.clear_context();
            }
            Some(Key::CtrlU) => {
                state.frame.clear_input();
                state.last_input = Instant::now();
                state.idle_done = false;
            }
            Some(Key::Up) => state.frame.history_up(),
            Some(Key::Down) => state.frame.history_down(),
            Some(Key::Left) => state.frame.move_cursor_left(),
            Some(Key::Right) => state.frame.move_cursor_right(),
            Some(Key::PageUp) => state.frame.scroll_focused(-8),
            Some(Key::PageDown) => state.frame.scroll_focused(8),
            Some(Key::Char(c)) => {
                state.frame.insert_char(c);
                state.last_input = Instant::now();
                state.idle_done = false;
            }
            Some(_) => {}
        }
    }

    state.save_if_allowed();

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
    fn stub_plan_has_allowed_verbs() {
        let plan = stub_plan("hello");
        for step in &plan.steps {
            assert!(matches!(
                step.verb.as_str(),
                "shell" | "shomagent" | "top_commands" | "say"
            ));
        }
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
    fn plan_system_prioritizes_shomagent_web() {
        assert!(
            PLAN_SYSTEM.contains("shomagent web"),
            "planner must know about shomagent web"
        );
        assert!(
            PLAN_SYSTEM.contains("Do NOT use curl"),
            "planner must deprioritize curl scrapers for pages"
        );
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
}
