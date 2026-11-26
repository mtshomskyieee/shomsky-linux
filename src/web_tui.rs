//! Split text browser (`:web`).
//!
//! Left: page snapshot. Right: conversation. Bottom: url and chat inputs.
//! Same ANSI + BusyBox `stty` approach as `:chat` — no TUI crates.

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use shomagent::daemon::rpc;
use shomagent::protocol::{ContextUsage, Request, Response};

/// Same glyphs as `:chat` / `spinner.rs` so Alpine TTYs stay ASCII-only.
const SPIN_FRAMES: &[char] = &['-', '\\', '|', '/'];
const SPIN_TICK: Duration = Duration::from_millis(120);
/// Gap between live re-reads after the first paint. Script-heavy pages fill in
/// after `readyState` is already `complete`.
const SETTLE_DELAY: Duration = Duration::from_secs(2);
/// Two identical reads in a row means the visible text has stopped changing.
const SETTLE_STABLE_NEED: u8 = 2;
/// Stop even if a ticker keeps the text changing.
const SETTLE_MAX_TICKS: u8 = 6;

use crate::term::{self, RawGuard};
use crate::ui;
use crate::web_history::{self, title_from_snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Url,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub user: bool,
    pub text: String,
}

/// In-flight work. The frame redraws on a short poll so the glyph cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Page,
    Chat,
}

#[derive(Debug, Clone)]
pub struct TuiState {
    pub member_id: Option<u32>,
    pub page: String,
    pub page_scroll: usize,
    pub turns: Vec<Turn>,
    pub chat_scroll: usize,
    pub follow_chat: bool,
    pub url: String,
    pub url_cursor: usize,
    pub chat: String,
    pub chat_cursor: usize,
    pub focus: Focus,
    pub status: Option<String>,
    pub history_path: PathBuf,
    pub busy: Option<Busy>,
    pub busy_since: Option<Instant>,
    /// Last talk-prompt meter from the daemon (`ctx used/buffer tok · est %`).
    pub context: Option<ContextUsage>,
    /// Bumped on a real navigation so a late script-settle cannot overwrite a newer page.
    pub nav_gen: u64,
    /// Set while post-load re-reads are still watching for script output.
    pub scripts_since: Option<Instant>,
}

impl TuiState {
    pub fn new(history_path: PathBuf, initial_url: Option<String>) -> Self {
        let url = initial_url.unwrap_or_default();
        let url_cursor = url.chars().count();
        Self {
            member_id: None,
            page: String::new(),
            page_scroll: 0,
            turns: Vec::new(),
            chat_scroll: 0,
            follow_chat: true,
            url,
            url_cursor,
            chat: String::new(),
            chat_cursor: 0,
            focus: Focus::Url,
            status: None,
            history_path,
            busy: None,
            busy_since: None,
            context: None,
            nav_gen: 0,
            scripts_since: None,
        }
    }

    pub fn context_label(&self) -> String {
        match &self.context {
            Some(usage) => usage.summary(),
            None => "ctx —".into(),
        }
    }

    pub fn set_busy(&mut self, kind: Busy) {
        self.busy = Some(kind);
        self.busy_since = Some(Instant::now());
        self.status = None;
    }

    pub fn clear_busy(&mut self) {
        self.busy = None;
        self.busy_since = None;
    }

    /// `[-] loading 4s` / `[-] thinking 4s` / `[-] scripts 4s`, or nothing when idle.
    pub fn wait_line(&self) -> Option<String> {
        let (label, elapsed) = if let Some(kind) = self.busy {
            let elapsed = self.busy_since.map(|t| t.elapsed()).unwrap_or_default();
            let label = match kind {
                Busy::Page => "loading",
                Busy::Chat => "thinking",
            };
            (label, elapsed)
        } else if let Some(since) = self.scripts_since {
            ("scripts", since.elapsed())
        } else {
            return None;
        };
        Some(format!(
            "[{}] {label} {}",
            spinner_glyph(elapsed),
            format_elapsed(elapsed)
        ))
    }

    fn view_h_for(&self, view_h: usize, pane: Focus) -> usize {
        let banner = matches!(
            (self.busy, pane),
            (Some(Busy::Page), Focus::Url) | (Some(Busy::Chat), Focus::Chat)
        );
        if banner {
            view_h.saturating_sub(1).max(1)
        } else {
            view_h
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Url => Focus::Chat,
            Focus::Chat => Focus::Url,
        };
    }

    pub fn page_lines(&self, width: usize) -> Vec<String> {
        if self.page.trim().is_empty() {
            return vec!["(enter a url below)".into()];
        }
        wrap_lines(&self.page, width.max(1))
    }

    pub fn chat_lines(&self, width: usize) -> Vec<String> {
        if self.turns.is_empty() {
            return wrap_lines(
                "Type click <n> to follow a numbered link\n\
                 from Links and buttons on the left,\n\
                 or ask a question about this page.\n\
                 \n\
                 Also: type <label> -- <text> · clear / ^L",
                width,
            );
        }
        let mut lines = Vec::new();
        for t in &self.turns {
            let who = if t.user { "you" } else { "page" };
            lines.push(format!("{who}"));
            lines.extend(wrap_lines(&t.text, width.max(1)));
            lines.push(String::new());
        }
        lines
    }

    pub fn scroll_focused(&mut self, delta: isize, view_h: usize, page_w: usize, chat_w: usize) {
        match self.focus {
            Focus::Url => {
                let total = self.page_lines(page_w).len();
                let h = self.view_h_for(view_h, Focus::Url);
                scroll_clamp(&mut self.page_scroll, delta, total, h);
            }
            Focus::Chat => {
                self.follow_chat = false;
                let total = self.chat_lines(chat_w).len();
                let h = self.view_h_for(view_h, Focus::Chat);
                scroll_clamp(&mut self.chat_scroll, delta, total, h);
            }
        }
    }

