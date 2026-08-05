use anyhow::{Context, Result};
use engram_cmdfreq::{format_top, FrequencyStore, DEFAULT_TOP_LIMIT, MAX_TOP};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::Editor;
use std::path::PathBuf;

use crate::completer::EngramHelper;
use crate::config::{Config, ToolStyle};
use crate::halluc_fs::HallucFs;
use crate::halluc_session;
use crate::llm::{ChatMessage, LlmClient, ToolCall};
use crate::mcp::McpRegistry;
use crate::modes::Mode;
use crate::skills::SkillStore;
use crate::spinner::{self, with_progress_bar, with_spinner};
use crate::tools::{self, shell_exec, shell_exec_interactive};
use crate::ui;

const MAX_TOOL_ROUNDS: usize = 8;

type LineEditor = Editor<EngramHelper, DefaultHistory>;

pub struct Repl {
    cfg: Config,
    skills: SkillStore,
    llm: LlmClient,
    history: Vec<ChatMessage>,
    halluc_fs: HallucFs,
    /// Active hallucination session JSON path (auto-saved each turn).
    halluc_session_path: Option<PathBuf>,
    /// Process cwd for agent/root modes (`cd` persists across turns).
    /// Hallucination mode keeps its virtual FS and does not mutate this.
    cwd: PathBuf,
    mcp: Option<McpRegistry>,
}

