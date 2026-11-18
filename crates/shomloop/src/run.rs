//! Execute one planned run and append a stamped block to its log.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::store::{self, PlannedRun};

pub fn run_job(id: u32, ledger: &Path, log_dir: &Path) -> Result<i32> {
    let runs = store::load(ledger)?;
    let Some(run) = runs.into_iter().find(|r| r.id == id) else {
        bail!("no planned run {id}");
    };
    fs::create_dir_all(log_dir).with_context(|| format!("mkdir {}", log_dir.display()))?;
    let log_path = log_dir.join(format!("{id}.log"));
    if !run.enabled {
        append_block(
            &log_path,
            &run,
            &store::now_stamp(),
            b"",
            b"",
            0,
            Some("skipped: disabled"),
        )?;
        return Ok(0);
    }
    let start = store::now_stamp();
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&run.command);
    with_local_path(&mut cmd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd
        .output()
        .with_context(|| format!("run planned job {id}"))?;
    let code = output.status.code().unwrap_or(1);
    append_block(
        &log_path,
        &run,
        &start,
        &output.stdout,
        &output.stderr,
        code,
        None,
    )?;
    Ok(code)
}

fn with_local_path(cmd: &mut Command) {
    let path = std::env::var("PATH").unwrap_or_default();
    if path.split(':').any(|p| p == "/usr/local/bin") {
        return;
    }
    let merged = if path.is_empty() {
        "/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()
    } else {
        format!("/usr/local/bin:{path}")
    };
    cmd.env("PATH", merged);
}

fn append_block(
    path: &Path,
    run: &PlannedRun,
    start: &str,
    stdout: &[u8],
    stderr: &[u8],
    code: i32,
    note: Option<&str>,
) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "--- start {start} ---")?;
    writeln!(f, "command: {}", run.command)?;
    if let Some(note) = note {
        writeln!(f, "{note}")?;
    }
    writeln!(f, "stdout:")?;
    f.write_all(stdout)?;
    if !stdout.is_empty() && !stdout.ends_with(b"\n") {
        writeln!(f)?;
    }
    writeln!(f, "stderr:")?;
    f.write_all(stderr)?;
    if !stderr.is_empty() && !stderr.ends_with(b"\n") {
        writeln!(f)?;
    }
    writeln!(f, "exit: {code}")?;
    Ok(())
}

/// All job logs, or one id. `crond.log` is not a job log.
pub fn format_logs(dir: &Path, runs: &[PlannedRun], only: Option<u32>) -> Result<String> {
    if !dir.exists() {
        return Ok(format!("no logs in {}\n", dir.display()));
    }
    let mut ids: Vec<u32> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .flatten()
        .filter_map(|ent| {
            let name = ent.file_name();
            let s = name.to_string_lossy();
            let stem = s.strip_suffix(".log")?;
            stem.parse::<u32>().ok()
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if let Some(id) = only {
        ids.retain(|i| *i == id);
        if ids.is_empty() {
            return Ok(format!("no log yet for #{id}\n"));
        }
    }
    if ids.is_empty() {
        return Ok(format!("no logs in {}\n", dir.display()));
    }
    let mut out = String::new();
    for id in ids {
        let cmd = runs
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.command.as_str())
            .unwrap_or("(unknown command)");
        out.push_str(&format!("===== #{id} {cmd} =====\n"));
        let body = fs::read_to_string(dir.join(format!("{id}.log")))
            .with_context(|| format!("read log {id}"))?;
        out.push_str(&body);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{save, PlannedRun};

    fn sample(id: u32, enabled: bool, command: &str) -> PlannedRun {
        PlannedRun {
            id,
            enabled,
            frequency: "hourly".into(),
            cron: "0 * * * *".into(),
            command: command.into(),
            created_at: "1970-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn run_appends_stdout_and_exit() {
        let dir = std::env::temp_dir().join(format!("shomloop-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("planned_runs.csv");
        let logs = dir.join("logs");
        save(&ledger, &[sample(4, true, "echo hello-shomloop")]).unwrap();
        let code = run_job(4, &ledger, &logs).unwrap();
        assert_eq!(code, 0);
        let body = fs::read_to_string(logs.join("4.log")).unwrap();
        assert!(body.contains("command: echo hello-shomloop"));
        assert!(body.contains("hello-shomloop"));
        assert!(body.contains("exit: 0"));
        let shown = format_logs(&logs, &[sample(4, true, "echo hello-shomloop")], None).unwrap();
        assert!(shown.contains("===== #4 echo hello-shomloop ====="));
        assert!(shown.contains("hello-shomloop"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_skips_the_command() {
        let dir = std::env::temp_dir().join(format!("shomloop-skip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("planned_runs.csv");
        let logs = dir.join("logs");
        save(&ledger, &[sample(1, false, "echo should-not-run")]).unwrap();
        let code = run_job(1, &ledger, &logs).unwrap();
        assert_eq!(code, 0);
        let body = fs::read_to_string(logs.join("1.log")).unwrap();
        assert!(body.contains("skipped: disabled"));
        assert!(
            !body.contains("should-not-run\n") || body.contains("command: echo should-not-run")
        );
        assert!(!body.contains("stdout:\nshould-not-run"));
        let _ = fs::remove_dir_all(&dir);
    }
}
