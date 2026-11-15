//! NDJSON control protocol over the Unix socket.

use serde::{Deserialize, Serialize};

use crate::notify::Finding;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Roster,
    Spawn {
        command: String,
        #[serde(default)]
        tell_me: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        persist: bool,
    },
    Pause {
        id: u32,
    },
    Resume {
        id: u32,
    },
    Stop {
        id: u32,
    },
    Context {
        id: u32,
    },
    Talk {
        id: u32,
        message: String,
    },
    /// Attach a document as read-only (ask/talk only).
    Read {
        path: String,
        #[serde(default)]
        tell_me: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Attach a document as read/write (ask/talk + write content).
    ReadWrite {
        path: String,
        #[serde(default)]
        tell_me: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Append (or replace) text into a readwrite document member.
    Write {
        id: u32,
        content: String,
        #[serde(default)]
        replace: bool,
    },
    /// Attach a generative dream document (foreground `shomagent dream`).
    Dream {
        path: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Install a user-written opening seed (skips the LLM seed beat).
    DreamPlant {
        id: u32,
        text: String,
    },
    /// Generate one dream beat and append it to the member's document.
    DreamStep {
        id: u32,
        #[serde(default)]
        steering: Option<String>,
    },
    /// End a dream session (detach member; keep the file).
    DreamEnd {
        id: u32,
    },
    /// Attach a refine session over an idea document (foreground `shomagent refine`).
    Refine {
        path: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Rewrite the idea file from a standing task (overwrite, never append).
    RefineStep {
        id: u32,
        task: String,
    },
    /// End a refine session (detach member; keep the file).
    RefineEnd {
        id: u32,
    },
    /// Attach a live headless-browser member for a URL.
    Web {
        url: String,
        #[serde(default)]
        tell_me: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Click a link or button by visible text on a web member.
    Click {
        id: u32,
        text: String,
    },
    /// Type into a labeled field on a web member (does not submit).
    Type {
        id: u32,
        label: String,
        text: String,
    },
    /// Load a new URL in an existing web member.
    Navigate {
        id: u32,
        url: String,
    },
    /// Re-read the live page (DOM as it is now, including script that already ran).
    Refresh {
        id: u32,
    },
    /// Drop stored talk Q&A for a member (page snapshot stays).
    ClearTalk {
        id: u32,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member: Option<MemberStatus>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        roster: Vec<MemberStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<TalkContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<String>,
        /// Prompt size for this talk turn: tokens used, buffer, and percent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_usage: Option<ContextUsage>,
    },
    Err {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberStatus {
    pub id: u32,
    pub name: String,
    pub state: String,
    pub pid: Option<u32>,
    pub runtime_secs: u64,
    pub findings: u32,
    pub tell_me: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TalkContext {
    pub id: u32,
    pub name: String,
    pub tell_me: String,
    pub ring_excerpt: String,
    pub findings: Vec<Finding>,
}

/// How much of the model window a talk prompt used.
///
/// `tokens_used` is `chars / 4` over the messages actually sent (same estimate
/// as `:chat`). `buffer_tokens` is the allowed window. `percent` is
/// `tokens_used * 100 / buffer_tokens` and may exceed 100.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextUsage {
    pub tokens_used: u64,
    pub buffer_tokens: u64,
    pub percent: u64,
    /// Talk messages still stored after this turn.
    #[serde(default)]
    pub history_kept: u64,
    /// Cap on stored talk messages.
    #[serde(default)]
    pub history_limit: u64,
    /// Talk messages removed because the cap was hit (cumulative).
    #[serde(default)]
    pub history_dropped: u64,
    /// Document or stream excerpt was shorter than the source.
    #[serde(default)]
    pub excerpt_clipped: bool,
}

impl ContextUsage {
    pub fn from_counts(tokens_used: u64, buffer_tokens: u64) -> Self {
        let percent = if buffer_tokens == 0 {
            0
        } else {
            tokens_used.saturating_mul(100) / buffer_tokens
        };
        Self {
            tokens_used,
            buffer_tokens,
            percent,
            history_kept: 0,
            history_limit: 0,
            history_dropped: 0,
            excerpt_clipped: false,
        }
    }

    /// Tokens used, allowed buffer, and percent. Extra clauses name a cutoff.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "ctx {}/{} tok · est {}%",
            self.tokens_used, self.buffer_tokens, self.percent
        );
        if self.history_dropped > 0 {
            let total = self.history_kept.saturating_add(self.history_dropped);
            line.push_str(&format!(
                " · history kept {} of {} (limit {})",
                self.history_kept, total, self.history_limit
            ));
        }
        if self.excerpt_clipped {
            line.push_str(" · excerpt clipped");
        }
        if self.buffer_tokens > 0 && self.tokens_used > self.buffer_tokens {
            line.push_str(" · over buffer");
        }
        line
    }
}
