//! Immersive framed chat (`:chat`).
//!
//! Transcript + one-line input, context counters, compactify. Same ANSI +
//! BusyBox `stty` approach as `:quickyn` — no TUI crates. New session; does not
//! read or write REPL history; no tools.

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::llm::{ChatMessage, LlmClient};
use crate::modes::Mode;
use crate::term::{self, RawGuard};
use crate::ui;

const SYSTEM_PROMPT: &str =
    "You are chatting with the operator in engram-shell. Answer directly. Do not call tools.";

const DEFAULT_CONTEXT_TOKENS: usize = 32_768;
const REPLY_MAX_TOKENS: u32 = 1024;
const COMPACTIFY_MAX_TOKENS: u32 = 512;
const STUB_REPLY: &str = "stub-reply";
const STUB_SUMMARY: &str = "stub-summary of the conversation so far.";
const COMPACTIFY_ASK: &str = "Summarize this conversation so far. Keep decisions, names, and open questions. Be concise. Output only the summary.";
/// Same glyphs as `spinner.rs` so Alpine TTYs stay ASCII-only.
const SPIN_FRAMES: &[char] = &['-', '\\', '|', '/'];
const SPIN_TICK: Duration = Duration::from_millis(120);

// ── Pure state ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn api_role(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Role::User => "you",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Compactify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    None,
    Reply,
    Compactify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendAction {
    None,
    Reply,
    Compactify,
}

#[derive(Debug, Clone)]
pub struct TuiState {
    pub system: String,
    pub turns: Vec<Turn>,
    pub input: String,
    pub cursor: usize,
    pub focus: Focus,
    pub scroll_top: usize,
    pub follow_bottom: bool,
    pub thinking: Thinking,
    pub thinking_since: Option<Instant>,
    pub status: Option<String>,
    pub window_tokens: usize,
    pub save_path: PathBuf,
    pub allow_save: bool,
}

impl TuiState {
    pub fn new(cwd: PathBuf, allow_save: bool) -> Self {
        Self {
            system: SYSTEM_PROMPT.into(),
            turns: Vec::new(),
            input: String::new(),
            cursor: 0,
            focus: Focus::Input,
            scroll_top: 0,
            follow_bottom: true,
            thinking: Thinking::None,
            thinking_since: None,
            status: None,
            window_tokens: context_window_tokens(),
            save_path: cwd.join(saved_chat_filename(&datetimestamp_slug())),
            allow_save,
        }
    }

    pub fn tokens(&self) -> usize {
        estimate_tokens(&self.system, &self.turns)
    }

    pub fn percent(&self) -> usize {
        percent(self.tokens(), self.window_tokens)
    }

    pub fn title_text(&self) -> String {
        format!(
            " chat ── ctx {} tok · est {}% ",
            self.tokens(),
            self.percent()
        )
    }

    pub fn api_messages(&self) -> Vec<ChatMessage> {
        let mut v = vec![ChatMessage::system(self.system.clone())];
        for t in &self.turns {
            match t.role {
                Role::User => v.push(ChatMessage::user(t.text.clone())),
                Role::Assistant => v.push(ChatMessage::assistant_text(t.text.clone())),
            }
        }
        v
    }

    pub fn compactify_messages(&self) -> Vec<ChatMessage> {
        let mut v = self.api_messages();
        v.push(ChatMessage::user(COMPACTIFY_ASK));
        v
    }

    pub fn display_lines(&self, width: usize) -> Vec<String> {
        let mut lines = transcript_lines(&self.turns, width);
        if self.thinking == Thinking::Reply {
            lines.push("assistant".into());
            lines.push("  ···".into());
        }
        if self.thinking == Thinking::Compactify {
            lines.push(format!("  {}", compactify_wait_line(self.thinking_elapsed())));
        }
        lines
    }