    pub fn clamp_scrolls(&mut self, view_h: usize, page_w: usize, chat_w: usize) {
        let page_n = self.page_lines(page_w).len();
        let chat_n = self.chat_lines(chat_w).len();
        let page_h = self.view_h_for(view_h, Focus::Url);
        let chat_h = self.view_h_for(view_h, Focus::Chat);
        let page_max = page_n.saturating_sub(page_h);
        let chat_max = chat_n.saturating_sub(chat_h);
        self.page_scroll = self.page_scroll.min(page_max);
        if self.follow_chat {
            self.chat_scroll = chat_max;
        } else {
            self.chat_scroll = self.chat_scroll.min(chat_max);
        }
    }

    /// Apply a successful page load. Resets page scroll, chat turns, and history.
    pub fn apply_page(&mut self, id: u32, snapshot: &str, usage: Option<ContextUsage>) {
        self.member_id = Some(id);
        self.page = snapshot.to_string();
        self.page_scroll = 0;
        // New page → drop the right-pane transcript (daemon also clears on navigate/click).
        self.turns.clear();
        self.chat_scroll = 0;
        self.follow_chat = true;
        if let Some(url) = url_from_snapshot(snapshot) {
            self.url = url;
            self.url_cursor = self.url.chars().count();
        }
        let title = title_from_snapshot(snapshot);
        let _ = web_history::record(&self.history_path, &self.url, &title);
        if let Some(usage) = usage {
            self.context = Some(usage);
        }
        self.clear_busy();
        self.scripts_since = None;
        self.nav_gen = self.nav_gen.wrapping_add(1);
        self.status = None;
    }

    /// Drop the chat transcript locally (pair with ClearTalk on the daemon).
    pub fn clear_chat(&mut self, usage: Option<ContextUsage>) {
        self.turns.clear();
        self.chat_scroll = 0;
        self.follow_chat = true;
        if let Some(usage) = usage {
            self.context = Some(usage);
        }
        self.clear_busy();
        self.status = Some("chat context cleared".into());
    }

