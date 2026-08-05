//! Fixed-size line ring buffer for member stdout/stderr.

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct RingBuffer {
    capacity: usize,
    lines: VecDeque<String>,
    /// Monotonic cursor of lines ever pushed (for "new since" summarizer).
    total_pushed: u64,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            lines: VecDeque::new(),
            total_pushed: 0,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.total_pushed += 1;
    }

    pub fn push_chunk(&mut self, chunk: &str) {
        for line in chunk.lines() {
            self.push(line.to_string());
        }
        // Keep a partial trailing line without newline as one entry if present.
        if !chunk.is_empty() && !chunk.ends_with('\n') {
            // Already handled by lines(); last incomplete piece is included by lines().
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn total_pushed(&self) -> u64 {
        self.total_pushed
    }

    pub fn lines_since(&self, cursor: u64) -> (Vec<String>, u64) {
        if cursor >= self.total_pushed {
            return (vec![], self.total_pushed);
        }
        let missing = self.total_pushed.saturating_sub(self.lines.len() as u64);
        let start = if cursor < missing {
            0
        } else {
            (cursor - missing) as usize
        };
        let out: Vec<String> = self.lines.iter().skip(start).cloned().collect();
        (out, self.total_pushed)
    }

    pub fn recent(&self, n: usize) -> Vec<String> {
        let len = self.lines.len();
        let start = len.saturating_sub(n);
        self.lines.iter().skip(start).cloned().collect()
    }

    pub fn join_recent(&self, n: usize) -> String {
        self.recent(n).join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest() {
        let mut r = RingBuffer::new(3);
        r.push("a");
        r.push("b");
        r.push("c");
        r.push("d");
        assert_eq!(r.recent(10), vec!["b", "c", "d"]);
        assert_eq!(r.total_pushed(), 4);
    }

    #[test]
    fn lines_since_tracks_cursor() {
        let mut r = RingBuffer::new(100);
        r.push("one");
        r.push("two");
        let (new, cur) = r.lines_since(0);
        assert_eq!(new, vec!["one", "two"]);
        assert_eq!(cur, 2);
        r.push("three");
        let (new2, cur2) = r.lines_since(cur);
        assert_eq!(new2, vec!["three"]);
        assert_eq!(cur2, 3);
    }
}
