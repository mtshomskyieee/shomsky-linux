//! Finding objects and the JSONL notification lane.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// Rotate the lane once it reaches this size. The previous file is kept as `notify.1`.
const MAX_NOTIFY_BYTES: u64 = 1024 * 1024;

fn write_finding(path: &Path, finding: &Finding) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_if_over(path, MAX_NOTIFY_BYTES)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut f, finding)?;
    f.write_all(b"\n")?;
    Ok(())
}

fn rotate_if_over(path: &Path, max: u64) -> std::io::Result<()> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if len < max {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("notify");
    let backup = path.with_file_name(format!("{name}.1"));
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(path, backup)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub member_id: u32,
    pub member_name: String,
    pub title: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub fingerprint: String,
    pub ts_unix: u64,
}

impl Finding {
    pub fn format_interrupt(&self) -> String {
        let mut s = format!("⟡ shomagent#{} ─ {}", self.member_id, self.title);
        if !self.detail.is_empty() {
            s.push('\n');
            s.push_str(&format!("  {}", self.detail));
        }
        if let Some(ref sug) = self.suggestion {
            s.push('\n');
            s.push_str(&format!("  suggested: {sug}"));
        }
        s
    }
}

#[derive(Debug)]
pub struct NotifyLane {
    path: PathBuf,
    /// Byte offset last consumed by this reader (REPL side).
    offset: u64,
}

impl NotifyLane {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
        }
    }

    pub fn default_path() -> Self {
        Self::new(paths::notify_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(finding: &Finding) -> std::io::Result<()> {
        write_finding(&paths::notify_path(), finding)
    }

    pub fn append_to(path: &Path, finding: &Finding) -> std::io::Result<()> {
        write_finding(path, finding)
    }

    /// Read new findings since last drain (updates offset).
    pub fn drain(&mut self) -> std::io::Result<Vec<Finding>> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let len = meta.len();
        if len < self.offset {
            // Truncated / rotated
            self.offset = 0;
        }
        if len == self.offset {
            return Ok(vec![]);
        }
        use std::io::Seek;
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(std::io::SeekFrom::Start(self.offset))?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        let mut bytes = self.offset;
        for line in reader.lines() {
            let line = line?;
            bytes += line.len() as u64 + 1; // + newline
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(f) = serde_json::from_str::<Finding>(trimmed) {
                out.push(f);
            }
        }
        self.offset = bytes.min(len);
        Ok(out)
    }
}

/// Drain notify lane from default path for engram-shell (stateless offset via file).
pub fn drain_new_findings(cursor_path: &Path) -> std::io::Result<Vec<Finding>> {
    let notify = paths::notify_path();
    let offset: u64 = std::fs::read_to_string(cursor_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let mut lane = NotifyLane {
        path: notify,
        offset,
    };
    let findings = lane.drain()?;
    if let Some(parent) = cursor_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(cursor_path, lane.offset.to_string())?;
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn append_and_drain() {
        let dir = std::env::temp_dir().join(format!("shomagent-notify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify");
        let f = Finding {
            member_id: 1,
            member_name: "t".into(),
            title: "boom".into(),
            detail: "detail".into(),
            suggestion: None,
            fingerprint: "fp".into(),
            ts_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        NotifyLane::append_to(&path, &f).unwrap();
        NotifyLane::append_to(&path, &f).unwrap();
        let mut lane = NotifyLane::new(&path);
        let got = lane.drain().unwrap();
        assert_eq!(got.len(), 2);
        let got2 = lane.drain().unwrap();
        assert!(got2.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotates_a_full_lane_before_appending() {
        let dir =
            std::env::temp_dir().join(format!("shomagent-notify-rot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify");
        std::fs::write(&path, vec![b'x'; MAX_NOTIFY_BYTES as usize]).unwrap();
        let f = Finding {
            member_id: 1,
            member_name: "t".into(),
            title: "after-rotate".into(),
            detail: "kept".into(),
            suggestion: None,
            fingerprint: "fp".into(),
            ts_unix: 1,
        };
        NotifyLane::append_to(&path, &f).unwrap();
        assert!(dir.join("notify.1").is_file());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("after-rotate"), "{body}");
        assert!(body.len() < 500, "new lane kept the old bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