impl Repl {
    pub fn new(cfg: Config, skills: SkillStore) -> Result<Self> {
        let llm = LlmClient::new(&cfg)?;
        let system = build_system_prompt(&cfg, &skills);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Ok(Self {
            cfg,
            skills,
            llm,
            history: vec![ChatMessage::system(system)],
            halluc_fs: HallucFs::new(),
            halluc_session_path: None,
            cwd,
            mcp: None,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        ui::print_splash(&self.cfg);
        self.wait_for_llm().await;
        self.connect_mcp().await;

        let mut rl = LineEditor::new().context("initializing readline")?;
        rl.set_helper(Some(EngramHelper::new(self.cfg.cmdfreq_path.clone())));
        let _ = rl.load_history("/engram/.shell_history");

        if self.cfg.mode == Mode::Hallucination {
            self.enter_hallucination_session(&mut rl)?;
        }

        loop {
            Self::drain_shomagent_notifications();
            let prompt = ui::prompt_string(self.cfg.mode, &self.cwd);
            let line = match rl.readline(&prompt) {
                Ok(l) => l,
                Err(ReadlineError::Interrupted) => {
                    println!("^C");
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    println!();
                    break;
                }
                Err(e) => return Err(e.into()),
            };

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let _ = rl.add_history_entry(&line);

            if line.starts_with(':') {
                if self.handle_meta(&line, Some(&mut rl)).await? {
                    break;
                }
                continue;
            }

            // Bare exit/logout/quit leave the linux shell (all modes).
            if is_shell_exit(&line) {
                let _ = self.save_halluc_session();
                break;
            }

            // Local identity — don't spend an LLM turn on uname.
            if ui::is_sys_command(&line) {
                ui::print_sys(&self.cfg, &self.cwd);
                continue;
            }

            match self.cfg.mode {
                Mode::Root => {
                    if tools::is_ignore_llm_command(&line, self.skills.ignore_llm_commands()) {
                        self.run_interactive_shell(&line).await?;
                    } else {
                        self.run_root(&line).await?;
                    }
                }
                Mode::Agent => {
                    if tools::is_ignore_llm_command(&line, self.skills.ignore_llm_commands()) {
                        self.run_interactive_shell(&line).await?;
                    } else if self.cfg.direct_shell && tools::looks_like_shell_command(&line) {
                        self.run_direct_shell(&line).await?;
                    } else {
                        self.run_llm(&line).await?;
                    }
                }
                Mode::Hallucination => self.run_hallucination(&line).await?,
            }
        }

        let _ = self.save_halluc_session();
        let _ = rl.save_history("/engram/.shell_history");
        self.shutdown_mcp().await;
        Ok(())
    }

    async fn ensure_mcp(&mut self) {
        if self.mcp.is_some() || self.cfg.mcp_servers.is_empty() {
            return;
        }
        self.cfg.apply_cmdfreq_env();
        self.mcp = Some(McpRegistry::connect(&self.cfg.mcp_servers).await);
    }

    async fn connect_mcp(&mut self) {
        self.ensure_mcp().await;
    }

    pub async fn shutdown_mcp(&mut self) {
        if let Some(reg) = self.mcp.take() {
            reg.shutdown().await;
        }
    }

    async fn record_command(&self, command: &str) {
        let recorded = if let Some(reg) = &self.mcp {
            reg.record_command_best_effort(command).await
        } else {
            false
        };
        if recorded {
            return;
        }
        // Local write when MCP is absent or its sidecar has died (Broken pipe).
        let path = &self.cfg.cmdfreq_path;
        if let Ok(mut store) = FrequencyStore::load(path) {
            store.record(command);
            let _ = store.save(path);
        }
    }

    fn print_top_commands(&self, limit: usize) {
        let limit = limit.clamp(1, MAX_TOP);
        let store = FrequencyStore::load(&self.cfg.cmdfreq_path).unwrap_or_default();
        let items = store.top(limit, "");
        print!("{}", format_top(&items));
    }

    pub async fn run_once(&mut self, prompt: &str) -> Result<()> {
        self.ensure_mcp().await;
        match self.cfg.mode {
            Mode::Root => {
                if tools::is_ignore_llm_command(prompt, self.skills.ignore_llm_commands()) {
                    self.run_interactive_shell(prompt).await?;
                } else {
                    self.run_root(prompt).await?;
                }
            }
            Mode::Agent => {
                if tools::is_ignore_llm_command(prompt, self.skills.ignore_llm_commands()) {
                    self.run_interactive_shell(prompt).await?;
                } else if self.cfg.direct_shell && tools::looks_like_shell_command(prompt) {
                    self.run_direct_shell(prompt).await?;
                } else {
                    self.run_llm(prompt).await?;
                }
            }
            Mode::Hallucination => self.run_hallucination(prompt).await?,
        }
        Ok(())
    }

    /// Run multiple prompts from a file (one per line). History is shared so
    /// hallucination-mode can track a virtual filesystem across turns.
    ///
    /// When `ENGRAM_SCRIPT_MARKERS=1`, each turn is wrapped so tests can split
    /// empty vs non-empty stdout:
    /// ```text
    /// <<<ENGRAM_TURN n>>>
    /// <stdout>
    /// <<<ENGRAM_END>>>
    /// ```
    pub async fn run_script(&mut self, path: &std::path::Path) -> Result<()> {
        let markers = matches!(
            std::env::var("ENGRAM_SCRIPT_MARKERS")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "on"
        );
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading script {}", path.display()))?;
        let mut turn: usize = 0;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with(':') {
                if self.handle_meta(line, None).await? {
                    break;
                }
                continue;
            }
            turn += 1;
            eprintln!("# [{turn}] {line}");
            if markers {
                println!("<<<ENGRAM_TURN {turn}>>>");
            }
            self.run_once(line).await?;
            if markers {
                println!("<<<ENGRAM_END>>>");
            }
        }
        Ok(())
    }

    /// Returns true if the shell should exit.
    async fn handle_meta(
        &mut self,
        line: &str,
        mut rl: Option<&mut LineEditor>,
    ) -> Result<bool> {
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next();

        match cmd {
            ":help" | ":h" => {
                println!("{}", HELP_TEXT);
            }
            ":sys" | ":uname" => {
                ui::print_sys(&self.cfg, &self.cwd);
            }
            ":top" => {
                let limit = arg
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(DEFAULT_TOP_LIMIT);
                self.print_top_commands(limit);
            }
            ":exit" | ":quit" | ":q" => {
                let _ = self.save_halluc_session();
                return Ok(true);
            }
            ":mode" => match arg {
                None => println!("current mode: {}", self.cfg.mode),
                Some(name) => match name.parse::<Mode>() {
                    Ok(Mode::Root) if !self.cfg.allow_root_mode => {
                        eprintln!("error: root-mode is disabled by configuration");
                    }
                    Ok(mode) => {
                        if self.cfg.mode == Mode::Hallucination && mode != Mode::Hallucination {
                            let _ = self.save_halluc_session();
                            self.halluc_session_path = None;
                        }
                        self.cfg.mode = mode;
                        if mode == Mode::Hallucination {
                            if let Some(editor) = rl.as_deref_mut() {
                                self.enter_hallucination_session(editor)?;
                            } else {
                                self.start_hallucination_fresh()?;
                            }
                        } else {
                            self.rebuild_system_prompt();
                        }
                        println!("mode set to {}", mode);
                    }
                    Err(e) => eprintln!("error: {e}"),
                },
            },
            ":tools" | ":tool-style" => match arg {
                None => println!("tool_style: {}", self.cfg.tool_style.as_str()),
                Some(name) => match name.parse::<ToolStyle>() {
                    Ok(style) => {
                        self.cfg.tool_style = style;
                        self.rebuild_system_prompt();
                        println!("tool_style set to {}", style.as_str());
                    }
                    Err(e) => eprintln!("error: {e}"),
                },
            },
            ":skills" => {
                for skill in self.skills.list() {
                    println!(
                        "{}{}  — {}",
                        skill.meta.name,
                        if skill.meta.always_load {
                            " [always]"
                        } else {
                            ""
                        },
                        skill.meta.description
                    );
                }
            },
            ":model" => {
                println!("model alias: {}", self.cfg.model_alias);
                println!("llm url:     {}", self.cfg.llm_url);
                println!("llm model:   {}", self.cfg.llm_model);
                println!("tool style:  {}", self.cfg.tool_style.as_str());
                println!("max context: {}", self.cfg.max_context_messages);
                if let Some(path) = &self.halluc_session_path {
                    println!("halluc session: {}", path.display());
                }
                println!(
                    "halluc history dir: {}",
                    halluc_session::history_dir().display()
                );
            },
            ":clear" => {
                if self.cfg.mode == Mode::Hallucination {
                    self.start_hallucination_fresh()?;
                    println!("hallucination session cleared (new file); virtual FS reset");
                } else {
                    self.rebuild_system_prompt();
                    println!("conversation history cleared");
                }
            },
            ":logs" => {
                let path = std::env::var("MISTRALRS_LOG")
                    .unwrap_or_else(|_| "/engram/mistralrs.log".into());
                let n: usize = arg.and_then(|s| s.parse().ok()).unwrap_or(40);
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let lines: Vec<&str> = text.lines().collect();
                        let start = lines.len().saturating_sub(n);
                        for line in &lines[start..] {
                            println!("{line}");
                        }
                        if lines.is_empty() {
                            println!("(log empty: {path})");
                        }
                    }
                    Err(e) => eprintln!("cannot read {path}: {e}"),
                }
            },
            ":shomagent" | ":agents" => {
                self.handle_shomagent_meta(arg, parts.next()).await;
            },
            other => eprintln!("unknown meta-command '{other}' (try :help)"),
        }
        Ok(false)
    }

    /// Drain JSONL notification lane into the TTY (earned interrupts only).
    fn drain_shomagent_notifications() {
        let cursor = shomagent::paths::run_dir().join("notify.cursor");
        match shomagent::notify::drain_new_findings(&cursor) {
            Ok(findings) => {
                for f in findings {
                    println!("{}", f.format_interrupt());
                }
            }
            Err(_) => {
                // Daemon not running or lane missing — silent.
            }
        }
    }

    async fn handle_shomagent_meta(&mut self, arg: Option<&str>, talk_id: Option<&str>) {
        match arg {
            None | Some("roster") => match shomagent::daemon::rpc(&shomagent::protocol::Request::Roster).await {
                Ok(shomagent::protocol::Response::Ok { roster, .. }) => {
                    if roster.is_empty() {
                        println!("(no shomagent members — try: shomagent watch \"…\")");
                        return;
                    }
                    for m in roster {
                        let glyph = match m.state.as_str() {
                            "running" => "▶",
                            "paused" => "⏸",
                            "read" => "◇",
                            "readwrite" => "◆",
                            _ => "■",
                        };
                        println!(
                            "  #{} {:12} {}  {} finding{}",
                            m.id,
                            m.name,
                            glyph,
                            m.findings,
                            if m.findings == 1 { "" } else { "s" }
                        );
                    }
                }
                Ok(shomagent::protocol::Response::Err { message }) => {
                    eprintln!("shomagent: {message}");
                }
                Err(e) => eprintln!("shomagent: {e} (is shomagentd running?)"),
            },
            Some("talk") => {
                let Some(id_s) = talk_id else {
                    eprintln!("usage: :shomagent talk <n>");
                    return;
                };
                let Ok(id) = id_s.parse::<u32>() else {
                    eprintln!("usage: :shomagent talk <n>");
                    return;
                };
                println!("use `shomagent talk {id}` for a scoped chat session");
                match shomagent::daemon::rpc(&shomagent::protocol::Request::Context { id }).await {
                    Ok(shomagent::protocol::Response::Ok {
                        context: Some(ctx), ..
                    }) => {
                        println!("#{} {} — {} finding(s)", ctx.id, ctx.name, ctx.findings.len());
                        if let Some(last) = ctx.findings.last() {
                            println!("{}", last.format_interrupt());
                        }
                    }
                    Ok(shomagent::protocol::Response::Err { message }) => {
                        eprintln!("shomagent: {message}");
                    }
                    Err(e) => eprintln!("shomagent: {e}"),
                    _ => {}
                }
            }
            Some(other) => eprintln!("unknown :shomagent subcommand '{other}' (roster|talk)"),
        }
    }

    /// Interactive: resume latest session or start fresh.
    fn enter_hallucination_session(&mut self, rl: &mut LineEditor) -> Result<()> {
        let dir = halluc_session::ensure_history_dir()?;
        let latest = halluc_session::latest_session()?;

        // Non-interactive / CI: ENGRAM_HALLUC_RESUME=0|1|fresh|latest
        if let Ok(policy) = std::env::var("ENGRAM_HALLUC_RESUME") {
            match policy.to_ascii_lowercase().as_str() {
                "0" | "false" | "no" | "fresh" | "new" => {
                    return self.start_hallucination_fresh();
                }
                "1" | "true" | "yes" | "latest" | "resume" => {
                    if let Some(path) = latest {
                        return self.resume_hallucination_session(&path);
                    }
                    println!("no prior hallucination session; starting fresh");
                    return self.start_hallucination_fresh();
                }
                _ => {}
            }
        }

        println!("Hallucination mode — sessions in {}", dir.display());
        if let Some(ref path) = latest {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("session");
            println!("  Latest: {name}");
            println!("  [r] resume latest    [f] start fresh");
            let choice = match rl.readline("hallucination> ") {
                Ok(s) => s.trim().to_ascii_lowercase(),
                Err(_) => "f".into(),
            };
            if choice.is_empty()
                || choice == "r"
                || choice == "resume"
                || choice == "y"
                || choice == "yes"
            {
                return self.resume_hallucination_session(path);
            }
            return self.start_hallucination_fresh();
        }

        println!("  No prior sessions. Starting a new one.");
        self.start_hallucination_fresh()
    }

    fn start_hallucination_fresh(&mut self) -> Result<()> {
        self.halluc_fs = HallucFs::new();
        self.rebuild_system_prompt();
        let path = halluc_session::new_session_path()?;
        self.halluc_session_path = Some(path.clone());
        self.save_halluc_session()?;
        // stderr: keep scripted golden stdout (ENGRAM_SCRIPT_MARKERS) clean
        eprintln!("hallucination session: {}", path.display());
        Ok(())
    }

    fn resume_hallucination_session(&mut self, path: &std::path::Path) -> Result<()> {
        let (messages, fs) = halluc_session::load_session(path)?;
        self.halluc_fs = fs;
        if messages.is_empty() {
            self.rebuild_system_prompt();
        } else {
            // Refresh system prompt for current binary, keep user/assistant turns.
            let system = build_system_prompt(&self.cfg, &self.skills);
            let mut rest: Vec<ChatMessage> = messages
                .into_iter()
                .filter(|m| m.role != "system")
                .collect();
            self.history = vec![ChatMessage::system(system)];
            self.history.append(&mut rest);
            self.trim_history();
        }
        self.halluc_session_path = Some(path.to_path_buf());
        let nfiles = self.halluc_fs.files().len();
        eprintln!(
            "resumed {} ({} virtual file{})",
            path.display(),
            nfiles,
            if nfiles == 1 { "" } else { "s" }
        );
        let _ = self.save_halluc_session();
        Ok(())
    }

    fn save_halluc_session(&self) -> Result<()> {
        let Some(path) = &self.halluc_session_path else {
            return Ok(());
        };
        halluc_session::save_session(path, &self.history, &self.halluc_fs)
            .with_context(|| format!("saving hallucination session {}", path.display()))?;
        Ok(())
    }

    /// Guided hallucination shell: deterministic builtins via virtual FS,
    /// otherwise ask the LLM (still no real disk writes).
    async fn run_hallucination(&mut self, line: &str) -> Result<()> {
        self.ensure_halluc_session()?;
        if let Some(out) = self.halluc_fs.try_exec(line) {
            let display = out.trim_end_matches('\n');
            if !display.is_empty() {
                println!("{display}");
            }
            self.history.push(ChatMessage::user(line));
            self.history
                .push(ChatMessage::assistant_text(display.to_string()));
            self.trim_history();
            if let Err(e) = self.save_halluc_session() {
                eprintln!("warning: could not save hallucination session: {e:#}");
            }
            return Ok(());
        }
        self.run_llm(line).await?;
        if let Err(e) = self.save_halluc_session() {
            eprintln!("warning: could not save hallucination session: {e:#}");
        }
        Ok(())
    }

    /// For --once/--script (no interactive enter): honor ENGRAM_HALLUC_RESUME.
    fn ensure_halluc_session(&mut self) -> Result<()> {
        if self.halluc_session_path.is_some() {
            return Ok(());
        }
        let policy = std::env::var("ENGRAM_HALLUC_RESUME").unwrap_or_else(|_| "fresh".into());
        match policy.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "latest" | "resume" => {
                if let Some(path) = halluc_session::latest_session()? {
                    return self.resume_hallucination_session(&path);
                }
                self.start_hallucination_fresh()
            }
            _ => self.start_hallucination_fresh(),
        }
    }

    /// Interactive startup: poll `/v1/models` until the backend is ready.
    /// On a TTY, shows a spinner + progress bar under the splash. Continues
    /// into the prompt on timeout so a linux shell still comes up.
    /// Non-TTY / spinner-off still polls silently (entrypoint may have skipped
    /// its own wait for interactive sessions).
    async fn wait_for_llm(&self) {
        let max_secs: u64 = std::env::var("ENGRAM_LLM_READY_TRIES")
            .ok()
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                std::env::var("LLM_READY_TRIES")
                    .ok()
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or(600);
        let start = std::time::Instant::now();
        let poll = async {
            loop {
                if self.llm.health().await.is_ok() {
                    return true;
                }
                if start.elapsed().as_secs() >= max_secs {
                    return false;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        };
        let ready = if spinner::enabled() {
            with_progress_bar("warming model", max_secs, poll).await
        } else {
            poll.await
        };
        let secs = start.elapsed().as_secs();
        if ready {
            ui::print_ready_line(&self.cfg, secs, &self.cwd);
        } else {
            eprintln!(
                "warning: LLM not ready after {secs}s — continuing anyway \
                 (raise ENGRAM_LLM_READY_TRIES or check :logs)"
            );
        }
    }

    /// Run a TUI / interactive command with an inherited TTY (no LLM, no capture).
    async fn run_interactive_shell(&mut self, line: &str) -> Result<()> {
        let home = self.cfg.mode.home_dir();
        // No spinner — it would corrupt the TUI.
        let out =
            shell_exec_interactive(&self.cfg.busybox_ash, line, &self.cwd, &home).await?;
        tools::adopt_cwd(&mut self.cwd, out.cwd);
        if !out.output.is_empty() {
            print!("{}", out.output);
            if !out.output.ends_with('\n') {
                println!();
            }
        }
        self.record_command(line).await;
        if self.cfg.mode == Mode::Agent {
            // Note the invocation without dumping TUI noise into history.
            self.history.push(ChatMessage::user(line));
            self.history.push(ChatMessage::assistant_text(format!(
                "$ {line}\n(interactive TUI — output not captured)"
            )));
            self.trim_history();
        }
        Ok(())
    }

    /// Execute a shell-shaped line immediately via BusyBox (agent mode only).
    async fn run_direct_shell(&mut self, line: &str) -> Result<()> {
        let call = ToolCall {
            id: format!("direct_{}", uuid::Uuid::new_v4()),
            kind: "function".into(),
            function: crate::llm::FunctionCall {
                name: "shell_exec".into(),
                arguments: serde_json::json!({ "command": line }).to_string(),
            },
        };
        let result = with_spinner("running shell", async {
            tools::execute_tool(
                &call,
                self.cfg.mode,
                &self.cfg,
                &self.skills,
                self.mcp.as_ref(),
                &mut self.cwd,
            )
            .await
        })
        .await?;
        print!("{result}");
        if !result.ends_with('\n') {
            println!();
        }
        self.record_command(line).await;
        // Keep conversation coherent for later chat turns.
        self.history.push(ChatMessage::user(line));
        self.history.push(ChatMessage::assistant_text(format!(
            "$ {line}\n{result}"
        )));
        self.trim_history();
        Ok(())
    }

    fn rebuild_system_prompt(&mut self) {
        let system = build_system_prompt(&self.cfg, &self.skills);
        self.history = vec![ChatMessage::system(system)];
    }

    fn trim_history(&mut self) {
        let max = self.cfg.max_context_messages.max(2);
        while self.history.len() > max {
            // Keep index 0 (system); drop the oldest non-system message.
            if self.history.len() > 1 {
                self.history.remove(1);
            } else {
                break;
            }
        }
    }

    async fn run_root(&mut self, line: &str) -> Result<()> {
        if !self.cfg.allow_root_mode {
            eprintln!("error: root-mode is disabled by configuration");
            return Ok(());
        }
        let out = with_spinner("running shell", async {
            let home = Mode::Root.home_dir();
            shell_exec(&self.cfg.busybox_ash, line, &self.cwd, &home).await
        })
        .await?;
        tools::adopt_cwd(&mut self.cwd, out.cwd);
        print!("{}", out.output);
        if !out.output.ends_with('\n') {
            println!();
        }
        self.record_command(line).await;
        Ok(())
    }

    async fn run_llm(&mut self, line: &str) -> Result<()> {
        self.history.push(ChatMessage::user(line));
        self.trim_history();

        // Hallucination is a pure LLM shell simulator — do not advertise tools.
        let schemas = if self.cfg.mode == Mode::Hallucination {
            vec![]
        } else if self.cfg.tool_style.send_native_schemas() {
            tools::tool_schemas(self.mcp.as_ref())
        } else {
            vec![]
        };
        let mcp_names = self
            .mcp
            .as_ref()
            .map(|m| m.tool_names())
            .unwrap_or_default();

        for round in 0..MAX_TOOL_ROUNDS {
            self.trim_history();
            let label = if round == 0 {
                "thinking".to_string()
            } else {
                format!("thinking (round {})", round + 1)
            };
            let response = match with_spinner(&label, self.llm.chat(&self.history, &schemas, false))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("llm error: {e:#}");
                    self.history.pop();
                    return Ok(());
                }
            };

            // 1) Native OpenAI tool_calls (agent only)
            let mut calls: Vec<ToolCall> = if self.cfg.mode != Mode::Hallucination
                && self.cfg.tool_style != ToolStyle::Text
            {
                response.tool_calls.clone()
            } else {
                vec![]
            };

            // 2) Text-pattern fallback (agent auto/text only)
            if calls.is_empty()
                && self.cfg.mode != Mode::Hallucination
                && self.cfg.tool_style != ToolStyle::Native
                && response.content.as_ref().is_some_and(|c| !c.is_empty())
            {
                if let Some(content) = &response.content {
                    calls = tools::extract_tool_calls_with(content, &mcp_names);
                }
            }

            if !calls.is_empty() {
                // Keep the raw assistant text (with markup) in history so the
                // model sees what it emitted; user-facing print strips markup.
                let raw = response.content.clone().unwrap_or_default();
                if !raw.is_empty() {
                    let visible = tools::strip_tool_markup(&raw);
                    if !visible.is_empty() {
                        println!("{visible}");
                    }
                }

                if self.cfg.tool_style == ToolStyle::Text
                    || response.tool_calls.is_empty()
                {
                    // Text-pattern path: store as plain assistant text + synthetic tool results
                    self.history.push(ChatMessage::assistant_text(if raw.is_empty() {
                        format!("(calling {} tools)", calls.len())
                    } else {
                        raw
                    }));
                } else {
                    self.history.push(ChatMessage::assistant_tools(
                        response.content.clone(),
                        calls.clone(),
                    ));
                }

                for call in &calls {
                    eprintln!(
                        "  ↳ {}({})",
                        call.function.name,
                        truncate_args(&call.function.arguments, 80)
                    );
                    let tool_label = format!("running {}", call.function.name);
                    let result = with_spinner(&tool_label, async {
                        tools::execute_tool(
                            call,
                            self.cfg.mode,
                            &self.cfg,
                            &self.skills,
                            self.mcp.as_ref(),
                            &mut self.cwd,
                        )
                        .await
                    })
                    .await?;
                    // Always show tool output to the user (not only to the model).
                    print!("{result}");
                    if !result.ends_with('\n') {
                        println!();
                    }
                    if let Some(cmd) = tools::shell_exec_command(call) {
                        self.record_command(&cmd).await;
                    }

                    if self.cfg.tool_style == ToolStyle::Text
                        || response.tool_calls.is_empty()
                    {
                        // For text-style models, feed results as a user/tool observation.
                        self.history.push(ChatMessage::user(format!(
                            "<tool_result name=\"{}\">\n{}\n</tool_result>\n\
                             Continue. Use another tool if needed, or answer the user.",
                            call.function.name, result
                        )));
                    } else {
                        self.history.push(ChatMessage::tool_result(
                            &call.id,
                            &call.function.name,
                            result,
                        ));
                    }
                }
                continue;
            }

            if let Some(content) = response.content {
                let cleaned = if self.cfg.mode == Mode::Hallucination {
                    clean_hallucination_text(&content, line)
                } else {
                    clean_assistant_text(&content)
                };
                if cleaned.is_empty() {
                    if self.cfg.mode == Mode::Hallucination {
                        // Empty stdout is valid (redirects, empty ls, mkdir, …).
                        self.history.push(ChatMessage::assistant_text(String::new()));
                    } else {
                        println!(
                            "(no useful response — try a clearer question, or a shell command)"
                        );
                        self.history.push(ChatMessage::assistant_text("(empty)"));
                    }
                } else {
                    println!("{cleaned}");
                    self.history.push(ChatMessage::assistant_text(cleaned));
                }
            } else if self.cfg.mode == Mode::Hallucination {
                self.history.push(ChatMessage::assistant_text(String::new()));
            } else {
                println!("(no response)");
            }
            return Ok(());
        }

        eprintln!("error: exceeded maximum tool rounds ({MAX_TOOL_ROUNDS})");
        Ok(())
    }
}

