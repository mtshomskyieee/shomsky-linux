//! shomagent — CLI for the background crew.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use shomagent::daemon::rpc;
use shomagent::protocol::{Request, Response};
use shomagent::rain::{self, LineBuf};
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        /// Journal path, or `seed` to plant a user-written opening first
        arg0: Option<String>,
        /// Journal path when arg0 is `seed` (default dream.md)
        arg1: Option<String>,
    },
    /// Iteratively refine an idea document (overwrite each pass)
    Refine {
        /// Must be `idea`
        kind: String,
        /// Idea file path (default idea.md)
        path: Option<String>,
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
        Some(Commands::Dream { arg0, arg1 }) => {
            let (path, user_seed) = parse_dream_args(arg0, arg1)?;
            dream_loop(path, user_seed).await
        }
        Some(Commands::Refine { kind, path }) => {
            if kind != "idea" {
                bail!("usage: shomagent refine idea [path]");
            }
            let path = path.unwrap_or_else(|| "idea.md".into());
            refine_loop(path).await
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
                    "dream" => "~",
                    "refine" => "✦",
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

fn parse_dream_args(arg0: Option<String>, arg1: Option<String>) -> Result<(String, bool)> {
    match (arg0.as_deref(), arg1.as_deref()) {
        (None, None) => Ok(("dream.md".into(), false)),
        (Some("seed"), None) => Ok(("dream.md".into(), true)),
        (Some("seed"), Some(p)) => Ok((p.to_string(), true)),
        (Some(p), None) => Ok((p.to_string(), false)),
        (Some(_), Some(_)) => {
            bail!("usage: shomagent dream [path] | shomagent dream seed [path]")
        }
        (None, Some(_)) => unreachable!("arg1 without arg0"),
    }
}

/// Collect a multi-line seed from stdin until `.` alone, quit, or EOF.
fn read_user_seed() -> Result<Option<String>> {
    let stdin = io::stdin();
    let mut lines = Vec::new();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if is_quit(trimmed) && lines.iter().all(|l: &String| l.trim().is_empty()) {
            return Ok(None);
        }
        if trimmed == "." {
            break;
        }
        lines.push(line);
    }
    let text = lines.join("\n");
    if text.trim().is_empty() {
        bail!("empty seed — write something, then a line with only `.`");
    }
    Ok(Some(text))
}

async fn dream_loop(path: String, user_seed: bool) -> Result<()> {
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
    let mut beat_index: u32 = 0;

    if user_seed {
        println!("write your seed below");
        println!("end with a line containing only `.` · Ctrl-D also works · quit to leave");
        let _ = io::stdout().flush();
        match read_user_seed()? {
            None => {
                let _ = rpc(&Request::DreamEnd { id }).await;
                println!("quit — leaving dream");
                return Ok(());
            }
            Some(text) => match rpc(&Request::DreamPlant {
                id,
                text,
            })
            .await?
            {
                Response::Ok {
                    reply: Some(body), ..
                } => {
                    let display = format!("── seed ──\n{}", body.trim_end());
                    beat_index = 1;
                    let rained = if tty {
                        rain::play_text(&display, &mut LineBuf::default(), None, |_| {})
                    } else {
                        false
                    };
                    if !rained {
                        println!("{display}");
                    }
                    if tty {
                        let _ = write!(io::stdout(), "\x1b[?25h");
                    }
                }
                Response::Err { message } => {
                    let _ = rpc(&Request::DreamEnd { id }).await;
                    bail!("{message}");
                }
                _ => {
                    let _ = rpc(&Request::DreamEnd { id }).await;
                    bail!("dream plant failed");
                }
            },
        }
    }

    println!("type a line at the steer> prompt · Enter to send · quit to leave");
    let _ = io::stdout().flush();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc_hook(stop);
    }

    // Cooked stdin + echo: a background thread blocking-reads whole lines so the
    // LLM wait stays async and the user always sees what they type.
    let line_rx = spawn_line_reader(Arc::clone(&stop), tty);

    let mut steering: Option<String> = None;
    let mut stdin_eof = false;
    let mut fail_streak: u32 = 0;

    let result = async {
        // Honor a quit (or early steer) already on stdin — e.g. after
        // `dream seed` collected the plant text from the same pipe.
        if user_seed {
            for _ in 0..50 {
                match drain_line_rx(&line_rx) {
                    LineEvent::Quit => {
                        println!("quit — leaving dream");
                        return Ok(());
                    }
                    LineEvent::Steer(s) => {
                        steering = Some(s);
                        break;
                    }
                    LineEvent::Eof => {
                        stdin_eof = true;
                        break;
                    }
                    LineEvent::None => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            if stdin_eof && !tty {
                println!("quit — leaving dream");
                return Ok(());
            }
        }

        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            show_steer_prompt(tty);

            // Keep a copy so a failed beat can retry with the same steer.
            let step_steering = steering.clone();
            let step_req = Request::DreamStep {
                id,
                steering: step_steering.clone(),
            };
            let step_fut = rpc(&step_req);
            tokio::pin!(step_fut);

            let mut abort_quit = false;
            let mut step_failed: Option<String> = None;
            let think_start = Instant::now();
            let mut spin_frame = 0usize;
            let mut last_spin = Instant::now();
            // Draw first spinner frame immediately so the wait feels alive.
            if tty {
                tick_dream_spinner(spin_frame, steering.is_some(), think_start.elapsed());
                spin_frame = 1;
            }

            let reply = loop {
                tokio::select! {
                    resp = &mut step_fut => {
                        break match resp {
                            Ok(Response::Ok { reply: Some(r), .. }) => {
                                steering = None; // consumed by a successful beat
                                r
                            }
                            Ok(Response::Ok { reply: None, .. }) => {
                                step_failed = Some("(no dream reply)".into());
                                String::new()
                            }
                            Ok(Response::Err { message }) => {
                                step_failed = Some(message);
                                String::new()
                            }
                            Err(e) => {
                                step_failed = Some(format!("{e:#}"));
                                String::new()
                            }
                        };
                    }
                    _ = tokio::time::sleep(Duration::from_millis(40)) => {
                        if stop.load(Ordering::SeqCst) {
                            abort_quit = true;
                            break String::new();
                        }
                        match drain_line_rx(&line_rx) {
                            LineEvent::Quit => {
                                abort_quit = true;
                                break String::new();
                            }
                            LineEvent::Steer(s) => {
                                steering = Some(s);
                                if tty {
                                    tick_dream_spinner(
                                        spin_frame,
                                        true,
                                        think_start.elapsed(),
                                    );
                                }
                            }
                            LineEvent::Eof => {
                                stdin_eof = true;
                            }
                            LineEvent::None => {}
                        }
                        if tty && last_spin.elapsed() >= Duration::from_millis(120) {
                            tick_dream_spinner(
                                spin_frame,
                                steering.is_some(),
                                think_start.elapsed(),
                            );
                            spin_frame = spin_frame.wrapping_add(1);
                            last_spin = Instant::now();
                        }
                    }
                }
            };

            if tty {
                // Finish the steer line so the beat does not collide with it.
                println!();
            }

            if abort_quit {
                println!("quit — leaving dream");
                break;
            }

            if let Some(err) = step_failed {
                fail_streak = fail_streak.saturating_add(1);
                eprintln!("dream: beat failed — {err}");
                if steering.is_none() {
                    steering = step_steering;
                }
                if fail_streak >= 2 {
                    eprintln!(
                        "dream: pausing after repeated failures — type a steer line to retry, or quit"
                    );
                    // Wait for the user instead of hammering a slow model.
                    let mut pause_quit = false;
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            pause_quit = true;
                            break;
                        }
                        match drain_line_rx(&line_rx) {
                            LineEvent::Quit => {
                                pause_quit = true;
                                break;
                            }
                            LineEvent::Steer(s) => {
                                steering = Some(s);
                                fail_streak = 0;
                                break;
                            }
                            LineEvent::Eof => {
                                stdin_eof = true;
                                if !tty {
                                    pause_quit = true;
                                    break;
                                }
                            }
                            LineEvent::None => {}
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    if pause_quit {
                        println!("quit — leaving dream");
                        break;
                    }
                    continue;
                }
                eprintln!("dream: retrying once more…");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            fail_streak = 0;

            let banner = if beat_index == 0 {
                "── seed ──".to_string()
            } else {
                format!("── beat {beat_index} ──")
            };
            let display = format!("{banner}\n{}", reply.trim_end());
            beat_index += 1;

            // Rain is visual only; steer input stays on the cooked line reader.
            let rained = if tty {
                rain::play_text(&display, &mut LineBuf::default(), None, |_| {})
            } else {
                false
            };
            if !rained {
                println!("{display}");
            }
            // Ensure cursor is visible again after rain hides it.
            if tty {
                let _ = write!(io::stdout(), "\x1b[?25h");
            }

            // Apply any line that arrived during rain / the last think slice.
            match drain_line_rx(&line_rx) {
                LineEvent::Quit => break,
                LineEvent::Steer(s) => steering = Some(s),
                LineEvent::Eof => stdin_eof = true,
                LineEvent::None => {}
            }

            if stdin_eof && !tty {
                break;
            }
            if !tty && steering.is_none() {
                // Piped with no further steer queued: one more drain then exit.
                match drain_line_rx(&line_rx) {
                    LineEvent::Quit | LineEvent::Eof | LineEvent::None => break,
                    LineEvent::Steer(s) => steering = Some(s),
                }
                if steering.is_none() {
                    break;
                }
            }
            // Next loop iteration reprints steer> under "thinking…".
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    stop.store(true, Ordering::SeqCst);

    let _ = rpc(&Request::DreamEnd { id }).await;
    if let Err(e) = result {
        eprintln!("dream error: {e}");
        return Err(e);
    }
    println!("dream ended — journal kept at {path}");
    Ok(())
}

enum LineEvent {
    None,
    Quit,
    Steer(String),
    Eof,
}

/// Blocking `read_line` on a thread (cooked terminal, normal echo).
fn spawn_line_reader(stop: Arc<AtomicBool>, echo_prompt_hint: bool) -> Receiver<Option<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        // First prompt is printed by the async loop; this thread only reads.
        let _ = echo_prompt_hint;
        while !stop.load(Ordering::SeqCst) {
            let mut buf = String::new();
            match stdin.lock().read_line(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(None);
                    break;
                }
                Ok(_) => {
                    if tx.send(Some(buf)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = tx.send(None);
                    break;
                }
            }
        }
    });
    rx
}

fn drain_line_rx(rx: &Receiver<Option<String>>) -> LineEvent {
    // One line per call (FIFO) so piped `steer\nquit\n` applies steer then quit.
    match rx.try_recv() {
        Err(TryRecvError::Empty) => LineEvent::None,
        Err(TryRecvError::Disconnected) => LineEvent::Eof,
        Ok(None) => LineEvent::Eof,
        Ok(Some(line)) => match classify_line(&line) {
            Some(DreamInput::Quit) => LineEvent::Quit,
            Some(DreamInput::Steer(s)) => LineEvent::Steer(s),
            None => LineEvent::None,
        },
    }
}

/// Same frames as engram-shell's wait spinner (`- \ | /`).
const DREAM_SPIN: &[char] = &['-', '\\', '|', '/'];

fn spinner_enabled() -> bool {
    match std::env::var("ENGRAM_SPINNER")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "off" | "false" | "no" => false,
        "always" | "1" | "on" | "true" | "yes" => true,
        _ => io::stdout().is_terminal(),
    }
}

