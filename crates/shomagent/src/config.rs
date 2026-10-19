//! Member definitions from shomagent_crew.d TOML drop-ins.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberDef {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub tell_me: String,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_budget")]
    pub budget_tokens: u32,
    #[serde(default = "default_ring")]
    pub ring_lines: usize,
    /// Optional shell form: if set, run via `sh -c`.
    #[serde(default)]
    pub shell: Option<String>,
    /// Set by the loader. Cwd drop-ins do not autostart unless
    /// `SHOMAGENT_LOCAL_AUTOSTART` is set.
    #[serde(default, skip)]
    pub from_cwd: bool,
}

fn default_budget() -> u32 {
    512
}

fn default_ring() -> usize {
    2000
}

impl MemberDef {
    pub fn from_shell_command(name: &str, cmdline: &str, tell_me: &str) -> Self {
        Self {
            name: name.to_string(),
            command: "sh".into(),
            args: vec!["-c".into(), cmdline.to_string()],
            tell_me: tell_me.to_string(),
            restart: RestartPolicy::Never,
            autostart: false,
            budget_tokens: default_budget(),
            ring_lines: default_ring(),
            shell: Some(cmdline.to_string()),
            from_cwd: false,
        }
    }
}

/// Load and merge system then cwd drop-ins. Later files with the same `name` win.
pub fn load_crew_defs(cwd: &Path) -> Result<Vec<MemberDef>> {
    let mut by_name: std::collections::BTreeMap<String, MemberDef> =
        std::collections::BTreeMap::new();
    load_dir_into(&paths::system_crew_dir(), &mut by_name, false)?;
    load_dir_into(&paths::local_crew_dir(cwd), &mut by_name, true)?;
    Ok(by_name.into_values().collect())
}

fn load_dir_into(
    dir: &Path,
    into: &mut std::collections::BTreeMap<String, MemberDef>,
    from_cwd: bool,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading crew dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "toml")
                .unwrap_or(false)
        })
        .collect();
    entries.sort();
    for path in entries {
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut def: MemberDef =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        def.from_cwd = from_cwd;
        into.insert(def.name.clone(), def);
    }
    Ok(())
}

/// Persist a member def under cwd `shomagent_crew.d/`.
pub fn persist_local(cwd: &Path, def: &MemberDef) -> Result<PathBuf> {
    let dir = paths::local_crew_dir(cwd);
    fs::create_dir_all(&dir)?;
    let safe: String = def
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.toml"));
    let text = toml::to_string_pretty(def).context("serializing member def")?;
    fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_cwd_overrides_system() {
        let root = std::env::temp_dir().join(format!("shomagent-crew-{}", uuid::Uuid::new_v4()));
        let sys = root.join("system");
        let cwd = root.join("cwd");
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(cwd.join(paths::LOCAL_CREW_DIR)).unwrap();
        fs::write(
            sys.join("a.toml"),
            r#"
name = "watch"
command = "echo"
args = ["sys"]
tell_me = "sys"
"#,
        )
        .unwrap();
        fs::write(
            sys.join("b.toml"),
            r#"
name = "sys-only"
command = "echo"
args = ["sys-only"]
autostart = true
"#,
        )
        .unwrap();
        fs::write(
            cwd.join(paths::LOCAL_CREW_DIR).join("a.toml"),
            r#"
name = "watch"
command = "echo"
args = ["local"]
tell_me = "local"
"#,
        )
        .unwrap();
        std::env::set_var("SHOMAGENT_SYSTEM_CREW", sys.to_str().unwrap());
        let defs = load_crew_defs(&cwd).unwrap();
        std::env::remove_var("SHOMAGENT_SYSTEM_CREW");
        assert_eq!(defs.len(), 2);
        let watch = defs.iter().find(|d| d.name == "watch").unwrap();
        assert_eq!(watch.args, vec!["local"]);
        assert!(watch.from_cwd);
        let sys_only = defs.iter().find(|d| d.name == "sys-only").unwrap();
        assert!(!sys_only.from_cwd);
        let _ = fs::remove_dir_all(&root);
    }
}
