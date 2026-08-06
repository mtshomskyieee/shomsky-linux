use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Run BATS against a test file. Returns Ok(()) on success, Err with stderr/stdout on fail.
pub fn run_bats(bats_file: &Path) -> Result<()> {
    let bats = find_bats().context("bats not found (install bats-core)")?;
    let output = Command::new(&bats)
        .arg(bats_file)
        .output()
        .with_context(|| format!("running {} {}", bats.display(), bats_file.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "bats failed (exit {}):\n{stdout}{stderr}",
        output.status.code().unwrap_or(-1)
    );
}

fn find_bats() -> Option<std::path::PathBuf> {
    for cand in ["/usr/bin/bats", "/usr/local/bin/bats", "/bin/bats"] {
        let p = std::path::PathBuf::from(cand);
        if p.is_file() {
            return Some(p);
        }
    }
    which("bats")
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}
