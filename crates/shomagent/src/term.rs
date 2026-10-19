//! Shared TTY helpers for dream rain (BusyBox `stty`, no libc).

use std::process::{Command, Stdio};

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
    (rows.clamp(1, MAX_ROWS), cols.clamp(1, MAX_COLS))
}
