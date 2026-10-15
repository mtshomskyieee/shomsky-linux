//! Shared TTY helpers for dream rain / stdin (BusyBox `stty`, no libc).

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const MAX_COLS: usize = 300;
pub const MAX_ROWS: usize = 100;

pub fn stty(args: &[&str]) -> Option<String> {
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
pub fn term_size() -> (usize, usize) {
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
    (
        rows.clamp(1, MAX_ROWS),
        cols.clamp(1, MAX_COLS),
    )
}

/// Puts the terminal in non-blocking raw mode and always restores it on drop.
pub struct RawGuard {
    saved: Option<String>,
}

impl RawGuard {
    pub fn enter() -> Self {
        let saved = stty(&["-g"]);
        if saved.is_some() {
            stty(&["raw", "-echo", "min", "0", "time", "0"]);
        }
        RawGuard { saved }
    }

    pub fn active(&self) -> bool {
        self.saved.is_some()
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = write!(out, "\x1b[0m\x1b[?25h");
        let _ = out.flush();
        if let Some(saved) = &self.saved {
            stty(&[saved.as_str()]);
        }
    }
}

/// Read available stdin bytes (non-blocking when raw min=0).
pub fn read_available(buf: &mut [u8]) -> usize {
    match io::stdin().read(buf) {
        Ok(n) => n,
        Err(_) => 0,
    }
}

/// Current cursor row (1-based) via DSR `ESC [ 6 n`.
pub fn cursor_row() -> Option<usize> {
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
