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
