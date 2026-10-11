use anyhow::{bail, Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use crate::paths::LearnPaths;

/// Artifacts produced by the promote LLM (or stub).
#[derive(Debug, Clone)]
pub struct PromoteArtifacts {
    pub script: String,
    pub man: String,
    pub bats: String,
    pub description: String,
    pub flags: Vec<String>,
    /// Bash completion word list (flags + maybe --help).
    pub completion_words: Vec<String>,
}

/// Write artifacts into a temporary workdir for BATS, then optionally install.
pub fn write_workdir(
    work: &std::path::Path,
    name: &str,
    artifacts: &PromoteArtifacts,
) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(work).context("mkdir promote workdir")?;
    let script_path = work.join(name);
    let bats_path = work.join(format!("{name}.bats"));
    std::fs::write(&script_path, ensure_shebang(&artifacts.script))
        .with_context(|| format!("write {}", script_path.display()))?;
    let mut perms = std::fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms)?;
    // Make script discoverable as `name` on PATH for bats.
    // Drop anything before the first @test. Small models emit a preamble such as
    // "BATS test file ...", which bats sources and tries to execute.
    let bats_body = sanitize_bats(name, &artifacts.bats)?.replace(
        &format!("$BATS_TEST_DIRNAME/{name}"),
        &script_path.display().to_string(),
    );
    // Also export PATH so bare `name` works if tests call it that way.
    let bats_wrapped = if bats_body.contains("setup()") {
        bats_body
    } else {
        format!(
            "setup() {{\n  export PATH=\"{}:$PATH\"\n}}\n\n{bats_body}",
            work.display()
        )
    };
    std::fs::write(&bats_path, bats_wrapped)
        .with_context(|| format!("write {}", bats_path.display()))?;
    Ok((script_path, bats_path))
}

/// Keep the bats suite and discard a leading prose preamble.
/// When the model emitted no `@test`, use one help-exit test so promotion
/// still checks the script instead of stopping on the missing marker.
pub fn sanitize_bats(name: &str, bats: &str) -> Result<String> {
    if let Some(i) = bats.find("@test") {
        return Ok(bats[i..].to_string());
    }
    Ok(format!(
        "@test \"{name} help exits 0\" {{\n  \
         run {name} --help\n  \
         if [ \"$status\" -ne 0 ]; then\n    \
         run {name} -h\n  \
         fi\n  \
         [ \"$status\" -eq 0 ]\n\
         }}\n"
    ))
}

fn ensure_shebang(script: &str) -> String {
    let trimmed = script.trim_start();
    if trimmed.starts_with("#!") {
        if !script.ends_with('\n') {
            return format!("{script}\n");
        }
        return script.to_string();
    }
    format!("#!/bin/sh\n{script}\n")
}

pub fn install_artifacts(
    paths: &LearnPaths,
    name: &str,
    intent: &str,
    command: &str,
    artifacts: &PromoteArtifacts,
) -> Result<()> {
    crate::name::validate_name(name)?;
    paths.ensure_dirs().context("creating learn dirs")?;

    let script_path = paths.command_path(name);
    let man_path = paths.man_path(name);
    let bats_path = paths.bats_path(name);
    let completion_path = paths.completion_path(name);

    std::fs::write(&script_path, ensure_shebang(&artifacts.script))
        .with_context(|| format!("install script {}", script_path.display()))?;
    let mut perms = std::fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms)?;

    std::fs::write(&man_path, &artifacts.man)
        .with_context(|| format!("install man {}", man_path.display()))?;
    std::fs::write(&bats_path, sanitize_bats(name, &artifacts.bats)?)
        .with_context(|| format!("install bats {}", bats_path.display()))?;

    let words = if artifacts.completion_words.is_empty() {
        let mut w = artifacts.flags.clone();
        w.push("--help".into());
        w
    } else {
        artifacts.completion_words.clone()
    };
    let completion = format!(
        "# shomsky learn completion for {name}\ncomplete -W '{}' {name}\n",
        words.join(" ")
    );
    std::fs::write(&completion_path, completion)
        .with_context(|| format!("install completion {}", completion_path.display()))?;

    let entry = crate::registry::LearnEntry {
        name: name.to_string(),
        description: artifacts.description.clone(),
        version: "0.1.0".into(),
        intent: intent.to_string(),
        command: command.to_string(),
        flags: artifacts.flags.clone(),
        script_path: script_path.display().to_string(),
        man_path: man_path.display().to_string(),
        bats_path: bats_path.display().to_string(),
        completion_path: completion_path.display().to_string(),
    };
    crate::registry::save_entry(paths, &entry)?;
    Ok(())
}

/// Prefer `abuild` when present; otherwise build a minimal unsigned apk via tar.
pub fn prefer_abuild() -> bool {
    Command::new("abuild")
        .arg("-h")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}

pub fn refuse_overwrite(paths: &LearnPaths, name: &str, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    if paths.command_path(name).exists() {
        bail!("command '{name}' already exists at {} (use force to overwrite)", 
            paths.command_path(name).display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("shomsky-inst-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn installs_layout() {
        let home = tmp();
        let paths = LearnPaths::new(&home);
        let art = PromoteArtifacts {
            script: "#!/bin/sh\necho hi\n".into(),
            man: ".TH FOO 1\n.SH NAME\nfoo \\- test\n".into(),
            bats: "@test \"runs\" { run foo; [ \"$status\" -eq 0 ]; }\n".into(),
            description: "test cmd".into(),
            flags: vec!["--help".into()],
            completion_words: vec!["--help".into()],
        };
        install_artifacts(&paths, "foo", "say hi", "echo hi", &art).unwrap();
        assert!(paths.command_path("foo").is_file());
        assert!(paths.man_path("foo").is_file());
        assert!(paths.registry_path("foo").is_file());
        let installed = std::fs::read_to_string(paths.bats_path("foo")).unwrap();
        assert!(installed.starts_with("@test"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn drops_bats_preamble() {
        let body = sanitize_bats(
            "list-files-largest",
            "BATS test file that exercises --help\n\n@test \"help\" {\n  true\n}\n",
        )
        .unwrap();
        assert!(body.starts_with("@test"));
        assert!(!body.contains("BATS test"));
    }

    #[test]
    fn supplies_help_test_when_missing() {
        let body = sanitize_bats("github-size", "not a test file\n").unwrap();
        assert!(body.starts_with("@test"));
        assert!(body.contains("run github-size --help"));
        assert!(body.contains("run github-size -h"));
    }
}
