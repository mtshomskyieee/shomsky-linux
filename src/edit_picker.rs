//! Full-screen file picker for bare `:edit` — browse cwd or create a new file.

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::chrome::{self, Canvas, Rect};
use crate::quickyn_tui::FileBrowser;
use crate::term::{self, RawGuard};
use crate::ui;

enum Mode {
    Browse,
    NewName { buf: String },
}

/// Row 0 is always "New file…"; rows 1+ map to `browser.entries`.
struct PickerState {
    browser: FileBrowser,
    /// Index into virtual list: 0 = New file, 1.. = browser entries.
    selected: usize,
    mode: Mode,
}

impl PickerState {
    fn new(cwd: PathBuf) -> Self {
        Self {
            browser: FileBrowser::open(cwd),
            selected: 1, // first real entry (`..`) after New file
            mode: Mode::Browse,
        }
    }

    fn row_count(&self) -> usize {
        1 + self.browser.entries.len()
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        let n = self.row_count();
        if n > 0 && self.selected + 1 < n {
            self.selected += 1;
        }
    }

    fn clamp_selected(&mut self) {
        let n = self.row_count();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Enter: Some(path) to open, None if navigated a directory / started new-name.
    fn activate(&mut self) -> Option<PathBuf> {
        match &self.mode {
            Mode::NewName { buf } => {
                let name = buf.trim();
                if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                    return None;
                }
                let path = self.browser.dir.join(name);
                return Some(path);
            }
            Mode::Browse => {
                if self.selected == 0 {
                    self.mode = Mode::NewName {
                        buf: String::new(),
                    };
                    return None;
                }
                let idx = self.selected - 1;
                // Temporarily select in browser then activate.
                if idx >= self.browser.entries.len() {
                    return None;
                }
                self.browser.selected = idx;
                match self.browser.activate() {
                    Some(path) => Some(path),
                    None => {
                        // Entered a directory — keep selection near top.
                        self.selected = 1.min(self.row_count().saturating_sub(1));
                        None
                    }
                }
            }
        }
    }
}

/// Raw + alt screen; always leaves the alt buffer on Drop.
struct PickerTtyGuard {
    _raw: RawGuard,
}

impl PickerTtyGuard {
    fn enter() -> Option<Self> {
        let raw = RawGuard::enter();
        if !raw.active() {
            return None;
        }
        {
            let mut out = io::stdout().lock();
            let _ = write!(out, "\x1b[?1049h\x1b[?25h");
            let _ = out.flush();
        }
        Some(Self { _raw: raw })
    }
}

