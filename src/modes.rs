use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// Operating modes for engram-shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lower")]
pub enum Mode {
    /// LLM with real tool execution.
    #[default]
    Agent,
    /// LLM with simulated writes/exec; reads remain real.
    Hallucination,
    /// Bypass LLM; execute lines with BusyBox ash.
    Root,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Agent => "agent",
            Mode::Hallucination => "hallucination",
            Mode::Root => "root",
        }
    }

    pub fn prompt_tag(self) -> &'static str {
        match self {
            Mode::Agent => "agent",
            Mode::Hallucination => "halluc",
            Mode::Root => "root",
        }
    }

    /// Home directory for `cd ~` / bare `cd` and prompt `~` shortening.
    ///
    /// - agent → `/engram/user` (override with `ENGRAM_USER_DIR`)
    /// - root → `/root`
    /// - hallucination → `/virt` (virtual; not used for real shell_exec)
    pub fn home_dir(self) -> PathBuf {
        match self {
            Mode::Agent => std::env::var_os("ENGRAM_USER_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/engram/user")),
            Mode::Root => PathBuf::from("/root"),
            Mode::Hallucination => PathBuf::from("/virt"),
        }
    }
}

/// Serializes tests that read or write `ENGRAM_USER_DIR`.
#[cfg(test)]
pub(crate) fn engram_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "agent" | "agent-mode" => Ok(Mode::Agent),
            "hallucination" | "hallucination-mode" | "halluc" => Ok(Mode::Hallucination),
            "root" | "root-mode" => Ok(Mode::Root),
            other => Err(format!(
                "unknown mode '{other}' (expected agent|hallucination|root)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dirs_by_mode() {
        let _guard = super::engram_env_lock();
        let prev = std::env::var_os("ENGRAM_USER_DIR");
        std::env::remove_var("ENGRAM_USER_DIR");
        assert_eq!(Mode::Agent.home_dir(), PathBuf::from("/engram/user"));
        match prev {
            Some(v) => std::env::set_var("ENGRAM_USER_DIR", v),
            None => std::env::remove_var("ENGRAM_USER_DIR"),
        }
        assert_eq!(Mode::Root.home_dir(), PathBuf::from("/root"));
        assert_eq!(Mode::Hallucination.home_dir(), PathBuf::from("/virt"));
    }
}
