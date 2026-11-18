//! User crontab block bounded by shomloop markers.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::store::PlannedRun;

pub const BEGIN_MARK: &str = "# BEGIN shomloop";
pub const END_MARK: &str = "# END shomloop";

pub fn log_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp/shomloop")
}

pub fn shomloop_exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("/usr/local/bin/shomloop"))
}

pub fn job_lines(runs: &[PlannedRun], exe: &Path) -> Vec<String> {
    let exe_s = cron_quote(&exe.display().to_string());
    runs.iter()
        .filter(|r| r.enabled)
        .map(|r| format!("{} {exe_s} run {}", r.cron, r.id))
        .collect()
}

/// Replace the marked block. An empty `lines` list removes the block.
/// Lines outside the markers are kept.
pub fn splice_block(existing: &str, lines: &[String]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        if line.trim() == BEGIN_MARK {
            skipping = true;
            continue;
        }
        if skipping {
            if line.trim() == END_MARK {
                skipping = false;
            }
            continue;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|l| l.is_empty()) {
        kept.pop();
    }
    let mut out = String::new();
    if !kept.is_empty() {
        out.push_str(&kept.join("\n"));
        out.push('\n');
    }
    if !lines.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(BEGIN_MARK);
        out.push('\n');
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(END_MARK);
        out.push('\n');
    }
    out
}

/// Startup line when enabled loops were written back into crontab.
/// Nothing to say when none were re-enrolled.
pub fn restore_notice(enabled: usize) -> Option<String> {
    if enabled == 0 {
        return None;
    }
    let loops = if enabled == 1 { "loop" } else { "loops" };
    Some(format!(
        "cron re-enrolled, {enabled} {loops}. Use `shomloop list` to find out more."
    ))
}

pub fn sync_crontab(runs: &[PlannedRun]) -> Result<()> {
    let existing = read_crontab()?;
    let lines = job_lines(runs, &shomloop_exe());
    let body = splice_block(&existing, &lines);
    write_crontab(&body)
}

fn read_crontab() -> Result<String> {
    let out = Command::new("crontab")
        .arg("-l")
        .output()
        .context("run crontab -l")?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write_crontab(body: &str) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("shomloop-crontab-{}", std::process::id()));
    fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    let status = Command::new("crontab")
        .arg(&tmp)
        .stdin(Stdio::null())
        .status()
        .context("run crontab")?;
    let _ = fs::remove_file(&tmp);
    if !status.success() {
        bail!("crontab exited {status}");
    }
    Ok(())
}

pub fn ensure_crond(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    if crond_running() {
        return Ok(());
    }
    let status = Command::new("crond")
        .arg("-b")
        .arg("-L")
        .arg(dir.join("crond.log"))
        .status()
        .context("start crond")?;
    if !status.success() {
        bail!("crond exited {status}");
    }
    Ok(())
}

fn crond_running() -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if !s.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = fs::read_to_string(ent.path().join("comm")).unwrap_or_default();
        if comm.trim() == "crond" {
            return true;
        }
    }
    false
}

fn cron_quote(s: &str) -> String {
    if s.chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::PlannedRun;

    fn run(id: u32, enabled: bool, cron: &str) -> PlannedRun {
        PlannedRun {
            id,
            enabled,
            frequency: "daily".into(),
            cron: cron.into(),
            command: "echo hi".into(),
            created_at: "1970-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn splice_preserves_foreign_lines_and_replaces_block() {
        let existing =
            "0 1 * * * /usr/bin/other\n# BEGIN shomloop\n0 0 * * * /old run 1\n# END shomloop\n";
        let lines = vec!["0 17 * * * /usr/local/bin/shomloop run 3".to_string()];
        let out = splice_block(existing, &lines);
        assert!(out.contains("0 1 * * * /usr/bin/other\n"));
        assert!(out.contains(
            "# BEGIN shomloop\n0 17 * * * /usr/local/bin/shomloop run 3\n# END shomloop\n"
        ));
        assert!(!out.contains("/old run"));
    }

    #[test]
    fn empty_jobs_drop_the_block() {
        let existing = "# BEGIN shomloop\n0 0 * * * /bin/shomloop run 1\n# END shomloop\n30 2 * * * /usr/bin/keep\n";
        let out = splice_block(existing, &[]);
        assert_eq!(out, "30 2 * * * /usr/bin/keep\n");
    }

    #[test]
    fn notice_only_when_loops_were_reenrolled() {
        assert_eq!(restore_notice(0), None);
        assert_eq!(
            restore_notice(1).as_deref(),
            Some("cron re-enrolled, 1 loop. Use `shomloop list` to find out more.")
        );
        assert_eq!(
            restore_notice(3).as_deref(),
            Some("cron re-enrolled, 3 loops. Use `shomloop list` to find out more.")
        );
    }

    #[test]
    fn disabled_rows_are_omitted() {
        let runs = vec![run(1, false, "0 0 * * *"), run(2, true, "0 17 * * *")];
        let lines = job_lines(&runs, Path::new("/usr/local/bin/shomloop"));
        assert_eq!(
            lines,
            vec!["0 17 * * * /usr/local/bin/shomloop run 2".to_string()]
        );
    }
}
