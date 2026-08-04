//! Persist hallucination-mode conversation + virtual FS as JSON sessions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::halluc_fs::HallucFs;
use crate::llm::ChatMessage;

pub const HISTORY_DIR_NAME: &str = ".hallucination_shell_history";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HallucSession {
    pub version: u32,
    pub saved_at: String,
    /// Conversation-style messages (system / user / assistant / …).
    pub messages: Vec<ChatMessage>,
    /// Virtual filesystem snapshot (filename → contents).
    pub files: BTreeMap<String, String>,
}

impl HallucSession {
    pub fn from_state(messages: &[ChatMessage], fs: &HallucFs) -> Self {
        Self {
            version: 1,
            saved_at: now_rfc3339_like(),
            messages: messages.to_vec(),
            files: fs.files().clone(),
        }
    }
}

/// Resolve history directory: `ENGRAM_HALLUC_HISTORY_DIR`, else `/engram/…`,
/// else `$HOME/…`, else `./…`.
pub fn history_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ENGRAM_HALLUC_HISTORY_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let engram = PathBuf::from("/engram").join(HISTORY_DIR_NAME);
    if Path::new("/engram").is_dir() {
        return engram;
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(HISTORY_DIR_NAME);
    }
    PathBuf::from(HISTORY_DIR_NAME)
}

pub fn ensure_history_dir() -> Result<PathBuf> {
    let dir = history_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating hallucination history dir {}", dir.display()))?;
    Ok(dir)
}

/// New session path: `hallucination_shell_<YYYYMMDDTHHMMSSZ>.json`
pub fn new_session_path() -> Result<PathBuf> {
    let dir = ensure_history_dir()?;
    let name = format!("hallucination_shell_{}.json", datetimestamp_slug());
    Ok(dir.join(name))
}

pub fn save_session(path: &Path, messages: &[ChatMessage], fs: &HallucFs) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let session = HallucSession::from_state(messages, fs);
    let text = serde_json::to_string_pretty(&session).context("serializing hallucination session")?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn load_session(path: &Path) -> Result<(Vec<ChatMessage>, HallucFs)> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let session: HallucSession =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok((session.messages, HallucFs::from_files(session.files)))
}

/// Sessions newest-first (by filename / mtime).
pub fn list_sessions() -> Result<Vec<PathBuf>> {
    let dir = history_dir();
    if !dir.is_dir() {
        return Ok(vec![]);
    }
    let mut paths: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|x| x.to_str()) == Some("json")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("hallucination_shell_"))
        })
        .map(|p| {
            let mtime = fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            (mtime, p)
        })
        .collect();
    paths.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(paths.into_iter().map(|(_, p)| p).collect())
}

pub fn latest_session() -> Result<Option<PathBuf>> {
    Ok(list_sessions()?.into_iter().next())
}

fn datetimestamp_slug() -> String {
    // Prefer `date -u +%Y%m%dT%H%M%SZ` when available (BusyBox/GNU).
    if let Ok(out) = std::process::Command::new("date")
        .args(["-u", "+%Y%m%dT%H%M%SZ"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == 'T' || c == 'Z')
            {
                return s;
            }
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}Z")
}

fn now_rfc3339_like() -> String {
    if let Ok(out) = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    datetimestamp_slug()
}