    /// Re-read the live DOM. Keeps the scroll position.
    pub fn request_refresh(&mut self) -> Result<u32, &'static str> {
        let Some(id) = self.member_id else {
            self.status = Some("load a page first".into());
            return Err("no page");
        };
        if self.page.trim().is_empty() {
            self.status = Some("load a page first".into());
            return Err("no page");
        }
        self.set_busy(Busy::Page);
        Ok(id)
    }

    /// Apply a re-read of the current page without jumping back to the top.
    pub fn replace_page(&mut self, snapshot: &str, usage: Option<ContextUsage>) {
        let scroll = self.page_scroll;
        self.page = snapshot.to_string();
        if let Some(url) = url_from_snapshot(snapshot) {
            self.url = url;
            self.url_cursor = self.url.chars().count();
        }
        let title = title_from_snapshot(snapshot);
        let _ = web_history::record(&self.history_path, &self.url, &title);
        if let Some(usage) = usage {
            self.context = Some(usage);
        }
        self.page_scroll = scroll;
        self.clear_busy();
        self.status = None;
    }

    /// Chat Enter. Supports questions plus `click` / `type` / `clear`.
    pub fn submit_chat(&mut self) -> Result<ChatAction, &'static str> {
        if self.page.trim().is_empty() || self.member_id.is_none() {
            self.status = Some("load a page first".into());
            return Err("load a page first");
        }
        let q = self.chat.trim().to_string();
        if q.is_empty() {
            return Err("empty");
        }
        self.chat.clear();
        self.chat_cursor = 0;
        self.follow_chat = true;

        let lower = q.to_ascii_lowercase();
        if lower == "clear" || lower == "clear context" || lower == "clear chat" {
            Ok(ChatAction::Clear)
        } else if lower == "click" || lower.starts_with("click ") {
            let text = q.get(5..).unwrap_or("").trim().to_string();
            if text.is_empty() {
                self.status = Some("usage: click <N|#N|visible text>".into());
                return Err("click usage");
            }
            self.set_busy(Busy::Page);
            Ok(ChatAction::Click(text))
        } else if lower.starts_with("type ") {
            let rest = q[5..].trim();
            let (label, text) = parse_type_args(rest).ok_or_else(|| {
                self.status = Some("usage: type <label> -- <text>".into());
                "type usage"
            })?;
            self.set_busy(Busy::Page);
            Ok(ChatAction::Type { label, text })
        } else {
            self.turns.push(Turn {
                user: true,
                text: q.clone(),
            });
            self.set_busy(Busy::Chat);
            Ok(ChatAction::Talk(q))
        }
    }

    pub fn push_reply(&mut self, text: String, usage: Option<ContextUsage>) {
        self.turns.push(Turn { user: false, text });
        if let Some(usage) = usage {
            self.context = Some(usage);
        }
        self.follow_chat = true;
        self.clear_busy();
        self.status = None;
    }

    pub fn set_error(&mut self, msg: String) {
        if self.busy == Some(Busy::Chat) && msg != "cancelled" {
            self.turns.push(Turn {
                user: false,
                text: msg.clone(),
            });
            self.follow_chat = true;
        }
        self.clear_busy();
        self.status = Some(msg);
    }

    pub fn submit_url(&mut self) -> Result<String, &'static str> {
        match normalize_web_url(&self.url) {
            Ok(url) => {
                self.url = url.clone();
                self.url_cursor = self.url.chars().count();
                self.set_busy(Busy::Page);
                Ok(url)
            }
            Err("empty") => {
                self.status = Some("url is empty".into());
                Err("empty")
            }
            Err(_) => {
                self.status = Some("only http and https".into());
                Err("scheme")
            }
        }
    }

    pub fn insert_char(&mut self, c: char) {
        match self.focus {
            Focus::Url => insert_at(&mut self.url, &mut self.url_cursor, c),
            Focus::Chat => insert_at(&mut self.chat, &mut self.chat_cursor, c),
        }
    }

    pub fn backspace(&mut self) {
        match self.focus {
            Focus::Url => backspace_at(&mut self.url, &mut self.url_cursor),
            Focus::Chat => backspace_at(&mut self.chat, &mut self.chat_cursor),
        }
    }

    pub fn cursor_left(&mut self) {
        match self.focus {
            Focus::Url => self.url_cursor = self.url_cursor.saturating_sub(1),
            Focus::Chat => self.chat_cursor = self.chat_cursor.saturating_sub(1),
        }
    }

    pub fn cursor_right(&mut self) {
        match self.focus {
            Focus::Url => {
                let n = self.url.chars().count();
                if self.url_cursor < n {
                    self.url_cursor += 1;
                }
            }
            Focus::Chat => {
                let n = self.chat.chars().count();
                if self.chat_cursor < n {
                    self.chat_cursor += 1;
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    Talk(String),
    Click(String),
    Type { label: String, text: String },
    Clear,
}

fn parse_type_args(rest: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let idx = parts.iter().position(|p| *p == "--")?;
    let label = parts[..idx].join(" ");
    let text = parts[idx + 1..].join(" ");
    if label.is_empty() || text.is_empty() {
        return None;
    }
    Some((label, text))
}

fn url_from_snapshot(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("URL:") {
            let u = rest.trim();
            if !u.is_empty() {
                return Some(u.to_string());
            }
        }
    }
    None
}

fn scroll_clamp(scroll: &mut usize, delta: isize, total: usize, view_h: usize) {
    let max = total.saturating_sub(view_h.max(1));
    if delta < 0 {
        *scroll = scroll.saturating_sub((-delta) as usize);
    } else {
        *scroll = scroll.saturating_add(delta as usize);
    }
    *scroll = (*scroll).min(max);
}

fn insert_at(buf: &mut String, cursor: &mut usize, c: char) {
    let mut chars: Vec<char> = buf.chars().collect();
    let at = (*cursor).min(chars.len());
    chars.insert(at, c);
    *buf = chars.into_iter().collect();
    *cursor = at + 1;
}

fn backspace_at(buf: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let mut chars: Vec<char> = buf.chars().collect();
    let at = (*cursor).min(chars.len());
    if at == 0 {
        return;
    }
    chars.remove(at - 1);
    *buf = chars.into_iter().collect();
    *cursor = at - 1;
}

/// What to do with one post-load re-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettleStep {
    pub apply: bool,
    pub stable: u8,
    pub ticks: u8,
    pub keep_going: bool,
}

/// `changed` is true when this read differs from the pane. Two identical reads
/// stop the watch; a changing page keeps it going until [`SETTLE_MAX_TICKS`].
pub fn next_settle(changed: bool, stable: u8, ticks: u8) -> SettleStep {
    let stable = if changed { 0 } else { stable.saturating_add(1) };
    let ticks = ticks.saturating_add(1);
    let keep_going = stable < SETTLE_STABLE_NEED && ticks < SETTLE_MAX_TICKS;
    SettleStep {
        apply: changed,
        stable,
        ticks,
        keep_going,
    }
}

fn spinner_glyph(elapsed: Duration) -> char {
    let tick = SPIN_TICK.as_millis().max(1);
    let i = (elapsed.as_millis() / tick) as usize;
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

/// Word-wrap `text` into lines of exactly `width` columns.
///
/// The panes scroll up and down only, so anything that does not fit continues
/// on the next line. Tabs and other whitespace are word breaks (table cells
/// from the page snapshot must not become terminal tab stops). A single word
/// longer than `width` is clipped onto successive lines. Every returned line
/// is padded with spaces, so drawing it cannot move the cursor past the pane.
fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.split_whitespace().next().is_none() {
            out.push(" ".repeat(width));
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            push_word(&mut out, &mut current, word, width);
        }
        if !current.is_empty() {
            out.push(pad_visible(&current, width));
        }
    }
    if out.is_empty() {
        out.push(" ".repeat(width));
    }
    out
}

/// Append `word` to `current`, wrapping onto a new line when it does not fit.
fn push_word(out: &mut Vec<String>, current: &mut String, word: &str, width: usize) {
    let mut rest: Vec<char> = word.chars().collect();
    while !rest.is_empty() {
        let cur_len = current.chars().count();
        if current.is_empty() {
            if rest.len() <= width {
                current.extend(rest.iter().copied());
                return;
            }
            let head: String = rest.drain(..width).collect();
            out.push(head);
            continue;
        }
        if cur_len + 1 + rest.len() <= width {
            current.push(' ');
            current.extend(rest.iter().copied());
            return;
        }
        out.push(pad_visible(current, width));
        current.clear();
    }
}

/// A `:web` URL must be http(s). A bare host (no scheme typed) is sent as `https://…`.
fn normalize_web_url(raw: &str) -> Result<String, &'static str> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("empty");
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Ok(url.to_string());
    }
    if has_non_http_scheme(url) {
        return Err("scheme");
    }
    if let Some(rest) = url.strip_prefix("//") {
        return Ok(format!("https://{rest}"));
    }
    Ok(format!("https://{url}"))
}

