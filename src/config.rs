use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::modes::Mode;

/// How the shell discovers tool invocations from the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolStyle {
    /// Prefer native OpenAI tool_calls; fall back to grepping text patterns.
    #[default]
    Auto,
    /// Only native OpenAI tool_calls (schemas sent to the API).
    Native,
    /// Only text-pattern tools (no schemas; for small / non-tool models).
    Text,
}

impl ToolStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolStyle::Auto => "auto",
            ToolStyle::Native => "native",
            ToolStyle::Text => "text",
        }
    }

    /// Whether to advertise OpenAI tool schemas on the request.
    pub fn send_native_schemas(self) -> bool {
        matches!(self, ToolStyle::Auto | ToolStyle::Native)
    }
}

impl FromStr for ToolStyle {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(ToolStyle::Auto),
            "native" => Ok(ToolStyle::Native),
            "text" | "pattern" | "grep" => Ok(ToolStyle::Text),
            other => Err(format!(
                "unknown tool_style '{other}' (expected auto|native|text)"
            )),
        }
    }
}

/// Local stdio MCP server (spawned as a child of engram-shell). No URLs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Operating mode: agent | hallucination | root
    pub mode: Mode,
    /// Whether root-mode is allowed
    pub allow_root_mode: bool,
    /// OpenAI-compatible base URL (no trailing /v1)
    pub llm_url: String,
    /// Model name advertised to the API (usually "default" for mistral.rs)
    pub llm_model: String,
    /// Directory containing skill markdown files
    pub skills_dir: PathBuf,
    /// Alias of the active model (from models.toml / SHOMSKY_MODEL)
    pub model_alias: String,
    /// Maximum characters of tool output returned to the model
    pub tool_output_limit: usize,
    /// Shell used for shell_exec and root-mode
    pub busybox_ash: PathBuf,
    /// Hostname shown in the prompt
    pub hostname: String,
    /// Tool discovery strategy
    pub tool_style: ToolStyle,
    /// Max messages retained in history (system + turns). Trims oldest non-system.
    pub max_context_messages: usize,
    /// Execute shell-shaped lines immediately without waiting for the LLM
    pub direct_shell: bool,
    /// In-container stdio MCP servers (no network transports).
    pub mcp_servers: Vec<McpServerConfig>,
    /// Path to command-frequency JSON store (shared with Completer / :top).
    pub cmdfreq_path: PathBuf,
    /// Path to package install/uninstall ledger (`apk add` / `apk del`).
    pub packages_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let model_alias = std::env::var("SHOMSKY_MODEL").unwrap_or_else(|_| "mistral".into());
        let tool_style = default_tool_style_for_alias(&model_alias);
        Self {
            mode: Mode::Agent,
            allow_root_mode: true,
            llm_url: "http://127.0.0.1:1234".into(),
            llm_model: "default".into(),
            skills_dir: PathBuf::from("/engram/skills"),
            model_alias,
            tool_output_limit: 16_384,
            busybox_ash: PathBuf::from("/bin/busybox"),
            hostname: hostname_fallback(),
            tool_style,
            max_context_messages: 24,
            direct_shell: true,
            mcp_servers: Vec::new(),
            cmdfreq_path: PathBuf::from("/engram/user/.command_freq.json"),
            packages_path: PathBuf::from("/engram/user/.packages.json"),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;

        if let Ok(alias) = std::env::var("SHOMSKY_MODEL") {
            if !alias.is_empty() {
                cfg.model_alias = alias;
            }
        }
        if let Ok(mode) = std::env::var("ENGRAM_MODE") {
            if let Ok(m) = mode.parse::<Mode>() {
                cfg.mode = m;
            }
        }
        if let Ok(url) = std::env::var("ENGRAM_LLM_URL") {
            if !url.is_empty() {
                cfg.llm_url = url;
            }
        }
        if let Ok(style) = std::env::var("ENGRAM_TOOL_STYLE") {
            if let Ok(s) = style.parse::<ToolStyle>() {
                cfg.tool_style = s;
            }
        } else if cfg.model_alias == "tiny" && cfg.tool_style == ToolStyle::Auto {
            // Tiny models rarely support native tool_calls — prefer text patterns.
            cfg.tool_style = ToolStyle::Text;
        }
        if let Ok(n) = std::env::var("ENGRAM_MAX_CONTEXT") {
            if let Ok(n) = n.parse::<usize>() {
                cfg.max_context_messages = n.max(2);
            }
        }
        if let Ok(v) = std::env::var("ENGRAM_DIRECT_SHELL") {
            cfg.direct_shell = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
        }
        if let Ok(p) = std::env::var("ENGRAM_CMDFREQ_PATH") {
            if !p.is_empty() {
                cfg.cmdfreq_path = PathBuf::from(p);
            }
        }
        if let Ok(p) = std::env::var("ENGRAM_PACKAGES_PATH") {
            if !p.is_empty() {
                cfg.packages_path = PathBuf::from(p);
            }
        }
        Ok(cfg)
    }

    /// Ensure cmdfreq MCP children inherit the configured store path.
    pub fn apply_cmdfreq_env(&mut self) {
        let path = self.cmdfreq_path.display().to_string();
        for srv in &mut self.mcp_servers {
            if srv.name == "cmdfreq" {
                srv.env
                    .entry("ENGRAM_CMDFREQ_PATH".into())
                    .or_insert_with(|| path.clone());
            }
        }
    }
}

fn default_tool_style_for_alias(alias: &str) -> ToolStyle {
    match alias {
        "tiny" => ToolStyle::Text,
        _ => ToolStyle::Auto,
    }
}

fn hostname_fallback() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shomsky".into())
}
