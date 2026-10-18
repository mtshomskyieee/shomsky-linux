mod completer;
mod config;
mod halluc_fs;
mod halluc_session;
mod learn;
mod llm;
mod matrix;
mod mcp;
mod menu;
mod modes;
mod packages;
mod quickyn_tui;
mod repl;
mod skills;
mod spinner;
mod term;
mod tools;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

use crate::config::Config;
use crate::modes::Mode;
use crate::repl::Repl;
use crate::skills::SkillStore;

#[derive(Debug, Parser)]
#[command(
    name = "engram-shell",
    about = "LLM-backed linux shell for Shomsky Linux",
    version
)]
struct Cli {
    /// Path to engram.toml
    #[arg(long, env = "ENGRAM_CONFIG", default_value = "/etc/engram/engram.toml")]
    config: PathBuf,

    /// Override operating mode
    #[arg(long, env = "ENGRAM_MODE")]
    mode: Option<Mode>,

    /// Skills directory
    #[arg(long, env = "ENGRAM_SKILLS_DIR")]
    skills_dir: Option<PathBuf>,

    /// LLM base URL (OpenAI-compatible)
    #[arg(long, env = "ENGRAM_LLM_URL")]
    llm_url: Option<String>,

    /// Non-interactive single prompt (for smoke tests)
    #[arg(long)]
    once: Option<String>,

    /// Non-interactive multi-turn script: one command/prompt per line
    /// (blank lines and `#` comments skipped). History persists across lines.
    #[arg(long, value_name = "PATH")]
    script: Option<PathBuf>,

    /// Exercise tools/modes without calling the LLM (CI / smoke)
    #[arg(long)]
    self_test: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = Config::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    if let Some(mode) = cli.mode {
        cfg.mode = mode;
    }
    if let Some(dir) = cli.skills_dir {
        cfg.skills_dir = dir;
    }
    if let Some(url) = cli.llm_url {
        cfg.llm_url = url;
    }

    let skills = SkillStore::load(&cfg.skills_dir)
        .with_context(|| format!("loading skills from {}", cfg.skills_dir.display()))?;

    if cli.self_test {
        return self_test::run(&cfg, &skills).await;
    }

    let mut cfg = cfg;
    cfg.apply_cmdfreq_env();
    let mut repl = Repl::new(cfg, skills)?;

    if let Some(path) = cli.script {
        repl.run_script(&path).await?;
        repl.shutdown_mcp().await;
    } else if let Some(prompt) = cli.once {
        repl.run_once(&prompt).await?;
        repl.shutdown_mcp().await;
    } else {
        repl.run().await?;
    }

    Ok(())
}

mod self_test {
    use anyhow::{bail, Result};
    use crate::config::Config;
    use crate::llm::ToolCall;
    use crate::modes::Mode;
    use crate::skills::SkillStore;
    use crate::tools::{self, execute_tool};
    use std::path::Path;

