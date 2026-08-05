//! Matrix-style green digital rain that resolves into the startup splash.
//!
//! The rain plays *in the primary screen*, inside the exact block of rows the
//! static splash would have occupied, and crystallizes into [`ui::SPLASH_ART`]
//! itself in the same green. When it finishes the art is simply already on
//! screen — `ui::print_splash` skips its own `writeln!` and only adds the
//! trailing text lines, so there is no redraw and no cut.
//!
//! Draws nothing unless stdout is a color-capable terminal, so `--once`,
//! `--script`, pipes and `NO_COLOR=1` are unaffected. Override with
//! `ENGRAM_MATRIX=0|off|false|no` (force off) or `always|1|on|true|yes`
//! (force on); `ENGRAM_MATRIX_SECS` sets the duration (default 2, max 10).
//!
//! Everything is hand-written ANSI — no TUI crate, and no libc: terminal size,
//! raw mode and the cursor-position query all go through BusyBox `stty` and
//! plain escape sequences.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::ui;

const TICK: Duration = Duration::from_millis(50);
const DEFAULT_SECS: f32 = 2.0;
const MAX_SECS: f32 = 10.0;
const MAX_COLS: usize = 300;
const MAX_ROWS: usize = 100;

/// Halfwidth katakana + digits — the iconic look, single-width cells.
const GLYPHS_KANA: &str = "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789";
/// Fallback for fonts that render halfwidth katakana double-width.
const GLYPHS_ASCII: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ<>*+-=/\\|{}[]$#@%&";

/// Whether the rain should play.
pub fn enabled() -> bool {
    match std::env::var("ENGRAM_MATRIX")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "off" | "false" | "no" => false,
        "always" | "1" | "on" | "true" | "yes" => true,
        _ => ui::splash_enabled() && ui::color_enabled(),
    }
}

/// Duration in seconds; 0 disables the animation entirely.
fn duration_secs() -> f32 {
    match std::env::var("ENGRAM_MATRIX_SECS") {
        Ok(v) => v.trim().parse::<f32>().unwrap_or(DEFAULT_SECS).clamp(0.0, MAX_SECS),
        Err(_) => DEFAULT_SECS,
    }
}

fn glyphs() -> Vec<char> {
    let ascii = matches!(std::env::var("ENGRAM_MATRIX_GLYPHS"), Ok(v) if v.eq_ignore_ascii_case("ascii"));
    if ascii { GLYPHS_ASCII } else { GLYPHS_KANA }.chars().collect()
}

// ---------------------------------------------------------------- rng

/// Tiny xorshift64* — enough entropy for falling text, no `rand` dependency.
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

// ---------------------------------------------------------------- stty

