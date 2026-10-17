//! shomagentd: supervise members, ring buffers, summarizer, control socket.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::interval;

use crate::config::{load_crew_defs, persist_local, MemberDef, RestartPolicy};
use crate::llm::{ChatMessage, LlmClient};
use crate::notify::{Finding, NotifyLane};
use crate::novelty::NoveltyFilter;
use crate::paths;
use crate::protocol::{MemberStatus, Request, Response, TalkContext};
use crate::ring::RingBuffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberState {
    Running,
    Paused,
    Stopped,
    Exited,
}

impl MemberState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Exited => "exited",
        }
    }
}

struct Member {
    id: u32,
    def: MemberDef,
    state: MemberState,
    child: Option<Child>,
    pid: Option<u32>,
    ring: RingBuffer,
    started: Instant,
    findings_count: u32,
    findings: Vec<Finding>,
    summarize_cursor: u64,
    novelty: NoveltyFilter,
    talk_history: Vec<ChatMessage>,
    /// When set, this member is a document (no process); ask/talk read the file.
    document: Option<PathBuf>,
    /// If true, `write <id>` may modify the document. Read-only when false.
    document_writable: bool,
    /// Foreground generative dream session over a document.
    dream: bool,
    /// Foreground refine session: load → rewrite → overwrite an idea file.
    refine: bool,
    /// Number of dream beats written so far (0 = seed not yet written).
    dream_beats: u32,
    /// Topic chosen at seed (language + subject).
    dream_topic: String,
    /// Internal one-sentence gist of the last beat (never shown as a CARRY line).
    dream_carry: String,
    /// Short excerpt of the last beat body for continuity (~500 chars).
    dream_excerpt: String,
}

pub struct Daemon {
    members: HashMap<u32, Member>,
    next_id: u32,
    llm: LlmClient,
    cwd: PathBuf,
}

impl Daemon {
    pub fn new(llm: LlmClient, cwd: PathBuf) -> Self {
        Self {
            members: HashMap::new(),
            next_id: 1,
            llm,
            cwd,
        }
    }

    pub async fn bootstrap_autostart(&mut self) -> Result<()> {
        let defs = load_crew_defs(&self.cwd)?;
        for def in defs {
            if def.autostart {
                let _ = self.spawn_def(def, false).await?;
            }
        }
        Ok(())
    }

    async fn spawn_def(&mut self, def: MemberDef, persist: bool) -> Result<MemberStatus> {
        if persist {
            let _ = persist_local(&self.cwd, &def)?;
        }
        let id = self.next_id;
        self.next_id += 1;
        let ring_lines = def.ring_lines;
        let member = Member {
            id,
            def,
            state: MemberState::Stopped,
            child: None,
            pid: None,
            ring: RingBuffer::new(ring_lines),
            started: Instant::now(),
            findings_count: 0,
            findings: Vec::new(),
            summarize_cursor: 0,
            novelty: NoveltyFilter::new(Duration::from_secs(3600)),
            talk_history: Vec::new(),
            document: None,
            document_writable: false,
            dream: false,
            refine: false,
            dream_beats: 0,
            dream_topic: String::new(),
            dream_carry: String::new(),
            dream_excerpt: String::new(),
        };
        self.members.insert(id, member);
        self.start_process_by_id(id).await?;
        let status = self
            .members
            .get(&id)
            .map(status_of)
            .ok_or_else(|| anyhow!("member vanished after spawn"))?;
        Ok(status)
    }
}

/// Global handle so pump tasks can append to rings.
static DAEMON_HANDLE: once_cell_placeholder::OnceCell<Arc<Mutex<Daemon>>> =
    once_cell_placeholder::OnceCell::new();

/// Tiny once_cell substitute without extra dep.
mod once_cell_placeholder {
    use std::sync::OnceLock;
    pub struct OnceCell<T>(OnceLock<T>);
    impl<T> OnceCell<T> {
        pub const fn new() -> Self {
            Self(OnceLock::new())
        }
        pub fn set(&self, val: T) -> Result<(), T> {
            self.0.set(val)
        }
        pub fn get(&self) -> Option<&T> {
            self.0.get()
        }
    }
}

async fn pump_lines<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    id: u32,
    reader: R,
    _stdout: bool,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(handle) = DAEMON_HANDLE.get() {
            let mut d = handle.lock().await;
            if let Some(m) = d.members.get_mut(&id) {
                m.ring.push(line);
            }
        }
    }
}

fn status_of(m: &Member) -> MemberStatus {
    let (state, command) = if m.dream {
        (
            "dream".into(),
            m.document
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| m.def.command.clone()),
        )
    } else if m.refine {
        (
            "refine".into(),
            m.document
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| m.def.command.clone()),
        )
    } else if let Some(ref doc) = m.document {
        let state = if m.document_writable {
            "readwrite"
        } else {
            "read"
        };
        (state.into(), doc.display().to_string())
    } else {
        (
            m.state.as_str().into(),
            if let Some(ref sh) = m.def.shell {
                sh.clone()
            } else {
                format!("{} {}", m.def.command, m.def.args.join(" "))
            },
        )
    };
    MemberStatus {
        id: m.id,
        name: m.def.name.clone(),
        state,
        pid: m.pid,
        runtime_secs: m.started.elapsed().as_secs(),
        findings: m.findings_count,
        tell_me: m.def.tell_me.clone(),
        command,
    }
}

fn read_document(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading document {}", path.display()))?;
    const MAX: usize = 80_000;
    if text.len() <= MAX {
        Ok(text)
    } else {
        Ok(format!(
            "{}\n\n… [truncated; {} bytes total]",
            &text[..MAX],
            text.len()
        ))
    }
}

