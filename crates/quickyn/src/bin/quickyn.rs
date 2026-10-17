//! quickyn — yes/no decision with confidence from the current local model.

use anyhow::{bail, Context, Result};
use clap::Parser;
use quickyn::{
    build_user_message, file_size_warning, parse_verdict, should_warn_file_size, system_prompt,
    ChatMessage, LlmClient, EXIT_ERROR,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "quickyn",
    about = "Yes/no decision with confidence (local LLM)",
    long_about = "Ask the current model a yes/no question. Optional file is chain-of-thought \
                  context placed before the question.\n\n\
                  Output: one line on stdout — `yes 0.91` or `no 0.42`.\n\
                  Exit: 0 = yes, 1 = no, 2 = error.\n\n\
                  Examples:\n  \
                  quickyn \"is paris in france?\"\n  \
                  quickyn \"ship this?\" notes.md"
)]
struct Cli {
    /// Yes/no question (quote when it contains spaces).
    question: String,
    /// Optional file used as pre-prompt context before the question.
    file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("quickyn: {e:#}");
            ExitCode::from(EXIT_ERROR as u8)
        }
    }
}

async fn run() -> Result<i32> {
    let cli = Cli::parse();
    let question = cli.question.trim();
    if question.is_empty() {
        bail!("usage: quickyn <question> [file]");
    }

    let file_body = if let Some(path) = &cli.file {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        let n = body.chars().count();
        if should_warn_file_size(&body) {
            eprintln!("{}", file_size_warning(&path.display().to_string(), n));
        }
        Some(body)
    } else {
        None
    };

    let messages = vec![
        ChatMessage::system(system_prompt()),
        ChatMessage::user(build_user_message(
            question,
            file_body.as_deref(),
        )),
    ];

    let client = LlmClient::from_env()?;
    let raw = client.chat(&messages, 64).await?;
    let Some(verdict) = parse_verdict(&raw) else {
        eprintln!("quickyn: could not parse decision from model reply:");
        eprintln!("{raw}");
        return Ok(EXIT_ERROR);
    };

    println!("{}", verdict.format_line());
    Ok(verdict.exit_code())
}