/// Drop empty markdown code fences and trim noise from tiny-model replies.
fn clean_assistant_text(s: &str) -> String {
    let mut out = s.trim().to_string();
    // Collapse ``` ... ``` that contain only whitespace / the original command echo.
    if out.starts_with("```") {
        let body = out
            .trim_start_matches('`')
            .trim_start_matches(|c: char| c.is_ascii_alphabetic()) // language tag
            .trim_start_matches('\n')
            .trim_end_matches('`')
            .trim();
        // If fence body is empty or only backticks leftovers, treat as empty.
        if body.is_empty() || body.chars().all(|c| c == '`' || c.is_whitespace()) {
            return String::new();
        }
        out = body.to_string();
    }
    // Strip tool markup if any leaked into a final answer.
    let stripped = tools::strip_tool_markup(&out);
    stripped.trim().to_string()
}

/// Hallucination replies: strip meta labels tiny models echo from the prompt,
/// and map sentinel words to a true empty stdout.
fn clean_hallucination_text(s: &str, user_cmd: &str) -> String {
    // Simple redirects write to the virtual FS and print nothing.
    if user_cmd.contains('>') {
        return String::new();
    }

    let mut out = clean_assistant_text(s);
    if out.is_empty() {
        return out;
    }

    // Model echoed the command line back — not stdout.
    if out.trim() == user_cmd.trim() {
        return String::new();
    }

    // If the model replayed a Command/Output or user/you script, keep the last
    // Output/you payload only.
    if out.to_ascii_lowercase().contains("command:")
        || out.to_ascii_lowercase().contains("output:")
        || out.contains("user:")
        || out.contains("you:")
    {
        let lower = out.to_ascii_lowercase();
        if let Some(idx) = lower.rfind("output:") {
            out = out[idx + "output:".len()..].trim().to_string();
        } else if let Some(idx) = lower.rfind("you:") {
            out = out[idx + "you:".len()..].trim().to_string();
        }
        // Still looks like a dialogue — drop it.
        let l = out.to_ascii_lowercase();
        if l.contains("command:") || l.contains("user:") || l.starts_with("you:") {
            return String::new();
        }
    }

    let t = out.trim();
    let tl = t.to_ascii_lowercase();
    if tl.is_empty()
        || tl == "nothing"
        || tl == "empty"
        || tl == "(empty)"
        || tl == "(no output)"
        || tl == "null"
        || tl == "none"
    {
        return String::new();
    }

    // Drop non-ASCII so terminals stay readable.
    let ascii: String = t
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || (*c >= ' ' && *c <= '~'))
        .collect();

    // Virtual ls policy: never surface "." / ".." (tiny models love to spam these).
    let filtered: Vec<&str> = ascii
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "." && *l != "..")
        .collect();
    filtered.join("\n")
}

