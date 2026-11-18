//! shomloop — record a planned run and install it into the user crontab.

use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use shomloop::{
    default_ledger_path,     ensure_crond, format_logs, list_runs, load, log_dir, next_id, now_stamp,
    parse_frequency, restore_notice, run_job, save, sync_crontab, PlannedRun, FREQUENCY_HELP,
};

#[derive(Debug, Parser)]
#[command(
    name = "shomloop",
    about = "Schedule a command and keep it in ~/.loop/planned_runs.csv",
    long_about = "Quote the frequency when it contains spaces. The rest of the line is the command.\n\n\
                  Examples:\n  \
                  shomloop \"daily at 5pm\" echo hello\n  \
                  shomloop hourly apk update\n  \
                  shomloop list\n  \
                  shomloop logs\n  \
                  shomloop disable 1\n\n\
                  The container entrypoint runs `shomloop restore`, which rewrites crontab from the CSV and starts crond.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Print every planned run.
    List,
    /// Print logs under /tmp/shomloop. Optional id selects one job.
    Logs { id: Option<u32> },
    /// Turn a planned run back on and install its crontab line.
    Enable { id: u32 },
    /// Keep the row, but omit it from crontab.
    Disable { id: u32 },
    /// Remove a planned run and its crontab line.
    Delete { id: u32 },
    /// Replace the frequency and command. The id stays.
    Edit {
        id: u32,
        frequency: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Run one planned job. Crontab calls this.
    Run { id: u32 },
    /// Rewrite crontab from the CSV and start crond if it is not running.
    Restore,
    #[command(external_subcommand, hide = true)]
    Schedule(Vec<String>),
}

fn main() -> ExitCode {
    match dispatch() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("shomloop: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn dispatch() -> Result<i32> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => {
            let runs = list_runs()?;
            print_list(&runs);
            Ok(0)
        }
        Cmd::Logs { id } => {
            let runs = list_runs().unwrap_or_default();
            let text = format_logs(&log_dir(), &runs, id)?;
            print!("{text}");
            Ok(0)
        }
        Cmd::Enable { id } => set_enabled(id, true),
        Cmd::Disable { id } => set_enabled(id, false),
        Cmd::Delete { id } => {
            let path = default_ledger_path();
            let mut runs = load(&path)?;
            let before = runs.len();
            runs.retain(|r| r.id != id);
            if runs.len() == before {
                bail!("no planned run {id}");
            }
            save(&path, &runs)?;
            install(&runs).context("ledger saved; crontab update failed")?;
            println!("deleted {id}");
            Ok(0)
        }
        Cmd::Edit {
            id,
            frequency,
            command,
        } => {
            let cron = parse_frequency(&frequency)?;
            let command = join_cmd(&command)?;
            let path = default_ledger_path();
            let mut runs = load(&path)?;
            let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
                bail!("no planned run {id}");
            };
            run.frequency = frequency.trim().to_string();
            run.cron = cron.clone();
            run.command = command.clone();
            save(&path, &runs)?;
            install(&runs).context("ledger saved; crontab update failed")?;
            println!("updated {id} ({cron}) {command}");
            Ok(0)
        }
        Cmd::Run { id } => run_job(id, &default_ledger_path(), &log_dir()),
        Cmd::Restore => {
            let path = default_ledger_path();
            let runs = load(&path)?;
            let enabled = runs.iter().filter(|r| r.enabled).count();
            install(&runs)?;
            if let Some(msg) = restore_notice(enabled) {
                eprintln!("shomloop: {msg}");
            }
            Ok(0)
        }
        Cmd::Schedule(args) => {
            let Some((frequency, rest)) = args.split_first() else {
                bail!("usage: shomloop \"<frequency>\" <command>\n{FREQUENCY_HELP}");
            };
            let cron = parse_frequency(frequency)?;
            let command = join_cmd(rest)?;
            let path = default_ledger_path();
            let mut runs = load(&path)?;
            let id = next_id(&runs);
            runs.push(PlannedRun {
                id,
                enabled: true,
                frequency: frequency.trim().to_string(),
                cron: cron.clone(),
                command: command.clone(),
                created_at: now_stamp(),
            });
            save(&path, &runs)?;
            install(&runs).context("ledger saved; crontab update failed")?;
            println!("scheduled {id} ({cron}) {command}");
            Ok(0)
        }
    }
}

fn set_enabled(id: u32, enabled: bool) -> Result<i32> {
    let path = default_ledger_path();
    let mut runs = load(&path)?;
    let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
        bail!("no planned run {id}");
    };
    run.enabled = enabled;
    save(&path, &runs)?;
    install(&runs).context("ledger saved; crontab update failed")?;
    if enabled {
        println!("enabled {id}");
    } else {
        println!("disabled {id}");
    }
    Ok(0)
}

fn install(runs: &[PlannedRun]) -> Result<()> {
    sync_crontab(runs)?;
    ensure_crond(&log_dir())?;
    Ok(())
}

fn join_cmd(parts: &[String]) -> Result<String> {
    if parts.is_empty() || parts.iter().all(|p| p.trim().is_empty()) {
        bail!("missing command");
    }
    Ok(parts.join(" "))
}

fn print_list(runs: &[PlannedRun]) {
    if runs.is_empty() {
        println!("no planned runs");
        return;
    }
    for run in runs {
        let state = if run.enabled { "on " } else { "off" };
        println!(
            "{}  {state}  {}  {}  {}",
            run.id, run.cron, run.frequency, run.command
        );
    }
}
