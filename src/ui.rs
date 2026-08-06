//! TTY splash, colored prompts, and system identity (`:sys` / `uname`).

use std::io::{self, IsTerminal, Write};

use crate::config::Config;
use crate::modes::Mode;

const CHARTER: &str = "agents · rust · native LLM in the shell";

/// SHOMSKY — both S letters use the tall Cool S. Also rasterized by
/// `matrix.rs` as the mask the digital rain crystallizes into.
pub const SPLASH_ART: &str = r#"
     ^  AGENTS                                ^
    / \     shomagent                        / \   shomagent watch "<cwd>" --tell-me "..."
  /     \   shomagent readwrite fil.md     /     \ shomagent spawn "<cmd>"
/         \ shomagent ask 1 "question.." /         \   NEW COMMAND begin with a comma example: 
|    |    ||shomagent talk 1             |    |    ||  , show the five largest files
|    |    ||                             |    |    ||
\    \    /   _   _     ___     __  __   \    \    /   _  __  __   __
 \    \  /   | | | |   / _ \   |  \/  |   \    \  /   | |/ /  \ \ / /
  \    \/    | |_| |  | | | |  | |\/| |    \    \/    | ' /    \ V /
  /\    \    |  _  |  | |_| |  | |  | |    /\    \    | . \     | |
 /  \    \   |_| |_|   \___/   |_|  |_|   /  \    \   |_|\_\    |_|
/    \    \                              /    \    \
|    |    || MODES                       |    |    ||  === LINUX ===
|    |    || :mode agent = LLM assist    |    |    ||  - LLM + Shell
\         /  :mode hallucination =       \         /   - 2 cool esses
 \       /          LLM dreams it's a CLI \       /
   \   /    :help = shell options           \   /
     v                                        v
"#;

/// Whether we should paint splash / colors on this TTY.
pub fn splash_enabled() -> bool {
    if no_color() {
        return false;
    }
    io::stdout().is_terminal()
}

pub fn color_enabled() -> bool {
    if no_color() {
        return false;
    }
    io::stdout().is_terminal() && term_ok()
}

fn no_color() -> bool {
    match std::env::var("NO_COLOR") {
        Ok(v) if !v.is_empty() => true,
        _ => false,
    }
}

fn term_ok() -> bool {
    matches!(std::env::var("TERM"), Ok(t) if !t.is_empty() && t != "dumb")
}

/// ANSI mode color (cyan / magenta / yellow). Empty when color is off.
pub fn mode_color(mode: Mode) -> &'static str {
    if !color_enabled() {
        return "";
    }
    match mode {
        Mode::Agent => "\x1b[36m",        // cyan
        Mode::Hallucination => "\x1b[35m", // magenta
        Mode::Root => "\x1b[33m",          // yellow
    }
}

pub fn reset() -> &'static str {
    if color_enabled() {
        "\x1b[0m"
    } else {
        ""
    }
}

/// Working directory shown in the prompt.
/// Hallucination mode uses the virtual root `/virt` (never the real process cwd).
pub fn prompt_path(mode: Mode, cwd: &std::path::Path) -> String {
    match mode {
        Mode::Hallucination => "/virt".into(),
        Mode::Agent | Mode::Root => shorten_path(cwd, &mode.home_dir()),
    }
}

fn shorten_path(path: &std::path::Path, home: &std::path::Path) -> String {
    let path_str = path.to_string_lossy();
    let home_str = home.to_string_lossy();
    if path_str == home_str {
        return "~".into();
    }
    if let Some(rest) = path_str.strip_prefix(home_str.as_ref()) {
        if rest.starts_with('/') {
            return format!("~{rest}");
        }
    }
    path_str.into_owned()
}

/// Slim readline prompt: `halluc:/virt> ` with optional colored mode tag.
///
/// rustyline 15 measures ANSI escape width itself — do **not** wrap with
/// readline `\x01`/`\x02` ignore markers (those were shifting the cursor
/// several columns to the right of `>`).
pub fn prompt_string(mode: Mode, cwd: &std::path::Path) -> String {
    let tag = mode.prompt_tag();
    let path = prompt_path(mode, cwd);
    if color_enabled() {
        format!("{}{tag}{}:{path}> ", mode_color(mode), reset())
    } else {
        format!("{tag}:{path}> ")
    }
}

