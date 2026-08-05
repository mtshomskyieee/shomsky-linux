//! Novelty filter: suppress repeat findings of the same error class.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::notify::Finding;

#[derive(Debug)]
pub struct NoveltyFilter {
    ttl: Duration,
    seen: HashMap<String, Instant>,
}

impl NoveltyFilter {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            seen: HashMap::new(),
        }
    }

    pub fn fingerprint(finding: &Finding) -> String {
        fingerprint_text(&format!(
            "{}|{}|{}",
            finding.title, finding.detail, finding.suggestion.as_deref().unwrap_or("")
        ))
    }

    /// Returns true if this finding is novel (should interrupt).
    pub fn accept(&mut self, finding: &Finding) -> bool {
        self.gc();
        let fp = Self::fingerprint(finding);
        if let Some(when) = self.seen.get(&fp) {
            if when.elapsed() < self.ttl {
                return false;
            }
        }
        self.seen.insert(fp, Instant::now());
        true
    }

    /// Accept based on raw text (stub / heuristic path).
    pub fn accept_text(&mut self, text: &str) -> bool {
        self.gc();
        let fp = fingerprint_text(text);
        if let Some(when) = self.seen.get(&fp) {
            if when.elapsed() < self.ttl {
                return false;
            }
        }
        self.seen.insert(fp, Instant::now());
        true
    }

    fn gc(&mut self) {
        let ttl = self.ttl;
        self.seen.retain(|_, t| t.elapsed() < ttl);
    }
}

fn fingerprint_text(text: &str) -> String {
    let lower = text.to_lowercase();
    // Strip digits / volatile ids so "500 x3" and "500 x4" collapse.
    let mut out = String::with_capacity(lower.len());
    let mut last_space = false;
    for ch in lower.chars() {
        if ch.is_ascii_digit() {
            continue;
        }
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else if ch.is_alphanumeric() || ch == '/' || ch == '.' || ch == ':' {
            out.push(ch);
            last_space = false;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Finding;

    #[test]
    fn suppresses_repeat_class() {
        let mut f = NoveltyFilter::new(Duration::from_secs(3600));
        let a = Finding {
            member_id: 1,
            member_name: "dev".into(),
            title: "3 × 500 on POST /api/orders".into(),
            detail: "db pool exhausted".into(),
            suggestion: Some("max=20".into()),
            fingerprint: String::new(),
            ts_unix: 0,
        };
        let b = Finding {
            member_id: 1,
            member_name: "dev".into(),
            title: "5 × 500 on POST /api/orders".into(),
            detail: "db pool exhausted".into(),
            suggestion: Some("max=20".into()),
            fingerprint: String::new(),
            ts_unix: 0,
        };
        assert!(f.accept(&a));
        assert!(!f.accept(&b));
    }

    #[test]
    fn different_paths_are_novel() {
        let mut f = NoveltyFilter::new(Duration::from_secs(3600));
        assert!(f.accept_text("error in /api/orders pool"));
        assert!(f.accept_text("error in /api/checkout pool"));
    }
}
