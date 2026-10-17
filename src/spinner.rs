//! TTY-gated ASCII spinner for slow LLM / tool waits.
//!
//! Draws nothing when stderr is not a terminal (tests, pipes, `--once`), so
//! captured output stays clean. Override with `ENGRAM_SPINNER=0|off|false`
//! (force off) or `ENGRAM_SPINNER=always` (force on).

use std::future::Future;
use std::io::{self, IsTerminal, Read, Write};
use std::time::{Duration, Instant};

use crate::term::RawGuard;

const FRAMES: &[char] = &['-', '\\', '|', '/'];
const WARMUP: Duration = Duration::from_millis(200);
const TICK: Duration = Duration::from_millis(120);
const BAR_WIDTH: usize = 24;
const KEY_TICK: Duration = Duration::from_millis(30);
/// Lone Esc is also the prefix of arrow keys. Wait this long for a follow-up
/// byte before treating it as a real cancel.
const ESC_GRACE: Duration = Duration::from_millis(40);

/// Why a wait was cancelled from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelKey {
    Esc,
    /// Ctrl-C arrives as a byte once the tty is in raw mode.
    CtrlC,
}

impl CancelKey {
    pub fn label(self) -> &'static str {
        match self {
            CancelKey::Esc => "esc",
            CancelKey::CtrlC => "^C",
        }
    }
}

/// Result of a wait that can be abandoned from the keyboard.
pub enum Wait<T> {
    Done(T),
    Cancelled(CancelKey),
}

/// Line printed after Esc / Ctrl-C actually stops a wait.
pub fn cancel_notice(key: CancelKey, what: &str) -> String {
    format!("{} — cancelled {what}", key.label())
}

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
                draw_spinner(label, frame, start.elapsed(), "");
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

/// Like [`with_spinner`], but Esc (or Ctrl-C) drops `fut` and returns
/// [`Wait::Cancelled`].
///
/// Listens only when stdin is a terminal and raw mode can be entered. Pipes,
/// `--once`, and tests just await `fut`. Dropping `fut` is what aborts an
/// LLM request or a `kill_on_drop` child.
pub async fn with_spinner_cancel<F: Future>(label: &str, fut: F) -> Wait<F::Output> {
    if !io::stdin().is_terminal() {
        return Wait::Done(fut.await);
    }
    let guard = RawGuard::enter();
    if !guard.active() {
        drop(guard);
        return Wait::Done(fut.await);
    }

    let draw = enabled();
    let mut fut = std::pin::pin!(fut);
    let start = Instant::now();
    let mut frame = 0usize;
    let mut drew = false;
    let mut watch = EscWatch::default();
    let mut keys = tokio::time::interval(KEY_TICK);
    keys.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + WARMUP, TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let outcome = loop {
        tokio::select! {
            biased;
            v = &mut fut => break Wait::Done(v),
            _ = keys.tick() => {
                if let Some(key) = watch.poll() {
                    break Wait::Cancelled(key);
                }
            }
            _ = ticker.tick(), if draw => {
                draw_spinner(label, frame, start.elapsed(), "  · esc cancels");
                drew = true;
                frame = frame.wrapping_add(1);
            }
        }
    };

    // Drop the work before restoring the tty so a killed child is signalled
    // and the acknowledgement line is written in cooked mode.
    drop(fut);
    if drew {
        erase_line();
    }
    drop(guard);
    outcome
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

fn draw_spinner(label: &str, frame: usize, elapsed: Duration, hint: &str) {
    let glyph = FRAMES[frame % FRAMES.len()];
    let msg = format!(
        "[{glyph}] {label}... {}{hint}",
        format_elapsed(elapsed)
    );
    paint_line(&msg);
}

#[derive(Debug, PartialEq, Eq)]
enum KeyHit {
    None,
    Cancel(CancelKey),
    Other,
    /// Buffer ended on a bare Esc; a CSI sequence may still be arriving.
    PendingEsc,
}

fn classify_keys(buf: &[u8]) -> KeyHit {
    if buf.is_empty() {
        return KeyHit::None;
    }
    let mut i = 0;
    let mut pending = false;
    let mut other = false;
    while i < buf.len() {
        match buf[i] {
            0x03 => return KeyHit::Cancel(CancelKey::CtrlC),
            0x1b => match buf.get(i + 1) {
                None => {
                    pending = true;
                    i += 1;
                }
                Some(b'[') | Some(b'O') => {
                    i += 2;
                    while i < buf.len() && !(0x40..=0x7e).contains(&buf[i]) {
                        i += 1;
                    }
                    if i < buf.len() {
                        i += 1;
                    }
                    other = true;
                }
                Some(0x1b) => return KeyHit::Cancel(CancelKey::Esc),
                Some(_) => {
                    i += 2;
                    other = true;
                }
            },
            _ => {
                i += 1;
                other = true;
            }
        }
    }
    if pending {
        KeyHit::PendingEsc
    } else if other {
        KeyHit::Other
    } else {
        KeyHit::None
    }
}

#[derive(Default)]
struct EscWatch {
    pending_since: Option<Instant>,
}

impl EscWatch {
    fn poll(&mut self) -> Option<CancelKey> {
        let mut buf = [0u8; 64];
        let n = match io::stdin().read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return None,
            Err(_) => 0,
        };
        if n == 0 {
            if self
                .pending_since
                .is_some_and(|t| t.elapsed() >= ESC_GRACE)
            {
                self.pending_since = None;
                return Some(CancelKey::Esc);
            }
            return None;
        }
        let mut bytes = Vec::with_capacity(n + 1);
        if self.pending_since.take().is_some() {
            bytes.push(0x1b);
        }
        bytes.extend_from_slice(&buf[..n]);
        match classify_keys(&bytes) {
            KeyHit::Cancel(key) => Some(key),
            KeyHit::PendingEsc => {
                self.pending_since = Some(Instant::now());
                None
            }
            KeyHit::None | KeyHit::Other => None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_esc_is_pending_until_grace() {
        assert_eq!(classify_keys(&[0x1b]), KeyHit::PendingEsc);
    }

    #[test]
    fn arrow_keys_are_not_cancel() {
        assert_eq!(classify_keys(&[0x1b, b'[', b'A']), KeyHit::Other);
        assert_eq!(classify_keys(&[0x1b, b'O', b'B']), KeyHit::Other);
    }

    #[test]
    fn ctrl_c_and_double_esc_cancel() {
        assert_eq!(classify_keys(&[0x03]), KeyHit::Cancel(CancelKey::CtrlC));
        assert_eq!(classify_keys(&[0x1b, 0x1b]), KeyHit::Cancel(CancelKey::Esc));
    }

    #[test]
    fn cancel_notice_names_the_key() {
        assert_eq!(
            cancel_notice(CancelKey::Esc, "thinking"),
            "esc — cancelled thinking"
        );
        assert_eq!(
            cancel_notice(CancelKey::Esc, "shell_exec: git add ."),
            "esc — cancelled shell_exec: git add ."
        );
        assert_eq!(
            cancel_notice(CancelKey::CtrlC, "thinking"),
            "^C — cancelled thinking"
        );
    }
}
