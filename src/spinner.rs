//! TTY-gated ASCII spinner for slow LLM / tool waits.
//!
//! Draws nothing when stderr is not a terminal (tests, pipes, `--once`), so
//! captured output stays clean. Override with `ENGRAM_SPINNER=0|off|false`
//! (force off) or `ENGRAM_SPINNER=always` (force on).

use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

const FRAMES: &[char] = &['-', '\\', '|', '/'];
const WARMUP: Duration = Duration::from_millis(200);
const TICK: Duration = Duration::from_millis(120);
const BAR_WIDTH: usize = 24;

/// Whether the spinner should draw on stderr.
pub fn enabled() -> bool {
    match std::env::var("ENGRAM_SPINNER")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "off" | "false" | "no" => false,
        "always" | "1" | "on" | "true" | "yes" => true,
        _ => io::stderr().is_terminal(),
    }
}

/// Run `fut` while showing an animated status line on stderr (when enabled).
/// Fast operations (< ~200ms) draw nothing. On completion the line is erased.
pub async fn with_spinner<F: Future>(label: &str, fut: F) -> F::Output {
    if !enabled() {
        return fut.await;
    }

    let mut fut = std::pin::pin!(fut);
    let start = Instant::now();
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + WARMUP, TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut frame = 0usize;
    let mut drew = false;

    let out = loop {
        tokio::select! {
            biased;
            v = &mut fut => break v,
            _ = ticker.tick() => {
                draw_spinner(label, frame, start.elapsed());
                drew = true;
                frame = frame.wrapping_add(1);
            }
        }
    };

    if drew {
        erase_line();
    }
    out
}

/// Run `fut` with a spinner + progress bar (for model warm-up).
/// Draws immediately when enabled (no warmup delay). Erases the line when done.
/// When the spinner is disabled, still awaits `fut` with no drawing.
pub async fn with_progress_bar<F: Future>(label: &str, max_secs: u64, fut: F) -> F::Output {
    if !enabled() {
        return fut.await;
    }

    let mut fut = std::pin::pin!(fut);
    let start = Instant::now();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut frame = 0usize;
    let max_secs = max_secs.max(1);

    // First frame immediately so the splash→bar transition feels alive.
    draw_bar(label, frame, start.elapsed(), max_secs);
    frame = frame.wrapping_add(1);

    let out = loop {
        tokio::select! {
            biased;
            v = &mut fut => break v,
            _ = ticker.tick() => {
                draw_bar(label, frame, start.elapsed(), max_secs);
                frame = frame.wrapping_add(1);
            }
        }
    };

    erase_line();
    out
}

fn use_ansi() -> bool {
    if matches!(std::env::var("NO_COLOR"), Ok(v) if !v.is_empty()) {
        return false;
    }
    matches!(std::env::var("TERM"), Ok(t) if !t.is_empty() && t != "dumb")
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn draw_spinner(label: &str, frame: usize, elapsed: Duration) {
    let glyph = FRAMES[frame % FRAMES.len()];
    let msg = format!(
        "[{glyph}] {label}... {}",
        format_elapsed(elapsed)
    );
    paint_line(&msg);
}

fn draw_bar(label: &str, frame: usize, elapsed: Duration, max_secs: u64) {
    let glyph = FRAMES[frame % FRAMES.len()];
    let secs = elapsed.as_secs().min(max_secs);
    // Soft asymptotic fill so a long wait never looks "stuck" at empty,
    // and finishing early still looks mostly full.
    let ratio = (secs as f64 / max_secs as f64).clamp(0.0, 0.95);
    let filled = ((ratio * BAR_WIDTH as f64).round() as usize).min(BAR_WIDTH);
    // Pulse the leading edge with the spinner so the bar feels alive.
    let mut bar = String::with_capacity(BAR_WIDTH);
    for i in 0..BAR_WIDTH {
        if i < filled {
            bar.push('█');
        } else if i == filled {
            bar.push(match glyph {
                '-' => '▓',
                '\\' => '▒',
                '|' => '░',
                _ => '▒',
            });
        } else {
            bar.push('░');
        }
    }
    let pct = ((ratio * 100.0).round() as u64).min(99);
    let color = if use_ansi() { "\x1b[32m" } else { "" };
    let reset = if use_ansi() { "\x1b[0m" } else { "" };
    let msg = format!(
        "  [{glyph}] {label} {color}{bar}{reset} {pct:>2}%  {}",
        format_elapsed(elapsed)
    );
    paint_line(&msg);
}

fn paint_line(msg: &str) {
    let mut err = io::stderr().lock();
    if use_ansi() {
        let _ = write!(err, "\r\x1b[2K{msg}");
    } else {
        // Overwrite with spaces so the previous longer frame does not linger.
        let _ = write!(err, "\r{msg}                    ");
    }
    let _ = err.flush();
}

fn erase_line() {
    let mut err = io::stderr().lock();
    if use_ansi() {
        let _ = write!(err, "\r\x1b[2K");
    } else {
        let _ = write!(err, "\r                                        \r");
    }
    let _ = err.flush();
}