fn show_steer_prompt(tty: bool) {
    // Status line (animated while waiting) + cooked steer prompt under it.
    if tty && spinner_enabled() {
        println!("thinking… -");
    } else {
        println!("thinking…");
    }
    print!("steer(type quit to exit)> ");
    let _ = write!(io::stdout(), "\x1b[?25h");
    let _ = io::stdout().flush();
}

fn show_task_prompt(tty: bool) {
    if tty && spinner_enabled() {
        println!("thinking… -");
    } else {
        println!("thinking…");
    }
    print!("task(type quit to exit)> ");
    let _ = write!(io::stdout(), "\x1b[?25h");
    let _ = io::stdout().flush();
}

/// Collect a multi-line refinement task from stdin until `.` alone, quit, or EOF.
fn read_refine_task() -> Result<Option<String>> {
    // Same delimiter UX as dream seed.
    read_user_seed()
}

async fn refine_loop(path: String) -> Result<()> {
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();

    let id = match rpc(&Request::Refine {
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
                .ok_or_else(|| anyhow::anyhow!("refine attach returned no member id"))?
        }
        Response::Err { message } => bail!("{message}"),
        _ => bail!("refine attach failed"),
    };

    println!("shomagent refine — {path}");
    println!("write your refinement task below");
    println!("end with a line containing only `.` · Ctrl-D also works · quit to leave");
    let _ = io::stdout().flush();

    let standing_task = match read_refine_task()? {
        None => {
            let _ = rpc(&Request::RefineEnd { id }).await;
            println!("quit — leaving refine");
            return Ok(());
        }
        Some(text) => text,
    };

    println!("type a line at the task> prompt to replace the task · Enter to send · quit to leave");
    let _ = io::stdout().flush();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc_hook(stop);
    }

    let line_rx = spawn_line_reader(Arc::clone(&stop), tty);

    let mut task = standing_task;
    let mut stdin_eof = false;
    let mut fail_streak: u32 = 0;
    let mut first_pass = true;
    let mut pending_quit = false;

    let result = async {
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            show_task_prompt(tty);

            let step_task = task.clone();
            let step_req = Request::RefineStep {
                id,
                task: step_task.clone(),
            };
            let step_fut = rpc(&step_req);
            tokio::pin!(step_fut);

            let mut abort_quit = false;
            let mut step_failed: Option<String> = None;
            let think_start = Instant::now();
            let mut spin_frame = 0usize;
            let mut last_spin = Instant::now();
            if tty {
                tick_dream_spinner(spin_frame, false, think_start.elapsed());
                spin_frame = 1;
            }

            let reply = loop {
                tokio::select! {
                    resp = &mut step_fut => {
                        break match resp {
                            Ok(Response::Ok { reply: Some(r), .. }) => r,
                            Ok(Response::Ok { reply: None, .. }) => {
                                step_failed = Some("(no refine reply)".into());
                                String::new()
                            }
                            Ok(Response::Err { message }) => {
                                step_failed = Some(message);
                                String::new()
                            }
                            Err(e) => {
                                step_failed = Some(format!("{e:#}"));
                                String::new()
                            }
                        };
                    }
                    _ = tokio::time::sleep(Duration::from_millis(40)) => {
                        if stop.load(Ordering::SeqCst) {
                            abort_quit = true;
                            break String::new();
                        }
                        match drain_line_rx(&line_rx) {
                            LineEvent::Quit => {
                                // Honor quit after the first save; during the first
                                // pass keep going so the task is applied once.
                                if first_pass {
                                    pending_quit = true;
                                } else {
                                    abort_quit = true;
                                    break String::new();
                                }
                            }
                            LineEvent::Steer(s) => {
                                task = s;
                                if tty {
                                    tick_dream_spinner(
                                        spin_frame,
                                        true,
                                        think_start.elapsed(),
                                    );
                                }
                            }
                            LineEvent::Eof => {
                                stdin_eof = true;
                            }
                            LineEvent::None => {}
                        }
                        if tty && last_spin.elapsed() >= Duration::from_millis(120) {
                            tick_dream_spinner(
                                spin_frame,
                                false,
                                think_start.elapsed(),
                            );
                            spin_frame = spin_frame.wrapping_add(1);
                            last_spin = Instant::now();
                        }
                    }
                }
            };

            if tty {
                println!();
            }

            if abort_quit {
                println!("quit — leaving refine");
                break;
            }

            if let Some(err) = step_failed {
                fail_streak = fail_streak.saturating_add(1);
                eprintln!("refine: pass failed — {err}");
                if fail_streak >= 2 {
                    eprintln!(
                        "refine: pausing after repeated failures — type a new task to retry, or quit"
                    );
                    let mut pause_quit = false;
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            pause_quit = true;
                            break;
                        }
                        match drain_line_rx(&line_rx) {
                            LineEvent::Quit => {
                                pause_quit = true;
                                break;
                            }
                            LineEvent::Steer(s) => {
                                task = s;
                                fail_streak = 0;
                                break;
                            }
                            LineEvent::Eof => {
                                stdin_eof = true;
                                if !tty {
                                    pause_quit = true;
                                    break;
                                }
                            }
                            LineEvent::None => {}
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    if pause_quit {
                        println!("quit — leaving refine");
                        break;
                    }
                    continue;
                }
                eprintln!("refine: retrying once more…");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            fail_streak = 0;
            first_pass = false;

            let display = format!("── refine ──\n{}", reply.trim_end());
            let rained = if tty {
                rain::play_text(&display, &mut LineBuf::default(), None, |_| {})
            } else {
                false
            };
            if !rained {
                println!("{display}");
            }
            if tty {
                let _ = write!(io::stdout(), "\x1b[?25h");
            }

            if pending_quit {
                println!("quit — leaving refine");
                break;
            }

            // Apply any line that arrived during rain / the last think slice.
            let mut got_new_task = false;
            match drain_line_rx(&line_rx) {
                LineEvent::Quit => {
                    println!("quit — leaving refine");
                    break;
                }
                LineEvent::Steer(s) => {
                    task = s;
                    got_new_task = true;
                }
                LineEvent::Eof => stdin_eof = true,
                LineEvent::None => {}
            }

            if stdin_eof && !tty && !got_new_task {
                break;
            }
            if !tty {
                if got_new_task {
                    // Another pass with the replacement task.
                    continue;
                }
                // Brief poll so a piped replacement task is not lost to timing.
                let mut leave = true;
                for _ in 0..50 {
                    match drain_line_rx(&line_rx) {
                        LineEvent::Quit | LineEvent::Eof => break,
                        LineEvent::Steer(s) => {
                            task = s;
                            leave = false;
                            break;
                        }
                        LineEvent::None => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
                if leave {
                    break;
                }
                continue;
            }
            // TTY: immediately continue with the standing (or replaced) task.
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    stop.store(true, Ordering::SeqCst);

    let _ = rpc(&Request::RefineEnd { id }).await;
    if let Err(e) = result {
        eprintln!("refine error: {e}");
        return Err(e);
    }
    println!("refine ended — idea kept at {path}");
    Ok(())
}

/// Update the status line above `steer>` without disturbing typed input.
fn tick_dream_spinner(frame: usize, steered: bool, elapsed: Duration) {
    if !spinner_enabled() {
        return;
    }
    let spin = DREAM_SPIN[frame % DREAM_SPIN.len()];
    let dots = match (frame / 2) % 4 {
        0 => ".  ",
        1 => ".. ",
        2 => "...",
        _ => "   ",
    };
    let secs = elapsed.as_secs();
    let label = if steered {
        format!("thinking… (steered) [{spin}] {dots} {secs}s")
    } else {
        format!("thinking… [{spin}] {dots} {secs}s")
    };
    let mut out = io::stdout().lock();
    // Save cursor (on steer> / typed text), rewrite the line above, restore.
    let _ = write!(out, "\x1b7\x1b[1A\r\x1b[K{label}\x1b8");
    let _ = out.flush();
}

fn ctrlc_hook(stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.store(true, Ordering::SeqCst);
    });
}
