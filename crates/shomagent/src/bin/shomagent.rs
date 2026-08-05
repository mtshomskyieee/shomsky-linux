//! shomagent — CLI for the background crew.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use shomagent::daemon::rpc;
use shomagent::protocol::{Request, Response};
use std::io::{self, BufRead, Write};

#[derive(Parser, Debug)]
#[command(
    name = "shomagent",
    about = "Background agents you can hold a conversation with",
    long_about = "Spawn watchers that do the boring vigilance and only interrupt when they've earned it."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Attach to a command stream and watch for a brief
    Watch {
        /// Shell command to run
        command: String,
        /// What to tell you about
        #[arg(long = "tell-me", default_value = "")]
        tell_me: String,
        /// Member display name
        #[arg(long)]
        name: Option<String>,
        /// Write drop-in under ./shomagent_crew.d/
        #[arg(long)]
        persist: bool,
    },
    /// Same as watch (alias for the pitch)
    Spawn {
        command: String,
        #[arg(long = "tell-me", default_value = "")]
        tell_me: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        persist: bool,
    },
    /// Scoped chat with one member's stream
    Talk {
        id: u32,
    },
    /// One-shot question to a member (non-interactive talk)
    Ask {
        /// Member id from the roster
        id: u32,
        /// Question (remaining words joined)
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
    },
    /// Attach a document read-only (ask/talk only)
    Read {
        /// Path to create or attach (e.g. details.md)
        path: String,
        #[arg(long = "tell-me", default_value = "")]
        tell_me: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Attach a document with read + write (ask/talk + write)
    #[command(name = "readwrite")]
    ReadWrite {
        path: String,
        #[arg(long = "tell-me", default_value = "")]
        tell_me: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Append text into a readwrite document member
    Write {
        /// Member id (must be readwrite)
        id: u32,
        /// Text to save (remaining words joined)
        #[arg(required = true, num_args = 1..)]
        content: Vec<String>,
        /// Replace the whole file instead of appending
        #[arg(long)]
        replace: bool,
    },
    /// Pause a member
    Pause {
        id: u32,
    },
    /// Resume a paused member
    Resume {
        id: u32,
    },
    /// Stop a member
    Stop {
        id: u32,
    },
    /// List members (same as bare `shomagent`)
    Roster,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None | Some(Commands::Roster) => print_roster().await,
        Some(Commands::Watch {
            command,
            tell_me,
            name,
            persist,
        })
        | Some(Commands::Spawn {
            command,
            tell_me,
            name,
            persist,
        }) => {
            let req = Request::Spawn {
                command,
                tell_me,
                name,
                watch: true,
                persist,
            };
            match rpc(&req).await? {
                Response::Ok { message, member, .. } => {
                    if let Some(msg) = message {
                        println!("{msg}");
                    } else if let Some(m) = member {
                        println!(
                            "⟡ shomagent#{} attached (pid {})",
                            m.id,
                            m.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
                        );
                    }
                    Ok(())
                }
                Response::Err { message } => bail!("{message}"),
            }
        }
        Some(Commands::Talk { id }) => talk_loop(id).await,
        Some(Commands::Ask { id, question }) => ask_once(id, &question.join(" ")).await,
        Some(Commands::Read {
            path,
            tell_me,
            name,
        }) => doc_attach(Request::Read {
            path,
            tell_me,
            name,
        })
        .await,
        Some(Commands::ReadWrite {
            path,
            tell_me,
            name,
        }) => doc_attach(Request::ReadWrite {
            path,
            tell_me,
            name,
        })
        .await,
        Some(Commands::Write {
            id,
            content,
            replace,
        }) => {
            let content = content.join(" ");
            if content.trim().is_empty() {
                bail!("usage: shomagent write <id> <text>");
            }
            match rpc(&Request::Write {
                id,
                content,
                replace,
            })
            .await?
            {
                Response::Ok { message, .. } => {
                    if let Some(m) = message {
                        println!("{m}");
                    }
                    Ok(())
                }
                Response::Err { message } => bail!("{message}"),
            }
        }
        Some(Commands::Pause { id }) => simple(Request::Pause { id }).await,
        Some(Commands::Resume { id }) => simple(Request::Resume { id }).await,
        Some(Commands::Stop { id }) => simple(Request::Stop { id }).await,
    }
}

async fn doc_attach(req: Request) -> Result<()> {
    match rpc(&req).await? {
        Response::Ok { message, .. } => {
            if let Some(m) = message {
                println!("{m}");
            }
            Ok(())
        }
        Response::Err { message } => bail!("{message}"),
    }
}

async fn simple(req: Request) -> Result<()> {
    match rpc(&req).await? {
        Response::Ok { message, .. } => {
            if let Some(m) = message {
                println!("{m}");
            }
            Ok(())
        }
        Response::Err { message } => bail!("{message}"),
    }
}

async fn print_roster() -> Result<()> {
    match rpc(&Request::Roster).await? {
        Response::Ok { roster, .. } => {
            if roster.is_empty() {
                println!("(no shomagent members)");
                return Ok(());
            }
            for m in roster {
                let glyph = match m.state.as_str() {
                    "running" => "▶",
                    "paused" => "⏸",
                    "read" => "◇",
                    "readwrite" => "◆",
                    _ => "■",
                };
                let runtime = format_runtime(m.runtime_secs);
                println!(
                    "  #{} {:12} {} {:4}   {} finding{}",
                    m.id,
                    m.name,
                    glyph,
                    runtime,
                    m.findings,
                    if m.findings == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }
        Response::Err { message } => bail!("{message}"),
    }
}

fn format_runtime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

async fn ask_once(id: u32, question: &str) -> Result<()> {
    let question = question.trim();
    if question.is_empty() {
        bail!("usage: shomagent ask <id> <question>");
    }
    match rpc(&Request::Talk {
        id,
        message: question.to_string(),
    })
    .await?
    {
        Response::Ok { reply: Some(r), .. } => {
            println!("#{id} > {r}");
            Ok(())
        }
        Response::Ok { reply: None, .. } => bail!("(no reply)"),
        Response::Err { message } => bail!("{message}"),
    }
}

async fn talk_loop(id: u32) -> Result<()> {
    // Verify member exists
    match rpc(&Request::Context { id }).await? {
        Response::Ok {
            context: Some(ctx), ..
        } => {
            println!("shomagent talk #{} ({}) — empty line or :q to leave", id, ctx.name);
        }
        Response::Err { message } => bail!("{message}"),
        _ => bail!("no context for #{id}"),
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    loop {
        print!("you> ");
        stdout.flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() || line == ":q" || line == ":quit" || line == "exit" {
            break;
        }
        match rpc(&Request::Talk {
            id,
            message: line.to_string(),
        })
        .await?
        {
            Response::Ok { reply: Some(r), .. } => {
                println!("#{id} > {r}");
            }
            Response::Err { message } => {
                eprintln!("error: {message}");
            }
            _ => eprintln!("(no reply)"),
        }
    }
    Ok(())
}