fn build_system_prompt(cfg: &Config, skills: &SkillStore) -> String {
    match cfg.mode {
        Mode::Hallucination => build_hallucination_prompt(cfg),
        Mode::Agent => build_agent_prompt(cfg, skills),
        Mode::Root => String::new(),
    }
}

fn build_hallucination_prompt(cfg: &Config) -> String {
    format!(
        "You are the shell of Shomsky Linux, similar to ash, (root@shomsky, cwd=/root, model={alias}).\n\
         Brand: Shomsky Linux. Never call yourself a simulator, emulator, or \
         \"BusyBox ash simulator\". You may mention BusyBox only as the underlying \
         command set, the same way a real Alpine shell would.\n\
         For each shell command, write only the stdout/stderr bytes. English ASCII only.\n\
         Never write the words Command, Output, user, or you as labels.\n\
         Never write NOTHING or EMPTY — if there is no stdout, reply with an empty message.\n\
         For plain English questions (not shell commands), answer briefly as Shomsky Linux.\n\
         \n\
         echo TEXT           -> print TEXT\n\
         echo TEXT > FILE    -> print nothing; remember FILE contains TEXT\n\
         ls / ls -a          -> names of files created in this chat only (not . or ..); \
         if none, print nothing\n\
         cat FILE            -> file contents from chat memory\n\
         \n\
         Example session:\n\
         Command: ls -a\n\
         Output:\n\
         Command: echo hi > hi.txt\n\
         Output:\n\
         Command: ls -a\n\
         Output:\n\
         hi.txt\n\
         Command: cat hi.txt\n\
         Output:\n\
         hi\n\
         Command: echo hello-halluc\n\
         Output:\n\
         hello-halluc\n",
        alias = cfg.model_alias
    )
}