fn member_context_excerpt(m: &Member) -> String {
    if let Some(ref doc) = m.document {
        match read_document(doc) {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) => format!("(document {} is empty — edit it, then ask again)", doc.display()),
            Err(e) => format!("(could not read {}: {e})", doc.display()),
        }
    } else {
        m.ring.join_recent(200)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Daemon {
    pub async fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Ok {
                message: Some("pong".into()),
                member: None,
                roster: vec![],
                context: None,
                reply: None,
            },
            Request::Roster => {
                let mut roster: Vec<_> = self.members.values().map(status_of).collect();
                roster.sort_by_key(|m| m.id);
                Response::Ok {
                    message: None,
                    member: None,
                    roster,
                    context: None,
                    reply: None,
                }
            }
            Request::Spawn {
                command,
                tell_me,
                name,
                watch: _,
                persist,
            } => {
                let name = name.unwrap_or_else(|| {
                    format!("agent-{}", self.next_id)
                });
                let def = MemberDef::from_shell_command(&name, &command, &tell_me);
                match self.spawn_def(def, persist).await {
                    Ok(member) => Response::Ok {
                        message: Some(format!(
                            "⟡ shomagent#{} {} (pid {})",
                            member.id,
                            if member.state == "running" {
                                "attached"
                            } else {
                                "running"
                            },
                            member.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
                        )),
                        member: Some(member),
                        roster: vec![],
                        context: None,
                        reply: None,
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Request::Read {
                path,
                tell_me,
                name,
            } => match self.attach_document(&path, &tell_me, name.as_deref(), false) {
                Ok(member) => Response::Ok {
                    message: Some(format!(
                        "⟡ shomagent#{} read ({})",
                        member.id, member.command
                    )),
                    member: Some(member),
                    roster: vec![],
                    context: None,
                    reply: None,
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            Request::ReadWrite {
                path,
                tell_me,
                name,
            } => match self.attach_document(&path, &tell_me, name.as_deref(), true) {
                Ok(member) => Response::Ok {
                    message: Some(format!(
                        "⟡ shomagent#{} readwrite ({})",
                        member.id, member.command
                    )),
                    member: Some(member),
                    roster: vec![],
                    context: None,
                    reply: None,
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            Request::Write {
                id,
                content,
                replace,
            } => match self.write_to_document(id, &content, replace) {
                Ok(msg) => Response::Ok {
                    message: Some(msg),
                    member: self.members.get(&id).map(status_of),
                    roster: vec![],
                    context: None,
                    reply: None,
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            Request::Pause { id } => self.with_member(id, |m| {
                if m.document.is_some() {
                    return Response::Err {
                        message: format!("#{id} is a document — use stop to detach"),
                    };
                }
                if let Some(child) = m.child.as_mut() {
                    let _ = child.start_kill();
                }
                m.state = MemberState::Paused;
                m.pid = None;
                m.child = None;
                Response::Ok {
                    message: Some(format!("#{} paused", id)),
                    member: Some(status_of(m)),
                    roster: vec![],
                    context: None,
                    reply: None,
                }
            }),
            Request::Resume { id } => {
                if self.members.get(&id).is_some_and(|m| m.document.is_some()) {
                    return Response::Err {
                        message: format!("#{id} is a document — edit the file and ask again"),
                    };
                }
                let ok = match self.members.get(&id) {
                    Some(m) => matches!(
                        m.state,
                        MemberState::Paused | MemberState::Stopped | MemberState::Exited
                    ),
                    None => {
                        return Response::Err {
                            message: format!("unknown member #{id}"),
                        };
                    }
                };
                if !ok {
                    return Response::Err {
                        message: format!("#{id} is not paused/stopped"),
                    };
                }
                match self.start_process_by_id(id).await {
                    Ok(()) => {
                        let st = self.members.get(&id).map(status_of);
                        Response::Ok {
                            message: Some(format!("#{} resumed", id)),
                            member: st,
                            roster: vec![],
                            context: None,
                            reply: None,
                        }
                    }
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Request::Stop { id } => {
                if self.members.get(&id).is_some_and(|m| m.document.is_some()) {
                    self.members.remove(&id);
                    return Response::Ok {
                        message: Some(format!(
                            "#{id} detached (document file kept on disk)"
                        )),
                        member: None,
                        roster: vec![],
                        context: None,
                        reply: None,
                    };
                }
                self.with_member(id, |m| {
                    if let Some(child) = m.child.as_mut() {
                        let _ = child.start_kill();
                    }
                    m.state = MemberState::Stopped;
                    m.pid = None;
                    m.child = None;
                    Response::Ok {
                        message: Some(format!("#{} stopped", id)),
                        member: Some(status_of(m)),
                        roster: vec![],
                        context: None,
                        reply: None,
                    }
                })
            }
            Request::Context { id } => match self.members.get(&id) {
                Some(m) => Response::Ok {
                    message: None,
                    member: None,
                    roster: vec![],
                    context: Some(TalkContext {
                        id: m.id,
                        name: m.def.name.clone(),
                        tell_me: m.def.tell_me.clone(),
                        ring_excerpt: member_context_excerpt(m),
                        findings: m.findings.clone(),
                    }),
                    reply: None,
                },
                None => Response::Err {
                    message: format!("unknown member #{id}"),
                },
            },
            Request::Talk { id, message } => match self.talk(id, &message).await {
                Ok(reply) => Response::Ok {
                    message: None,
                    member: None,
                    roster: vec![],
                    context: None,
                    reply: Some(reply),
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            Request::Dream { path, name } => match self.attach_dream(&path, name.as_deref()) {
                Ok(member) => Response::Ok {
                    message: Some(format!(
                        "⟡ shomagent#{} dream ({})",
                        member.id, member.command
                    )),
                    member: Some(member),
                    roster: vec![],
                    context: None,
                    reply: None,
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            Request::DreamPlant { id, text } => match self.dream_plant(id, &text) {
                Ok(reply) => Response::Ok {
                    message: None,
                    member: self.members.get(&id).map(status_of),
                    roster: vec![],
                    context: None,
                    reply: Some(reply),
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            // DreamStep / RefineStep are handled in handle_client so the LLM
            // await does not hold the daemon mutex (otherwise quit/End blocks
            // for the whole generation).
            Request::DreamStep { .. } => Response::Err {
                message: "internal: DreamStep must be dispatched unlocked".into(),
            },
            Request::DreamEnd { id } => {
                let Some(m) = self.members.get(&id) else {
                    return Response::Err {
                        message: format!("unknown member #{id}"),
                    };
                };
                if !m.dream {
                    return Response::Err {
                        message: format!("#{id} is not a dream member"),
                    };
                }
                self.members.remove(&id);
                Response::Ok {
                    message: Some(format!(
                        "#{id} dream ended (journal file kept on disk)"
                    )),
                    member: None,
                    roster: vec![],
                    context: None,
                    reply: None,
                }
            }
            Request::Refine { path, name } => match self.attach_refine(&path, name.as_deref()) {
                Ok(member) => Response::Ok {
                    message: Some(format!(
                        "⟡ shomagent#{} refine ({})",
                        member.id, member.command
                    )),
                    member: Some(member),
                    roster: vec![],
                    context: None,
                    reply: None,
                },
                Err(e) => Response::Err {
                    message: e.to_string(),
                },
            },
            Request::RefineStep { .. } => Response::Err {
                message: "internal: RefineStep must be dispatched unlocked".into(),
            },
            Request::RefineEnd { id } => {
                let Some(m) = self.members.get(&id) else {
                    return Response::Err {
                        message: format!("unknown member #{id}"),
                    };
                };
                if !m.refine {
                    return Response::Err {
                        message: format!("#{id} is not a refine member"),
                    };
                }
                self.members.remove(&id);
                Response::Ok {
                    message: Some(format!(
                        "#{id} refine ended (idea file kept on disk)"
                    )),
                    member: None,
                    roster: vec![],
                    context: None,
                    reply: None,
                }
            }
        }
    }

    fn attach_document(
        &mut self,
        path: &str,
        tell_me: &str,
        name: Option<&str>,
        writable: bool,
    ) -> Result<MemberStatus> {
        let path_buf = if std::path::Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        };
        if let Some(parent) = path_buf.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        if !path_buf.exists() {
            let mode = if writable { "readwrite" } else { "read" };
            let stub = format!(
                "# {}\n\n(document attached via shomagent {mode}; ask with: shomagent ask <id> \"…\")\n",
                path_buf
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("document")
            );
            std::fs::write(&path_buf, stub)
                .with_context(|| format!("creating {}", path_buf.display()))?;
        } else if !path_buf.is_file() {
            return Err(anyhow!("{} is not a file", path_buf.display()));
        }

        let name = name
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                path_buf
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("document")
                    .to_string()
            });
        let brief = if tell_me.is_empty() {
            if writable {
                format!(
                    "answer from and (when asked to update) note changes for the document at {}",
                    path_buf.display()
                )
            } else {
                format!(
                    "answer questions using the read-only document at {}",
                    path_buf.display()
                )
            }
        } else {
            tell_me.to_string()
        };
        let id = self.next_id;
        self.next_id += 1;
        let mode = if writable { "readwrite" } else { "read" };
        let def = MemberDef {
            name: name.clone(),
            command: mode.into(),
            args: vec![path_buf.display().to_string()],
            tell_me: brief,
            restart: RestartPolicy::Never,
            autostart: false,
            budget_tokens: 512,
            ring_lines: 2000,
            shell: None,
        };
        let member = Member {
            id,
            def,
            state: MemberState::Running,
            child: None,
            pid: None,
            ring: RingBuffer::new(2000),
            started: Instant::now(),
            findings_count: 0,
            findings: Vec::new(),
            summarize_cursor: 0,
            novelty: NoveltyFilter::new(Duration::from_secs(3600)),
            talk_history: Vec::new(),
            document: Some(path_buf.clone()),
            document_writable: writable,
            dream: false,
            refine: false,
            dream_beats: 0,
            dream_topic: String::new(),
            dream_carry: String::new(),
            dream_excerpt: String::new(),
        };
        let status = status_of(&member);
        self.members.insert(id, member);
        Ok(status)
    }

    fn attach_dream(&mut self, path: &str, name: Option<&str>) -> Result<MemberStatus> {
        let path_buf = if std::path::Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        };
        if let Some(parent) = path_buf.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        if !path_buf.exists() {
            let title = path_buf
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("dream");
            let stub = format!("# {title}\n\n");
            std::fs::write(&path_buf, stub)
                .with_context(|| format!("creating {}", path_buf.display()))?;
        } else if !path_buf.is_file() {
            return Err(anyhow!("{} is not a file", path_buf.display()));
        }

        let name = name
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                path_buf
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dream")
                    .to_string()
            });
        let id = self.next_id;
        self.next_id += 1;
        let def = MemberDef {
            name: name.clone(),
            command: "dream".into(),
            args: vec![path_buf.display().to_string()],
            tell_me: format!(
                "generative dream journal at {}",
                path_buf.display()
            ),
            restart: RestartPolicy::Never,
            autostart: false,
            budget_tokens: 768,
            ring_lines: 2000,
            shell: None,
        };
        let member = Member {
            id,
            def,
            state: MemberState::Running,
            child: None,
            pid: None,
            ring: RingBuffer::new(2000),
            started: Instant::now(),
            findings_count: 0,
            findings: Vec::new(),
            summarize_cursor: 0,
            novelty: NoveltyFilter::new(Duration::from_secs(3600)),
            talk_history: Vec::new(),
            document: Some(path_buf.clone()),
            document_writable: true,
            dream: true,
            refine: false,
            dream_beats: 0,
            dream_topic: String::new(),
            dream_carry: String::new(),
            dream_excerpt: String::new(),
        };
        let status = status_of(&member);
        self.members.insert(id, member);
        Ok(status)
    }

    /// Attach a refine session. Creates parent dirs; does **not** create the file.
    fn attach_refine(&mut self, path: &str, name: Option<&str>) -> Result<MemberStatus> {
        let path_buf = if std::path::Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        };
        if let Some(parent) = path_buf.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        if path_buf.exists() && !path_buf.is_file() {
            return Err(anyhow!("{} is not a file", path_buf.display()));
        }

        let name = name.map(|s| s.to_string()).unwrap_or_else(|| {
            path_buf
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("idea")
                .to_string()
        });
        let id = self.next_id;
        self.next_id += 1;
        let def = MemberDef {
            name: name.clone(),
            command: "refine".into(),
            args: vec![path_buf.display().to_string()],
            tell_me: format!("refine idea document at {}", path_buf.display()),
            restart: RestartPolicy::Never,
            autostart: false,
            budget_tokens: 1200,
            ring_lines: 2000,
            shell: None,
        };
        let member = Member {
            id,
            def,
            state: MemberState::Running,
            child: None,
            pid: None,
            ring: RingBuffer::new(2000),
            started: Instant::now(),
            findings_count: 0,
            findings: Vec::new(),
            summarize_cursor: 0,
            novelty: NoveltyFilter::new(Duration::from_secs(3600)),
            talk_history: Vec::new(),
            document: Some(path_buf.clone()),
            document_writable: true,
            dream: false,
            refine: true,
            dream_beats: 0,
            dream_topic: String::new(),
            dream_carry: String::new(),
            dream_excerpt: String::new(),
        };
        let status = status_of(&member);
        self.members.insert(id, member);
        Ok(status)
    }

    /// Install a user-written seed and advance past the LLM seed beat.
    fn dream_plant(&mut self, id: u32, text: &str) -> Result<String> {
        let m = self
            .members
            .get(&id)
            .ok_or_else(|| anyhow!("unknown member #{id}"))?;
        if !m.dream {
            return Err(anyhow!("#{id} is not a dream member"));
        }
        if m.dream_beats > 0 {
            return Err(anyhow!("#{id} already has a seed — refuse to replant"));
        }
        if text.trim().is_empty() {
            return Err(anyhow!("empty seed"));
        }
        let path = m
            .document
            .clone()
            .ok_or_else(|| anyhow!("#{id} has no document"))?;

        let body = format_user_dream_seed(text);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("appending dream seed to {}", path.display()))?;
            f.write_all(body.as_bytes())?;
        }

        if let Some(m) = self.members.get_mut(&id) {
            m.dream_topic = extract_dream_topic(&body);
            m.dream_carry = fallback_dream_carry(&body);
            m.dream_excerpt = excerpt_dream_body(&body, 520);
            m.dream_beats = 1;
            m.talk_history.clear();
        }
        Ok(body)
    }

    /// Build dream LLM messages without calling the model (lock may be held).
    ///
    /// Continuity is hidden: we pass a short excerpt + an internal gist, never
    /// ask the model to emit summary lines. Default craft is bash/python; a
    /// steer may leave that freely.
    fn dream_step_begin(
        &self,
        id: u32,
        steering: Option<&str>,
    ) -> Result<(LlmClient, Vec<ChatMessage>, DreamStepMeta)> {
        let m = self
            .members
            .get(&id)
            .ok_or_else(|| anyhow!("unknown member #{id}"))?;
        if !m.dream {
            return Err(anyhow!("#{id} is not a dream member"));
        }
        let path = m
            .document
            .clone()
            .ok_or_else(|| anyhow!("#{id} has no document"))?;
        let beats = m.dream_beats;
        let topic = m.dream_topic.clone();
        let carry = m.dream_carry.clone();
        let excerpt = m.dream_excerpt.clone();
        let is_seed = beats == 0;
        let nonce = now_unix();
        let steer = steering
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| truncate_chars(s, 240))
            .unwrap_or_default();

        let messages = if is_seed {
            dream_seed_messages(id, nonce)
        } else {
            dream_continue_messages(id, beats, &topic, &carry, &excerpt, &steer)
        };

        Ok((
            self.llm.clone(),
            messages,
            DreamStepMeta {
                id,
                path,
                beats,
                is_seed,
            },
        ))
    }

    /// Append a generated beat. No-op if the dream member was already ended.
    fn dream_step_finish(&mut self, meta: DreamStepMeta, mut reply: String) -> Result<String> {
        reply = reply.trim().to_string();
        // Drop any accidental CARRY: the model was never asked to emit.
        let (body, _ignored_carry) = split_dream_carry(&reply);
        let mut body = scrub_dream_body(&body);
        if body.trim().is_empty() {
            body = "## beat\n**reason:** the thread went quiet for a moment\n\n\
                    silence is also a kind of prompt\n"
                .into();
        }
        if !body.ends_with('\n') {
            body.push('\n');
        }
        if meta.is_seed && !body.contains("## seed") {
            body = format!("## seed\n{body}");
        }
        if !meta.is_seed {
            let stripped = strip_leading_beat_heading(&body);
            body = format!("## beat {}\n{}", meta.beats, stripped);
        }
        let body = truncate_chars(&body, 1200);
        let mut body = body;
        if !body.ends_with('\n') {
            body.push('\n');
        }

        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&meta.path)
                .with_context(|| format!("appending dream to {}", meta.path.display()))?;
            if meta.beats > 0 {
                f.write_all(b"\n")?;
            }
            f.write_all(body.as_bytes())?;
        }

        if let Some(m) = self.members.get_mut(&meta.id) {
            if !m.dream {
                return Ok(body);
            }
            if meta.is_seed {
                m.dream_topic = extract_dream_topic(&body);
                m.dream_beats = 1;
            } else {
                m.dream_beats = meta.beats + 1;
            }
            // Compactification stays internal — never a visible CARRY line.
            m.dream_carry = fallback_dream_carry(&body);
            m.dream_excerpt = excerpt_dream_body(&body, 520);
            m.talk_history.clear();
        }

        Ok(body)
    }

    /// Build refine LLM messages without calling the model (lock may be held).
    fn refine_step_begin(
        &self,
        id: u32,
        task: &str,
    ) -> Result<(LlmClient, Vec<ChatMessage>, RefineStepMeta)> {
        let m = self
            .members
            .get(&id)
            .ok_or_else(|| anyhow!("unknown member #{id}"))?;
        if !m.refine {
            return Err(anyhow!("#{id} is not a refine member"));
        }
        let path = m
            .document
            .clone()
            .ok_or_else(|| anyhow!("#{id} has no document"))?;
        let task = task.trim();
        if task.is_empty() {
            return Err(anyhow!("empty refine task"));
        }

        let (current, clipped) = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let clipped = raw.chars().count() > 6_000;
            (truncate_chars(&raw, 6_000), clipped)
        } else {
            (String::new(), false)
        };

        let messages = refine_messages(id, task, &current, clipped);
        Ok((
            self.llm.clone(),
            messages,
            RefineStepMeta { id, path },
        ))
    }

    /// Overwrite the idea file with the refined document.
    fn refine_step_finish(&mut self, meta: RefineStepMeta, mut reply: String) -> Result<String> {
        reply = reply.trim().to_string();
        let mut body = strip_wrapping_fence(&reply);
        if body.trim().is_empty() {
            body = "# idea\n\n(empty refine — try again with a clearer task)\n".into();
        }
        let mut body = truncate_chars(&body, 8_000);
        if !body.ends_with('\n') {
            body.push('\n');
        }

        std::fs::write(&meta.path, &body)
            .with_context(|| format!("writing {}", meta.path.display()))?;

        if let Some(m) = self.members.get_mut(&meta.id) {
            if m.refine {
                m.talk_history.clear();
            }
        }

        Ok(body)
    }

    fn write_to_document(&mut self, id: u32, content: &str, replace: bool) -> Result<String> {
        let m = self
            .members
            .get(&id)
            .ok_or_else(|| anyhow!("unknown member #{id}"))?;
        let Some(ref path) = m.document else {
            return Err(anyhow!(
                "#{id} is not a document — use read/readwrite first"
            ));
        };
        if !m.document_writable {
            return Err(anyhow!(
                "#{id} is read-only — attach with: shomagent readwrite <path>"
            ));
        }
        let path = path.clone();
        let mut text = content.to_string();
        if !replace && !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if replace {
            std::fs::write(&path, &text)
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(format!("⟡ shomagent#{id} wrote {} (replaced)", path.display()))
        } else {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            f.write_all(text.as_bytes())
                .with_context(|| format!("appending {}", path.display()))?;
            Ok(format!("⟡ shomagent#{id} wrote {} (appended)", path.display()))
        }
    }

    fn with_member<F>(&mut self, id: u32, f: F) -> Response
    where
        F: FnOnce(&mut Member) -> Response,
    {
        match self.members.get_mut(&id) {
            Some(m) => f(m),
            None => Response::Err {
                message: format!("unknown member #{id}"),
            },
        }
    }

    async fn start_process_by_id(&mut self, id: u32) -> Result<()> {
        if self.members.get(&id).is_some_and(|m| m.document.is_some()) {
            return Err(anyhow!("#{id} is a document member"));
        }
        // Extract needed fields then spawn to satisfy borrow checker
        let (command, args, name) = {
            let m = self
                .members
                .get(&id)
                .ok_or_else(|| anyhow!("unknown member"))?;
            (m.def.command.clone(), m.def.args.clone(), m.def.name.clone())
        };
        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(&self.cwd);
        let mut child = cmd.spawn().with_context(|| format!("resuming {name}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if let Some(m) = self.members.get_mut(&id) {
            m.child = Some(child);
            m.pid = pid;
            m.state = MemberState::Running;
            m.started = Instant::now();
        }
        if let Some(out) = stdout {
            tokio::spawn(pump_lines(id, out, true));
        }
        if let Some(err) = stderr {
            tokio::spawn(pump_lines(id, err, false));
        }
        Ok(())
    }

    async fn talk(&mut self, id: u32, message: &str) -> Result<String> {
        let (system, history_snip) = {
            let m = self
                .members
                .get(&id)
                .ok_or_else(|| anyhow!("unknown member #{id}"))?;
            let findings = m
                .findings
                .iter()
                .map(|f| format!("- {}: {}", f.title, f.detail))
                .collect::<Vec<_>>()
                .join("\n");
            let body = member_context_excerpt(m);
            let system = if m.document.is_some() {
                let mode = if m.document_writable {
                    "read/write"
                } else {
                    "read-only"
                };
                format!(
                    "You are shomagent#{} ({}). This is a {mode} document member.\n\
                     Answer using ONLY the document below.\n\
                     Brief: {}\n\nDocument:\n{}\n\nKeep answers short and concrete. \
                     If the document does not contain the answer, say so.",
                    m.id, m.def.name, m.def.tell_me, body
                )
            } else {
                format!(
                    "You are shomagent#{} ({}). Answer only from this member's stream and findings.\n\
                     Watch brief: {}\n\nRecent stream:\n{}\n\nFindings:\n{}\n\
                     Keep answers short and concrete.",
                    m.id,
                    m.def.name,
                    m.def.tell_me,
                    body,
                    if findings.is_empty() {
                        "(none yet)".into()
                    } else {
                        findings
                    }
                )
            };
            let hist: Vec<ChatMessage> = m.talk_history.iter().cloned().collect();
            (system, hist)
        };

        let mut messages = vec![ChatMessage::system(system)];
        messages.extend(history_snip);
        messages.push(ChatMessage::user(message.to_string()));

        let reply = self.llm.chat(&messages, 256).await?;

        if let Some(m) = self.members.get_mut(&id) {
            m.talk_history.push(ChatMessage::user(message.to_string()));
            m.talk_history.push(ChatMessage::assistant(reply.clone()));
            if m.talk_history.len() > 12 {
                let drain = m.talk_history.len() - 12;
                m.talk_history.drain(0..drain);
            }
        }
        Ok(reply)
    }

    /// Reap exited children and apply restart policy.
    pub async fn reap(&mut self) {
        let ids: Vec<u32> = self.members.keys().copied().collect();
        for id in ids {
            let should_restart = {
                let Some(m) = self.members.get_mut(&id) else {
                    continue;
                };
                if m.document.is_some() {
                    continue;
                }
                if m.state != MemberState::Running {
                    continue;
                }
                let Some(child) = m.child.as_mut() else {
                    continue;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        m.pid = None;
                        m.child = None;
                        m.state = MemberState::Exited;
                        let success = status.success();
                        match m.def.restart {
                            RestartPolicy::Always => true,
                            RestartPolicy::OnFailure => !success,
                            RestartPolicy::Never => false,
                        }
                    }
                    Ok(None) => false,
                    Err(_) => {
                        m.state = MemberState::Exited;
                        m.child = None;
                        m.pid = None;
                        false
                    }
                }
            };
            if should_restart {
                let _ = self.start_process_by_id(id).await;
            }
        }
    }

    /// Summarize new ring lines for each running member.
    pub async fn summarize_pass(&mut self) {
        let ids: Vec<u32> = self.members.keys().copied().collect();
        for id in ids {
            let (tell_me, budget, new_lines, name) = {
                let Some(m) = self.members.get_mut(&id) else {
                    continue;
                };
                if m.document.is_some() {
                    continue;
                }
                if m.state != MemberState::Running && m.state != MemberState::Paused {
                    // Still summarize paused if there is backlog? skip stopped
                }
                let (lines, new_cur) = m.ring.lines_since(m.summarize_cursor);
                if lines.is_empty() {
                    continue;
                }
                m.summarize_cursor = new_cur;
                (
                    m.def.tell_me.clone(),
                    m.def.budget_tokens,
                    lines,
                    m.def.name.clone(),
                )
            };

            if tell_me.is_empty() && !self.llm.is_stub() {
                // No brief — skip LLM unless stub might still catch errors
                continue;
            }

            let chunk = new_lines.join("\n");
            if chunk.trim().is_empty() {
                continue;
            }

            let system = format!(
                "You watch a process stream for shomagent member '{name}'.\n\
                 User brief: {tell_me}\n\
                 If nothing notable matching the brief, reply with exactly: {{\"none\":true}}\n\
                 Otherwise reply with ONLY JSON: {{\"title\":\"...\",\"detail\":\"...\",\"suggestion\":null_or_string,\"none\":false}}\n\
                 structured finding JSON. Be concise. Do not repeat the same issue."
            );
            let user = format!("New log lines:\n{chunk}");
            let messages = vec![
                ChatMessage::system(system),
                ChatMessage::user(user),
            ];

            let raw = match self.llm.chat(&messages, budget.max(64)).await {
                Ok(s) => s,
                Err(_) => continue,
            };

            if let Some(finding) = parse_finding_json(&raw, id, &name) {
                let accept = {
                    let Some(m) = self.members.get_mut(&id) else {
                        continue;
                    };
                    m.novelty.accept(&finding)
                };
                if accept {
                    let mut finding = finding;
                    finding.fingerprint = NoveltyFilter::fingerprint(&finding);
                    if let Some(m) = self.members.get_mut(&id) {
                        m.findings_count += 1;
                        m.findings.push(finding.clone());
                        if m.findings.len() > 50 {
                            m.findings.remove(0);
                        }
                    }
                    let _ = NotifyLane::append(&finding);
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct FindingJson {
    #[serde(default)]
    none: bool,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    suggestion: Option<String>,
}

struct DreamStepMeta {
    id: u32,
    path: PathBuf,
    beats: u32,
    is_seed: bool,
}

struct RefineStepMeta {
    id: u32,
    path: PathBuf,
}

fn refine_messages(id: u32, task: &str, current: &str, clipped: bool) -> Vec<ChatMessage> {
    let task = truncate_chars(task, 800);
    let clip_note = if clipped {
        "\n(Note: the current document was truncated for the model; refine the idea as given.)"
    } else {
        ""
    };
    let system = format!(
        "You are shomagent refine #{id}: rewrite an idea document.\n\
         Apply the reader's task to the current file contents.\n\
         Return the COMPLETE refined markdown document only — no preamble, no commentary.\n\
         Keep under ~6000 characters."
    );
    let user = if current.trim().is_empty() {
        format!(
            "There is no existing document yet.\n\
             Refinement task:\n{task}\n\n\
             Write a complete idea.md from this task."
        )
    } else {
        format!(
            "Current document:\n\
             ---\n{current}\n\
             ---{clip_note}\n\n\
             Refinement task:\n{task}\n\n\
             Rewrite the whole document to satisfy the task. Output only the new document."
        )
    };
    vec![
        ChatMessage::system(truncate_chars(&system, 500)),
        ChatMessage::user(truncate_chars(&user, 7_000)),
    ]
}

/// Strip a single wrapping ``` or ```markdown fence if the whole reply is fenced.
fn strip_wrapping_fence(text: &str) -> String {
    let t = text.trim();
    if !t.starts_with("```") {
        return t.to_string();
    }
    let mut lines = t.lines();
    let first = lines.next().unwrap_or("");
    let fence = first.trim();
    if fence != "```" && fence != "```markdown" && fence != "```md" {
        return t.to_string();
    }
    let rest: Vec<&str> = lines.collect();
    if rest.is_empty() {
        return t.to_string();
    }
    let last = rest.last().map(|l| l.trim()).unwrap_or("");
    if last != "```" {
        return t.to_string();
    }
    let inner = &rest[..rest.len() - 1];
    inner.join("\n")
}

fn dream_seed_messages(id: u32, nonce: u64) -> Vec<ChatMessage> {
    // Concrete few-shot — small models copy examples, not angle-bracket templates.
    let examples = [
        (
            "bash",
            "## seed\n\
             **bash** — process substitution as a secret doorway\n\n\
             `<(cmd)` feels like opening a door that only exists while you walk through it.\n\
             ```bash\ndiff <(sort a.txt) <(sort b.txt)\n```\n",
        ),
        (
            "python",
            "## seed\n\
             **python** — walrus operator as a quiet aside\n\n\
             Walrus lets you name a value in the middle of asking a question.\n\
             ```python\nif (n := len(items)) > 3:\n    print(n)\n```\n",
        ),
        (
            "bash",
            "## seed\n\
             **bash** — here-strings that whisper into stdin\n\n\
             `<<<` feeds a string like a note slipped under a door.\n\
             ```bash\ngrep -o '[0-9]+' <<< 'room 42'\n```\n",
        ),
        (
            "python",
            "## seed\n\
             **python** — pathlib as a map of the filesystem\n\n\
             Paths become objects you can ask polite questions.\n\
             ```python\nfrom pathlib import Path\nprint(Path.home().joinpath('bin').exists())\n```\n",
        ),
    ];
    let (lang_hint, example) = examples[(nonce as usize) % examples.len()];
    let system = format!(
        "You are shomagent dream #{id}: a late-night creative journal.\n\
         Default craft is real bash or python language ideas — curious, concrete, a little poetic.\n\
         Avoid cloud/SaaS product pitches. Keep the whole reply under ~900 characters.\n\
         Write markdown only. Start with ## seed then **bash** or **python** — title."
    );
    vec![
        ChatMessage::system(system),
        ChatMessage::user(format!(
            "Format example (do not copy this topic):\n\n{example}"
        )),
        ChatMessage::assistant(example.to_string()),
        ChatMessage::user(format!(
            "Invent a NEW seed about a different {lang_hint} idea (nonce={nonce}). \
             Same shape. Do not copy the example."
        )),
    ]
}

fn dream_continue_messages(
    id: u32,
    beats: u32,
    topic: &str,
    carry: &str,
    excerpt: &str,
    steer: &str,
) -> Vec<ChatMessage> {
    let topic = truncate_chars(topic, 140);
    let carry = if carry.is_empty() {
        "take one small step further".into()
    } else {
        truncate_chars(carry, 320)
    };
    let excerpt = if excerpt.is_empty() {
        carry.clone()
    } else {
        truncate_chars(excerpt, 520)
    };

    let example = "## beat 1\n\
**reason:** process substitution still feels like a temporary hallway.\n\n\
What if both sides of the diff were live commands?\n\
```bash\ncomm -3 <(ls a) <(ls b)\n```\n";

    let steered = !steer.is_empty();
    let system = if steered {
        format!(
            "You are shomagent dream #{id}: continue a creative journal.\n\
             Opening topic was: {topic}\n\
             The reader is steering — follow their direction even if it leaves bash/python.\n\
             Be concrete and playful. Never paste instructions or placeholders.\n\
             Keep under ~900 characters. Write ## beat {beats} then **reason:** and a short act."
        )
    } else {
        format!(
            "You are shomagent dream #{id}: continue a bash/python craft journal.\n\
             Topic: {topic}\n\
             Be concrete and playful. Prefer a tiny real snippet or a sharp observation.\n\
             Never paste instructions or placeholders. Keep under ~900 characters.\n\
             Write ## beat {beats} then **reason:** and a short act."
        )
    };

    let mut user = format!(
        "Recent journal thread:\n\
         ---\n{excerpt}\n\
         ---\n\
         Where it is heading: {carry}\n\n\
         Tone example (not the topic):\n{example}\n\
         Write the next beat as a natural continuation."
    );
    if steered {
        user.push_str(&format!("\n\nReader steering (follow this): {steer}"));
    }

    vec![
        ChatMessage::system(truncate_chars(&system, 650)),
        ChatMessage::user(truncate_chars(&user, 1100)),
    ]
}

/// Normalize a user-written opening into a `## seed` journal block.
fn format_user_dream_seed(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return "## seed\n\n".into();
    }
    let body = if t.to_ascii_lowercase().starts_with("## seed") {
        t.to_string()
    } else {
        format!("## seed\n{t}")
    };
    if body.ends_with('\n') {
        body
    } else {
        format!("{body}\n")
    }
}

fn extract_dream_topic(seed: &str) -> String {
    for line in seed.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("**") {
            if let Some((head, tail)) = rest.split_once("**") {
                let topic = format!("{head}{tail}").trim().to_string();
                if !topic.is_empty()
                    && !topic.contains('<')
                    && !topic.to_ascii_lowercase().contains("short title")
                {
                    return truncate_chars(&topic, 120);
                }
            }
            let topic = rest.trim_matches('*').trim().to_string();
            if !topic.is_empty() && !topic.contains('<') {
                return truncate_chars(&topic, 120);
            }
        }
    }
    // Fall back to first prose sentence if the model skipped the bold title.
    for line in seed.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("```") {
            continue;
        }
        return truncate_chars(t, 120);
    }
    "bash / python exploration".into()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Split a model reply into journal body + optional `CARRY:` sentence.
fn split_dream_carry(reply: &str) -> (String, String) {
    let mut body_lines = Vec::new();
    let mut carry = String::new();
    for line in reply.lines() {
        let t = line.trim();
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("carry:") || lower.starts_with("**carry:**") {
            let raw = t
                .split_once(':')
                .map(|(_, r)| r.trim())
                .unwrap_or("")
                .trim_matches('*')
                .trim();
            if !raw.is_empty() {
                carry = raw.to_string();
            }
            continue;
        }
        body_lines.push(line);
    }
    while body_lines.last().is_some_and(|l| l.trim().is_empty()) {
        body_lines.pop();
    }
    (body_lines.join("\n"), carry)
}

fn scrub_dream_body(body: &str) -> String {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_fence = !in_fence;
            out.push(line.to_string());
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }
        let lower = t.to_ascii_lowercase();
        // Drop leaked prompt scaffolding small models love to echo.
        if lower.contains("<brief act")
            || lower.contains("<2 short")
            || lower.contains("<1-2")
            || lower.contains("<short title")
            || lower.contains("<bash|python>")
            || lower.contains("<python|bash>")
            || lower.contains("optional tiny fenced")
            || lower.starts_with("no preamble")
            || lower.starts_with("do not run")
            || lower.starts_with("do not execute")
            || lower == "## next beat"
            || lower.starts_with("write only compact")
            || lower.starts_with("always end with")
        {
            continue;
        }
        out.push(line.to_string());
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

fn strip_leading_beat_heading(body: &str) -> String {
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.peek() {
        let t = line.trim();
        if t.is_empty() {
            lines.next();
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("## beat") {
            lines.next();
            continue;
        }
        break;
    }
    lines.collect::<Vec<_>>().join("\n")
}

fn fallback_dream_carry(body: &str) -> String {
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("```") {
            continue;
        }
        if let Some(rest) = t.strip_prefix("**reason:**") {
            let s = rest.trim();
            if !s.is_empty() {
                return truncate_chars(s, 320);
            }
            continue;
        }
        if t.starts_with("**") {
            continue;
        }
        return truncate_chars(t, 320);
    }
    "continue the dream from here".into()
}

/// Keep enough of the last beat for continuity without shipping the whole file.
fn excerpt_dream_body(body: &str, max: usize) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    // Prefer the end of the beat (where the act usually lands).
    let chars: Vec<char> = trimmed.chars().collect();
    let start = chars.len().saturating_sub(max);
    let mut out: String = chars[start..].iter().collect();
    if start > 0 {
        out = format!("…{out}");
    }
    out
}

fn parse_finding_json(raw: &str, id: u32, name: &str) -> Option<Finding> {
    let trimmed = raw.trim();
    // Extract JSON object if wrapped in markdown fences
    let json_str = if let Some(start) = trimmed.find('{') {
        let end = trimmed.rfind('}')?;
        &trimmed[start..=end]
    } else {
        return None;
    };
    let parsed: FindingJson = serde_json::from_str(json_str).ok()?;
    if parsed.none {
        return None;
    }
    let title = parsed.title.filter(|t| !t.is_empty())?;
    Some(Finding {
        member_id: id,
        member_name: name.to_string(),
        title,
        detail: parsed.detail.unwrap_or_default(),
        suggestion: parsed.suggestion.filter(|s| !s.is_empty()),
        fingerprint: String::new(),
        ts_unix: now_unix(),
    })
}

pub async fn run_daemon() -> Result<()> {
    paths::ensure_run_dirs()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let llm = LlmClient::from_env()?;
    let daemon = Arc::new(Mutex::new(Daemon::new(llm, cwd)));
    DAEMON_HANDLE
        .set(daemon.clone())
        .map_err(|_| anyhow!("daemon handle already set"))?;

    {
        let mut d = daemon.lock().await;
        d.bootstrap_autostart().await?;
    }

    let sock_path = paths::ctl_sock();
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding {}", sock_path.display()))?;

    let daemon_reap = daemon.clone();
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let mut d = daemon_reap.lock().await;
            d.reap().await;
        }
    });

    let daemon_sum = daemon.clone();
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let mut d = daemon_sum.lock().await;
            d.summarize_pass().await;
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, daemon).await {
                eprintln!("shomagentd: client error: {e}");
            }
        });
    }
}

