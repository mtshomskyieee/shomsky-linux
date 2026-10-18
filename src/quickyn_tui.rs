//! Split-screen quickyn TUI (`:quickyn`).
//!
//! Left: question + optional file. Right top: ASCII check / X. Right bottom: verdict.
//! Same ANSI + BusyBox `stty` approach as `:menu` — no TUI crates.

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use quickyn::{
    decide, file_size_warning, should_warn_file_size, DecideError, Decision, Verdict,
};

use crate::term::{self, RawGuard};
use crate::ui;

// ── ASCII glyphs (plain ASCII so BusyBox/Alpine TTYs always show them).
// Each set is left-aligned within a fixed block width; the drawer centers the
// *block*, not each line (per-line centering warps the shape).

const CHECK_LARGE: &[&str] = &[
    r"              ####",
    r"             ####",
    r"            ####",
    r" ##        ####",
    r"  ##      ####",
    r"   ##    ####",
    r"    ##  ####",
    r"     ######",
    r"      ####",
];

const CHECK_MEDIUM: &[&str] = &[
    r"        ##",
    r"       ##",
    r" ##   ##",
    r"  ## ##",
    r"   ###",
];

const CHECK_SMALL: &[&str] = &[
    r"   #",
    r"# #",
    r" #",
];

const X_LARGE: &[&str] = &[
    r"####        ####",
    r" ####      ####",
    r"  ####    ####",
    r"   ####  ####",
    r"    ########",
    r"    ########",
    r"   ####  ####",
    r"  ####    ####",
    r" ####      ####",
    r"####        ####",
];

const X_MEDIUM: &[&str] = &[
    r"##    ##",
    r" ##  ##",
    r"  ####",
    r" ##  ##",
    r"##    ##",
];

const X_SMALL: &[&str] = &[
    r"# #",
    r" #",
    r"# #",
];

const THINKING_GLYPH: &[&str] = &[
    r" . . . ",
    r". . . .",
    r" . . . ",
];

/// Pick check / X art sized for the glyph pane height.
pub fn glyph_lines(decision: Decision, pane_h: usize) -> &'static [&'static str] {
    match (decision, pane_h) {
        (Decision::Yes, h) if h >= 12 => CHECK_LARGE,
        (Decision::Yes, h) if h >= 7 => CHECK_MEDIUM,
        (Decision::Yes, _) => CHECK_SMALL,
        (Decision::No, h) if h >= 12 => X_LARGE,
        (Decision::No, h) if h >= 7 => X_MEDIUM,
        (Decision::No, _) => X_SMALL,
    }
}

pub fn thinking_glyph() -> &'static [&'static str] {
    THINKING_GLYPH
}

/// Max visible width of a glyph block (for whole-block centering).
fn glyph_block_width(lines: &[&str]) -> usize {
    lines.iter().map(|l| l.chars().count()).max().unwrap_or(0)
}

// ── Pure state ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Question,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileBrowser {
    pub dir: PathBuf,
    pub entries: Vec<BrowserEntry>,
    pub filter: String,
    pub selected: usize,
}

impl FileBrowser {
    pub fn open(dir: PathBuf) -> Self {
        let mut b = Self {
            dir,
            entries: Vec::new(),
            filter: String::new(),
            selected: 0,
        };
        b.reload();
        b
    }

