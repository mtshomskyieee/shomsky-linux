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
        watch: bool,
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
