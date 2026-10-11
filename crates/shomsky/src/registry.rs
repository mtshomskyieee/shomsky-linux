use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::paths::LearnPaths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnEntry {
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    pub intent: String,
    pub command: String,
    #[serde(default)]
    pub flags: Vec<String>,
    pub script_path: String,
    pub man_path: String,
    pub bats_path: String,
    #[serde(default)]
    pub completion_path: String,
}

fn default_version() -> String {
    "0.1.0".into()
}

pub fn save_entry(paths: &LearnPaths, entry: &LearnEntry) -> Result<()> {
    paths.ensure_dirs().context("creating learn dirs")?;
    let path = paths.registry_path(&entry.name);
    let text = toml::to_string_pretty(entry).context("serialize learn entry")?;
    std::fs::write(&path, text)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn load_entry(paths: &LearnPaths, name: &str) -> Result<LearnEntry> {
    let path = paths.registry_path(name);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading learn registry {}", path.display()))?;
    let entry: LearnEntry = toml::from_str(&text).context("parse learn registry")?;
    Ok(entry)
}

pub fn entry_exists(paths: &LearnPaths, name: &str) -> bool {
    Path::new(&paths.registry_path(name)).is_file() && paths.command_path(name).is_file()
}

/// Saved learn commands, sorted by name. Unreadable registry files are skipped.
pub fn list_entries(paths: &LearnPaths) -> Vec<LearnEntry> {
    let Ok(rd) = std::fs::read_dir(paths.registry_dir()) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(entry) = toml::from_str::<LearnEntry>(&text) {
            if paths.command_path(&entry.name).is_file() {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Remove a saved command: script, man page, bats, completion, and registry entry.
pub fn remove_entry(paths: &LearnPaths, name: &str) -> Result<()> {
    for path in [
        paths.command_path(name),
        paths.man_path(name),
        paths.bats_path(name),
        paths.completion_path(name),
        paths.registry_path(name),
    ] {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_home() -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("shomsky-reg-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trip() {
        let home = tmp_home();
        let paths = LearnPaths::new(&home);
        let entry = LearnEntry {
            name: "recent-fat".into(),
            description: "biggest recent files".into(),
            version: "0.1.0".into(),
            intent: "find big files".into(),
            command: "find . -mtime -7".into(),
            flags: vec!["--days".into(), "--count".into()],
            script_path: paths.command_path("recent-fat").display().to_string(),
            man_path: paths.man_path("recent-fat").display().to_string(),
            bats_path: paths.bats_path("recent-fat").display().to_string(),
            completion_path: paths.completion_path("recent-fat").display().to_string(),
        };
        save_entry(&paths, &entry).unwrap();
        let loaded = load_entry(&paths, "recent-fat").unwrap();
        assert_eq!(loaded.name, "recent-fat");
        assert_eq!(loaded.flags.len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }
}