/// `ftp://…` and `javascript:…` are explicit schemes. `host:8080` is a port, not a scheme.
fn has_non_http_scheme(url: &str) -> bool {
    let Some(idx) = url.find(':') else {
        return false;
    };
    let scheme = &url[..idx];
    let Some(first) = scheme.chars().next() else {
        return false;
    };
    if !first.is_ascii_alphabetic()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    let rest = &url[idx + 1..];
    if rest.starts_with("//") {
        return true;
    }
    let port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let is_port = !port.is_empty() && port.chars().all(|c| c.is_ascii_digit());
    !is_port
}

/// Field text, URL to open, and an error to show when the typed scheme is not http(s).
fn prepare_initial_url(
    initial: Option<String>,
) -> (Option<String>, Option<String>, Option<&'static str>) {
    let Some(raw) = initial else {
        return (None, None, None);
    };
    match normalize_web_url(&raw) {
        Ok(url) => (Some(url.clone()), Some(url), None),
        Err("empty") => (None, None, Some("url is empty")),
        Err(_) => (Some(raw), None, Some("only http and https")),
    }
}

pub async fn run(initial_url: Option<String>) {
    if !io::stdout().is_terminal() {
        println!("web: interactive only");
        return;
    }
    let guard = RawGuard::enter();
    if !guard.active() {
        println!("web: could not enter raw mode — try :web from a real TTY");
        return;
    }
    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049h\x1b[?25h");
        let _ = out.flush();
    }

    let (display_url, open_url, url_status) = prepare_initial_url(initial_url);
    let mut state = TuiState::new(web_history::history_path(), display_url);
    if let Some(msg) = url_status {
        state.status = Some(msg.into());
    }
    let mut pending: Option<tokio::task::JoinHandle<JobResult>> = None;
    let mut settle: Option<tokio::task::JoinHandle<JobResult>> = None;
    let mut settle_stable: u8 = 0;
    let mut settle_ticks: u8 = 0;
    if let Some(url) = open_url {
        state.set_busy(Busy::Page);
        pending = Some(spawn_open(url));
    }

    loop {
        {
            let (body_h, page_w, chat_w) = pane_sizes();
            state.clamp_scrolls(body_h, page_w, chat_w);
        }
        draw(&state);

        if let Some(job) = pending.as_mut() {
            if job.is_finished() {
                let job = pending.take().unwrap();
                match job.await {
                    Ok(JobResult::Page {
                        id,
                        snapshot,
                        usage,
                    }) => {
                        state.apply_page(id, &snapshot, usage);
                        arm_settle(
                            &mut state,
                            &mut settle,
                            &mut settle_stable,
                            &mut settle_ticks,
                        );
                    }
                    Ok(JobResult::Refreshed { snapshot, usage }) => {
                        state.replace_page(&snapshot, usage);
                        arm_settle(
                            &mut state,
                            &mut settle,
                            &mut settle_stable,
                            &mut settle_ticks,
                        );
                    }
                    Ok(JobResult::Settled { .. }) => {}
                    Ok(JobResult::Reply { text, usage }) => state.push_reply(text, usage),
                    Ok(JobResult::Cleared { usage }) => state.clear_chat(usage),
                    Ok(JobResult::Err(e)) => state.set_error(e),
                    Err(e) => state.set_error(format!("task: {e}")),
                }
                continue;
            }
        }

        if pending.is_none() {
            if let Some(job) = settle.as_mut() {
                if job.is_finished() {
                    let job = settle.take().unwrap();
                    match job.await {
                        Ok(JobResult::Settled {
                            gen,
                            snapshot,
                            usage,
                        }) if gen == state.nav_gen => {
                            let step =
                                next_settle(snapshot != state.page, settle_stable, settle_ticks);
                            settle_stable = step.stable;
                            settle_ticks = step.ticks;
                            if step.apply {
                                state.replace_page(&snapshot, usage);
                            }
                            if step.keep_going {
                                if let Some(id) = state.member_id {
                                    settle = Some(spawn_settle_tick(id, state.nav_gen));
                                }
                            } else {
                                state.scripts_since = None;
                            }
                        }
                        Ok(JobResult::Err(e)) => {
                            state.scripts_since = None;
                            state.status = Some(e);
                        }
                        _ => state.scripts_since = None,
                    }
                    continue;
                }
            }
        }

        let (body_h, page_w, chat_w) = pane_sizes();
        match poll_key(Duration::from_millis(50)) {
            Some(Key::CtrlC) => break,
            Some(Key::Esc) if pending.is_none() => break,
            Some(Key::Esc) => {
                if let Some(job) = pending.take() {
                    job.abort();
                }
                stop_settle(&mut state, &mut settle);
                state.set_error("cancelled".into());
            }
            Some(Key::CtrlR) if pending.is_none() => {
                stop_settle(&mut state, &mut settle);
                if let Ok(id) = state.request_refresh() {
                    pending = Some(spawn_refresh(id));
                }
            }
            Some(Key::CtrlL) if pending.is_none() => {
                if let Some(id) = state.member_id {
                    stop_settle(&mut state, &mut settle);
                    pending = Some(spawn_clear_talk(id));
                } else {
                    state.clear_chat(None);
                }
            }
            Some(Key::Tab) if pending.is_none() => state.toggle_focus(),
            Some(Key::Enter) if pending.is_none() => match state.focus {
                Focus::Url => {
                    if let Ok(url) = state.submit_url() {
                        stop_settle(&mut state, &mut settle);
                        pending = Some(match state.member_id {
                            Some(id) => spawn_navigate(id, url),
                            None => spawn_open(url),
                        });
                    }
                }
                Focus::Chat => {
                    if let Ok(action) = state.submit_chat() {
                        stop_settle(&mut state, &mut settle);
                        if let Some(id) = state.member_id {
                            pending = Some(match action {
                                ChatAction::Talk(q) => spawn_talk(id, q),
                                ChatAction::Click(text) => spawn_click(id, text),
                                ChatAction::Type { label, text } => spawn_type(id, label, text),
                                ChatAction::Clear => spawn_clear_talk(id),
                            });
                        }
                    }
                }
            },
            Some(Key::Char(c)) if pending.is_none() => state.insert_char(c),
            Some(Key::Backspace) if pending.is_none() => state.backspace(),
            Some(Key::Left) if pending.is_none() => state.cursor_left(),
            Some(Key::Right) if pending.is_none() => state.cursor_right(),
            Some(Key::Up) => state.scroll_focused(-1, body_h, page_w, chat_w),
            Some(Key::Down) => state.scroll_focused(1, body_h, page_w, chat_w),
            Some(Key::PageUp) => {
                state.scroll_focused(-(body_h as isize).max(1), body_h, page_w, chat_w)
            }
            Some(Key::PageDown) => state.scroll_focused(body_h as isize, body_h, page_w, chat_w),
            _ => {}
        }
    }

    if let Some(job) = pending.take() {
        job.abort();
    }
    if let Some(job) = settle.take() {
        job.abort();
    }
    if let Some(id) = state.member_id {
        let _ = rpc(&Request::Stop { id }).await;
    }

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049l\x1b[?25h\x1b[0m");
        let _ = out.flush();
    }
    drop(guard);
}

enum JobResult {
    Page {
        id: u32,
        snapshot: String,
        usage: Option<ContextUsage>,
    },
    /// Live DOM re-read. Does not reset page scroll.
    Refreshed {
        snapshot: String,
        usage: Option<ContextUsage>,
    },
    /// One post-load re-read. Applied only when `gen` still matches.
    Settled {
        gen: u64,
        snapshot: String,
        usage: Option<ContextUsage>,
    },
    Reply {
        text: String,
        usage: Option<ContextUsage>,
    },
    Cleared {
        usage: Option<ContextUsage>,
    },
    Err(String),
}

fn spawn_open(url: String) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Web {
            url,
            tell_me: String::new(),
            name: None,
        })
        .await
        {
            Ok(Response::Ok {
                member: Some(m), ..
            }) => match rpc(&Request::Context { id: m.id }).await {
                Ok(Response::Ok {
                    context: Some(ctx),
                    context_usage,
                    ..
                }) => JobResult::Page {
                    id: m.id,
                    snapshot: ctx.ring_excerpt,
                    usage: context_usage,
                },
                Ok(Response::Err { message }) => JobResult::Err(message),
                Err(e) => JobResult::Err(e.to_string()),
                _ => JobResult::Page {
                    id: m.id,
                    snapshot: m.command,
                    usage: None,
                },
            },
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("web attach returned no member".into()),
        }
    })
}

