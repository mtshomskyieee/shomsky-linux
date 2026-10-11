//! Ledger of packages installed / removed via `apk add` / `apk del`.
//!
//! Persisted under `/engram/user` so Compose bind-mounts keep history.
//! Surfaced in `:menu` → Packages and `:packages`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Soft cap on unique package names tracked.
pub const MAX_ENTRIES: usize = 500;

/// Image essentials — menu warns before uninstall; skill refuses without insist.
pub const ESSENTIAL_PACKAGES: &[&str] = &[
    "busybox",
    "bash",
    "bats",
    "ca-certificates",
    "curl",
    "libgcc",
    "libstdc++",
    "openssl",
];

pub fn is_essential(name: &str) -> bool {
    ESSENTIAL_PACKAGES.iter().any(|p| *p == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageStatus {
    Installed,
    Removed,
}

impl PackageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageStatus::Installed => "installed",
            PackageStatus::Removed => "removed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub status: PackageStatus,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageStore {
    pub packages: BTreeMap<String, PackageEntry>,
    #[serde(default)]
    pub updated_at: String,
}

impl PackageStore {
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

    pub fn record_install(&mut self, name: &str) {
        self.set(name, PackageStatus::Installed);
    }

    pub fn record_remove(&mut self, name: &str) {
        self.set(name, PackageStatus::Removed);
    }

    fn set(&mut self, name: &str, status: PackageStatus) {
        let name = name.trim();
        if name.is_empty() || name.starts_with('-') {
            return;
        }
        // Strip apk version pin: neovim=0.9.5-r0 → neovim
        let name = name.split('=').next().unwrap_or(name).trim();
        if name.is_empty() {
            return;
        }
        let now = now_rfc3339();
        self.packages.insert(
            name.to_string(),
            PackageEntry {
                status,
                updated_at: now.clone(),
            },
        );
        self.updated_at = now;
        self.evict_if_needed();
    }

    fn evict_if_needed(&mut self) {
        while self.packages.len() > MAX_ENTRIES {
            let victim = self
                .packages
                .iter()
                .min_by(|a, b| {
                    a.1.updated_at
                        .cmp(&b.1.updated_at)
                        .then_with(|| a.0.cmp(b.0))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = victim {
                self.packages.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Installed first, then removed; each group by name.
    pub fn list_sorted(&self) -> Vec<(&str, &PackageEntry)> {
        let mut items: Vec<(&str, &PackageEntry)> = self
            .packages
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        items.sort_by(|a, b| {
            status_rank(a.1.status)
                .cmp(&status_rank(b.1.status))
                .then_with(|| a.0.cmp(b.0))
        });
        items
    }
}

fn status_rank(s: PackageStatus) -> u8 {
    match s {
        PackageStatus::Installed => 0,
        PackageStatus::Removed => 1,
    }
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Compact UTC-ish stamp without chrono dependency.
    format!("{secs}")
}

/// Apply mutations parsed from a successful shell command line.
pub fn record_from_command(path: &Path, command: &str) {
    let mutations = parse_apk_mutations(command);
    if mutations.is_empty() {
        return;
    }
    let mut store = PackageStore::load(path).unwrap_or_default();
    for (status, name) in mutations {
        match status {
            PackageStatus::Installed => store.record_install(&name),
            PackageStatus::Removed => store.record_remove(&name),
        }
    }
    let _ = store.save(path);
}

/// Parse `apk add` / `apk del` package names from a BusyBox ash command line.
pub fn parse_apk_mutations(command: &str) -> Vec<(PackageStatus, String)> {
    let mut out = Vec::new();
    for segment in split_shell_segments(command) {
        let tokens = shell_tokens(segment);
        let Some(apk_idx) = tokens.iter().position(|t| t == "apk") else {
            continue;
        };
        let Some(action) = tokens.get(apk_idx + 1).map(|s| s.as_str()) else {
            continue;
        };
        let status = match action {
            "add" => PackageStatus::Installed,
            "del" | "delete" | "remove" => PackageStatus::Removed,
            _ => continue,
        };
        for tok in tokens.iter().skip(apk_idx + 2) {
            if tok.starts_with('-') {
                continue;
            }
            // Stop at redirections
            if tok == ">" || tok == ">>" || tok == "<" || tok.starts_with('>') {
                break;
            }
            out.push((status, tok.clone()));
        }
    }
    out
}

fn split_shell_segments(command: &str) -> Vec<&str> {
    let mut segs = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b';' || bytes[i] == b'\n' {
            segs.push(&command[start..i]);
            start = i + 1;
            i += 1;
            continue;
        }
        if i + 1 < bytes.len()
            && ((bytes[i] == b'&' && bytes[i + 1] == b'&')
                || (bytes[i] == b'|' && bytes[i + 1] == b'|'))
        {
            segs.push(&command[start..i]);
            start = i + 2;
            i += 2;
            continue;
        }
        // Do not split on single `|` (pipeline) — apk rarely piped; treat whole as one.
        i += 1;
    }
    segs.push(&command[start..]);
    segs
}

fn shell_tokens(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn format_list(store: &PackageStore) -> String {
    let items = store.list_sorted();
    if items.is_empty() {
        return "No packages tracked yet. Successful `apk add` / `apk del` are recorded."
            .to_string();
    }
    let mut lines = Vec::new();
    lines.push(format!(
        "{:<20} {:<10} {}",
        "PACKAGE", "STATUS", "UPDATED"
    ));
    for (name, entry) in items {
        lines.push(format!(
            "{:<20} {:<10} {}",
            name,
            entry.status.as_str(),
            entry.updated_at
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_path(prefix: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("engram-packages-{prefix}-{n}.json"))
    }

    #[test]
    fn parse_apk_add_simple() {
        let m = parse_apk_mutations("apk add neovim");
        assert_eq!(m, vec![(PackageStatus::Installed, "neovim".into())]);
    }

    #[test]
    fn parse_apk_add_flags_and_multi() {
        let m = parse_apk_mutations("apk add --no-cache neovim htop");
        assert_eq!(
            m,
            vec![
                (PackageStatus::Installed, "neovim".into()),
                (PackageStatus::Installed, "htop".into()),
            ]
        );
    }

    #[test]
    fn parse_apk_del() {
        let m = parse_apk_mutations("apk del neovim");
        assert_eq!(m, vec![(PackageStatus::Removed, "neovim".into())]);
    }

    #[test]
    fn parse_compound() {
        let m = parse_apk_mutations("apk update && apk add neovim");
        assert_eq!(m, vec![(PackageStatus::Installed, "neovim".into())]);
    }

    #[test]
    fn parse_ignores_search() {
        assert!(parse_apk_mutations("apk search -q neovim").is_empty());
    }

    #[test]
    fn parse_version_pin() {
        let m = parse_apk_mutations("apk add neovim=0.9.5-r0");
        assert_eq!(m, vec![(PackageStatus::Installed, "neovim=0.9.5-r0".into())]);
        let mut store = PackageStore::default();
        store.record_install("neovim=0.9.5-r0");
        assert!(store.packages.contains_key("neovim"));
    }

    #[test]
    fn round_trip_save() {
        let path = tmp_path("rt");
        let mut store = PackageStore::default();
        store.record_install("neovim");
        store.record_remove("htop");
        store.save(&path).unwrap();
        let loaded = PackageStore::load(&path).unwrap();
        assert_eq!(
            loaded.packages["neovim"].status,
            PackageStatus::Installed
        );
        assert_eq!(loaded.packages["htop"].status, PackageStatus::Removed);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn record_from_command_writes() {
        let path = tmp_path("cmd");
        record_from_command(&path, "apk add neovim");
        let loaded = PackageStore::load(&path).unwrap();
        assert_eq!(
            loaded.packages["neovim"].status,
            PackageStatus::Installed
        );
        record_from_command(&path, "apk del neovim");
        let loaded = PackageStore::load(&path).unwrap();
        assert_eq!(loaded.packages["neovim"].status, PackageStatus::Removed);
        let _ = fs::remove_file(&path);
    }
}