fn stty(args: &[&str]) -> Option<String> {
    let out = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Terminal size as `(rows, cols)`, with env then 24x80 as fallbacks.
fn term_size() -> (usize, usize) {
    let parsed = stty(&["size"]).and_then(|s| {
        let mut it = s.split_whitespace();
        let rows = it.next()?.parse::<usize>().ok()?;
        let cols = it.next()?.parse::<usize>().ok()?;
        Some((rows, cols))
    });
    let (rows, cols) = parsed.unwrap_or_else(|| {
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
        (env("LINES").unwrap_or(24), env("COLUMNS").unwrap_or(80))
    });
    (rows.clamp(1, MAX_ROWS), cols.clamp(1, MAX_COLS))
}

/// Puts the terminal in non-blocking raw mode and always restores it on drop.
///
/// `min 0 time 0` is what makes a `read` on stdin return immediately with zero
/// bytes when nothing is typed — that is the whole keypress-to-skip mechanism,
/// with no `fcntl` and therefore no libc dependency. It is also what lets us
/// read the terminal's reply to the cursor-position query.
struct RawGuard {
    saved: Option<String>,
}

impl RawGuard {
    fn enter() -> Self {
        let saved = stty(&["-g"]);
        if saved.is_some() {
            stty(&["raw", "-echo", "min", "0", "time", "0"]);
        }
        RawGuard { saved }
    }

    /// True when raw mode actually took, i.e. keypress polling is meaningful.
    fn active(&self) -> bool {
        self.saved.is_some()
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // Un-hide the cursor and clear any lingering SGR even if we panicked
        // mid-frame, so a crash can never wedge the user's terminal.
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[0m\x1b[?25h");
        let _ = out.flush();
        if let Some(saved) = &self.saved {
            stty(&[saved.as_str()]);
        }
    }
}

/// True if any byte is waiting on stdin. Consumes what it reads (up to 64
/// bytes), so the keystroke that skipped the intro does not leak into the
/// first rustyline prompt.
fn key_pressed() -> bool {
    let mut buf = [0u8; 64];
    matches!(io::stdin().read(&mut buf), Ok(n) if n > 0)
}

/// Current cursor row (1-based) via DSR `ESC [ 6 n`.
///
/// Raw mode is non-blocking, so poll briefly for the `ESC [ row ; col R`
/// reply. `None` means the terminal never answered and the caller should fall
/// back to assuming we are parked at the bottom of the screen.
fn cursor_row() -> Option<usize> {
    {
        let mut out = io::stdout().lock();
        write!(out, "\x1b[6n").ok()?;
        out.flush().ok()?;
    }
    let mut reply = Vec::new();
    for _ in 0..40 {
        let mut buf = [0u8; 32];
        if let Ok(n) = io::stdin().read(&mut buf) {
            reply.extend_from_slice(&buf[..n]);
            if reply.contains(&b'R') {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let text = String::from_utf8_lossy(&reply);
    let body = text.rsplit('[').next()?;
    body.split(';').next()?.trim().parse::<usize>().ok()
}

// ---------------------------------------------------------------- logo mask

/// The splash art as terminal rows. Row 0 is the blank line the art's leading
/// newline produces, so indices line up with what `print_splash` would write.
fn art_rows() -> Vec<&'static str> {
    ui::SPLASH_ART.lines().collect()
}

/// Cells of the splash art as `(row, col, char)`, positioned exactly where the
/// static splash puts them: left-aligned at column 0, in art-line order.
///
/// Columns past the terminal width are dropped — the rain then reveals the
/// visible left portion of the real art rather than inventing a smaller logo.
/// Empty when the art is taller than the region, in which case the caller
/// declines to animate at all.
fn logo_cells(rows: usize, cols: usize) -> Vec<(usize, usize, char)> {
    let lines = art_rows();
    if lines.is_empty() || lines.len() > rows {
        return Vec::new();
    }
    let mut cells = Vec::new();
    for (r, line) in lines.iter().enumerate() {
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

// ---------------------------------------------------------------- animation

/// One falling stream. `head` is fractional so speeds can differ smoothly.
struct Column {
    head: f32,
    speed: f32,
    len: usize,
    delay: usize,
    glyphs: Vec<char>,
}

/// Brightness tiers, mapped to SGR in [`paint`].
const LVL_HEAD: u8 = 0;
const LVL_BRIGHT: u8 = 1;
const LVL_MID: u8 = 2;
const LVL_DIM: u8 = 3;
const LVL_LOGO: u8 = 4;

/// `logo` is the splash accent, so the final frame is byte-identical to what
/// `print_splash` would have written.
fn paint(level: u8, logo: &str) -> &str {
    match level {
        LVL_HEAD => "\x1b[97m",
        LVL_BRIGHT => "\x1b[92m",
        LVL_MID => "\x1b[32m",
        LVL_LOGO => logo,
        _ => "\x1b[2;32m",
    }
}

/// Play the intro, leaving the splash art drawn in place.
///
/// Returns `true` when the art is now on screen (and the cursor is parked
/// exactly where the static `writeln!` would have left it), `false` when the
/// animation was skipped and the caller must print the splash itself. Blocks
/// for up to `ENGRAM_MATRIX_SECS`; returns early on any keypress.
pub fn play(accent: &str) -> bool {
    if !enabled() {
        return false;
    }
    let secs = duration_secs();
    if secs <= 0.0 {
        return false;
    }

    let (rows, cols) = term_size();
    let grid_rows = art_rows().len();
    // grid_rows for the art + one trailing blank line, and we refuse to squeeze
    // the whole splash into a screen that has no room left for the prompt.
    if cols < 20 || grid_rows == 0 || rows < grid_rows + 2 {
        return false;
    }
    if logo_cells(grid_rows, cols).is_empty() {
        return false;
    }

    let guard = RawGuard::enter();

    // Reserve the block by scrolling it into existence, then find out where it
    // actually landed. `top` is the row the art's leading blank line occupies.
    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[?25l");
        for _ in 0..=grid_rows {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
    let bottom = if guard.active() {
        cursor_row().unwrap_or(rows)
    } else {
        rows
    };
    let top = bottom.saturating_sub(grid_rows + 1).max(1);

    let logo_sgr = if accent.is_empty() { "\x1b[0m" } else { accent };
    run_frames(top, grid_rows, cols, secs, logo_sgr, &guard);

    // Park the cursor on the line after the trailing blank, i.e. exactly where
    // `writeln!(out, "{accent}{SPLASH_ART}{r}")` would have left it.
    {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[0m\x1b[{};1H", top + grid_rows + 1);
        let _ = out.flush();
    }
    // `guard` drops here: restores stty and shows the cursor.
    true
}

fn run_frames(top: usize, rows: usize, cols: usize, secs: f32, logo_sgr: &str, guard: &RawGuard) {
    let mut rng = Rng::new();
    let alphabet = glyphs();
    let logo = logo_cells(rows, cols);

    let mut columns: Vec<Column> = (0..cols)
        .map(|_| new_column(rows, &alphabet, &mut rng, true))
        .collect();

    // prev/next grids so each frame only emits the cells that changed.
    let cell_count = rows * cols;
    let mut prev: Vec<Option<(char, u8)>> = vec![None; cell_count];
    let mut next: Vec<Option<(char, u8)>> = vec![None; cell_count];
    let mut frame_buf = String::with_capacity(cell_count * 8);

    let total = Duration::from_secs_f32(secs);
    let start = Instant::now();

    loop {
        let t = (start.elapsed().as_secs_f32() / secs).min(1.0);

        // Three acts: rain, crystallize the logo out of the rain, fade.
        let spawning = t < 0.60;
        let reveal = if logo.is_empty() {
            0.0
        } else {
            ((t - 0.55) / 0.30).clamp(0.0, 1.0)
        };
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
                // Tail dims away as the logo takes over.
                if rain_fade > 0.0 {
                    if rain_fade >= 1.0 || rng.unit() < rain_fade {
                        continue;
                    }
                    level = LVL_DIM;
                }
                let g = col.glyphs[r as usize % col.glyphs.len()];
                next[r as usize * cols + ci] = Some((g, level));
            }

            // Shimmer: occasionally re-roll one glyph in the stream.
            if rng.unit() < 0.25 {
                let i = rng.below(col.glyphs.len());
                col.glyphs[i] = alphabet[rng.below(alphabet.len())];
            }

            if col.head - col.len as f32 > rows as f32 {
                *col = new_column(rows, &alphabet, &mut rng, false);
                if !spawning {
                    col.delay = usize::MAX; // parked for the rest of the run
                }
            }
        }

        // Logo cells win over rain, revealed progressively top-to-bottom.
        if reveal > 0.0 {
            let cutoff = (logo.len() as f32 * reveal) as usize;
            for &(r, c, ch) in logo.iter().take(cutoff) {
                next[r * cols + c] = Some((ch, LVL_LOGO));
            }
        }

        emit_diff(&prev, &next, top, cols, logo_sgr, &mut frame_buf);
        std::mem::swap(&mut prev, &mut next);

        if start.elapsed() >= total {
            break;
        }
        if guard.active() && key_pressed() {
            break;
        }
        std::thread::sleep(TICK);
    }

    // Whether we ran out of time or were skipped mid-reveal, land on the same
    // picture: the complete art, nothing else.
    next.iter_mut().for_each(|c| *c = None);
    for &(r, c, ch) in &logo {
        next[r * cols + c] = Some((ch, LVL_LOGO));
    }
    emit_diff(&prev, &next, top, cols, logo_sgr, &mut frame_buf);
}

/// Streams start just above the top edge and stagger only briefly: the intro is
/// ~2s (40 frames), so a longer runway would leave the screen empty throughout.
fn new_column(rows: usize, alphabet: &[char], rng: &mut Rng, initial: bool) -> Column {
    let len = 4 + rng.below((rows / 2).max(2));
    Column {
        head: -(rng.below(6) as f32),
        speed: 0.5 + rng.unit() * 1.2,
        len,
        delay: if initial { rng.below(12) } else { rng.below(6) },
        glyphs: (0..rows.max(1)).map(|_| alphabet[rng.below(alphabet.len())]).collect(),
    }
}

/// Write only the cells that differ, coalescing cursor moves and SGR changes,
/// then flush once — a single write per frame is what keeps it flicker-free.
/// `top` is the absolute screen row of grid row 0.
fn emit_diff(
    prev: &[Option<(char, u8)>],
    next: &[Option<(char, u8)>],
    top: usize,
    cols: usize,
    logo_sgr: &str,
    buf: &mut String,
) {
    use std::fmt::Write as _;

    buf.clear();
    let mut cursor: Option<(usize, usize)> = None;
    let mut level: Option<u8> = None;

    for (i, (p, n)) in prev.iter().zip(next.iter()).enumerate() {
        if p == n {
            continue;
        }
        let (r, c) = (i / cols, i % cols);
        if cursor != Some((r, c)) {
            let _ = write!(buf, "\x1b[{};{}H", top + r, c + 1);
        }
        match n {
            Some((ch, lvl)) => {
                if level != Some(*lvl) {
                    buf.push_str(paint(*lvl, logo_sgr));
                    level = Some(*lvl);
                }
                buf.push(*ch);
            }
            None => {
                if level.is_some() {
                    buf.push_str("\x1b[0m");
                    level = None;
                }
                buf.push(' ');
            }
        }
        cursor = Some((r, c + 1));
    }

    if buf.is_empty() {
        return;
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// `set_var` is process-global; serialize the env-reading tests.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn env_forces_matrix_off_and_on() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_MATRIX", "off");
        assert!(!enabled());
        std::env::set_var("ENGRAM_MATRIX", "always");
        assert!(enabled());
        std::env::remove_var("ENGRAM_MATRIX");
    }

    #[test]
    fn duration_clamps_and_defaults() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENGRAM_MATRIX_SECS", "99");
        assert_eq!(duration_secs(), MAX_SECS);
        std::env::set_var("ENGRAM_MATRIX_SECS", "junk");
        assert_eq!(duration_secs(), DEFAULT_SECS);
        std::env::set_var("ENGRAM_MATRIX_SECS", "0");
        assert_eq!(duration_secs(), 0.0);
        std::env::remove_var("ENGRAM_MATRIX_SECS");
    }

    #[test]
    fn term_size_is_always_sane() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let (rows, cols) = term_size();
        assert!((1..=MAX_ROWS).contains(&rows));
        assert!((1..=MAX_COLS).contains(&cols));
    }

    #[test]
    fn logo_mask_matches_splash_art_positions() {
        let lines = art_rows();
        let cells = logo_cells(lines.len(), 400);
        assert!(!cells.is_empty());
        for &(r, c, ch) in &cells {
            let expected = lines[r].chars().nth(c).expect("cell inside its art line");
            assert_eq!(ch, expected, "mismatch at row {r} col {c}");
        }
        let non_space: usize = lines
            .iter()
            .map(|l| l.chars().filter(|ch| !ch.is_whitespace()).count())
            .sum();
        assert_eq!(cells.len(), non_space, "every visible art cell is masked");
    }

    #[test]
    fn logo_mask_truncates_to_terminal_width() {
        let cells = logo_cells(art_rows().len(), 40);
        assert!(!cells.is_empty(), "80-column terminals still get the real art");
        assert!(cells.iter().all(|&(_, c, _)| c < 40));
    }

    #[test]
    fn logo_mask_is_empty_when_the_region_is_too_short() {
        assert!(logo_cells(art_rows().len() - 1, 400).is_empty());
        assert!(logo_cells(5, 400).is_empty());
    }

    #[test]
    fn diff_emits_nothing_when_frames_match() {
        let grid = vec![Some(('x', LVL_MID)); 4];
        let mut buf = String::new();
        emit_diff(&grid, &grid, 1, 2, "\x1b[32m", &mut buf);
        assert!(buf.is_empty());
    }
}
