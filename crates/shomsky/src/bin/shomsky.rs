use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use shomsky::{publish_apk, validate_name, LearnPaths};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "shomsky",
    about = "Shomsky Linux helpers (learn publish)",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Package a learned ~/bin command as an apk (installs to /usr/local/bin).
    Publish {
        /// Command name (must exist in ~/bin/.learn/).
        name: String,
        /// Override agent home (default: ENGRAM_USER_DIR or HOME or /engram/user).
        #[arg(long, env = "ENGRAM_USER_DIR")]
        home: Option<PathBuf>,
    },
    /// List learned commands from the registry directory.
    List {
        #[arg(long, env = "ENGRAM_USER_DIR")]
        home: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("shomsky: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Commands::Publish { name, home } => {
            validate_name(&name)?;
            let paths = paths_for(home);
            let apk = publish_apk(&paths, &name)?;
            println!("✓ wrote {}", apk.display());
            println!(
                "  install with: apk add --allow-untrusted {}",
                apk.display()
            );
            Ok(())
        }
        Commands::List { home } => {
            let paths = paths_for(home);
            let dir = paths.registry_dir();
            if !dir.is_dir() {
                println!("(no learned commands)");
                return Ok(());
            }
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", dir.display()))?
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().into_string().ok()?;
                    n.strip_suffix(".toml").map(|s| s.to_string())
                })
                .collect();
            names.sort();
            if names.is_empty() {
                println!("(no learned commands)");
            } else {
                for n in names {
                    println!("{n}");
                }
            }
            Ok(())
        }
    }
}

fn paths_for(home: Option<PathBuf>) -> LearnPaths {
    match home {
        Some(h) if !h.as_os_str().is_empty() => LearnPaths::new(h),
        _ => LearnPaths::from_env(),
    }
}

#[allow(dead_code)]
fn _ensure_anyhow() -> Result<()> {
    bail!("unused")
}