fn spawn_navigate(id: u32, url: String) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Navigate { id, url }).await {
            Ok(Response::Ok {
                reply: Some(snapshot),
                ..
            }) => {
                let usage = match rpc(&Request::Context { id }).await {
                    Ok(Response::Ok { context_usage, .. }) => context_usage,
                    _ => None,
                };
                JobResult::Page {
                    id,
                    snapshot,
                    usage,
                }
            }
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("navigate returned no page".into()),
        }
    })
}

fn spawn_refresh(id: u32) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Refresh { id }).await {
            Ok(Response::Ok {
                reply: Some(snapshot),
                ..
            }) => {
                let usage = match rpc(&Request::Context { id }).await {
                    Ok(Response::Ok { context_usage, .. }) => context_usage,
                    _ => None,
                };
                JobResult::Refreshed { snapshot, usage }
            }
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("refresh returned no page".into()),
        }
    })
}

fn spawn_settle_tick(id: u32, gen: u64) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        tokio::time::sleep(SETTLE_DELAY).await;
        match rpc(&Request::Refresh { id }).await {
            Ok(Response::Ok {
                reply: Some(snapshot),
                ..
            }) => {
                let usage = match rpc(&Request::Context { id }).await {
                    Ok(Response::Ok { context_usage, .. }) => context_usage,
                    _ => None,
                };
                JobResult::Settled {
                    gen,
                    snapshot,
                    usage,
                }
            }
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("refresh returned no page".into()),
        }
    })
}

fn arm_settle(
    state: &mut TuiState,
    settle: &mut Option<tokio::task::JoinHandle<JobResult>>,
    stable: &mut u8,
    ticks: &mut u8,
) {
    if let Some(job) = settle.take() {
        job.abort();
    }
    let Some(id) = state.member_id else {
        state.scripts_since = None;
        return;
    };
    *stable = 0;
    *ticks = 0;
    state.scripts_since = Some(Instant::now());
    *settle = Some(spawn_settle_tick(id, state.nav_gen));
}

fn stop_settle(state: &mut TuiState, settle: &mut Option<tokio::task::JoinHandle<JobResult>>) {
    if let Some(job) = settle.take() {
        job.abort();
    }
    state.scripts_since = None;
}

fn spawn_talk(id: u32, message: String) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Talk { id, message }).await {
            Ok(Response::Ok {
                reply: Some(text),
                context_usage,
                ..
            }) => JobResult::Reply {
                text,
                usage: context_usage,
            },
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("no reply".into()),
        }
    })
}

fn spawn_click(id: u32, text: String) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Click { id, text }).await {
            Ok(Response::Ok {
                reply: Some(snapshot),
                ..
            }) => {
                let usage = match rpc(&Request::Context { id }).await {
                    Ok(Response::Ok { context_usage, .. }) => context_usage,
                    _ => None,
                };
                JobResult::Page {
                    id,
                    snapshot,
                    usage,
                }
            }
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("click returned no page".into()),
        }
    })
}

