//! Matrix-style green rain that crystallizes into dream beat text.
//!
//! Modeled on engram-shell's splash rain. Same env gates (`ENGRAM_MATRIX*`).
//! Unlike the splash, typed bytes are kept: a finished line snaps to the final
//! frame and is returned to the caller as steering / quit.

use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant, SystemTime};

use crate::term;

const TICK: Duration = Duration::from_millis(50);
const DEFAULT_SECS: f32 = 2.0;
const MAX_SECS: f32 = 10.0;

const GLYPHS_KANA: &str = "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789";
const GLYPHS_ASCII: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ<>*+-=/\\|{}[]$#@%&";

const LVL_HEAD: u8 = 0;
const LVL_BRIGHT: u8 = 1;
const LVL_MID: u8 = 2;
const LVL_DIM: u8 = 3;
const LVL_LOGO: u8 = 4;

/// Line buffer shared between rain frames and the dream session loop.
#[derive(Debug, Default)]
pub struct LineBuf {
    partial: String,
    /// Completed lines waiting to be drained (newest wins for steering).
    completed: Vec<String>,
    eof: bool,
}

impl LineBuf {
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'\n' | b'\r' => {
                    let line = std::mem::take(&mut self.partial);
                    self.completed.push(line);
                }
                0x7f | 0x08 => {
                    self.partial.pop();
                }
                // Ctrl-C
                0x03 => {
                    self.completed.push("quit".into());
                }
                c if c.is_ascii_graphic() || c == b' ' || c == b'\t' => {
                    self.partial.push(c as char);
                }
                _ => {}
            }
        }
    }

    pub fn mark_eof(&mut self) {
        self.eof = true;
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.completed.push(line);
        }
    }

    pub fn eof(&self) -> bool {
        self.eof
    }

    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// Take the latest completed line, discarding earlier ones.
    pub fn take_latest(&mut self) -> Option<String> {
        let last = self.completed.pop()?;
        self.completed.clear();
        Some(last)
    }

    pub fn has_completed(&self) -> bool {
        !self.completed.is_empty()
    }
}

/// Whether the rain should play (same knobs as the splash intro).
pub fn enabled() -> bool {
    match std::env::var("ENGRAM_MATRIX")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "off" | "false" | "no" => false,
        "always" | "1" | "on" | "true" | "yes" => true,
        _ => color_tty(),
    }
}

fn color_tty() -> bool {
    if matches!(std::env::var("NO_COLOR"), Ok(v) if !v.is_empty()) {
        return false;
    }
    if matches!(std::env::var("TERM"), Ok(t) if t.is_empty() || t == "dumb") {
        return false;
    }
    io::stdout().is_terminal()
}

fn duration_secs() -> f32 {
    match std::env::var("ENGRAM_MATRIX_SECS") {
        Ok(v) => v
            .trim()
            .parse::<f32>()
            .unwrap_or(DEFAULT_SECS)
            .clamp(0.0, MAX_SECS),
        Err(_) => DEFAULT_SECS,
    }
}

