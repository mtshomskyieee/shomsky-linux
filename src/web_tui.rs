//! Split text browser (`:web`).
//!
//! Left: page snapshot. Right: conversation. Bottom: url and chat inputs.
//! Same ANSI + BusyBox `stty` approach as `:chat` — no TUI crates.

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use shomagent::daemon::rpc;
use shomagent::protocol::{Request, Response};

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
            return vec!["(ask about the page)".into()];
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
                scroll_clamp(&mut self.page_scroll, delta, total, view_h);
            }
            Focus::Chat => {
                self.follow_chat = false;
                let total = self.chat_lines(chat_w).len();
                scroll_clamp(&mut self.chat_scroll, delta, total, view_h);
            }
        }
    }

    pub fn clamp_scrolls(&mut self, view_h: usize, page_w: usize, chat_w: usize) {
        let page_n = self.page_lines(page_w).len();
        let chat_n = self.chat_lines(chat_w).len();
        let page_max = page_n.saturating_sub(view_h);
        let chat_max = chat_n.saturating_sub(view_h);
        self.page_scroll = self.page_scroll.min(page_max);
        if self.follow_chat {
            self.chat_scroll = chat_max;
        } else {
            self.chat_scroll = self.chat_scroll.min(chat_max);
        }
    }

    /// Apply a successful page load. Resets page scroll and records history.
    pub fn apply_page(&mut self, id: u32, snapshot: &str) {
        self.member_id = Some(id);
        self.page = snapshot.to_string();
        self.page_scroll = 0;
        if let Some(url) = url_from_snapshot(snapshot) {
            self.url = url;
            self.url_cursor = self.url.chars().count();
        }
        let title = title_from_snapshot(snapshot);
        let _ = web_history::record(&self.history_path, &self.url, &title);
        self.status = None;
    }

    /// Chat Enter. Rejects when no page is loaded.
    pub fn submit_chat(&mut self) -> Result<String, &'static str> {
        if self.page.trim().is_empty() || self.member_id.is_none() {
            self.status = Some("load a page first".into());
            return Err("load a page first");
        }
        let q = self.chat.trim().to_string();
        if q.is_empty() {
            return Err("empty");
        }
        self.turns.push(Turn {
            user: true,
            text: q.clone(),
        });
        self.chat.clear();
        self.chat_cursor = 0;
        self.follow_chat = true;
        self.status = Some("thinking…".into());
        Ok(q)
    }

    pub fn push_reply(&mut self, text: String) {
        self.turns.push(Turn {
            user: false,
            text,
        });
        self.follow_chat = true;
        self.status = None;
    }

    pub fn submit_url(&mut self) -> Result<String, &'static str> {
        let url = self.url.trim().to_string();
        if url.is_empty() {
            self.status = Some("url is empty".into());
            return Err("empty");
        }
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            self.status = Some("only http and https".into());
            return Err("scheme");
        }
        self.status = Some("opening…".into());
        Ok(url)
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

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for chunk in chars.chunks(width) {
            out.push(chunk.iter().collect());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
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

    let mut state = TuiState::new(web_history::history_path(), initial_url.clone());
    let mut pending: Option<tokio::task::JoinHandle<JobResult>> = None;
    if let Some(url) = initial_url {
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
                    Ok(JobResult::Page { id, snapshot }) => state.apply_page(id, &snapshot),
                    Ok(JobResult::Reply(text)) => state.push_reply(text),
                    Ok(JobResult::Err(e)) => {
                        state.status = Some(e);
                    }
                    Err(e) => state.status = Some(format!("task: {e}")),
                }
                continue;
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
                state.status = Some("cancelled".into());
            }
            Some(Key::Tab) if pending.is_none() => state.toggle_focus(),
            Some(Key::Enter) if pending.is_none() => match state.focus {
                Focus::Url => {
                    if let Ok(url) = state.submit_url() {
                        pending = Some(match state.member_id {
                            Some(id) => spawn_navigate(id, url),
                            None => spawn_open(url),
                        });
                    }
                }
                Focus::Chat => {
                    if let Ok(q) = state.submit_chat() {
                        if let Some(id) = state.member_id {
                            pending = Some(spawn_talk(id, q));
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
            Some(Key::PageDown) => {
                state.scroll_focused(body_h as isize, body_h, page_w, chat_w)
            }
            _ => {}
        }
    }

    if let Some(job) = pending.take() {
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
    Page { id: u32, snapshot: String },
    Reply(String),
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
                    ..
                }) => JobResult::Page {
                    id: m.id,
                    snapshot: ctx.ring_excerpt,
                },
                Ok(Response::Err { message }) => JobResult::Err(message),
                Err(e) => JobResult::Err(e.to_string()),
                _ => JobResult::Page {
                    id: m.id,
                    snapshot: m.command,
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
            }) => JobResult::Page { id, snapshot },
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("navigate returned no page".into()),
        }
    })
}