    pub fn reload(&mut self) {
        let filter_l = self.filter.to_ascii_lowercase();
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                if !filter_l.is_empty() && !name.to_ascii_lowercase().contains(&filter_l) {
                    continue;
                }
                let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let e = BrowserEntry { name, is_dir };
                if is_dir {
                    dirs.push(e);
                } else {
                    files.push(e);
                }
            }
        }
        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        files.sort_by(|a, b| a.name.cmp(&b.name));
        self.entries.clear();
        self.entries.push(BrowserEntry {
            name: "..".into(),
            is_dir: true,
        });
        self.entries.extend(dirs);
        self.entries.extend(files);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    pub fn filter_push(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
        self.reload();
    }

    pub fn filter_backspace(&mut self) {
        self.filter.pop();
        self.selected = 0;
        self.reload();
    }

    /// Enter on selection: `Ok(Some(file))` attach, `Ok(None)` entered dir, `Err` ignore.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?.clone();
        if entry.name == ".." {
            if let Some(parent) = self.dir.parent() {
                self.dir = parent.to_path_buf();
                self.filter.clear();
                self.selected = 0;
                self.reload();
            }
            return None;
        }
        let path = self.dir.join(&entry.name);
        if entry.is_dir {
            self.dir = path;
            self.filter.clear();
            self.selected = 0;
            self.reload();
            None
        } else {
            Some(path)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok(Verdict),
    Err(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuiState {
    pub question: String,
    pub q_cursor: usize,
    pub file_path: String,
    pub f_cursor: usize,
    pub focus: Focus,
    pub browser: Option<FileBrowser>,
    pub outcome: Option<Outcome>,
    pub thinking: bool,
    pub file_warn: Option<String>,
    pub cwd: PathBuf,
}

impl TuiState {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            question: String::new(),
            q_cursor: 0,
            file_path: String::new(),
            f_cursor: 0,
            focus: Focus::Question,
            browser: None,
            outcome: None,
            thinking: false,
            file_warn: None,
            cwd,
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Question => Focus::File,
            Focus::File => Focus::Question,
        };
    }

    fn active_buf_mut(&mut self) -> (&mut String, &mut usize) {
        match self.focus {
            Focus::Question => (&mut self.question, &mut self.q_cursor),
            Focus::File => (&mut self.file_path, &mut self.f_cursor),
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.browser.is_some() {
            if let Some(b) = self.browser.as_mut() {
                b.filter_push(c);
            }
            return;
        }
        let (buf, cur) = self.active_buf_mut();
        let byte = char_to_byte(buf, *cur);
        buf.insert(byte, c);
        *cur += 1;
    }

    pub fn backspace(&mut self) {
        if self.browser.is_some() {
            if let Some(b) = self.browser.as_mut() {
                b.filter_backspace();
            }
            return;
        }
        let (buf, cur) = self.active_buf_mut();
        if *cur == 0 {
            return;
        }
        let byte = char_to_byte(buf, *cur - 1);
        buf.remove(byte);
        *cur -= 1;
    }

    pub fn clear_focused(&mut self) {
        match self.focus {
            Focus::Question => {
                self.question.clear();
                self.q_cursor = 0;
            }
            Focus::File => {
                self.file_path.clear();
                self.f_cursor = 0;
                self.file_warn = None;
            }
        }
    }

    pub fn cursor_left(&mut self) {
        if self.browser.is_some() {
            return;
        }
        let (_, cur) = self.active_buf_mut();
        if *cur > 0 {
            *cur -= 1;
        }
    }

    pub fn cursor_right(&mut self) {
        if self.browser.is_some() {
            return;
        }
        let (buf, cur) = self.active_buf_mut();
        if *cur < buf.chars().count() {
            *cur += 1;
        }
    }

    /// Enter on question: `Some(question)` to ask if non-empty. File focus commits path.
    pub fn on_enter(&mut self) -> Option<String> {
        if let Some(b) = self.browser.as_mut() {
            if let Some(path) = b.activate() {
                self.attach_file(path);
                self.browser = None;
            }
            return None;
        }
        match self.focus {
            Focus::Question => {
                let q = self.question.trim();
                if q.is_empty() {
                    None
                } else {
                    Some(q.to_string())
                }
            }
            Focus::File => {
                self.commit_file_path();
                None
            }
        }
    }

    pub fn commit_file_path(&mut self) {
        let path = self.file_path.trim().to_string();
        self.file_path = path.clone();
        self.f_cursor = self.file_path.chars().count();
        if path.is_empty() {
            self.file_warn = None;
            return;
        }
        let full = resolve_path(&self.cwd, &path);
        match std::fs::read_to_string(&full) {
            Ok(body) => {
                let n = body.chars().count();
                if should_warn_file_size(&body) {
                    self.file_warn = Some(file_size_warning(&full.display().to_string(), n));
                } else {
                    self.file_warn = None;
                }
            }
            Err(e) => {
                self.file_warn = Some(format!("quickyn: {}: {e}", full.display()));
            }
        }
    }

    pub fn attach_file(&mut self, path: PathBuf) {
        let display = path_display_relative(&self.cwd, &path);
        self.file_path = display;
        self.f_cursor = self.file_path.chars().count();
        self.focus = Focus::Question;
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                let n = body.chars().count();
                if should_warn_file_size(&body) {
                    self.file_warn = Some(file_size_warning(&path.display().to_string(), n));
                } else {
                    self.file_warn = None;
                }
            }
            Err(e) => {
                self.file_warn = Some(format!("quickyn: {}: {e}", path.display()));
            }
        }
    }

    pub fn open_browser(&mut self) {
        let start = if self.file_path.trim().is_empty() {
            self.cwd.clone()
        } else {
            let p = resolve_path(&self.cwd, self.file_path.trim());
            if p.is_dir() {
                p
            } else {
                p.parent()
                    .map(|x| x.to_path_buf())
                    .unwrap_or_else(|| self.cwd.clone())
            }
        };
        self.browser = Some(FileBrowser::open(start));
    }

    pub fn close_browser(&mut self) {
        self.browser = None;
    }

    pub fn set_thinking(&mut self) {
        self.thinking = true;
        self.outcome = None;
    }

    pub fn set_verdict(&mut self, v: Verdict) {
        self.thinking = false;
        self.outcome = Some(Outcome::Ok(v));
    }

    pub fn set_error(&mut self, msg: String) {
        self.thinking = false;
        self.outcome = Some(Outcome::Err(msg));
    }

    /// Read optional file body for decide; updates warn on failure.
    pub fn load_file_body(&mut self) -> Option<String> {
        let path = self.file_path.trim();
        if path.is_empty() {
            return None;
        }
        let full = resolve_path(&self.cwd, path);
        match std::fs::read_to_string(&full) {
            Ok(body) => {
                let n = body.chars().count();
                if should_warn_file_size(&body) {
                    self.file_warn = Some(file_size_warning(&full.display().to_string(), n));
                }
                Some(body)
            }
            Err(e) => {
                self.file_warn = Some(format!("quickyn: {}: {e}", full.display()));
                None
            }
        }
    }

    pub fn status_line(&self) -> String {
        if self.thinking {
            return "thinking…".into();
        }
        match &self.outcome {
            Some(Outcome::Ok(v)) => v.format_line(),
            Some(Outcome::Err(e)) => e.clone(),
            None => String::new(),
        }
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn path_display_relative(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

// ── Interactive runner ──────────────────────────────────────────────────────

pub fn available() -> bool {
    io::stdout().is_terminal() && ui::color_enabled()
}

/// Open the quickyn TUI. Returns when the user presses Esc / Ctrl-C.
pub async fn run(cwd: PathBuf) {
    if !available() {
        println!("quickyn: interactive only — use quickyn \"question\" [file]");
        return;
    }

    let guard = RawGuard::enter();
    if !guard.active() {
        println!("quickyn: could not enter raw mode — try :quickyn from a real TTY");
        return;
    }

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049h\x1b[?25h");
        let _ = out.flush();
    }

    let mut state = TuiState::new(cwd);
    let mut pending: Option<tokio::task::JoinHandle<Result<Verdict, DecideError>>> = None;

    loop {
        draw(&state);

        // Poll in-flight decide
        if let Some(handle) = pending.as_mut() {
            if handle.is_finished() {
                match pending.take().unwrap().await {
                    Ok(Ok(v)) => state.set_verdict(v),
                    Ok(Err(DecideError::Unparseable { raw })) => {
                        state.set_error(format!("unparseable:\n{raw}"));
                    }
                    Ok(Err(DecideError::Llm(e))) => {
                        state.set_error(format!("{e:#}"));
                    }
                    Err(e) => state.set_error(format!("task: {e}")),
                }
                continue;
            }
        }

        match poll_key(Duration::from_millis(50)) {
            Some(Key::CtrlC) | Some(Key::Esc) if state.browser.is_none() && pending.is_none() => {
                break;
            }
            Some(Key::Esc) if state.browser.is_some() => {
                state.close_browser();
            }
            Some(Key::Esc) if pending.is_some() => {
                if let Some(h) = pending.take() {
                    h.abort();
                }
                state.thinking = false;
                state.outcome = Some(Outcome::Err("cancelled".into()));
            }
            Some(Key::CtrlC) => break,
            Some(Key::Tab) if state.browser.is_none() => state.toggle_focus(),
            Some(Key::CtrlO) if state.browser.is_none() && !state.thinking => {
                state.open_browser();
            }
            Some(Key::CtrlU) if state.browser.is_none() => state.clear_focused(),
            Some(Key::Enter) => {
                if let Some(q) = state.on_enter() {
                    if pending.is_none() {
                        let file_body = state.load_file_body();
                        // If path set but unreadable, show warn and skip ask
                        if !state.file_path.trim().is_empty() && file_body.is_none() {
                            state.set_error(
                                state
                                    .file_warn
                                    .clone()
                                    .unwrap_or_else(|| "file read failed".into()),
                            );
                        } else {
                            state.set_thinking();
                            let fb = file_body;
                            pending = Some(tokio::spawn(async move {
                                decide(&q, fb.as_deref()).await
                            }));
                        }
                    }
                }
            }
            Some(Key::Up) if state.browser.is_some() => {
                if let Some(b) = state.browser.as_mut() {
                    b.move_up();
                }
            }
            Some(Key::Down) if state.browser.is_some() => {
                if let Some(b) = state.browser.as_mut() {
                    b.move_down();
                }
            }
            Some(Key::Left) => state.cursor_left(),
            Some(Key::Right) => state.cursor_right(),
            Some(Key::Backspace) => state.backspace(),
            Some(Key::Char(c)) => state.insert_char(c),
            _ => {}
        }
    }

    if let Some(h) = pending.take() {
        h.abort();
    }

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049l\x1b[?25h\x1b[0m");
        let _ = out.flush();
    }
    drop(guard);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    Backspace,
    CtrlC,
    CtrlO,
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
        0x0f => Key::CtrlO,
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

fn draw(state: &TuiState) {
    let (rows, cols) = term::term_size();
    // Never exceed the real terminal width (raw mode: no auto CR on LF).
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(12, term::MAX_ROWS);

    let color = ui::color_enabled();
    let r = "\x1b[0m";
    let bold = if color { "\x1b[1m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };
    let green = if color { "\x1b[32m" } else { "" };
    let red = if color { "\x1b[31m" } else { "" };
    let inv = if color { "\x1b[7m" } else { "" };

    // Full-bleed frame: title + body + footer. Borders take 3 cols (│ left │ mid │).
    let body_rows = rows.saturating_sub(2).max(8);
    let inner = cols.saturating_sub(3);
    let left_w = (inner / 2).max(16);
    let right_w = inner.saturating_sub(left_w).max(12);
    // Rebalance if rounding left us short of inner.
    let left_w = if left_w + right_w > inner {
        inner.saturating_sub(right_w)
    } else {
        left_w
    };
    let glyph_h = (body_rows * 2 / 3).max(4);
    let status_h = body_rows.saturating_sub(glyph_h).max(2);

    let mut buf = String::with_capacity(rows * cols * 2);
    use std::fmt::Write as _;
    let _ = write!(buf, "\x1b[2J\x1b[H");

    // Title — absolute position (row 1, col 1), same pattern as :menu
    let title = " quickyn ";
    let dash_n = cols
        .saturating_sub(2)
        .saturating_sub(title.chars().count())
        .max(1);
    let _ = write!(
        buf,
        "\x1b[1;1H{cyan}┌{bold}{title}{r}{cyan}{}┐{r}",
        "─".repeat(dash_n)
    );

    for row in 0..body_rows {
        let y = row + 2; // 1-based screen row under title
        let left_text = left_cell(state, row, body_rows, left_w, &inv, r, dim);
        let right_text = right_cell(
            state,
            row,
            glyph_h,
            status_h,
            right_w,
            green,
            red,
            dim,
            bold,
            r,
        );
        let left = pad_visible(&left_text, left_w);
        let right = pad_visible(&right_text, right_w);
        let _ = write!(
            buf,
            "\x1b[{y};1H{cyan}│{r}{left}{cyan}│{r}{right}{cyan}│{r}"
        );
    }

    let footer = " Enter ask · Tab file · ^O browse · ^U clear · Esc back ";
    let foot_y = body_rows + 2;
    let _ = write!(
        buf,
        "\x1b[{foot_y};1H{cyan}└{dim}{}{r}{cyan}┘{r}",
        pad_visible(footer, cols.saturating_sub(2))
    );

    // Place cursor in the active input field (absolute).
    if state.browser.is_none() {
        let (crow, ccol) = cursor_pos(state, left_w, body_rows);
        let screen_row = crow + 2; // body starts at screen row 2
        let screen_col = ccol + 2; // after left border
        let _ = write!(buf, "\x1b[?25h\x1b[{screen_row};{screen_col}H");
    } else {
        let _ = write!(buf, "\x1b[?25l");
    }

    let mut out = io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn left_cell(
    state: &TuiState,
    row: usize,
    body_rows: usize,
    width: usize,
    inv: &str,
    reset: &str,
    dim: &str,
) -> String {
    if let Some(browser) = &state.browser {
        return browser_cell(browser, row, body_rows, width, inv, reset, dim);
    }

    // Rows: 0 label, 1..body-3 question wrap, body-2 file label, body-1 file path
    let q_area = body_rows.saturating_sub(3).max(1);
    if row == 0 {
        return if state.focus == Focus::Question {
            format!("{inv}> Question{reset}")
        } else {
            format!("{dim}  Question{reset}")
        };
    }
    if row >= 1 && row < 1 + q_area {
        let line_i = row - 1;
        let wrapped = wrap_lines(&state.question, width);
        let text = wrapped.get(line_i).cloned().unwrap_or_default();
        // Bold question text for contrast on dark backgrounds.
        return if text.is_empty() {
            text
        } else if state.focus == Focus::Question {
            format!("\x1b[1m{text}\x1b[0m")
        } else {
            text
        };
    }
    if row == 1 + q_area {
        return if state.focus == Focus::File {
            format!("{inv}> File{reset}")
        } else {
            format!("{dim}  File{reset}")
        };
    }
    // file path line
    if row == 2 + q_area {
        let path = if state.file_path.is_empty() {
            format!("{dim}(none){reset}")
        } else {
            state.file_path.clone()
        };
        return path;
    }
    String::new()
}

fn browser_cell(
    browser: &FileBrowser,
    row: usize,
    body_rows: usize,
    width: usize,
    inv: &str,
    reset: &str,
    dim: &str,
) -> String {
    if row == 0 {
        let title = format!(" {}", browser.dir.display());
        return truncate_visible(&title, width);
    }
    if row == 1 && !browser.filter.is_empty() {
        return format!("{dim}filter:{reset} {}", browser.filter);
    }
    let list_start = if browser.filter.is_empty() { 1 } else { 2 };
    if row < list_start {
        return String::new();
    }
    let idx = row - list_start;
    let scroll = browser.selected.saturating_sub(body_rows.saturating_sub(list_start + 1));
    let i = idx + scroll;
    if let Some(e) = browser.entries.get(i) {
        let mark = if e.is_dir { "/" } else { " " };
        let label = format!("{mark}{}", e.name);
        if i == browser.selected {
            format!("{inv}{}{reset}", pad_visible(&label, width))
        } else {
            label
        }
    } else {
        String::new()
    }
}

fn right_cell(
    state: &TuiState,
    row: usize,
    glyph_h: usize,
    _status_h: usize,
    width: usize,
    green: &str,
    red: &str,
    dim: &str,
    bold: &str,
    reset: &str,
) -> String {
    if row < glyph_h {
        let lines: &[&str] = if state.thinking {
            thinking_glyph()
        } else {
            match &state.outcome {
                Some(Outcome::Ok(v)) => glyph_lines(v.decision, glyph_h),
                _ => &[],
            }
        };
        if lines.is_empty() {
            return String::new();
        }
        let color = match &state.outcome {
            Some(Outcome::Ok(v)) if !state.thinking => match v.decision {
                Decision::Yes => green,
                Decision::No => red,
            },
            _ if state.thinking => dim,
            _ => "",
        };
        let art_h = lines.len();
        let block_w = glyph_block_width(lines);
        let top_pad = glyph_h.saturating_sub(art_h) / 2;
        if row < top_pad || row >= top_pad + art_h {
            return String::new();
        }
        let line = lines[row - top_pad];
        // Pad line to block width first (keeps relative shape), then center block.
        let line_pad = block_w.saturating_sub(line.chars().count());
        let padded_line = format!("{line}{}", " ".repeat(line_pad));
        let left_pad = width.saturating_sub(block_w) / 2;
        return format!(
            "{}{}{}{}{}",
            " ".repeat(left_pad),
            color,
            bold,
            padded_line,
            reset
        );
    }

    // Status area — full-width rule, then verdict / warnings
    let srow = row - glyph_h;
    if srow == 0 {
        return format!("{dim}{}{reset}", "─".repeat(width.min(64).max(8)));
    }
    let status = state.status_line();
    let warn = state.file_warn.clone().unwrap_or_default();
    if srow == 1 {
        // Bright verdict line for contrast on dark themes
        return format!("{bold}{}{reset}", truncate_visible(&status, width));
    }
    if srow >= 2 && !warn.is_empty() {
        let warn_line = srow - 2;
        let wlines: Vec<&str> = warn.lines().collect();
        if let Some(l) = wlines.get(warn_line) {
            return format!("{dim}{}{reset}", truncate_visible(l, width));
        }
    }
    if srow >= 1 && status.contains('\n') {
        let lines: Vec<&str> = status.lines().collect();
        if let Some(l) = lines.get(srow - 1) {
            return format!("{bold}{}{reset}", truncate_visible(l, width));
        }
    }
    String::new()
}

fn cursor_pos(state: &TuiState, left_w: usize, body_rows: usize) -> (usize, usize) {
    let q_area = body_rows.saturating_sub(3).max(1);
    let w = left_w.max(1);
    match state.focus {
        Focus::Question => {
            let line = state.q_cursor / w;
            let col = state.q_cursor % w;
            let max_line = q_area.saturating_sub(1);
            (1 + line.min(max_line), col)
        }
        Focus::File => {
            let row = 2 + q_area;
            let col = state.f_cursor.min(left_w.saturating_sub(1));
            (row, col)
        }
    }
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

fn visible_len(s: &str) -> usize {
    // Strip simple ANSI for width math
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        n += 1;
    }
    n
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

    #[test]
    fn glyph_yes_no_sizes() {
        assert_eq!(glyph_lines(Decision::Yes, 14), CHECK_LARGE);
        assert_eq!(glyph_lines(Decision::Yes, 8), CHECK_MEDIUM);
        assert_eq!(glyph_lines(Decision::Yes, 4), CHECK_SMALL);
        assert_eq!(glyph_lines(Decision::No, 14), X_LARGE);
        assert_eq!(glyph_lines(Decision::No, 8), X_MEDIUM);
        assert_eq!(glyph_lines(Decision::No, 3), X_SMALL);
        assert!(glyph_lines(Decision::No, 14)
            .iter()
            .any(|l| l.contains('#')));
    }

    #[test]
    fn glyph_block_centering_preserves_shape() {
        // Per-line centering would shift columns; block centering keeps relative offsets.
        let lines = CHECK_MEDIUM;
        let block_w = glyph_block_width(lines);
        let width = 40;
        let left_pad = (width - block_w) / 2;
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                let line_pad = block_w - line.chars().count();
                format!(
                    "{}{}{}",
                    " ".repeat(left_pad),
                    line,
                    " ".repeat(line_pad)
                )
            })
            .collect();
        // Every rendered line is exactly `width` or `width-1` (odd leftover) chars…
        // more importantly: the right-stroke `#` column is stable across rows.
        let rightmost_hash = |s: &str| s.rfind('#').unwrap();
        let c0 = rightmost_hash(&rendered[0]);
        let c1 = rightmost_hash(&rendered[1]);
        let c2 = rightmost_hash(&rendered[2]);
        // CHECK_MEDIUM right stroke steps left by 1 each row — same relative shape.
        assert_eq!(c0 - left_pad, lines[0].rfind('#').unwrap());
        assert_eq!(c1 - left_pad, lines[1].rfind('#').unwrap());
        assert_eq!(c2 - left_pad, lines[2].rfind('#').unwrap());
        assert_eq!(c0, c1 + 1);
        assert!(block_w <= width);
    }

    #[test]
    fn left_cell_shows_question_text() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        for c in "the moon is made of cheese".chars() {
            s.insert_char(c);
        }
        let body_rows = 20;
        let left_w = 40;
        let cell = left_cell(&s, 1, body_rows, left_w, "", "", "");
        assert!(
            cell.contains("the moon is made of cheese"),
            "expected question in left cell, got {cell:?}"
        );
        let label = left_cell(&s, 0, body_rows, left_w, "\x1b[7m", "\x1b[0m", "");
        assert!(label.contains("Question"));
    }

    #[test]
    fn right_cell_shows_ascii_x_on_no() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        s.set_verdict(Verdict {
            decision: Decision::No,
            confidence: 0.91,
        });
        let glyph_h = 10;
        let mut found = false;
        for row in 0..glyph_h {
            let cell = right_cell(&s, row, glyph_h, 4, 40, "", "", "", "", "");
            if cell.contains('#') {
                found = true;
                break;
            }
        }
        assert!(found, "expected ASCII X (#) in glyph pane for Decision::No");
        assert_eq!(s.status_line(), "no 0.91");
    }

    #[test]
    fn empty_submit_ignored() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        assert!(s.on_enter().is_none());
        s.insert_char('a');
        assert_eq!(s.on_enter().as_deref(), Some("a"));
    }

    #[test]
    fn tab_toggles_focus() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        assert_eq!(s.focus, Focus::Question);
        s.toggle_focus();
        assert_eq!(s.focus, Focus::File);
        s.toggle_focus();
        assert_eq!(s.focus, Focus::Question);
    }

    #[test]
    fn path_edit_and_clear() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        s.toggle_focus();
        for c in "notes.md".chars() {
            s.insert_char(c);
        }
        assert_eq!(s.file_path, "notes.md");
        s.clear_focused();
        assert!(s.file_path.is_empty());
        assert_eq!(s.f_cursor, 0);
    }

    #[test]
    fn question_typing_and_cursor() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        for c in "hi".chars() {
            s.insert_char(c);
        }
        assert_eq!(s.question, "hi");
        assert_eq!(s.q_cursor, 2);
        s.cursor_left();
        s.backspace();
        assert_eq!(s.question, "i");
        assert_eq!(s.q_cursor, 0);
    }

    #[test]
    fn thinking_clears_prior_outcome() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        s.set_verdict(Verdict {
            decision: Decision::Yes,
            confidence: 0.9,
        });
        assert_eq!(s.status_line(), "yes 0.90");
        s.set_thinking();
        assert!(s.thinking);
        assert_eq!(s.status_line(), "thinking…");
    }

    #[test]
    fn file_warn_when_large() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("quickyn_tui_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("big.txt");
        let body = "x".repeat(1001);
        std::fs::write(&path, &body).unwrap();
        let mut s = TuiState::new(dir.clone());
        s.attach_file(path.clone());
        assert!(s.file_warn.as_ref().unwrap().contains("1001"));
        assert!(s.file_warn.as_ref().unwrap().contains("may not be a quick decision"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn browser_select_file() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("quickyn_browse_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("alpha.txt"), "hi").unwrap();
        std::fs::write(dir.join("beta.txt"), "yo").unwrap();
        let mut s = TuiState::new(dir.clone());
        s.open_browser();
        let b = s.browser.as_mut().unwrap();
        // skip ".."
        b.move_down();
        // first file after dirs — both are files
        let name = b.entries[b.selected].name.clone();
        assert!(name.ends_with(".txt"));
        let path = b.activate().unwrap();
        s.attach_file(path);
        s.browser = None;
        assert!(s.file_path.ends_with(".txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn browser_cancel() {
        let mut s = TuiState::new(PathBuf::from("/tmp"));
        s.open_browser();
        assert!(s.browser.is_some());
        s.close_browser();
        assert!(s.browser.is_none());
    }
}