/// Big colored splash — TTY only. No-op otherwise.
pub fn print_splash(cfg: &Config) {
    if !splash_enabled() {
        return;
    }
    let accent = if color_enabled() { "\x1b[32m" } else { "" }; // green
    let dim = if color_enabled() { "\x1b[2m" } else { "" };
    let bold = if color_enabled() { "\x1b[1m" } else { "" };
    let r = reset();
    let mode_c = mode_color(cfg.mode);

    // The rain reveals the art in place and leaves the cursor exactly where the
    // static write below would have; only print the art ourselves if it didn't.
    let drawn = crate::matrix::play(accent);

    let mut out = io::stdout().lock();
    if !drawn {
        let _ = writeln!(out, "{accent}{SPLASH_ART}{r}");
    }
    let _ = writeln!(
        out,
        "  {bold}Shomsky Linux{r}  {dim}{CHARTER}{r}"
    );
    let _ = writeln!(
        out,
        "  {dim}mode{r} {mode_c}{}{r}  {dim}·  :help  ·  :sys{r}",
        cfg.mode.prompt_tag()
    );
    let _ = writeln!(out);
    let _ = out.flush();
}

/// Compact post-ready line after the loading bar finishes.
pub fn print_ready_line(cfg: &Config, secs: u64, cwd: &std::path::Path) {
    if !splash_enabled() {
        println!("model ready ({secs}s)");
        return;
    }
    let dim = if color_enabled() { "\x1b[2m" } else { "" };
    let r = reset();
    let mode_c = mode_color(cfg.mode);
    println!(
        "  ready ({secs}s)  {mode_c}{}{r}:{path}  {dim}type :help · :sys{r}",
        cfg.mode.prompt_tag(),
        path = prompt_path(cfg.mode, cwd)
    );
    println!();
}

/// Identity dump for `:sys` / `uname` (always prints; no TTY gate).
pub fn print_sys(cfg: &Config, cwd: &std::path::Path) {
    println!("Shomsky Linux  engram-shell");
    println!("{CHARTER}");
    println!(
        "mode={}  model={}  tools={}",
        cfg.mode,
        cfg.model_alias,
        cfg.tool_style.as_str()
    );
    println!(
        "llm={}  model_id={}",
        cfg.llm_url, cfg.llm_model
    );
    println!(
        "host={}  path={}  context={}",
        cfg.hostname,
        prompt_path(cfg.mode, cwd),
        cfg.max_context_messages
    );
    if let Ok(ver) = std::env::var("SHOMSKY_VERSION") {
        if !ver.is_empty() {
            println!("version={ver}");
        }
    }
}

/// True when the line is a local identity query (`uname`, `uname -a`, …).
pub fn is_sys_command(line: &str) -> bool {
    let t = line.trim();
    t == "uname"
        || t == "uname -a"
        || t == "uname -all"
        || t.starts_with("uname ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_has_mode_and_virt_path() {
        // Force no color so the string is stable in CI.
        std::env::set_var("NO_COLOR", "1");
        let p = prompt_string(Mode::Hallucination, std::path::Path::new("/tmp"));
        assert_eq!(p, "halluc:/virt> ");
        // No readline ignore markers (they break rustyline cursor column).
        assert!(!p.as_bytes().contains(&0x01));
        assert!(!p.as_bytes().contains(&0x02));
    }

    #[test]
    fn prompt_shortens_mode_home_to_tilde() {
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(
            prompt_string(Mode::Agent, std::path::Path::new("/engram/user")),
            "agent:~> "
        );
        assert_eq!(
            prompt_string(Mode::Root, std::path::Path::new("/root")),
            "root:~> "
        );
        assert_eq!(
            prompt_string(Mode::Agent, std::path::Path::new("/engram/user/work")),
            "agent:~/work> "
        );
    }

    #[test]
    fn uname_is_sys_command() {
        assert!(is_sys_command("uname"));
        assert!(is_sys_command("uname -a"));
        assert!(!is_sys_command("unamex"));
        assert!(!is_sys_command("echo uname"));
    }
}