async fn handle_client(mut stream: UnixStream, daemon: Arc<Mutex<Daemon>>) -> Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let req: Request = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = Response::Err {
                        message: format!("bad request: {e}"),
                    };
                    write_response(&mut stream, &resp).await?;
                    continue;
                }
            };
            let resp = match req {
                Request::DreamStep { id, steering } => {
                    let begun = {
                        let d = daemon.lock().await;
                        d.dream_step_begin(id, steering.as_deref())
                    };
                    match begun {
                        Err(e) => Response::Err {
                            message: e.to_string(),
                        },
                        Ok((llm, messages, meta)) => {
                            // LLM runs unlocked so DreamEnd / quit can proceed.
                            // One quick retry only — small models are slow; stacking
                            // 120s timeouts made dream look stuck for minutes.
                            let mut last_err = None;
                            let mut raw_ok = None;
                            for attempt in 1..=2 {
                                // Short generations; body capped ~900–1200 chars.
                                match llm
                                    .chat_with_temperature_timeout(
                                        &messages,
                                        320,
                                        0.8,
                                        Some(Duration::from_secs(45)),
                                    )
                                    .await
                                {
                                    Ok(raw) => {
                                        raw_ok = Some(raw);
                                        break;
                                    }
                                    Err(e) => {
                                        last_err = Some(e);
                                        if attempt < 2 {
                                            tokio::time::sleep(Duration::from_millis(500)).await;
                                        }
                                    }
                                }
                            }
                            match raw_ok {
                                Some(raw) => {
                                    let mut d = daemon.lock().await;
                                    match d.dream_step_finish(meta, raw) {
                                        Ok(reply) => Response::Ok {
                                            message: None,
                                            member: d.members.get(&id).map(status_of),
                                            roster: vec![],
                                            context: None,
                                            reply: Some(reply),
                                        },
                                        Err(e) => Response::Err {
                                            message: format!("{e:#}"),
                                        },
                                    }
                                }
                                None => Response::Err {
                                    message: format!(
                                        "{:#}",
                                        last_err.unwrap_or_else(|| anyhow!("dream LLM failed"))
                                    ),
                                },
                            }
                        }
                    }
                }
                Request::RefineStep { id, task } => {
                    let begun = {
                        let d = daemon.lock().await;
                        d.refine_step_begin(id, &task)
                    };
                    match begun {
                        Err(e) => Response::Err {
                            message: e.to_string(),
                        },
                        Ok((llm, messages, meta)) => {
                            let mut last_err = None;
                            let mut raw_ok = None;
                            for attempt in 1..=2 {
                                match llm
                                    .chat_with_temperature_timeout(
                                        &messages,
                                        1200,
                                        0.4,
                                        Some(Duration::from_secs(90)),
                                    )
                                    .await
                                {
                                    Ok(raw) => {
                                        raw_ok = Some(raw);
                                        break;
                                    }
                                    Err(e) => {
                                        last_err = Some(e);
                                        if attempt < 2 {
                                            tokio::time::sleep(Duration::from_millis(500)).await;
                                        }
                                    }
                                }
                            }
                            match raw_ok {
                                Some(raw) => {
                                    let mut d = daemon.lock().await;
                                    match d.refine_step_finish(meta, raw) {
                                        Ok(reply) => Response::Ok {
                                            message: None,
                                            member: d.members.get(&id).map(status_of),
                                            roster: vec![],
                                            context: None,
                                            reply: Some(reply),
                                        },
                                        Err(e) => Response::Err {
                                            message: format!("{e:#}"),
                                        },
                                    }
                                }
                                None => Response::Err {
                                    message: format!(
                                        "{:#}",
                                        last_err.unwrap_or_else(|| anyhow!("refine LLM failed"))
                                    ),
                                },
                            }
                        }
                    }
                }
                other => {
                    let mut d = daemon.lock().await;
                    d.handle(other).await
                }
            };
            write_response(&mut stream, &resp).await?;
        }
    }
    Ok(())
}

async fn write_response(stream: &mut UnixStream, resp: &Response) -> Result<()> {
    let mut line = serde_json::to_vec(resp)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    Ok(())
}

/// Client helper: send one request and read one response.
pub async fn rpc(req: &Request) -> Result<Response> {
    let path = paths::ctl_sock();
    let mut stream = UnixStream::connect(&path)
        .await
        .with_context(|| {
            format!(
                "connecting to {} (is shomagentd running?)",
                path.display()
            )
        })?;
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.contains(&b'\n') {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next().unwrap_or("");
    let resp: Response = serde_json::from_str(line).context("decoding response")?;
    Ok(resp)
}
