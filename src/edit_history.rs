//! Recent files opened in the SINE editor (`:edit` / menu Editor), newest first.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const HISTORY_CAP: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditVisit {
    pub path: String,
    pub visited_at: u64,
}

pub fn history_path() -> PathBuf {
    let dir = std::env::var("ENGRAM_USER_DIR").unwrap_or_else(|_| "/engram/user".into());
    PathBuf::from(dir).join(".edit_history.json")
}

pub fn load(path: &Path) -> Vec<EditVisit> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn record(hist_path: &Path, file_path: &Path) -> std::io::Result<()> {
    let file = file_path.to_string_lossy();
    let file = file.trim();
    if file.is_empty() {
        return Ok(());
    }
    let mut visits = load(hist_path);
    visits.retain(|v| v.path != file);
    visits.insert(
        0,
        EditVisit {
            path: file.to_string(),
            visited_at: now_unix(),
        },
    );
    if visits.len() > HISTORY_CAP {
        visits.truncate(HISTORY_CAP);
    }
    write_all(hist_path, &visits)
}

pub fn clear(path: &Path) -> std::io::Result<()> {
    write_all(path, &Vec::<EditVisit>::new())
}

fn write_all(path: &Path, visits: &[EditVisit]) -> std::io::Result<()> {
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
        let dir = std::env::temp_dir().join(format!("edit-hist-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        let _ = std::fs::remove_file(&path);
        record(&path, Path::new("/a.py")).unwrap();
        record(&path, Path::new("/b.py")).unwrap();
        record(&path, Path::new("/a.py")).unwrap();
        let v = load(&path);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].path, "/a.py");
        assert_eq!(v[1].path, "/b.py");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_caps_at_fifty() {
        let dir = std::env::temp_dir().join(format!("edit-hist-cap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        for i in 0..60 {
            record(&path, Path::new(&format!("/f{i}.txt"))).unwrap();
        }
        let v = load(&path);
        assert_eq!(v.len(), HISTORY_CAP);
        assert_eq!(v[0].path, "/f59.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_empties_file() {
        let dir = std::env::temp_dir().join(format!("edit-hist-clr-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("h.json");
        record(&path, Path::new("/a.py")).unwrap();
        clear(&path).unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
