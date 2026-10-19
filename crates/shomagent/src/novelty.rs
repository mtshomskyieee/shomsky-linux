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
            finding.title,
            finding.detail,
            finding.suggestion.as_deref().unwrap_or("")
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
    let chars: Vec<char> = lower.chars().collect();
    // Drop only count multipliers next to × or a times-sign x ("3 × 500",
    // "500 x3"). CVE ids, ports, pids, and versions stay in the fingerprint.
    let mut drop_digit = vec![false; chars.len()];
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if digit_run_is_count(&chars, start, i) {
                for slot in &mut drop_digit[start..i] {
                    *slot = true;
                }
            }
        } else {
            i += 1;
        }
    }
    let mut out = String::with_capacity(lower.len());
    let mut last_space = false;
    for (idx, ch) in chars.iter().copied().enumerate() {
        if ch.is_ascii_digit() {
            if drop_digit[idx] {
                continue;
            }
            out.push(ch);
            last_space = false;
            continue;
        }
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else if ch.is_alphanumeric() || ch == '/' || ch == '.' || ch == ':' || ch == '×' {
            out.push(ch);
            last_space = false;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A digit run is a repeat-count when the nearest non-space neighbor is `×`
/// or an `x` that is not part of a word (`3 ×`, `x3`, `500 x 4`).
fn digit_run_is_count(chars: &[char], start: usize, end: usize) -> bool {
    let before = nearest_nonspace(chars, start as isize - 1, -1);
    let after = nearest_nonspace(chars, end as isize, 1);
    is_times_sign(chars, before, true) || is_times_sign(chars, after, false)
}

fn nearest_nonspace(chars: &[char], mut i: isize, dir: isize) -> Option<(usize, char)> {
    while i >= 0 && (i as usize) < chars.len() {
        let ch = chars[i as usize];
        if !ch.is_whitespace() {
            return Some((i as usize, ch));
        }
        i += dir;
    }
    None
}

fn is_times_sign(chars: &[char], hit: Option<(usize, char)>, before_run: bool) -> bool {
    let Some((idx, ch)) = hit else {
        return false;
    };
    if ch == '×' {
        return true;
    }
    if ch != 'x' {
        return false;
    }
    // "max=20" and "box" are words. A times-sign x has a non-letter on the
    // side opposite the number (`x3`, `500 x4`, `3 x 500`).
    let other = if before_run {
        idx.checked_sub(1).map(|i| chars[i])
    } else {
        chars.get(idx + 1).copied()
    };
    !other.is_some_and(|c| c.is_ascii_alphabetic())
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

    #[test]
    fn count_suffixes_collapse_but_cves_stay_novel() {
        let mut f = NoveltyFilter::new(Duration::from_secs(3600));
        assert!(f.accept_text("500 x3 on POST /api/orders"));
        assert!(!f.accept_text("500 x4 on POST /api/orders"));
        assert!(f.accept_text("CVE-2024-1234 in openssl"));
        assert!(f.accept_text("CVE-2024-5678 in openssl"));
        assert!(f.accept_text("listen 443 failed"));
        assert!(f.accept_text("listen 8080 failed"));
    }
}