fn spawn_talk(id: u32, message: String) -> tokio::task::JoinHandle<JobResult> {
    tokio::spawn(async move {
        match rpc(&Request::Talk { id, message }).await {
            Ok(Response::Ok {
                reply: Some(text), ..
            }) => JobResult::Reply(text),
            Ok(Response::Err { message }) => JobResult::Err(message),
            Err(e) => JobResult::Err(e.to_string()),
            _ => JobResult::Err("no reply".into()),
        }
    })
}

fn pane_sizes() -> (usize, usize, usize) {
    let (rows, cols) = term::term_size();
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(10, term::MAX_ROWS);
    let body = rows.saturating_sub(4).max(3);
    let left = (cols / 2).max(16);
    let page_w = left.saturating_sub(2).max(8);
    let chat_w = cols.saturating_sub(left).saturating_sub(2).max(8);
    (body, page_w, chat_w)
}

fn draw(state: &TuiState) {
    let (rows, cols) = term::term_size();
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(10, term::MAX_ROWS);
    let body = rows.saturating_sub(4).max(3);
    let left = (cols / 2).max(16);
    let right = cols.saturating_sub(left);
    let page_w = left.saturating_sub(2).max(8);
    let chat_w = right.saturating_sub(2).max(8);

    let color = ui::color_enabled();
    let reset = "\x1b[0m";
    let cyan = if color { "\x1b[36m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let inv = if color { "\x1b[7m" } else { "" };

    let page = state.page_lines(page_w);
    let chat = state.chat_lines(chat_w);
    let page_view = page
        .iter()
        .skip(state.page_scroll)
        .take(body)
        .cloned()
        .collect::<Vec<_>>();
    let chat_view = chat
        .iter()
        .skip(state.chat_scroll)
        .take(body)
        .cloned()
        .collect::<Vec<_>>();

    let mut out = io::stdout().lock();
    let _ = write!(out, "\x1b[2J\x1b[H");
    let title = if let Some(s) = &state.status {
        format!(" web  {s} ")
    } else {
        " web ".into()
    };
    let _ = write!(
        out,
        "\x1b[1;1H{cyan}{}{reset}",
        pad_visible(&title, cols)
    );

    for i in 0..body {
        let row = 2 + i;
        let left_cell = page_view.get(i).map(|s| s.as_str()).unwrap_or("");
        let right_cell = chat_view.get(i).map(|s| s.as_str()).unwrap_or("");
        let _ = write!(
            out,
            "\x1b[{row};1H{cyan}│{reset}{}{cyan}│{reset}{}",
            pad_visible(left_cell, left.saturating_sub(2)),
            pad_visible(right_cell, right.saturating_sub(1))
        );
    }

    let split = body + 2;
    let _ = write!(
        out,
        "\x1b[{split};1H{cyan}{}{reset}",
        "─".repeat(cols.min(term::MAX_COLS))
    );

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

fn pad_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in s.chars() {
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
        s.page = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        s.focus = Focus::Url;
        s.scroll_focused(1000, 5, 20, 20);
        let max = s.page_lines(20).len().saturating_sub(5);
        assert_eq!(s.page_scroll, max);
        s.scroll_focused(-10_000, 5, 20, 20);
        assert_eq!(s.page_scroll, 0);
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
        );
        assert_eq!(s.page_scroll, 0);
        assert_eq!(s.member_id, Some(1));
        assert_eq!(s.url, "https://example.com/");
        let hist = web_history::load(&path);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].title, "Example Domain");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
