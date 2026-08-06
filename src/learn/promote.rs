use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use shomsky::{
    install_artifacts, refuse_overwrite, run_bats, validate_name, write_workdir, LearnPaths,
    PromoteArtifacts,
};

use crate::llm::{ChatMessage, LlmClient};
use crate::learn::learn_stub;

/// State kept after a successful learn run so `s` can promote.
#[derive(Debug, Clone)]
pub struct PendingPromote {
    pub intent: String,
    pub command: String,
    pub output_summary: String,
    pub suggested_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromoteJson {
    script: String,
    man: String,
    bats: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    completion_words: Vec<String>,
}

const PROMOTE_SYSTEM: &str = "\
You crystallize a shell one-liner into a reusable POSIX sh command for Alpine/BusyBox.
Output ONLY a JSON object with keys:
  \"script\": full file body starting with #!/bin/sh; use getopts for inferred flags;
             include a usage() and --help
  \"man\": troff man page for section 1
  \"bats\": BATS test file that exercises --help and one happy path
  \"description\": one-line package description
  \"flags\": array of flag strings like \"--days\", \"--count\"
  \"completion_words\": array of completion words (flags + --help)
Generalize numeric/path constants from the original command into flags with defaults.
No markdown fences. No commentary outside JSON.";

pub async fn promote(
    llm: &LlmClient,
    pending: &PendingPromote,
    name: &str,
    force: bool,
) -> Result<bool> {
    validate_name(name).context("invalid command name")?;
    let paths = LearnPaths::from_env();
    refuse_overwrite(&paths, name, force)?;

    let mut artifacts = fetch_artifacts(llm, pending, name, None).await?;
    let work = std::env::temp_dir().join(format!("shomsky-promote-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);

    match try_bats_gate(&work, name, &artifacts) {
        Ok(()) => {}
        Err(e1) => {
            eprintln!("  ⟡ bats failed; repairing once…\n{e1:#}");
            artifacts = fetch_artifacts(llm, pending, name, Some(&format!("{e1:#}"))).await?;
            let work2 =
                std::env::temp_dir().join(format!("shomsky-promote-{name}-r2-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&work2);
            if let Err(e2) = try_bats_gate(&work2, name, &artifacts) {
                let _ = std::fs::remove_dir_all(&work);
                let _ = std::fs::remove_dir_all(&work2);
                eprintln!("  ✗ bats still failing; not installing.\n{e2:#}");
                return Ok(false);
            }
            let _ = std::fs::remove_dir_all(&work2);
        }
    }
    let _ = std::fs::remove_dir_all(&work);

    install_artifacts(
        &paths,
        name,
        &pending.intent,
        &pending.command,
        &artifacts,
    )?;

    // Seed cmdfreq so tab-complete sees the new command immediately.
    seed_cmdfreq(name);

    let home = paths.home_path().display().to_string();
    let tilde = |p: std::path::PathBuf| {
        let s = p.display().to_string();
        s.replacen(&home, "~", 1)
    };
    println!("  ✓ wrote {}", tilde(paths.command_path(name)));
    if !artifacts.flags.is_empty() {
        println!("      (+ {} flags inferred)", artifacts.flags.join(", "));
    }
    println!("  ✓ wrote {}", tilde(paths.man_path(name)));
    println!("  ✓ wrote {}  → passing", tilde(paths.bats_path(name)));
    println!("  ✓ added tab-completion");
    Ok(true)
}

fn try_bats_gate(
    work: &std::path::Path,
    name: &str,
    artifacts: &PromoteArtifacts,
) -> Result<()> {
    let (_script, bats_path) = write_workdir(work, name, artifacts)?;
    run_bats(&bats_path)
}

async fn fetch_artifacts(
    llm: &LlmClient,
    pending: &PendingPromote,
    name: &str,
    repair: Option<&str>,
) -> Result<PromoteArtifacts> {
    if learn_stub() {
        return Ok(stub_artifacts(name, pending));
    }

    let user = if let Some(err) = repair {
        format!(
            "Command name: {name}\nIntent: {}\nOriginal command: {}\nSample output:\n{}\n\n\
             Previous artifacts failed BATS:\n{err}\n\nReturn fixed JSON only.",
            pending.intent, pending.command, pending.output_summary
        )
    } else {
        format!(
            "Command name: {name}\nIntent: {}\nOriginal command: {}\nSample output:\n{}",
            pending.intent, pending.command, pending.output_summary
        )
    };

    let messages = vec![
        ChatMessage::system(PROMOTE_SYSTEM),
        ChatMessage::user(user),
    ];
    let raw = llm
        .chat_text_max(&messages, 4096)
        .await
        .context("learn promote LLM")?;
    parse_promote_json(&raw).or_else(|e| {
        // one inline parse retry is handled by caller for bats; here just fail
        Err(e).context("promote JSON parse")
    })
}

fn stub_artifacts(name: &str, pending: &PendingPromote) -> PromoteArtifacts {
    if pending.intent.contains("FAILBATS") {
        return PromoteArtifacts {
            script: format!("#!/bin/sh\necho {name}\n"),
            man: format!(".TH {} 1\n.SH NAME\n{name}\n", name.to_ascii_uppercase()),
            bats: "@test \"always fails\" {\n  false\n}\n".into(),
            description: "intentionally broken".into(),
            flags: vec![],
            completion_words: vec!["--help".into()],
        };
    }
    let script = format!(
        "#!/bin/sh\n# learned from: {intent}\nCOUNT=5\nwhile getopts \"n:h\" opt; do\n  case \"$opt\" in\n    n) COUNT=\"$OPTARG\" ;;\n    h) echo \"usage: {name} [-n count]\"; exit 0 ;;\n    *) echo \"usage: {name} [-n count]\" >&2; exit 1 ;;\n  esac\ndone\nshift $((OPTIND - 1))\nif [ $# -gt 0 ]; then\n  echo \"$*\"\nelse\n  {cmd}\nfi\n",
        intent = pending.intent.replace('\'', ""),
        name = name,
        cmd = pending.command,
    );
    let man = format!(
        ".TH {uname} 1\n.SH NAME\n{name} \\- {desc}\n.SH SYNOPSIS\n.B {name}\n[\\-n count]\n.SH DESCRIPTION\nLearned command for: {intent}\n.SH OPTIONS\n.TP\n\\-n\nCount (default 5).\n.TP\n\\-h\nShow help.\n",
        uname = name.to_ascii_uppercase(),
        name = name,
        desc = pending.intent.replace('\n', " "),
        intent = pending.intent.replace('\n', " "),
    );
    let bats = format!(
        "@test \"{name} --help exits 0\" {{\n  run {name} -h\n  [ \"$status\" -eq 0 ]\n}}\n\n\
         @test \"{name} runs\" {{\n  run {name}\n  [ \"$status\" -eq 0 ]\n}}\n",
        name = name,
    );
    PromoteArtifacts {
        script,
        man,
        bats,
        description: pending.intent.clone(),
        flags: vec!["-n".into(), "-h".into()],
        completion_words: vec!["-n".into(), "-h".into(), "--help".into()],
    }
}

pub fn parse_promote_json(raw: &str) -> Result<PromoteArtifacts> {
    let json_str = extract_json_object(raw).ok_or_else(|| anyhow!("no JSON object in promote reply"))?;
    let p: PromoteJson = serde_json::from_str(json_str).context("deserialize promote")?;
    if p.script.trim().is_empty() || p.bats.trim().is_empty() {
        bail!("promote JSON missing script or bats");
    }
    Ok(PromoteArtifacts {
        script: p.script,
        man: if p.man.trim().is_empty() {
            ".TH CMD 1\n.SH NAME\ncmd\n".into()
        } else {
            p.man
        },
        bats: p.bats,
        description: if p.description.is_empty() {
            "learned command".into()
        } else {
            p.description
        },
        flags: p.flags,
        completion_words: p.completion_words,
    })
}

fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&s[start..=end])
}

fn seed_cmdfreq(name: &str) {
    let path = std::env::var_os("ENGRAM_CMDFREQ_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let home = LearnPaths::from_env();
            home.home_path().join(".command_freq.json")
        });
    if let Ok(mut store) = engram_cmdfreq::FrequencyStore::load(&path) {
        store.record(name);
        let _ = store.save(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_promote() {
        let raw = r##"{
          "script": "#!/bin/sh\necho ok\n",
          "man": ".TH X 1\n",
          "bats": "@test \"t\" { true; }\n",
          "description": "x",
          "flags": ["--help"],
          "completion_words": ["--help"]
        }"##;
        let a = parse_promote_json(raw).unwrap();
        assert!(a.script.contains("#!/bin/sh"));
    }
}
