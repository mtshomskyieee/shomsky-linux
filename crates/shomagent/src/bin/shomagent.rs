//! shomagent — CLI for the background crew.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use shomagent::daemon::rpc;
use shomagent::protocol::{Request, Response};
use shomagent::rain::{self, LineBuf};
use shomagent::term::RawGuard;
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    /// Generative dream journal (persistent TUI until quit)
    Dream {
        /// Markdown journal path (default dream.md)
        #[arg(default_value = "dream.md")]
        path: String,
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
        Some(Commands::Dream { path }) => dream_loop(path).await,
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
                    "dream" => "~",
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
    match rpc(&Request::Context { id }).await? {
        Response::Ok {
            context: Some(ctx), ..
        } => {
            println!(
                "shomagent talk #{} ({}) — empty line or :q to leave",
                id, ctx.name
            );
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

fn is_quit(line: &str) -> bool {
    matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "quit" | ":q" | ":quit" | "exit"
    )
}

fn classify_line(line: &str) -> Option<DreamInput> {
    let t = line.trim();
    if t.is_empty() {
        return None;
    }
    if is_quit(t) {
        Some(DreamInput::Quit)
    } else {
        Some(DreamInput::Steer(t.to_string()))
    }
}

enum DreamInput {
    Quit,
    Steer(String),
}

async fn dream_loop(path: String) -> Result<()> {
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();

    let id = match rpc(&Request::Dream {
        path: path.clone(),
        name: None,
    })
    .await?
    {
        Response::Ok {
            member: Some(m),
            message,
            ..
        } => {
            if let Some(msg) = message {
                println!("{msg}");
            }
            m.id
        }
        Response::Ok {
            message: Some(m), ..
        } => {
            println!("{m}");
            m.split('#')
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("dream attach returned no member id"))?
        }
        Response::Err { message } => bail!("{message}"),
        _ => bail!("dream attach failed"),
    };

    println!("shomagent dream — {path}");
    println!("type a line to steer the next beat · quit to leave");
    let _ = io::stdout().flush();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc_hook(stop);
    }

    let mut lines = LineBuf::default();
    let raw = if tty { Some(RawGuard::enter()) } else { None };
    // Dedicated stdin pump so a blocking read can never starve the LLM wait.
    let stdin_rx = if tty {
        Some(spawn_stdin_pump(Arc::clone(&stop)))
    } else {
        None
    };

    let mut steering: Option<String> = None;
    let mut beat_index: u32 = 0;

    let result = async {
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            if tty {
                clear_steer_line();
                print_steer_line("thinking…", lines.partial());
            }

            let step_steering = steering.take();
            let step_req = Request::DreamStep {
                id,
                steering: step_steering,
            };
            let step_fut = rpc(&step_req);
            tokio::pin!(step_fut);

            let mut abort_quit = false;
            let reply = loop {
                tokio::select! {
                    resp = &mut step_fut => {
                        break match resp? {
                            Response::Ok { reply: Some(r), .. } => r,
                            Response::Ok { reply: None, .. } => bail!("(no dream reply)"),
                            Response::Err { message } => bail!("{message}"),
                        };
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(40)) => {
                        if stop.load(Ordering::SeqCst) {
                            abort_quit = true;
                            break String::new();
                        }
                        if let Some(rx) = stdin_rx.as_ref() {
                            let changed = drain_stdin_rx(rx, &mut lines);
                            if changed {
                                print_steer_line("thinking…", lines.partial());
                            }
                            match take_dream_input(&mut lines) {
                                Some(DreamInput::Quit) => {
                                    abort_quit = true;
                                    break String::new();
                                }
                                Some(DreamInput::Steer(s)) => {
                                    // Apply to the *next* beat after this one finishes.
                                    steering = Some(s);
                                    print_steer_line("thinking… (steered)", lines.partial());
                                }
                                None => {}
                            }
                        }
                    }
                }
            };

            if abort_quit {
                clear_steer_line();
                println!("quit — leaving dream");
                break;
            }

            clear_steer_line();

            let banner = if beat_index == 0 {
                "── seed ──".to_string()
            } else {
                format!("── beat {beat_index} ──")
            };
            let display = format!("{banner}\n{}", reply.trim_end());
            beat_index += 1;

            let rained = if tty {
                let rx = stdin_rx.as_ref();
                rain::play_text(&display, &mut lines, raw.as_ref(), |lb| {
                    if let Some(rx) = rx {
                        let _ = drain_stdin_rx(rx, lb);
                    }
                })
            } else {
                false
            };
            if !rained {
                println!("{display}");
            }
            if tty {
                print_steer_line("dreaming", lines.partial());
            } else {
                println!("type a line to steer · quit to leave");
            }
            let _ = io::stdout().flush();

            // Decide next action from stdin.
            let next = if tty {
                for _ in 0..20 {
                    if let Some(rx) = stdin_rx.as_ref() {
                        let changed = drain_stdin_rx(rx, &mut lines);
                        if changed {
                            print_steer_line("dreaming", lines.partial());
                        }
                    }
                    if lines.has_completed() || stop.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                // Prefer a line finished during rain / think; else keep polling briefly.
                take_dream_input(&mut lines)
            } else {
                match read_piped_line()? {
                    None => break,
                    Some(line) => classify_line(&line),
                }
            };

            match next {
                Some(DreamInput::Quit) => break,
                Some(DreamInput::Steer(s)) => {
                    steering = Some(s);
                }
                None => {
                    if !tty {
                        break;
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    stop.store(true, Ordering::SeqCst);

    drop(raw);

    let _ = rpc(&Request::DreamEnd { id }).await;
    if let Err(e) = result {
        eprintln!("dream error: {e}");
        return Err(e);
    }
    println!("dream ended — journal kept at {path}");
    Ok(())
}

fn take_dream_input(lines: &mut LineBuf) -> Option<DreamInput> {
    let line = lines.take_latest()?;
    classify_line(&line)
}

fn read_piped_line() -> Result<Option<String>> {
    let mut buf = String::new();
    let n = io::stdin().lock().read_line(&mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(buf))
}

fn ctrlc_hook(stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.store(true, Ordering::SeqCst);
    });
}

/// Background stdin reader so keystrokes never block the LLM wait future.
fn spawn_stdin_pump(stop: Arc<AtomicBool>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        while !stop.load(Ordering::SeqCst) {
            let n = shomagent::term::read_available(&mut buf);
            if n > 0 {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    });
    rx
}

fn drain_stdin_rx(rx: &std::sync::mpsc::Receiver<Vec<u8>>, lines: &mut LineBuf) -> bool {
    let mut changed = false;
    while let Ok(chunk) = rx.try_recv() {
        let before = lines.partial().to_string();
        let before_n = lines.has_completed();
        // Reuse DSR filtering via a temporary feed through poll path:
        // strip ESC[…R then feed the rest.
        let mut i = 0;
        while i < chunk.len() {
            if chunk[i] == 0x1b {
                let mut j = i + 1;
                while j < chunk.len() && chunk[j] != b'R' {
                    j += 1;
                }
                if j < chunk.len() {
                    i = j + 1;
                    continue;
                }
            }
            lines.feed_bytes(&chunk[i..i + 1]);
            i += 1;
        }
        if lines.partial() != before || lines.has_completed() != before_n {
            changed = true;
        }
    }
    changed
}

fn print_steer_line(status: &str, partial: &str) {
    // Show the cursor and echo what the user is typing (raw mode has -echo).
    let mut out = io::stdout().lock();
    let _ = write!(
        out,
        "\x1b[?25h\r\x1b[K{status} · steer> {partial}\x1b[0m"
    );
    let _ = out.flush();
}

fn clear_steer_line() {
    let mut out = io::stdout().lock();
    let _ = write!(out, "\r\x1b[K");
    let _ = out.flush();
}
