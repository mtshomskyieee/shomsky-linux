use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Result of a BusyBox ash invocation, including the directory left after the
/// command (so `cd` persists across REPL turns).
#[derive(Debug, Clone)]
pub struct ShellResult {
    pub output: String,
    pub cwd: PathBuf,
}

pub async fn shell_exec(
    busybox: &Path,
    command: &str,
    cwd: &Path,
    home: &Path,
) -> Result<ShellResult> {
    let cwd_file = std::env::temp_dir().join(format!(
        "engram-cwd-{}",
        uuid::Uuid::new_v4().as_simple()
    ));
    // EXIT trap always records pwd (survives `set -e`) and restores the
    // command's exit status. Parent REPL then adopts that directory.
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
        // Fallback for non-BusyBox hosts (dev / CI outside the image).
        let mut c = Command::new("sh");
        c.args(["-c", &wrapped]);
        c
    };
    cmd.current_dir(cwd);
    // Mode-specific HOME so `cd ~` / bare `cd` land correctly.
    cmd.env("HOME", home);
    if let Ok(oldpwd) = std::env::var("OLDPWD") {
        cmd.env("OLDPWD", oldpwd);
    }
    cmd.env("PWD", cwd);

    let output = cmd
        .output()
        .await
        .with_context(|| {
            if busybox.exists() {
                format!("executing via {}", busybox.display())
            } else {
                "executing via sh".into()
            }
        })?;

    let new_cwd = read_captured_cwd(&cwd_file, cwd).await;
    let _ = tokio::fs::remove_file(&cwd_file).await;

    let mut out = String::new();
    if !output.stdout.is_empty() {
        out.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "[exit {}]",
            output.status.code().unwrap_or(-1)
        ));
    }
    if out.is_empty() {
        out.push_str("(no output)");
    }
    Ok(ShellResult {
        output: out,
        cwd: new_cwd,
    })
}

pub(crate) async fn read_captured_cwd(cwd_file: &Path, fallback: &Path) -> PathBuf {
    match tokio::fs::read_to_string(cwd_file).await {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                fallback.to_path_buf()
            } else {
                PathBuf::from(trimmed)
            }
        }
        Err(_) => fallback.to_path_buf(),
    }
}

pub(crate) fn shell_single_quote(s: &str) -> String {
    // POSIX single-quote escaping: 'foo'bar' → 'foo'\''bar'
    let mut out = String::from("'");
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(prefix: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("engram-shell-test-{prefix}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn cd_persists_in_result_cwd() {
        let base = unique_dir("cd");
        let target = base.join("sub");
        std::fs::create_dir_all(&target).unwrap();
        let home = unique_dir("home");

        let busybox = Path::new("/bin/busybox");
        let r = shell_exec(
            busybox,
            &format!("cd {}", target.display()),
            &base,
            &home,
        )
        .await
        .unwrap();
        assert_eq!(r.cwd, target);

        let r2 = shell_exec(busybox, "pwd", &r.cwd, &home).await.unwrap();
        assert!(
            r2.output.contains(target.to_string_lossy().as_ref()),
            "pwd output: {}",
            r2.output
        );
        assert_eq!(r2.cwd, target);
    }

    #[tokio::test]
    async fn failed_cd_keeps_cwd() {
        let base = unique_dir("cdfail");
        let home = unique_dir("home");
        let busybox = Path::new("/bin/busybox");
        let r = shell_exec(busybox, "cd /no/such/engram/dir", &base, &home)
            .await
            .unwrap();
        assert_eq!(r.cwd, base);
        assert!(
            r.output.contains("exit") || r.output.contains("can't cd") || r.output.contains("No such"),
            "expected failure output: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn compound_cd_and_ls_updates_cwd() {
        let base = unique_dir("compound");
        let target = base.join("here");
        let home = unique_dir("home");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("marker.txt"), "x").unwrap();

        let busybox = Path::new("/bin/busybox");
        let r = shell_exec(
            busybox,
            &format!("cd {} && ls", target.display()),
            &base,
            &home,
        )
        .await
        .unwrap();
        assert!(r.output.contains("marker.txt"), "ls output: {}", r.output);
        assert_eq!(r.cwd, target);
    }

    #[tokio::test]
    async fn cd_tilde_uses_home_env() {
        let base = unique_dir("tilde-base");
        let home = unique_dir("tilde-home");
        std::fs::write(home.join("marker.txt"), "x").unwrap();

        let busybox = Path::new("/bin/busybox");
        let r = shell_exec(busybox, "cd ~", &base, &home).await.unwrap();
        assert_eq!(r.cwd, home);

        let r2 = shell_exec(busybox, "cd && pwd", &base, &home)
            .await
            .unwrap();
        assert_eq!(r2.cwd, home);
        assert!(
            r2.output.contains(home.to_string_lossy().as_ref()),
            "bare cd pwd: {}",
            r2.output
        );
    }
}
