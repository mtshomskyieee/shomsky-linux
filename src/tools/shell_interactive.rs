//! Interactive (TTY-inherited) shell execution for TUI / ncurses programs.

use anyhow::{Context, Result};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use super::shell_exec::{read_captured_cwd, shell_single_quote, ShellResult};

/// Run a command with stdin/stdout/stderr inherited (for ncurses / TUI apps).
/// Still records final cwd via an EXIT trap file (no stdout marker).
pub async fn shell_exec_interactive(
    busybox: &Path,
    command: &str,
    cwd: &Path,
    home: &Path,
) -> Result<ShellResult> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(ShellResult {
            output: format!(
                "error: '{command}' needs an interactive TTY (not available under --once/--script)"
            ),
            cwd: cwd.to_path_buf(),
            success: false,
        });
    }

    let cwd_file = std::env::temp_dir().join(format!(
        "engram-cwd-{}",
        uuid::Uuid::new_v4().as_simple()
    ));
    let wrapped = format!(
        "trap 'st=$?; pwd > {}; exit $st' EXIT\n{}",
        shell_single_quote(&cwd_file.to_string_lossy()),
        command
    );

    let mut cmd = if busybox.exists() {
        let mut c = Command::new(busybox);
        c.args(["ash", "-c", &wrapped]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &wrapped]);
        c
    };
    cmd.current_dir(cwd)
        .env("HOME", home)
        .env("PWD", cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Ok(oldpwd) = std::env::var("OLDPWD") {
        cmd.env("OLDPWD", oldpwd);
    }

    // Leave a blank line so the TUI does not collide with the prompt.
    let _ = writeln!(std::io::stderr());

    let status = cmd
        .status()
        .await
        .with_context(|| {
            if busybox.exists() {
                format!("interactive exec via {}", busybox.display())
            } else {
                "interactive exec via sh".into()
            }
        })?;

    let new_cwd = read_captured_cwd(&cwd_file, cwd).await;
    let _ = tokio::fs::remove_file(&cwd_file).await;

    let mut out = String::new();
    if !status.success() {
        out.push_str(&format!(
            "[exit {}]",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(ShellResult {
        output: out,
        cwd: new_cwd,
        success: status.success(),
    })
}

/// Basename of the command that would run (`/usr/bin/top` → `top`,
/// `busybox top` → `top`).
pub fn primary_command_name(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with(':') {
        return None;
    }
    // Pipelines / compounds: still match the first command (e.g. `top`).
    let first_seg = t
        .split(['|', ';'])
        .next()
        .unwrap_or(t)
        .split("&&")
        .next()
        .unwrap_or(t)
        .split("||")
        .next()
        .unwrap_or(t)
        .trim();
    let mut words = first_seg.split_whitespace();
    let first = words.next()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    if base == "busybox" {
        let second = words.next()?;
        return Some(second.rsplit('/').next().unwrap_or(second));
    }
    Some(base)
}

pub fn is_ignore_llm_command(line: &str, commands: &std::collections::BTreeSet<String>) -> bool {
    primary_command_name(line).is_some_and(|name| commands.contains(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn primary_name_basics() {
        assert_eq!(primary_command_name("top"), Some("top"));
        assert_eq!(primary_command_name("  /usr/bin/mc -a "), Some("mc"));
        assert_eq!(primary_command_name("busybox top"), Some("top"));
        assert_eq!(primary_command_name("vim /tmp/x"), Some("vim"));
        assert!(primary_command_name("").is_none());
    }

    #[test]
    fn ignore_set_match() {
        let mut set = BTreeSet::new();
        set.insert("top".into());
        set.insert("mc".into());
        assert!(is_ignore_llm_command("top", &set));
        assert!(is_ignore_llm_command("mc /tmp", &set));
        assert!(is_ignore_llm_command("busybox top -d 1", &set));
        assert!(!is_ignore_llm_command("ls", &set));
        assert!(!is_ignore_llm_command("echo top", &set));
    }
}