    pub fn thinking_elapsed(&self) -> Duration {
        self.thinking_since
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    pub fn set_thinking(&mut self, kind: Thinking) {
        self.thinking = kind;
        self.thinking_since = if kind == Thinking::None {
            None
        } else {
            Some(Instant::now())
        };
        if kind != Thinking::None {
            self.status = None;
        }
    }

    pub fn clamp_scroll(&mut self, view_h: usize, total: usize) {
        let max_top = total.saturating_sub(view_h);
        if self.follow_bottom {
            self.scroll_top = max_top;
        } else if self.scroll_top > max_top {
            self.scroll_top = max_top;
        }
    }

    pub fn scroll_up(&mut self, n: usize, view_h: usize, total: usize) {
        self.follow_bottom = false;
        self.scroll_top = self.scroll_top.saturating_sub(n);
        self.clamp_scroll(view_h, total);
    }

    pub fn scroll_down(&mut self, n: usize, view_h: usize, total: usize) {
        let max_top = total.saturating_sub(view_h);
        self.scroll_top = self.scroll_top.saturating_add(n).min(max_top);
        if self.scroll_top >= max_top {
            self.follow_bottom = true;
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input => Focus::Compactify,
            Focus::Compactify => Focus::Input,
        };
    }

    pub fn insert_char(&mut self, c: char) -> SendAction {
        if self.thinking != Thinking::None {
            return SendAction::None;
        }
        if self.focus == Focus::Compactify {
            if c == 'c' || c == 'C' {
                return self.request_compactify();
            }
            return SendAction::None;
        }
        if self.input.is_empty() && (c == 'c' || c == 'C') {
            return self.request_compactify();
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        let i = self.cursor.min(chars.len());
        chars.insert(i, c);
        self.cursor = i + 1;
        self.input = chars.into_iter().collect();
        SendAction::None
    }

    pub fn backspace(&mut self) {
        if self.thinking != Thinking::None || self.focus != Focus::Input {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        let i = self.cursor - 1;
        if i < chars.len() {
            chars.remove(i);
            self.cursor = i;
            self.input = chars.into_iter().collect();
        }
    }

    pub fn clear_input(&mut self) {
        if self.thinking != Thinking::None || self.focus != Focus::Input {
            return;
        }
        self.input.clear();
        self.cursor = 0;
    }

    pub fn cursor_left(&mut self) {
        if self.focus == Focus::Input && self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn cursor_right(&mut self) {
        let len = self.input.chars().count();
        if self.focus == Focus::Input && self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn on_enter(&mut self) -> SendAction {
        if self.thinking != Thinking::None {
            return SendAction::None;
        }
        match self.focus {
            Focus::Compactify => self.request_compactify(),
            Focus::Input => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return SendAction::None;
                }
                self.input.clear();
                self.cursor = 0;
                self.status = None;
                self.turns.push(Turn {
                    role: Role::User,
                    text,
                });
                self.follow_bottom = true;
                SendAction::Reply
            }
        }
    }

    pub fn request_compactify(&mut self) -> SendAction {
        if self.thinking != Thinking::None {
            return SendAction::None;
        }
        if self.turns.is_empty() {
            self.status = Some("nothing to compactify".into());
            return SendAction::None;
        }
        self.status = None;
        SendAction::Compactify
    }

    pub fn apply_compactify(&mut self, summary: String) {
        let text = summary.trim().to_string();
        self.turns = vec![Turn {
            role: Role::User,
            text,
        }];
        self.follow_bottom = true;
        self.set_thinking(Thinking::None);
        self.status = Some("compactified".into());
    }

    pub fn push_assistant(&mut self, text: String) {
        self.turns.push(Turn {
            role: Role::Assistant,
            text,
        });
        self.set_thinking(Thinking::None);
        self.status = None;
    }

    pub fn set_error(&mut self, msg: String) {
        self.set_thinking(Thinking::None);
        self.status = Some(msg);
    }

    pub fn write_save(&self) -> io::Result<bool> {
        if !self.allow_save {
            return Ok(false);
        }
        std::fs::write(&self.save_path, transcript_markdown(&self.turns))?;
        Ok(true)
    }
}

pub fn saved_chat_filename(slug: &str) -> String {
    format!("saved_chat_{slug}.md")
}

pub fn estimate_tokens(system: &str, turns: &[Turn]) -> usize {
    let mut n = "system".chars().count() + system.chars().count();
    for t in turns {
        n += t.role.api_role().chars().count() + t.text.chars().count();
    }
    n / 4
}

pub fn percent(tokens: usize, window: usize) -> usize {
    if window == 0 {
        0
    } else {
        tokens.saturating_mul(100) / window
    }
}

pub fn spinner_glyph(elapsed: Duration) -> char {
    let i = (elapsed.as_millis() / SPIN_TICK.as_millis()) as usize;
    SPIN_FRAMES[i % SPIN_FRAMES.len()]
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// In-frame compactify wait: `[-] compactify 4s`.
pub fn compactify_wait_line(elapsed: Duration) -> String {
    format!(
        "[{}] compactify {}",
        spinner_glyph(elapsed),
        format_elapsed(elapsed)
    )
}

pub fn transcript_markdown(turns: &[Turn]) -> String {
    let mut s = String::from("# chat\n");
    for t in turns {
        s.push_str("\n## ");
        s.push_str(t.role.heading());
        s.push_str("\n\n");
        s.push_str(t.text.trim_end());
        s.push('\n');
    }
    s
}

pub fn transcript_lines(turns: &[Turn], width: usize) -> Vec<String> {
    let body_w = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    for t in turns {
        lines.push(t.role.label().to_string());
        if t.text.is_empty() {
            lines.push("  ".into());
            continue;
        }
        for raw in t.text.lines() {
            if raw.is_empty() {
                lines.push("  ".into());
                continue;
            }
            for chunk in wrap_lines(raw, body_w) {
                lines.push(format!("  {chunk}"));
            }
        }
    }
    lines
}

pub fn context_window_tokens() -> usize {
    std::env::var("ENGRAM_CONTEXT_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_CONTEXT_TOKENS)
}

fn stub_enabled() -> bool {
    match std::env::var("ENGRAM_CHAT_STUB") {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => false,
    }
}

fn datetimestamp_slug() -> String {
    if let Ok(out) = std::process::Command::new("date")
        .args(["-u", "+%Y%m%dT%H%M%SZ"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == 'T' || c == 'Z')
            {
                return s;
            }
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}Z")
}

// ── Interactive runner ──────────────────────────────────────────────────────

pub fn available() -> bool {
    io::stdout().is_terminal() && ui::color_enabled()
}

/// Open the chat TUI. Returns when the user presses Esc / Ctrl-C.
pub async fn run(cfg: &Config, cwd: PathBuf) {
    if !available() {
        println!("chat: interactive only");
        return;
    }

    let guard = RawGuard::enter();
    if !guard.active() {
        println!("chat: could not enter raw mode — try :chat from a real TTY");
        return;
    }

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049h\x1b[?25h");
        let _ = out.flush();
    }

    let allow_save = cfg.mode != Mode::Hallucination;
    let mut state = TuiState::new(cwd, allow_save);
    let stub = stub_enabled();
    let llm_cfg = if stub { None } else { Some(cfg.clone()) };
    let mut pending: Option<Pending> = None;

    loop {
        {
            let lay = layout();
            let total = state.display_lines(lay.inner).len();
            state.clamp_scroll(lay.transcript_h, total);
        }
        draw(&state);

        if let Some(job) = pending.as_mut() {
            if job.handle.is_finished() {
                let job = pending.take().unwrap();
                match job.handle.await {
                    Ok(Ok(text)) => match job.kind {
                        Thinking::Reply => {
                            state.push_assistant(text);
                            let _ = state.write_save();
                        }
                        Thinking::Compactify => {
                            if text.trim().is_empty() {
                                state.set_error("empty summary".into());
                            } else {
                                state.apply_compactify(text);
                                let _ = state.write_save();
                            }
                        }
                        Thinking::None => {}
                    },
                    Ok(Err(e)) => state.set_error(e),
                    Err(e) => state.set_error(format!("task: {e}")),
                }
                continue;
            }
        }

        let lay = layout();
        let total = state.display_lines(lay.inner).len();

        match poll_key(Duration::from_millis(50)) {
            Some(Key::CtrlC) => break,
            Some(Key::Esc) if pending.is_none() => break,
            Some(Key::Esc) => {
                if let Some(job) = pending.take() {
                    job.handle.abort();
                }
                state.set_thinking(Thinking::None);
                state.status = Some("cancelled".into());
            }
            Some(Key::Tab) if pending.is_none() => state.toggle_focus(),
            Some(Key::Enter) => match state.on_enter() {
                SendAction::Reply => {
                    state.set_thinking(Thinking::Reply);
                    pending = Some(spawn_job(
                        Thinking::Reply,
                        state.api_messages(),
                        REPLY_MAX_TOKENS,
                        stub,
                        llm_cfg.clone(),
                        STUB_REPLY,
                    ));
                }
                SendAction::Compactify => {
                    state.set_thinking(Thinking::Compactify);
                    pending = Some(spawn_job(
                        Thinking::Compactify,
                        state.compactify_messages(),
                        COMPACTIFY_MAX_TOKENS,
                        stub,
                        llm_cfg.clone(),
                        STUB_SUMMARY,
                    ));
                }
                SendAction::None => {}
            },
            Some(Key::Char(c)) => match state.insert_char(c) {
                SendAction::Compactify => {
                    state.set_thinking(Thinking::Compactify);
                    pending = Some(spawn_job(
                        Thinking::Compactify,
                        state.compactify_messages(),
                        COMPACTIFY_MAX_TOKENS,
                        stub,
                        llm_cfg.clone(),
                        STUB_SUMMARY,
                    ));
                }
                _ => {}
            },
            Some(Key::Up) => state.scroll_up(1, lay.transcript_h, total),
            Some(Key::Down) => state.scroll_down(1, lay.transcript_h, total),
            Some(Key::PageUp) => {
                state.scroll_up(lay.transcript_h.saturating_sub(1).max(1), lay.transcript_h, total)
            }
            Some(Key::PageDown) => {
                state.scroll_down(
                    lay.transcript_h.saturating_sub(1).max(1),
                    lay.transcript_h,
                    total,
                )
            }
            Some(Key::Left) => state.cursor_left(),
            Some(Key::Right) => state.cursor_right(),
            Some(Key::Backspace) => state.backspace(),
            Some(Key::CtrlU) => state.clear_input(),
            _ => {}
        }
    }

    if let Some(job) = pending.take() {
        job.handle.abort();
    }
    let saved = state.write_save();

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049l\x1b[?25h\x1b[0m");
        let _ = out.flush();
    }
    drop(guard);

    match saved {
        Ok(true) => println!("chat: saved {}", state.save_path.display()),
        Ok(false) => println!("chat: transcript not saved (hallucination mode)"),
        Err(e) => eprintln!("chat: could not save {}: {e}", state.save_path.display()),
    }
}

struct Pending {
    kind: Thinking,
    handle: tokio::task::JoinHandle<Result<String, String>>,
}

fn spawn_job(
    kind: Thinking,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    stub: bool,
    llm_cfg: Option<Config>,
    stub_text: &'static str,
) -> Pending {
    let handle = tokio::spawn(async move {
        if stub {
            return Ok(stub_text.to_string());
        }
        let cfg = llm_cfg.ok_or_else(|| "missing llm config".to_string())?;
        let llm = LlmClient::new(&cfg).map_err(|e| format!("{e:#}"))?;
        llm.chat_text_max(&messages, max_tokens)
            .await
            .map_err(|e| format!("{e:#}"))
    });
    Pending { kind, handle }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Tab,
    Backspace,
    CtrlC,
    CtrlU,
    Char(char),
    Other,
}

fn poll_key(timeout: Duration) -> Option<Key> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut stdin = io::stdin();
        let mut b = [0u8; 1];
        match stdin.read(&mut b) {
            Ok(1) => return Some(decode_key(b[0], &mut stdin)),
            Ok(0) | Err(_) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) => unreachable!(),
        }
    }
}

fn decode_key(first: u8, stdin: &mut impl Read) -> Key {
    match first {
        0x03 => Key::CtrlC,
        0x15 => Key::CtrlU,
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x08 | 0x7f => Key::Backspace,
        0x1b => {
            std::thread::sleep(Duration::from_millis(20));
            let mut rest = [0u8; 8];
            let n = stdin.read(&mut rest).unwrap_or(0);
            if n == 0 {
                return Key::Esc;
            }
            if rest[0] == b'[' && n >= 2 {
                return match rest[1] {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    b'5' if n >= 3 && rest[2] == b'~' => Key::PageUp,
                    b'6' if n >= 3 && rest[2] == b'~' => Key::PageDown,
                    _ => Key::Other,
                };
            }
            if rest[0] == 0x1b {
                return Key::Esc;
            }
            Key::Other
        }
        c if c >= 0x20 && c < 0x7f => Key::Char(c as char),
        _ => Key::Other,
    }
}

struct Layout {
    cols: usize,
    rows: usize,
    inner: usize,
    transcript_h: usize,
}

fn layout() -> Layout {
    let (rows, cols) = term::term_size();
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(12, term::MAX_ROWS);
    let inner = cols.saturating_sub(2);
    let transcript_h = rows.saturating_sub(4).max(4);
    Layout {
        cols,
        rows,
        inner,
        transcript_h,
    }
}

fn draw(state: &TuiState) {
    let lay = layout();

    let color = ui::color_enabled();
    let r = "\x1b[0m";
    let bold = if color { "\x1b[1m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };
    let inv = if color { "\x1b[7m" } else { "" };

    let mut buf = String::with_capacity(lay.rows * lay.cols * 2);
    use std::fmt::Write as _;
    let _ = write!(buf, "\x1b[2J\x1b[H");

    let title = state.title_text();
    let dash_n = lay
        .cols
        .saturating_sub(2)
        .saturating_sub(title.chars().count())
        .max(1);
    let _ = write!(
        buf,
        "\x1b[1;1H{cyan}┌{bold}{title}{r}{cyan}{}┐{r}",
        "─".repeat(dash_n)
    );

    let lines = state.display_lines(lay.inner);
    let start = state.scroll_top;
    for row in 0..lay.transcript_h {
        let y = row + 2;
        let text = lines.get(start + row).cloned().unwrap_or_default();
        let cell = style_transcript_line(&text, dim, r, lay.inner);
        let _ = write!(
            buf,
            "\x1b[{y};1H{cyan}│{r}{}{cyan}│{r}",
            pad_visible(&cell, lay.inner)
        );
    }

    let sep_y = lay.transcript_h + 2;
    let sep_label = if state.thinking == Thinking::Compactify {
        compactify_wait_line(state.thinking_elapsed())
    } else {
        state.status.clone().unwrap_or_default()
    };
    let sep_body = if sep_label.is_empty() {
        "─".repeat(lay.inner)
    } else {
        let label = format!(" {sep_label} ");
        let n = lay.inner.saturating_sub(label.chars().count());
        format!("{label}{}", "─".repeat(n))
    };
    let _ = write!(
        buf,
        "\x1b[{sep_y};1H{cyan}├{r}{dim}{}{r}{cyan}┤{r}",
        pad_visible(&sep_body, lay.inner)
    );

    let input_y = sep_y + 1;
    let prefix = "> ";
    let input_w = lay.inner.saturating_sub(prefix.len()).max(1);
    let (shown, cursor_col) = visible_input(&state.input, state.cursor, input_w);
    let input_cell = format!("{prefix}{shown}");
    let _ = write!(
        buf,
        "\x1b[{input_y};1H{cyan}│{r}{}{cyan}│{r}",
        pad_visible(&input_cell, lay.inner)
    );

    let foot_y = input_y + 1;
    let footer = footer_text(state, inv, r, dim);
    let _ = write!(
        buf,
        "\x1b[{foot_y};1H{cyan}└{r}{}{cyan}┘{r}",
        pad_visible(&footer, lay.inner)
    );

    if state.focus == Focus::Input && state.thinking == Thinking::None {
        let screen_col = 2 + prefix.len() + cursor_col;
        let _ = write!(buf, "\x1b[?25h\x1b[{input_y};{screen_col}H");
    } else {
        let _ = write!(buf, "\x1b[?25l");
    }

    let mut out = io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn style_transcript_line(text: &str, dim: &str, reset: &str, width: usize) -> String {
    if text == "you" || text == "assistant" {
        format!("{dim}{}{reset}", truncate_visible(text, width))
    } else {
        truncate_visible(text, width)
    }
}

fn footer_text(state: &TuiState, inv: &str, reset: &str, dim: &str) -> String {
    if state.thinking == Thinking::Compactify {
        return format!(
            "{dim} {} · Esc cancel {reset}",
            compactify_wait_line(state.thinking_elapsed())
        );
    }
    if state.thinking != Thinking::None {
        return format!("{dim} ··· Esc cancel {reset}");
    }
    let compact = if state.focus == Focus::Compactify {
        format!("{inv}[c] compactify{reset}")
    } else {
        "[c] compactify".to_string()
    };
    format!(" Enter send · Tab {compact} · ↑↓ scroll · Esc exit ")
}

fn visible_input(input: &str, cursor: usize, width: usize) -> (String, usize) {
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    if chars.len() <= width {
        return (input.to_string(), cursor);
    }
    let mut start = cursor.saturating_sub(width.saturating_sub(1));
    if start + width > chars.len() {
        start = chars.len().saturating_sub(width);
    }
    let shown: String = chars[start..start + width].iter().collect();
    (shown, cursor.saturating_sub(start))
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn pad_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap());
                for c2 in chars.by_ref() {
                    out.push(c2);
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if n >= width {
            break;
        }
        out.push(c);
        n += 1;
    }
    while n < width {
        out.push(' ');
        n += 1;
    }
    out
}

fn truncate_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in s.chars() {
        if n >= width {
            break;
        }
        out.push(c);
        n += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_state() -> TuiState {
        TuiState {
            system: "sys".into(),
            turns: Vec::new(),
            input: String::new(),
            cursor: 0,
            focus: Focus::Input,
            scroll_top: 0,
            follow_bottom: true,
            thinking: Thinking::None,
            thinking_since: None,
            status: None,
            window_tokens: 100,
            save_path: PathBuf::from("/tmp/saved_chat_test.md"),
            allow_save: true,
        }
    }

    #[test]
    fn token_estimate_and_percent() {
        // "system" (6) + "sys" (3) = 9 / 4 = 2
        assert_eq!(estimate_tokens("sys", &[]), 2);
        assert_eq!(percent(0, 32_768), 0);
        assert_eq!(percent(16_384, 32_768), 50);
        assert_eq!(percent(32_768, 32_768), 100);
        assert_eq!(percent(1, 100), 1);

        let mut s = test_state();
        s.system = "sys".into();
        s.window_tokens = 100;
        assert_eq!(s.tokens(), 2);
        assert_eq!(s.percent(), 2);
        s.turns.push(Turn {
            role: Role::User,
            text: "x".repeat(400),
        });
        // system+sys+user+400 = 6+3+4+400 = 413 / 4 = 103
        assert_eq!(s.tokens(), 103);
        assert_eq!(s.percent(), 103);
        assert!(s.title_text().contains("ctx 103 tok"));
        assert!(s.title_text().contains("est 103%"));
    }

    #[test]
    fn context_window_env_override() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_CONTEXT_TOKENS", "4096");
        assert_eq!(context_window_tokens(), 4096);
        std::env::set_var("ENGRAM_CONTEXT_TOKENS", "0");
        assert_eq!(context_window_tokens(), DEFAULT_CONTEXT_TOKENS);
        std::env::remove_var("ENGRAM_CONTEXT_TOKENS");
        assert_eq!(context_window_tokens(), DEFAULT_CONTEXT_TOKENS);
    }

    #[test]
    fn scroll_clamps_and_follows_bottom() {
        let mut s = test_state();
        s.follow_bottom = true;
        s.clamp_scroll(5, 20);
        assert_eq!(s.scroll_top, 15);
        assert!(s.follow_bottom);

        s.scroll_up(3, 5, 20);
        assert!(!s.follow_bottom);
        assert_eq!(s.scroll_top, 12);

        s.scroll_up(100, 5, 20);
        assert_eq!(s.scroll_top, 0);

        s.scroll_down(2, 5, 20);
        assert!(!s.follow_bottom);
        assert_eq!(s.scroll_top, 2);

        s.scroll_down(100, 5, 20);
        assert!(s.follow_bottom);
        assert_eq!(s.scroll_top, 15);

        s.follow_bottom = false;
        s.scroll_top = 12;
        s.clamp_scroll(5, 8);
        assert_eq!(s.scroll_top, 3);
    }

    #[test]
    fn markdown_shape_and_filename() {
        assert_eq!(
            saved_chat_filename("20261025T060000Z"),
            "saved_chat_20261025T060000Z.md"
        );
        let turns = vec![
            Turn {
                role: Role::User,
                text: "hello".into(),
            },
            Turn {
                role: Role::Assistant,
                text: "hi".into(),
            },
        ];
        let md = transcript_markdown(&turns);
        assert!(md.starts_with("# chat\n"));
        assert!(md.contains("## user"));
        assert!(md.contains("hello"));
        assert!(md.contains("## assistant"));
        assert!(md.contains("hi"));
        assert!(!md.to_lowercase().contains("## system"));
    }

    #[test]
    fn compacted_summary_is_user_section() {
        let mut s = test_state();
        s.turns.push(Turn {
            role: Role::User,
            text: "hello".into(),
        });
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "hi".into(),
        });
        s.apply_compactify("kept the names".into());
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].role, Role::User);
        let md = transcript_markdown(&s.turns);
        assert!(md.contains("## user"));
        assert!(md.contains("kept the names"));
        assert!(!md.contains("## assistant"));
    }

    #[test]
    fn enter_sends_nonempty_input() {
        let mut s = test_state();
        assert_eq!(s.on_enter(), SendAction::None);
        for c in "hello".chars() {
            assert_eq!(s.insert_char(c), SendAction::None);
        }
        assert_eq!(s.input, "hello");
        assert_eq!(s.cursor, 5);
        assert_eq!(s.on_enter(), SendAction::Reply);
        assert!(s.input.is_empty());
        assert_eq!(s.cursor, 0);
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].role, Role::User);
        assert_eq!(s.turns[0].text, "hello");
        assert!(s.follow_bottom);
    }

    #[test]
    fn typing_c_in_input_is_a_letter() {
        let mut s = test_state();
        assert_eq!(s.insert_char('a'), SendAction::None);
        assert_eq!(s.insert_char('c'), SendAction::None);
        assert_eq!(s.input, "ac");
        s.toggle_focus();
        assert_eq!(s.focus, Focus::Compactify);
        s.turns.push(Turn {
            role: Role::User,
            text: "prior".into(),
        });
        assert_eq!(s.insert_char('c'), SendAction::Compactify);
    }

    #[test]
    fn empty_c_compactifies() {
        let mut s = test_state();
        s.turns.push(Turn {
            role: Role::User,
            text: "prior".into(),
        });
        assert_eq!(s.insert_char('c'), SendAction::Compactify);
        assert!(s.input.is_empty());
    }

    #[test]
    fn compactify_lowers_token_count() {
        let mut s = test_state();
        s.window_tokens = 10_000;
        s.turns.push(Turn {
            role: Role::User,
            text: "a".repeat(400),
        });
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "b".repeat(400),
        });
        let before = s.tokens();
        assert!(before > 50);
        s.apply_compactify("short summary".into());
        let after = s.tokens();
        assert!(
            after < before,
            "expected compactify to shrink context: {before} -> {after}"
        );
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].role, Role::User);
        assert_eq!(s.status.as_deref(), Some("compactified"));
    }

    #[test]
    fn compactify_spinner_cycles_and_shows_elapsed() {
        assert_eq!(spinner_glyph(Duration::from_millis(0)), '-');
        assert_eq!(spinner_glyph(Duration::from_millis(120)), '\\');
        assert_eq!(spinner_glyph(Duration::from_millis(240)), '|');
        assert_eq!(spinner_glyph(Duration::from_millis(360)), '/');
        assert_eq!(spinner_glyph(Duration::from_millis(480)), '-');
        let line = compactify_wait_line(Duration::from_secs(4));
        assert!(line.contains("compactify"), "{line}");
        assert!(line.contains("4s"), "{line}");
        assert!(line.starts_with('['), "{line}");
        let long = compactify_wait_line(Duration::from_secs(75));
        assert!(long.contains("1m15s"), "{long}");

        let mut s = test_state();
        s.turns.push(Turn {
            role: Role::User,
            text: "prior".into(),
        });
        s.set_thinking(Thinking::Compactify);
        let shown = s.display_lines(40);
        assert!(
            shown.iter().any(|l| l.contains("compactify")),
            "expected compactify spinner in transcript, got {shown:?}"
        );
        s.set_thinking(Thinking::None);
        let after = s.display_lines(40);
        assert!(!after.iter().any(|l| l.contains("compactify")));
    }

    #[test]
    fn compactify_empty_transcript_does_not_call() {
        let mut s = test_state();
        assert_eq!(s.request_compactify(), SendAction::None);
        assert_eq!(s.status.as_deref(), Some("nothing to compactify"));
        assert!(s.turns.is_empty());
    }

    #[test]
    fn save_writes_markdown_and_skips_when_disallowed() {
        let dir = std::env::temp_dir().join(format!(
            "chat_tui_save_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("saved_chat_20261025T060000Z.md");
        let mut s = test_state();
        s.save_path = path.clone();
        s.allow_save = true;
        s.turns.push(Turn {
            role: Role::User,
            text: "hello".into(),
        });
        assert!(s.write_save().unwrap());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("## user"));
        assert!(body.contains("hello"));
        s.allow_save = false;
        assert!(!s.write_save().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcript_wraps_and_labels() {
        let turns = vec![Turn {
            role: Role::User,
            text: "abcdefghij".into(),
        }];
        let lines = transcript_lines(&turns, 6); // body width 4
        assert_eq!(lines[0], "you");
        assert_eq!(lines[1], "  abcd");
        assert_eq!(lines[2], "  efgh");
        assert_eq!(lines[3], "  ij");
    }
}
