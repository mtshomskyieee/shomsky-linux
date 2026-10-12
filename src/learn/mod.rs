//! Learn verb: intent → plan → confirm → execute → promote to ~/bin.

mod plan;
mod prefix;
mod promote;

use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::modes::Mode;
use crate::spinner::with_spinner;
use crate::tools::{self, shell_exec};

pub use prefix::parse_intent_prefix;
pub use promote::PendingPromote;

/// True when CI should skip interactive confirms.
pub fn learn_auto() -> bool {
    matches!(
        std::env::var("ENGRAM_LEARN_AUTO").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

pub fn learn_stub() -> bool {
    matches!(
        std::env::var("ENGRAM_LEARN_STUB").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Run the full intent → plan → confirm → execute loop.
/// Returns `Some(PendingPromote)` when the user may save on the next turn / inline.
pub async fn run_intent(
    intent: &str,
    cfg: &Config,
    llm: &LlmClient,
    cwd: &mut PathBuf,
    mode: Mode,
) -> Result<Option<PendingPromote>> {
    if mode != Mode::Agent {
        eprintln!(
            "⟡ learn: `,` / `??` intent only works in agent mode (current: {})",
            mode.as_str()
        );
        return Ok(None);
    }

    let plan = with_spinner("planning", plan::plan_command(llm, intent)).await?;
    println!("⟡ plan:");
    for line in plan.command.lines() {
        println!("    {line}");
    }
    if !plan.rationale.is_empty() {
        println!("  ({})", plan.rationale);
    }

    let command = match confirm_plan(&plan.command)? {
        Confirm::Quit => return Ok(None),
        Confirm::Run(cmd) => cmd,
    };

    let home = mode.home_dir();
    let result = with_spinner("running", shell_exec(&cfg.busybox_ash, &command, cwd, &home)).await?;
    tools::adopt_cwd(cwd, result.cwd.clone());
    if result.success {
        crate::packages::record_from_command(&cfg.packages_path, &command);
    }

    let lines = result.output.lines().count();
    let preview = truncate_output(&result.output, 40);
    if !preview.is_empty() {
        print!("{preview}");
        if !preview.ends_with('\n') {
            println!();
        }
    }
    println!("⟡ ran. {lines} lines.");

    Ok(Some(PendingPromote {
        intent: intent.to_string(),
        command,
        output_summary: truncate_chars(&result.output, 2000),
        suggested_name: shomsky::suggest_slug(intent, &plan.command),
    }))
}

enum Confirm {
    Run(String),
    Quit,
}

fn confirm_plan(command: &str) -> Result<Confirm> {
    if learn_auto() {
        return Ok(Confirm::Run(command.to_string()));
    }
    loop {
        eprint!("  [r]un / [e]dit / [q]uit? ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let t = line.trim().to_ascii_lowercase();
        match t.as_str() {
            "" | "r" | "run" | "y" | "yes" => return Ok(Confirm::Run(command.to_string())),
            "q" | "quit" | "n" | "no" => return Ok(Confirm::Quit),
            "e" | "edit" => {
                let edited = edit_command_line(command)?.trim().to_string();
                if edited.is_empty() {
                    continue;
                }
                return Ok(Confirm::Run(edited));
            }
            _ => {
                eprintln!("  type r, e, or q");
            }
        }
    }
}

/// Ask whether to save; if yes, run promote. Returns true if installed.
pub async fn offer_save(llm: &LlmClient, pending: &PendingPromote, force: bool) -> Result<bool> {
    if !learn_auto() {
        eprint!("  [s]ave as command? ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let t = line.trim().to_ascii_lowercase();
        if t != "s" && t != "save" && t != "y" && t != "yes" {
            return Ok(false);
        }
    }

    let name = if learn_auto() {
        pending.suggested_name.clone()
    } else {
        eprint!("  ⟡ name? (suggested: {}) ", pending.suggested_name);
        let _ = io::stderr().flush();
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let t = line.trim();
        if t.is_empty() {
            pending.suggested_name.clone()
        } else {
            t.to_string()
        }
    };

    promote::promote(llm, pending, &name, force).await
}

/// Handle a bare `s` / `save` line when `pending_promote` is set.
pub async fn handle_save_line(
    line: &str,
    llm: &LlmClient,
    pending: &PendingPromote,
) -> Result<Option<bool>> {
    let t = line.trim().to_ascii_lowercase();
    if t != "s" && t != "save" {
        return Ok(None);
    }
    let installed = offer_save(llm, pending, false).await?;
    Ok(Some(installed))
}

/// One editable line, cursor at the end, already filled with `command`.
/// A non-TTY falls back to a plain read (the command is printed first).
fn edit_command_line(command: &str) -> Result<String> {
    let initial = command.replace('\n', " ");
    if !io::stdin().is_terminal() {
        eprintln!("  ⟡ {initial}");
        eprint!("  ⟡ command: ");
        let _ = io::stderr().flush();
        let mut edited = String::new();
        io::stdin().read_line(&mut edited)?;
        return Ok(edited);
    }
    let mut rl = rustyline::DefaultEditor::new()?;
    match rl.readline_with_initial("  ⟡ ", (&initial, "")) {
        Ok(line) => Ok(line),
        Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
            Ok(String::new())
        }
        Err(e) => Err(e.into()),
    }
}

fn truncate_output(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        return s.to_string();
    }
    let mut out = lines[..max_lines].join("\n");
    out.push_str(&format!("\n…[{} more lines]\n", lines.len() - max_lines));
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