/// `exit`, `quit`, `logout` (optional status code) — leave the linux shell.
fn is_shell_exit(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("exit" | "quit" | "logout") => match parts.next() {
            None => true,
            Some(code) => code.chars().all(|c| c.is_ascii_digit()) && parts.next().is_none(),
        },
        _ => false,
    }
}

fn build_agent_prompt(cfg: &Config, skills: &SkillStore) -> String {
    let mut s = String::new();
    s.push_str(
        "You are the conversational assistant for Shomsky Linux, an Alpine-derived \
         container that hosts local LLMs.\n\n",
    );
    s.push_str(&format!("Operating mode: **agent**\n"));
    s.push_str(&format!("Tool style: **{}**\n\n", cfg.tool_style.as_str()));
    s.push_str(
        "Shell commands are usually executed by the host before they reach you. \
         You handle natural-language questions and anything that is not a direct \
         shell command. Answer clearly and concisely.\n\
         If you need filesystem facts, use tools rather than inventing paths.\n\n",
    );

    match cfg.tool_style {
        ToolStyle::Text => {
            s.push_str(tools::text_tool_instructions());
            s.push('\n');
        }
        ToolStyle::Auto => {
            s.push_str(
                "Prefer native function/tool calling when available. If you cannot \
                 emit API tool_calls, use the text tool protocol below.\n\n",
            );
            s.push_str(tools::text_tool_instructions());
            s.push('\n');
        }
        ToolStyle::Native => {
            s.push_str(
                "Use the provided API tools (shell_exec, read_file, write_file, \
                 list_dir, load_skill, and any mcp__* tools). Do not invent tool names.\n\n",
            );
        }
    }

    if !cfg.mcp_servers.is_empty() {
        s.push_str(
            "Local MCP tools may be available with names like mcp__cmdfreq__top_commands. \
             When the user asks for their most-used or top 10 commands, call \
             mcp__cmdfreq__top_commands with {\"limit\":10}.\n\n",
        );
    }

    s.push_str(&skills.system_prompt_fragment());
    s
}

