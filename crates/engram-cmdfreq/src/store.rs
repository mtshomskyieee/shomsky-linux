use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Soft cap on unique command keys.
pub const MAX_ENTRIES: usize = 500;
/// Hard cap for `top_commands` / `:top`.
pub const MAX_TOP: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FrequencyStore {
    pub commands: HashMap<String, u64>,
    #[serde(default)]
    pub updated_at: String,
}

impl FrequencyStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        Ok(())
    }

    pub fn record(&mut self, command: &str) {
        let cmd = command.trim();
        if cmd.is_empty() {
            return;
        }
        let entry = self.commands.entry(cmd.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        self.updated_at = now_rfc3339();
        self.evict_if_needed();
    }

    fn evict_if_needed(&mut self) {
        while self.commands.len() > MAX_ENTRIES {
            let victim = self
                .commands
                .iter()
                .min_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)))
                .map(|(k, _)| k.clone());
            if let Some(k) = victim {
                self.commands.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Ranked (command, count), highest first. Optional prefix filter (case-sensitive).
    pub fn top(&self, limit: usize, prefix: &str) -> Vec<(String, u64)> {
        let limit = limit.clamp(1, MAX_TOP);
        let mut items: Vec<(String, u64)> = self
            .commands
            .iter()
            .filter(|(cmd, _)| prefix.is_empty() || cmd.starts_with(prefix))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        items.truncate(limit);
        items
    }

    /// Completions for the current input line, frequency-ranked.
    pub fn completions(&self, line: &str, limit: usize) -> Vec<String> {
        let limit = limit.clamp(1, MAX_TOP);
        let prefix = line;
        let mut items: Vec<(String, u64)> = self
            .commands
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(prefix) && cmd.as_str() != prefix)
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        items.into_iter().take(limit).map(|(c, _)| c).collect()
    }
}

pub fn format_top(items: &[(String, u64)]) -> String {
    if items.is_empty() {
        return "No commands recorded yet.\n\
                Run shell commands in agent or root mode, then try again.\n\
                Tip: `:top` or ask: show my top 10 commands"
            .to_string();
    }
    let mut out = String::new();
    for (i, (cmd, count)) in items.iter().enumerate() {
        out.push_str(&format!("{:>2}. {:>5}  {}\n", i + 1, count, cmd));
    }
    out
}

pub fn default_store_path() -> PathBuf {
    if let Ok(p) = std::env::var("ENGRAM_CMDFREQ_PATH") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    // Persist on the Compose bind mount for /engram/user.
    PathBuf::from("/engram/user/.command_freq.json")
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn records_and_ranks() {
        let mut s = FrequencyStore::default();
        s.record("ls");
        s.record("ls");
        s.record("pwd");
        s.record("  ls  ");
        let top = s.top(10, "");
        assert_eq!(top[0].0, "ls");
        assert_eq!(top[0].1, 3);
        assert_eq!(top[1].0, "pwd");
    }

    #[test]
    fn prefix_filter() {
        let mut s = FrequencyStore::default();
        s.record("git status");
        s.record("git log");
        s.record("ls");
        let top = s.top(10, "git");
        assert_eq!(top.len(), 2);
        assert!(top.iter().all(|(c, _)| c.starts_with("git")));
    }

    #[test]
    fn roundtrip_file() {
        let dir = std::env::temp_dir().join(format!(
            "cmdfreq-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("freq.json");
        let mut s = FrequencyStore::default();
        s.record("echo hi");
        s.save(&path).unwrap();
        let loaded = FrequencyStore::load(&path).unwrap();
        assert_eq!(loaded.commands.get("echo hi"), Some(&1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn completions_ranked() {
        let mut s = FrequencyStore::default();
        s.record("ls -la");
        s.record("ls");
        s.record("ls");
        s.record("ls -la");
        s.record("ls -la");
        let c = s.completions("ls", 10);
        assert_eq!(c[0], "ls -la");
    }
}