fn spawn_type(id: u32, label: String, text: String) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Type { id, label, text }).await {
            Ok(Response::Ok {
                reply: Some(snapshot),
                ..
            }) => {
                let usage = match rpc(&Request::Context { id }).await {
                    Ok(Response::Ok { context_usage, .. }) => context_usage,
                    _ => None,
                };
                // Typing stays on the same page — keep scroll via Refreshed.
                JobResult::Refreshed { snapshot, usage }
            }
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("type returned no page".into()),
        }
    })
}

fn spawn_clear_talk(id: u32) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::ClearTalk { id }).await {
            Ok(Response::Ok { context_usage, .. }) => JobResult::Cleared {
                usage: context_usage,
            },
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
        }
    })
}

struct PaneLayout {
    cols: usize,
    body: usize,
    /// Columns for the left header and the `url>` field (no side border).
    left: usize,
    /// Columns for the right header and the `chat>` field.
    right: usize,
    /// Page cell between the two `│` borders. Exactly `left - 2`.
    page_w: usize,
    /// Chat cell after the split. Exactly `right - 2`.
    chat_w: usize,
}

fn pane_layout() -> PaneLayout {
    let (rows, cols) = term::term_size();
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(10, term::MAX_ROWS);
    let body = rows.saturating_sub(4).max(3);
    let left = (cols / 2).max(16);
    let right = cols.saturating_sub(left);
    let page_w = left.saturating_sub(2).max(1);
    let chat_w = right.saturating_sub(2).max(1);
    PaneLayout {
        cols,
        body,
        left,
        right,
        page_w,
        chat_w,
    }
}

fn pane_sizes() -> (usize, usize, usize) {
    let lay = pane_layout();
    (lay.body, lay.page_w, lay.chat_w)
}

fn draw(state: &TuiState) {
    let lay = pane_layout();
    let cols = lay.cols;
    let body = lay.body;
    let left = lay.left;
    let right = lay.right;
    let page_w = lay.page_w;
    let chat_w = lay.chat_w;

    let ta = crate::chrome::theme_ansi();
    let reset = ta.reset.as_str();
    let cyan = ta.border.as_str();
    let dim = ta.dim.as_str();
    let inv = ta.selection.as_str();

    let page_h = state.view_h_for(body, Focus::Url);
    let chat_h = state.view_h_for(body, Focus::Chat);
    let page = state.page_lines(page_w);
    let chat = state.chat_lines(chat_w);
    let page_view = page
        .iter()
        .skip(state.page_scroll)
        .take(page_h)
        .cloned()
        .collect::<Vec<_>>();
    let chat_view = chat
        .iter()
        .skip(state.chat_scroll)
        .take(chat_h)
        .cloned()
        .collect::<Vec<_>>();

    let mut out = io::stdout().lock();
    let _ = write!(out, "\x1b[2J\x1b[H");
    let left_head = if state.busy == Some(Busy::Page) || state.scripts_since.is_some() {
        state.wait_line().unwrap_or_else(|| " web".into())
    } else {
        " web  ^R refresh · chat: click N · ^L clear".into()
    };
    let right_head = state.context_label();
    let _ = write!(
        out,
        "\x1b[1;1H{cyan}{}{}{reset}",
        pad_visible(&left_head, left),
        pad_visible(&right_head, right)
    );

    let wait = state.wait_line();
    for i in 0..body {
        let row = 2 + i;
        let left_cell = if state.busy == Some(Busy::Page) && i + 1 == body {
            wait.clone().unwrap_or_default()
        } else {
            page_view.get(i).cloned().unwrap_or_default()
        };
        let right_cell = if state.busy == Some(Busy::Chat) && i + 1 == body {
            wait.clone().unwrap_or_default()
        } else {
            chat_view.get(i).cloned().unwrap_or_default()
        };
        let _ = write!(
            out,
            "\x1b[{row};1H{cyan}│{reset}{}{cyan}│{reset}{}",
            pad_visible(&left_cell, page_w),
            pad_visible(&right_cell, chat_w)
        );
    }

    let split = body + 2;
    let rule = if state.busy.is_none() {
        if let Some(s) = &state.status {
            rule_with_label(s, cols)
        } else {
            "─".repeat(cols)
        }
    } else {
        "─".repeat(cols)
    };
    let _ = write!(out, "\x1b[{split};1H{cyan}{}{reset}", rule);

    let input_row = split + 1;
    let url_label = format!("url> {}", state.url);
    let chat_label = format!("chat> {}", state.chat);
    let (url_style, chat_style) = match state.focus {
        Focus::Url => (inv, dim),
        Focus::Chat => (dim, inv),
    };
    let _ = write!(
        out,
        "\x1b[{input_row};1H{url_style}{}{reset}{chat_style}{}{reset}",
        pad_visible(&url_label, left),
        pad_visible(&chat_label, right)
    );
    let _ = out.flush();
}

fn rule_with_label(label: &str, width: usize) -> String {
    let shown = format!(" {label} ");
    if shown.chars().count() >= width {
        return pad_visible(&shown, width);
    }
    let n = width.saturating_sub(shown.chars().count());
    format!("{shown}{}", "─".repeat(n))
}