fn truncate_args(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}…", &s[..limit])
    }
}

const HELP_TEXT: &str = "\
Meta-commands:
  :help              Show this help
  :sys | :uname      System identity (mode, model, tools, llm) — also: uname
  :mode [name]       Show or set mode (agent|hallucination|root)
  :tools [style]     Show or set tool style (auto|native|text)
  :skills            List skills under /engram/skills
  :model             Show active model / LLM endpoint (detail)
  :top [N]           Show top N most-used commands (default 10)
  :logs [N]          Show last N lines of mistralrs log (default 40)
  :shomagent         Roster background agents (:agents); :shomagent talk N peeks
  :clear             Clear conversation history (hallucination: new session file)
  :exit | :quit      Leave the shell

Top 10 commands:
  :top               or  :top 10
  Or ask in agent mode:  show my top 10 commands
  Tool call: mcp__cmdfreq__top_commands  {\"limit\":10}

Prompt:
  mode:path>         e.g. halluc:/virt>  or  agent:~/work>
  Mode is colored on a TTY (cyan=agent, magenta=halluc, yellow=root).
  Model alias and tool style live in :sys / :model — not the prompt.

Modes:
  agent          LLM + real tools (default); cwd persists across turns
  hallucination  Guided virtual shell (builtins) + LLM; no disk writes
  root           Bypass LLM; BusyBox ash with persistent cwd

Shell-shaped lines (ls, cd, echo, cat, pipelines, redirects, ...) execute
immediately in agent mode — they do not wait on the LLM.
In agent/root, `cd` changes the session directory (shown in the prompt).
  agent: cd ~ → /engram/user    root: cd ~ → /root
Interactive TUI commands (top, mc, vim, …) listed in
  /engram/skills/ignore_llm_for_these_commands.md
run on the real TTY and never go through the LLM — edit that file to extend.
Hallucination mode stays on the virtual filesystem (/virt).
In hallucination mode, common builtins (ls, echo, redirects, cat, pwd)
run against an in-memory virtual filesystem for repeatable guided-shell
behavior; other lines go to the LLM. Nothing touches the real disk.
Sessions auto-save under /engram/.hallucination_shell_history as
hallucination_shell_<timestamp>.json (resume or fresh on enter).

Tool styles:
  auto    Native tool_calls if present, else grep text patterns
  native  OpenAI tool schemas only
  text    Text patterns only (for small models without tool support)

Logs:
  Server logs go to /engram/mistralrs.log by default.
  :logs [N]   show last N lines
  Set MISTRALRS_LOG_TO_TTY=1 to stream logs to the terminal instead.

Spinner (TTY only; silent under pipes / --once / --script):
  ENGRAM_SPINNER=auto|always|off   Status line while waiting on LLM/tools
  ENGRAM_LLM_READY_TRIES=N         Startup model wait budget in seconds (default 600)
  NO_COLOR=1                       Disable splash / prompt colors
";