fn glyphs() -> Vec<char> {
    let ascii = matches!(
        std::env::var("ENGRAM_MATRIX_GLYPHS"),
        Ok(v) if v.eq_ignore_ascii_case("ascii")
    );
    if ascii { GLYPHS_ASCII } else { GLYPHS_KANA }
        .chars()
        .collect()
}

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Rng(nanos | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

struct Column {
    head: f32,
    speed: f32,
    len: usize,
    delay: usize,
    glyphs: Vec<char>,
}

fn paint(level: u8, logo: &str) -> &str {
    match level {
        LVL_HEAD => "\x1b[97m",
        LVL_BRIGHT => "\x1b[92m",
        LVL_MID => "\x1b[32m",
        LVL_LOGO => logo,
        _ => "\x1b[2;32m",
    }
}

/// Wrap `text` to `cols`, returning display rows (no trailing empty-only pad).
pub fn wrap_text(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut rows = Vec::new();
    for paragraph in text.replace('\r', "").split('\n') {
        if paragraph.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut line = String::new();
        for ch in paragraph.chars() {
            if line.chars().count() >= cols {
                rows.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
        rows.push(line);
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

fn logo_cells(rows: &[String], cols: usize) -> Vec<(usize, usize, char)> {
    let mut cells = Vec::new();
    for (r, line) in rows.iter().enumerate() {
        for (c, ch) in line.chars().enumerate() {
            if c >= cols {
                break;
            }
            if !ch.is_whitespace() {
                cells.push((r, c, ch));
            }
        }
    }
    cells
}

/// Play rain that crystallizes into `text`. Returns `true` when the final text
/// is on screen; `false` when skipped (caller should print plainly).
///
/// Tall beats are truncated to the terminal height so the animation still runs;
/// any overflow lines are printed after the rain. Positioning is relative to the
/// current cursor, so it works with dream's cooked `steer>`
/// prompt.
///
pub fn play_text(text: &str) -> bool {
    if !enabled() {
        return false;
    }
    let secs = duration_secs();
    if secs <= 0.0 {
        return false;
    }
    if !io::stdout().is_terminal() {
        return false;
    }

    let (term_rows, cols) = term::term_size();
    if cols < 20 || term_rows < 4 {
        return false;
    }

    // Leave room for the steer> prompt under the beat.
    let max_rows = term_rows.saturating_sub(3).max(1);
    let mut art = wrap_text(text, cols);
    if art.is_empty() {
        return false;
    }
    let overflow: Vec<String> = if art.len() > max_rows {
        art.split_off(max_rows)
    } else {
        Vec::new()
    };
    let grid_rows = art.len();
    let logo = logo_cells(&art, cols);
    if logo.is_empty() {
        return false;
    }

    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?25l");
        // Reserve a block below the current cursor, climb to its top, DECSC-save.
        for _ in 0..grid_rows {
            let _ = out.write_all(b"\n");
        }
        let _ = write!(out, "\x1b[{grid_rows}A\r\x1b7");
        let _ = out.flush();
    }

    let logo_sgr = "\x1b[32m";
    run_frames(grid_rows, cols, secs, logo_sgr, &logo);

    {
        let mut out = io::stdout().lock();
        let _ = write!(
            out,
            "\x1b8\x1b[{}B\r\x1b[0m\x1b[?25h\n",
            grid_rows.saturating_sub(1)
        );
        let _ = out.flush();
        for line in &overflow {
            let _ = writeln!(out, "{line}");
        }
    }
    true
}

fn run_frames(rows: usize, cols: usize, secs: f32, logo_sgr: &str, logo: &[(usize, usize, char)]) {
    let mut rng = Rng::new();
    let alphabet = glyphs();
    let mut columns: Vec<Column> = (0..cols)
        .map(|_| new_column(rows, &alphabet, &mut rng, true))
        .collect();

    let cell_count = rows * cols;
    let mut next: Vec<Option<(char, u8)>> = vec![None; cell_count];
    let mut frame_buf = String::with_capacity(cell_count * 8);

    let total = Duration::from_secs_f32(secs);
    let start = Instant::now();

    loop {
        let t = (start.elapsed().as_secs_f32() / secs).min(1.0);
        let spawning = t < 0.60;
        let reveal = ((t - 0.55) / 0.30).clamp(0.0, 1.0);
        let rain_fade = ((t - 0.85) / 0.15).clamp(0.0, 1.0);

        next.iter_mut().for_each(|c| *c = None);

        for (ci, col) in columns.iter_mut().enumerate() {
            if col.delay > 0 {
                col.delay -= 1;
                continue;
            }
            col.head += col.speed;

            let head_row = col.head.floor() as isize;
            for d in 0..col.len {
                let r = head_row - d as isize;
                if r < 0 || r >= rows as isize {
                    continue;
                }
                let mut level = match d {
                    0 => LVL_HEAD,
                    1..=2 => LVL_BRIGHT,
                    _ if d * 3 < col.len * 2 => LVL_MID,
                    _ => LVL_DIM,
                };
                if rain_fade > 0.0 {
                    if rain_fade >= 1.0 || rng.unit() < rain_fade {
                        continue;
                    }
                    level = LVL_DIM;
                }
                let g = col.glyphs[r as usize % col.glyphs.len()];
                next[r as usize * cols + ci] = Some((g, level));
            }

            if rng.unit() < 0.25 {
                let i = rng.below(col.glyphs.len());
                col.glyphs[i] = alphabet[rng.below(alphabet.len())];
            }

            if col.head - col.len as f32 > rows as f32 {
                *col = new_column(rows, &alphabet, &mut rng, false);
                if !spawning {
                    col.delay = usize::MAX;
                }
            }
        }

        if reveal > 0.0 {
            let cutoff = (logo.len() as f32 * reveal) as usize;
            for &(r, c, ch) in logo.iter().take(cutoff) {
                next[r * cols + c] = Some((ch, LVL_LOGO));
            }
        }

        emit_frame(&next, rows, cols, logo_sgr, &mut frame_buf);

        if start.elapsed() >= total {
            break;
        }
        std::thread::sleep(TICK);
    }

    next.iter_mut().for_each(|c| *c = None);
    for &(r, c, ch) in logo {
        next[r * cols + c] = Some((ch, LVL_LOGO));
    }
    emit_frame(&next, rows, cols, logo_sgr, &mut frame_buf);
}

fn new_column(rows: usize, alphabet: &[char], rng: &mut Rng, initial: bool) -> Column {
    let len = 4 + rng.below((rows / 2).max(2));
    Column {
        head: -(rng.below(6) as f32),
        speed: 0.5 + rng.unit() * 1.2,
        len,
        delay: if initial { rng.below(12) } else { rng.below(6) },
        glyphs: (0..rows.max(1))
            .map(|_| alphabet[rng.below(alphabet.len())])
            .collect(),
    }
}

/// Full-frame redraw from the DECSC home saved at the top of the rain block.
fn emit_frame(
    grid: &[Option<(char, u8)>],
    rows: usize,
    cols: usize,
    logo_sgr: &str,
    buf: &mut String,
) {
    buf.clear();
    buf.push_str("\x1b8"); // DECRC — back to top-left of the reserved block
    for r in 0..rows {
        if r > 0 {
            buf.push('\n');
        }
        let mut level: Option<u8> = None;
        for c in 0..cols {
            match grid[r * cols + c] {
                Some((ch, lvl)) => {
                    if level != Some(lvl) {
                        buf.push_str(paint(lvl, logo_sgr));
                        level = Some(lvl);
                    }
                    buf.push(ch);
                }
                None => {
                    if level.is_some() {
                        buf.push_str("\x1b[0m");
                        level = None;
                    }
                    buf.push(' ');
                }
            }
        }
        buf.push_str("\x1b[K");
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_preserves_newlines() {
        let rows = wrap_text("hi\n\nthere", 80);
        assert_eq!(
            rows,
            vec!["hi".to_string(), String::new(), "there".to_string()]
        );
    }

    #[test]
    fn linebuf_quit_on_ctrl_c() {
        let mut lb = LineBuf::default();
        lb.feed_bytes(&[0x03]);
        assert_eq!(lb.take_latest().as_deref(), Some("quit"));
    }

    #[test]
    fn linebuf_latest_wins() {
        let mut lb = LineBuf::default();
        lb.feed_bytes(b"one\ntwo\n");
        assert_eq!(lb.take_latest().as_deref(), Some("two"));
        assert!(lb.take_latest().is_none());
    }

    #[test]
    fn wrap_long_text_can_fit_terminal() {
        let cols = 40;
        let rows = wrap_text(&"word ".repeat(200), cols);
        assert!(rows.len() > 5);
        let max = 10;
        let kept = rows.len().min(max);
        assert_eq!(kept, max);
    }
}