impl Drop for PickerTtyGuard {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?1049l\x1b[?25h\x1b[0m");
        let _ = out.flush();
    }
}

/// Returns a chosen path, or `None` if the user cancelled.
pub fn run(cwd: PathBuf) -> Option<PathBuf> {
    if !(io::stdout().is_terminal() && ui::color_enabled()) {
        println!("edit: interactive picker only — use :edit <path>");
        return None;
    }
    let Some(_guard) = PickerTtyGuard::enter() else {
        println!("edit: could not enter raw mode — try :edit from a real TTY");
        return None;
    };

    let mut state = PickerState::new(cwd);
    loop {
        draw(&state);
        match poll_key(Duration::from_millis(50)) {
            Some(Key::Esc) | Some(Key::CtrlC) => match &state.mode {
                Mode::NewName { .. } => {
                    state.mode = Mode::Browse;
                }
                Mode::Browse => return None,
            },
            Some(Key::Enter) => {
                if let Some(path) = state.activate() {
                    return Some(path);
                }
            }
            Some(Key::Up) if matches!(state.mode, Mode::Browse) => state.move_up(),
            Some(Key::Down) if matches!(state.mode, Mode::Browse) => state.move_down(),
            Some(Key::Backspace) => match &mut state.mode {
                Mode::NewName { buf } => {
                    buf.pop();
                }
                Mode::Browse => {
                    state.browser.filter_backspace();
                    state.clamp_selected();
                }
            },
            Some(Key::Char(c)) if !c.is_control() => match &mut state.mode {
                Mode::NewName { buf } => {
                    if buf.len() < 200 {
                        buf.push(c);
                    }
                }
                Mode::Browse => {
                    state.browser.filter_push(c);
                    state.selected = 1.min(state.row_count().saturating_sub(1));
                }
            },
            _ => {}
        }
    }
}

fn draw(state: &PickerState) {
    let (rows, cols) = term::term_size();
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(8, term::MAX_ROWS);
    let pal = chrome::palette();
    let border = pal.border;
    let title_st = pal.title;
    let dim = pal.dim;
    let text = pal.text;
    let sel = pal.selection();
    let bold = title_st;

    let mut canvas = Canvas::new(cols, rows);
    let outer = Rect::new(0, 0, cols, rows);
    let footer = match &state.mode {
        Mode::NewName { .. } => "Enter create · Esc cancel",
        Mode::Browse => "↑↓ · type filter · Enter open · Esc quit",
    };
    canvas.frame(outer, "edit — pick a file", footer, border, title_st);
    let inner = outer.inner();

    let dir = truncate(
        &state.browser.dir.display().to_string(),
        inner.w.saturating_sub(6),
    );
    canvas.fill_row(inner.x, inner.y, inner.w, &format!(" dir {dir}"), dim);

    match &state.mode {
        Mode::NewName { buf: name } => {
            canvas.fill_row(
                inner.x,
                inner.y + 2,
                inner.w,
                &format!(" New file name: {name}_"),
                bold,
            );
        }
        Mode::Browse => {
            let filter = if state.browser.filter.is_empty() {
                String::new()
            } else {
                format!(" filter:{}", state.browser.filter)
            };
            canvas.fill_row(inner.x, inner.y + 1, inner.w, &filter, dim);

            let body = inner.h.saturating_sub(3).max(4);
            let list_y0 = inner.y + 2;
            let offset = state.selected.saturating_sub(body.saturating_sub(1));
            for row in 0..body {
                let idx = offset + row;
                if idx >= state.row_count() {
                    break;
                }
                let label = if idx == 0 {
                    "  + New file…".to_string()
                } else {
                    let e = &state.browser.entries[idx - 1];
                    if e.is_dir {
                        format!("  [dir] {}/", e.name)
                    } else {
                        format!("        {}", e.name)
                    }
                };
                let line = truncate(&label, inner.w);
                let style = if idx == state.selected {
                    sel
                } else if idx == 0 {
                    bold
                } else {
                    text
                };
                canvas.fill_row(inner.x, list_y0 + row, inner.w, &line, style);
            }
        }
    }

    canvas.flush();
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else if max <= 1 {
        "…".to_string()
    } else {
        let take = max - 1;
        format!("{}…", s.chars().take(take).collect::<String>())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Enter,
    Esc,
    Backspace,
    CtrlC,
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
        b'\r' | b'\n' => Key::Enter,
        0x08 | 0x7f => Key::Backspace,
        0x1b => {
            // Settle so CSI tails of arrow keys are not mistaken for bare Esc
            // (same pattern as chat_tui / split_frame).
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
                    b'C' | b'D' => Key::Other,
                    _ => Key::Other,
                };
            }
            if rest[0] == 0x1b {
                return Key::Esc;
            }
            Key::Other
        }
        c if (0x20..0x7f).contains(&c) => Key::Char(c as char),
        _ => Key::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_file_name_joins_dir() {
        let dir = std::env::temp_dir().join(format!("edit-pick-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut state = PickerState::new(dir.clone());
        state.selected = 0;
        assert!(state.activate().is_none()); // enter new-name mode
        match &mut state.mode {
            Mode::NewName { buf } => *buf = "fresh.py".into(),
            _ => panic!("expected NewName"),
        }
        let path = state.activate().unwrap();
        assert_eq!(path, dir.join("fresh.py"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reject_empty_or_slash_name() {
        let dir = std::env::temp_dir().join(format!("edit-pick-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut state = PickerState::new(dir.clone());
        state.mode = Mode::NewName {
            buf: "a/b".into(),
        };
        assert!(state.activate().is_none());
        state.mode = Mode::NewName { buf: "  ".into() };
        assert!(state.activate().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncate_shortens() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn path_join_sanity() {
        let p = Path::new("/tmp").join("x.py");
        assert_eq!(p, PathBuf::from("/tmp/x.py"));
    }
}
