//! rustyline Completer: frequency-ranked commands + meta-commands.

use engram_cmdfreq::FrequencyStore;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::SystemTime;

const META_COMMANDS: &[&str] = &[
    ":help", ":h", ":sys", ":uname", ":exit", ":quit", ":q", ":mode", ":tools",
    ":tool-style", ":skills", ":model", ":clear", ":logs", ":top",
    ":shomagent", ":agents",
];

pub struct EngramHelper {
    store_path: PathBuf,
    cache: RefCell<Cache>,
}

struct Cache {
    mtime: Option<SystemTime>,
    commands: Vec<(String, u64)>,
}

impl EngramHelper {
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            cache: RefCell::new(Cache {
                mtime: None,
                commands: Vec::new(),
            }),
        }
    }

    fn refresh_if_needed(&self) {
        let mtime = std::fs::metadata(&self.store_path)
            .and_then(|m| m.modified())
            .ok();
        let mut cache = self.cache.borrow_mut();
        if cache.mtime == mtime && !cache.commands.is_empty() {
            return;
        }
        cache.mtime = mtime;
        cache.commands = FrequencyStore::load(&self.store_path)
            .map(|s| {
                let mut items: Vec<(String, u64)> =
                    s.commands.into_iter().collect();
                items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                items
            })
            .unwrap_or_default();
    }
}

impl Completer for EngramHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        let mut candidates: Vec<Pair> = Vec::new();

        if prefix.starts_with(':') {
            for m in META_COMMANDS {
                if m.starts_with(prefix) {
                    candidates.push(Pair {
                        display: m.to_string(),
                        replacement: m.to_string(),
                    });
                }
            }
            return Ok((0, candidates));
        }

        self.refresh_if_needed();
        let cache = self.cache.borrow();
        for (cmd, count) in cache.commands.iter() {
            if cmd.starts_with(prefix) && cmd.as_str() != prefix {
                candidates.push(Pair {
                    display: format!("{cmd}  ({count})"),
                    replacement: cmd.clone(),
                });
            }
            if candidates.len() >= 20 {
                break;
            }
        }
        Ok((0, candidates))
    }
}

impl Hinter for EngramHelper {
    type Hint = String;
}

impl Highlighter for EngramHelper {}
impl Validator for EngramHelper {}
impl Helper for EngramHelper {}
