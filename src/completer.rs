//! rustyline Completer: frequency-ranked commands + ~/bin + meta-commands.

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
    ":help", ":h", ":menu", ":m", ":sys", ":uname", ":splash", ":exit", ":quit", ":q",
    ":mode", ":tools", ":tool-style", ":skills", ":model", ":clear", ":logs", ":top",
    ":packages", ":pkgs", ":shomagent", ":agents",
];

pub struct EngramHelper {
    store_path: PathBuf,
    bin_dir: PathBuf,
    cache: RefCell<Cache>,
}

struct Cache {
    mtime: Option<SystemTime>,
    commands: Vec<(String, u64)>,
    bin_names: Vec<String>,
    bin_mtime: Option<SystemTime>,
}

impl EngramHelper {
    pub fn new(store_path: PathBuf, bin_dir: PathBuf) -> Self {
        Self {
            store_path,
            bin_dir,
            cache: RefCell::new(Cache {
                mtime: None,
                commands: Vec::new(),
                bin_names: Vec::new(),
                bin_mtime: None,
            }),
        }
    }

    fn refresh_if_needed(&self) {
        let mtime = std::fs::metadata(&self.store_path)
            .and_then(|m| m.modified())
            .ok();
        let bin_mtime = std::fs::metadata(&self.bin_dir)
            .and_then(|m| m.modified())
            .ok();
        let mut cache = self.cache.borrow_mut();
        if cache.mtime != mtime || cache.commands.is_empty() {
            cache.mtime = mtime;
            cache.commands = FrequencyStore::load(&self.store_path)
                .map(|s| {
                    let mut items: Vec<(String, u64)> = s.commands.into_iter().collect();
                    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    items
                })
                .unwrap_or_default();
        }
        if cache.bin_mtime != bin_mtime {
            cache.bin_mtime = bin_mtime;
            cache.bin_names = list_bin_names(&self.bin_dir);
        }
    }
}

fn list_bin_names(dir: &std::path::Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name();
        let Some(s) = name.to_str() else { continue };
        if s.starts_with('.') {
            continue;
        }
        if ent.path().is_file() {
            names.push(s.to_string());
        }
    }
    names.sort();
    names
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

        // Intent prefixes: don't complete as shell.
        if prefix.starts_with(',') || prefix.starts_with('?') {
            return Ok((0, candidates));
        }

        self.refresh_if_needed();
        let cache = self.cache.borrow();

        for name in cache.bin_names.iter() {
            if name.starts_with(prefix) && name.as_str() != prefix {
                candidates.push(Pair {
                    display: format!("{name}  (learn)"),
                    replacement: name.clone(),
                });
            }
            if candidates.len() >= 20 {
                break;
            }
        }

        for (cmd, count) in cache.commands.iter() {
            if candidates.len() >= 20 {
                break;
            }
            if cmd.starts_with(prefix) && cmd.as_str() != prefix {
                // Skip duplicates already listed from ~/bin.
                if cache.bin_names.iter().any(|n| n == cmd) {
                    continue;
                }
                candidates.push(Pair {
                    display: format!("{cmd}  ({count})"),
                    replacement: cmd.clone(),
                });
            }
        }
        Ok((0, candidates))
    }
}

impl Hinter for EngramHelper {
    type Hint = String;
    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

impl Highlighter for EngramHelper {}
impl Validator for EngramHelper {}
impl Helper for EngramHelper {}