    fn fake_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "selftest".into(),
            kind: "function".into(),
            function: crate::llm::FunctionCall {
                name: name.into(),
                arguments: args.to_string(),
            },
        }
    }

    pub async fn run(cfg: &Config, skills: &SkillStore) -> Result<()> {
        println!("engram-shell self-test");
        let mut cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));

        // Skills
        let shell = skills
            .get("shell")
            .ok_or_else(|| anyhow::anyhow!("missing always-loaded skill 'shell'"))?;
        assert!(shell.meta.always_load, "shell skill should be always_load");
        println!("  ok  skills: shell always_load");

        // list_dir / read_file (real in all LLM modes)
        let listing = execute_tool(
            &fake_call("list_dir", serde_json::json!({"path": "/etc/engram"})),
            Mode::Agent,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if !listing.contains("engram.toml") || !listing.contains("models.toml") {
            bail!("list_dir /etc/engram unexpected: {listing}");
        }
        println!("  ok  list_dir /etc/engram");

        let os = execute_tool(
            &fake_call("read_file", serde_json::json!({"path": "/etc/os-release"})),
            Mode::Agent,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if !os.contains("ID=shomsky") {
            bail!("read_file /etc/os-release missing ID=shomsky: {os}");
        }
        println!("  ok  read_file /etc/os-release");

        // agent-mode write is real
        let path = "/tmp/engram-selftest-agent.txt";
        let _ = std::fs::remove_file(path);
        let w = execute_tool(
            &fake_call(
                "write_file",
                serde_json::json!({"path": path, "content": "agent-wrote"}),
            ),
            Mode::Agent,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if !Path::new(path).exists() {
            bail!("agent-mode write_file did not create {path}: {w}");
        }
        println!("  ok  agent-mode write_file");

        // hallucination-mode write is a quiet no-op (no disk, no warning spam)
        let hpath = "/tmp/engram-selftest-halluc.txt";
        let _ = std::fs::remove_file(hpath);
        let hw = execute_tool(
            &fake_call(
                "write_file",
                serde_json::json!({"path": hpath, "content": "should-not-exist"}),
            ),
            Mode::Hallucination,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if Path::new(hpath).exists() {
            bail!("hallucination-mode wrote to disk unexpectedly");
        }
        if hw.contains("hallucination-mode") || hw.contains("disabled") {
            bail!("hallucination write_file leaked a warning: {hw}");
        }
        println!("  ok  hallucination-mode write_file (quiet no-op)");

        // hallucination shell_exec quiet no-op
        let hs = execute_tool(
            &fake_call(
                "shell_exec",
                serde_json::json!({"command": "touch /tmp/engram-selftest-halluc-exec"}),
            ),
            Mode::Hallucination,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if Path::new("/tmp/engram-selftest-halluc-exec").exists() {
            bail!("hallucination-mode shell_exec touched disk");
        }
        if hs.contains("hallucination-mode") || hs.contains("disabled") {
            bail!("hallucination shell_exec leaked a warning: {hs}");
        }
        println!("  ok  hallucination-mode shell_exec (quiet no-op)");

        // agent shell_exec real
        let out = execute_tool(
            &fake_call("shell_exec", serde_json::json!({"command": "echo shomsky-ok"})),
            Mode::Agent,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if !out.contains("shomsky-ok") {
            bail!("agent shell_exec unexpected: {out}");
        }
        println!("  ok  agent-mode shell_exec");

        // load_skill
        let body = execute_tool(
            &fake_call("load_skill", serde_json::json!({"name": "shell"})),
            Mode::Agent,
            cfg,
            skills,
            None,
            &mut cwd,
        )
        .await?;
        if !body.contains("BusyBox") && !body.contains("shell") {
            bail!("load_skill shell unexpected: {body}");
        }
        println!("  ok  load_skill shell");

        // schemas present
        let schemas = tools::tool_schemas(None);
        assert_eq!(schemas.len(), 5);
        println!("  ok  tool schemas ({})", schemas.len());

        // text-pattern extraction (small-model path)
        let parsed = tools::extract_tool_calls(
            "Sure.\nTOOL read_file path=/etc/os-release\n",
        );
        if parsed.len() != 1 || parsed[0].function.name != "read_file" {
            bail!("text TOOL line parse failed: {parsed:?}");
        }
        let rejected = tools::extract_tool_calls(
            r#"<tool_call>{"name":"egremain","arguments":{}}</tool_call>"#,
        );
        if !rejected.is_empty() {
            bail!("unknown text tool should be rejected");
        }
        println!("  ok  text-pattern tool parse + allow-list");

        assert!(tools::looks_like_shell_command("ls -la"));
        assert!(tools::looks_like_shell_command("echo hi > f.txt"));
        assert!(!tools::looks_like_shell_command("what files are here?"));
        println!("  ok  shell-command heuristic");

        println!("self-test passed");
        Ok(())
    }
}
