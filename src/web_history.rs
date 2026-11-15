//! Sites visited from `:web`, newest first.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const HISTORY_CAP: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub visited_at: u64,
}

pub fn history_path() -> PathBuf {
    let dir = std::env::var("ENGRAM_USER_DIR").unwrap_or_else(|_| "/engram/user".into());
    PathBuf::from(dir).join(".web_history.json")
}

pub fn load(path: &Path) -> Vec<Visit> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn record(path: &Path, url: &str, title: &str) -> std::io::Result<()> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(());
    }
    let mut visits = load(path);
    visits.retain(|v| v.url != url);
    visits.insert(
        0,
        Visit {
            url: url.to_string(),
            title: title.trim().to_string(),
            visited_at: now_unix(),
        },
    );
    if visits.len() > HISTORY_CAP {
        visits.truncate(HISTORY_CAP);
    }
    write_all(path, &visits)
}

pub fn clear(path: &Path) -> std::io::Result<()> {
    write_all(path, &Vec::<Visit>::new())
}

pub fn title_from_snapshot(text: &str) -> String {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Title:") {
            let t = rest.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    String::new()
}

fn write_all(path: &Path, visits: &[Visit]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let text = serde_json::to_string_pretty(visits).unwrap_or_else(|_| "[]".into());
    std::fs::write(path, text)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_dedupes_and_moves_to_front() {
        let dir = std::env::temp_dir().join(format!("web-hist-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        let _ = std::fs::remove_file(&path);
        record(&path, "https://a.example", "A").unwrap();
        record(&path, "https://b.example", "B").unwrap();
        record(&path, "https://a.example", "A2").unwrap();
        let v = load(&path);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].url, "https://a.example");
        assert_eq!(v[0].title, "A2");
        assert_eq!(v[1].url, "https://b.example");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_caps_at_fifty() {
        let dir = std::env::temp_dir().join(format!("web-hist-cap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        for i in 0..60 {
            record(&path, &format!("https://ex{i}.test"), "t").unwrap();
        }
        let v = load(&path);
        assert_eq!(v.len(), HISTORY_CAP);
        assert_eq!(v[0].url, "https://ex59.test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_empties_file() {
        let dir = std::env::temp_dir().join(format!("web-hist-clr-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        record(&path, "https://a.example", "A").unwrap();
        clear(&path).unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_line_from_snapshot() {
        let snap = "URL: https://example.com\nTitle: Example Domain\n\nbody\n";
        assert_eq!(title_from_snapshot(snap), "Example Domain");
        assert_eq!(title_from_snapshot("no title"), "");
    }
}