/// Clip or pad `s` to exactly `width` columns. A tab is a space so it cannot
/// jump the cursor past the pane.
fn pad_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in s.chars() {
        if n >= width {
            break;
        }
        out.push(if c == '\t' { ' ' } else { c });
        n += 1;
    }
    while n < width {
        out.push(' ');
        n += 1;
    }
    out
}

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
    CtrlR,
    CtrlL,
    Char(char),
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
        0x12 => Key::CtrlR,
        0x0c => Key::CtrlL,
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
                    _ => Key::Esc,
                };
            }
            Key::Esc
        }
        c if c >= 0x20 && c < 0x7f => Key::Char(c as char),
        _ => Key::Esc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> TuiState {
        TuiState::new(PathBuf::from("/tmp/web-tui-test-hist.json"), None)
    }

    #[test]
    fn toggle_focus() {
        let mut s = state();
        assert_eq!(s.focus, Focus::Url);
        s.toggle_focus();
        assert_eq!(s.focus, Focus::Chat);
        s.toggle_focus();
        assert_eq!(s.focus, Focus::Url);
    }

    #[test]
    fn page_scroll_clamps() {
        let mut s = state();
        s.page = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        s.focus = Focus::Url;
        s.scroll_focused(1000, 5, 20, 20);
        let max = s.page_lines(20).len().saturating_sub(5);
        assert_eq!(s.page_scroll, max);
        s.scroll_focused(-10_000, 5, 20, 20);
        assert_eq!(s.page_scroll, 0);
    }

    #[test]
    fn bare_host_is_sent_as_https() {
        let mut s = state();
        s.url = "example.com/docs".into();
        assert_eq!(s.submit_url().unwrap(), "https://example.com/docs");
        assert_eq!(s.url, "https://example.com/docs");
        assert!(s.busy.is_some());

        let mut s = state();
        s.url = "localhost:8080".into();
        assert_eq!(s.submit_url().unwrap(), "https://localhost:8080");

        let mut s = state();
        s.url = "//cdn.example/a".into();
        assert_eq!(s.submit_url().unwrap(), "https://cdn.example/a");

        let mut s = state();
        s.url = "HTTP://example.com".into();
        assert_eq!(s.submit_url().unwrap(), "HTTP://example.com");
    }

    #[test]
    fn explicit_non_http_scheme_is_rejected() {
        let mut s = state();
        s.url = "ftp://files.example".into();
        assert!(s.submit_url().is_err());
        assert_eq!(s.status.as_deref(), Some("only http and https"));
        assert!(s.busy.is_none());

        let mut s = state();
        s.url = "javascript:alert(1)".into();
        assert!(s.submit_url().is_err());
    }

    #[test]
    fn initial_url_without_scheme_opens_as_https() {
        let (display, open, status) = prepare_initial_url(Some("news.ycombinator.com".into()));
        assert_eq!(display.as_deref(), Some("https://news.ycombinator.com"));
        assert_eq!(open.as_deref(), Some("https://news.ycombinator.com"));
        assert!(status.is_none());

        let (display, open, status) = prepare_initial_url(Some("ftp://files.example".into()));
        assert_eq!(display.as_deref(), Some("ftp://files.example"));
        assert!(open.is_none());
        assert_eq!(status, Some("only http and https"));
    }

    #[test]
    fn chat_rejected_without_page() {
        let mut s = state();
        s.chat = "hello".into();
        s.focus = Focus::Chat;
        assert!(s.submit_chat().is_err());
        assert!(s.turns.is_empty());
    }

    #[test]
    fn apply_page_resets_scroll_and_records() {
        let dir = std::env::temp_dir().join(format!("web-tui-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        let _ = std::fs::remove_file(&path);
        let mut s = TuiState::new(path.clone(), None);
        s.page_scroll = 9;
        s.apply_page(
            1,
            "URL: https://example.com/\nTitle: Example Domain\n\nhello\n",
            None,
        );
        assert_eq!(s.page_scroll, 0);
        assert_eq!(s.member_id, Some(1));
        assert_eq!(s.url, "https://example.com/");
        let hist = web_history::load(&path);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].title, "Example Domain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_label_tracks_talk_usage() {
        let mut s = state();
        assert_eq!(s.context_label(), "ctx —");
        s.push_reply("headlines".into(), Some(ContextUsage::from_counts(50, 200)));
        assert_eq!(s.context_label(), "ctx 50/200 tok · est 25%");
        assert!(s.busy.is_none());
    }

    #[test]
    fn wait_line_spins_for_page_and_chat() {
        let mut s = state();
        assert!(s.wait_line().is_none());
        s.set_busy(Busy::Page);
        s.busy_since = Some(Instant::now() - Duration::from_secs(4));
        let loading = s.wait_line().unwrap();
        assert!(loading.starts_with('['), "{loading}");
        assert!(loading.contains("loading"));
        assert!(loading.contains("4s"), "{loading}");
        s.set_busy(Busy::Chat);
        s.busy_since = Some(Instant::now() - Duration::from_secs(75));
        let thinking = s.wait_line().unwrap();
        assert!(thinking.contains("thinking"));
        assert!(thinking.contains("1m15s"), "{thinking}");
        assert_ne!(
            spinner_glyph(Duration::from_millis(0)),
            spinner_glyph(Duration::from_millis(120))
        );
    }

    #[test]
    fn chat_error_lands_in_the_transcript_and_clears_the_spinner() {
        let mut s = state();
        s.member_id = Some(1);
        s.page = "URL: https://example.com/\nbody".into();
        s.chat = "top stories".into();
        assert_eq!(
            s.submit_chat().unwrap(),
            ChatAction::Talk("top stories".into())
        );
        assert_eq!(s.busy, Some(Busy::Chat));
        s.set_error("error sending request".into());
        assert!(s.busy.is_none());
        assert!(s.wait_line().is_none());
        assert_eq!(s.turns.len(), 2);
        assert!(s.turns[1].text.contains("error sending request"));
        assert_eq!(s.context_label(), "ctx —");
    }

    #[test]
    fn chat_parses_click_type_and_clear() {
        let mut s = state();
        s.member_id = Some(1);
        s.page = "URL: https://example.com/\nLinks and buttons\n1. Learn more\n".into();

        s.chat = "click 1".into();
        assert_eq!(s.submit_chat().unwrap(), ChatAction::Click("1".into()));
        assert_eq!(s.busy, Some(Busy::Page));

        s.clear_busy();
        s.chat = "type Search -- hello".into();
        assert_eq!(
            s.submit_chat().unwrap(),
            ChatAction::Type {
                label: "Search".into(),
                text: "hello".into(),
            }
        );

        s.clear_busy();
        s.turns.push(Turn {
            user: true,
            text: "old".into(),
        });
        s.chat = "clear".into();
        assert_eq!(s.submit_chat().unwrap(), ChatAction::Clear);
        assert!(s.turns.len() == 1); // clear action itself does not wipe until apply
    }

    #[test]
    fn page_load_clears_chat_and_records_context() {
        let mut s = state();
        s.set_busy(Busy::Page);
        s.turns.push(Turn {
            user: true,
            text: "about the old site".into(),
        });
        s.apply_page(
            3,
            "URL: https://example.com/\nTitle: Example\n",
            Some(ContextUsage::from_counts(10, 100)),
        );
        assert!(s.busy.is_none());
        assert!(s.turns.is_empty());
        assert_eq!(s.context_label(), "ctx 10/100 tok · est 10%");
    }

    #[test]
    fn refresh_keeps_scroll_and_needs_a_page() {
        let mut s = state();
        assert!(s.request_refresh().is_err());
        s.member_id = Some(2);
        s.page = "URL: https://example.com/\nTitle: Example\nold body\n".into();
        s.page_scroll = 4;
        assert_eq!(s.request_refresh(), Ok(2));
        assert_eq!(s.busy, Some(Busy::Page));
        s.replace_page(
            "URL: https://example.com/\nTitle: Example\nnew body after script\n",
            Some(ContextUsage::from_counts(20, 100)),
        );
        assert!(s.busy.is_none());
        assert_eq!(s.page_scroll, 4);
        assert!(s.page.contains("new body after script"));
        assert_eq!(s.context_label(), "ctx 20/100 tok · est 20%");
    }

    #[test]
    fn navigation_bumps_generation_and_a_reread_does_not() {
        let mut s = state();
        s.apply_page(1, "URL: https://a.test/\nTitle: A\n", None);
        assert_eq!(s.nav_gen, 1);
        assert!(s.scripts_since.is_none());
        s.page_scroll = 3;
        s.scripts_since = Some(Instant::now());
        s.replace_page("URL: https://a.test/\nTitle: A\nmore\n", None);
        assert_eq!(s.nav_gen, 1);
        assert_eq!(s.page_scroll, 3);
        assert!(s.scripts_since.is_some());
    }

    #[test]
    fn settle_updates_while_text_changes_then_stops() {
        let first = next_settle(true, 0, 0);
        assert!(first.apply);
        assert_eq!(first.stable, 0);
        assert!(first.keep_going);

        let same = next_settle(false, 0, 1);
        assert!(!same.apply);
        assert_eq!(same.stable, 1);
        assert!(same.keep_going);

        let done = next_settle(false, 1, 2);
        assert!(!done.apply);
        assert_eq!(done.stable, 2);
        assert!(!done.keep_going);

        let ticking = next_settle(true, 0, SETTLE_MAX_TICKS - 1);
        assert!(ticking.apply);
        assert!(!ticking.keep_going);
    }

    #[test]
    fn scripts_wait_line_is_separate_from_loading() {
        let mut s = state();
        s.scripts_since = Some(Instant::now() - Duration::from_secs(3));
        let line = s.wait_line().unwrap();
        assert!(line.contains("scripts"), "{line}");
        assert!(line.contains("3s"), "{line}");
    }

    fn assert_width(lines: &[String], width: usize) {
        for line in lines {
            assert_eq!(line.chars().count(), width, "{line:?}");
            assert!(!line.contains('\t'), "{line:?}");
        }
    }

    #[test]
    fn wrap_breaks_on_words_and_respects_width() {
        let narrow = wrap_lines("Show HN: a long title here", 10);
        assert_width(&narrow, 10);
        assert_eq!(
            narrow.iter().map(|l| l.trim_end()).collect::<Vec<_>>(),
            vec!["Show HN: a", "long title", "here"]
        );

        let wide = wrap_lines("Show HN: a long title here", 20);
        assert_width(&wide, 20);
        assert_eq!(wide[0].trim_end(), "Show HN: a long");
        assert!(wide.len() < narrow.len());
    }

    #[test]
    fn wrap_treats_tabs_as_spaces() {
        let lines = wrap_lines("1.\tShow HN: title", 12);
        assert_width(&lines, 12);
        assert_eq!(
            lines.iter().map(|l| l.trim_end()).collect::<Vec<_>>(),
            vec!["1. Show HN:", "title"]
        );
    }

    #[test]
    fn wrap_clips_one_overlong_word_onto_the_next_line() {
        let lines = wrap_lines("Supercalifragilistic", 6);
        assert_width(&lines, 6);
        assert_eq!(
            lines.iter().map(|l| l.trim_end()).collect::<Vec<_>>(),
            vec!["Superc", "alifra", "gilist", "ic"]
        );
    }

    #[test]
    fn wrap_pads_short_lines_and_keeps_blank_lines() {
        let short = wrap_lines("hi", 5);
        assert_eq!(short, vec!["hi   ".to_string()]);

        let blank = wrap_lines("hello\n\nworld", 8);
        assert_width(&blank, 8);
        assert_eq!(blank.len(), 3);
        assert_eq!(blank[0].trim_end(), "hello");
        assert_eq!(blank[1].trim(), "");
        assert_eq!(blank[2].trim_end(), "world");
    }

    #[test]
    fn pad_visible_replaces_tabs_and_clips() {
        assert_eq!(pad_visible("ab\tcd", 8), "ab cd   ");
        assert_eq!(pad_visible("abcdef", 4), "abcd");
    }
}
