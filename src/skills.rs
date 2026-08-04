use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Skill filename / name for the interactive-command bypass list.
pub const IGNORE_LLM_SKILL: &str = "ignore_llm_for_these_commands";

#[derive(Debug, Clone, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub always_load: bool,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub meta: SkillMeta,
    pub body: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SkillStore {
    skills: BTreeMap<String, Skill>,
    /// Basenames from `ignore_llm_for_these_commands` (TUI / interactive).
    ignore_llm_commands: BTreeSet<String>,
}

impl SkillStore {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut store = Self::default();
        if !dir.exists() {
            return Ok(store);
        }
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let text = fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let skill = parse_skill(&path, &text)
                .with_context(|| format!("parsing {}", path.display()))?;
            if skill.meta.name == IGNORE_LLM_SKILL
                || path.file_stem().and_then(|s| s.to_str()) == Some(IGNORE_LLM_SKILL)
            {
                store.ignore_llm_commands = parse_ignore_llm_commands(&skill.body);
            }
            store.skills.insert(skill.meta.name.clone(), skill);
        }
        Ok(store)
    }

    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn always_loaded_bodies(&self) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.meta.always_load)
            .collect()
    }

    pub fn ignore_llm_commands(&self) -> &BTreeSet<String> {
        &self.ignore_llm_commands
    }

    /// Build the system prompt fragment for skills.
    pub fn system_prompt_fragment(&self) -> String {
        let mut out = String::new();
        out.push_str("# Available skills\n\n");
        out.push_str(
            "Skills with full content below are always loaded. \
             Other skills may be loaded with the load_skill tool.\n\n",
        );

        out.push_str("## Skill index\n\n");
        for skill in self.skills.values() {
            // Keep the ignore list out of the model index — it is client-side only.
            if skill.meta.name == IGNORE_LLM_SKILL {
                continue;
            }
            out.push_str(&format!(
                "- **{}**{}: {}\n",
                skill.meta.name,
                if skill.meta.always_load {
                    " (always loaded)"
                } else {
                    ""
                },
                if skill.meta.description.is_empty() {
                    "(no description)"
                } else {
                    &skill.meta.description
                }
            ));
        }
        out.push('\n');

        if !self.ignore_llm_commands.is_empty() {
            out.push_str(
                "## Interactive commands (client-enforced)\n\n\
                 These commands are run on the real TTY by the shell and must not \
                 be invoked via shell_exec: ",
            );
            let mut names: Vec<&str> = self.ignore_llm_commands.iter().map(|s| s.as_str()).collect();
            names.sort_unstable();
            out.push_str(&names.join(", "));
            out.push_str(".\n\n");
        }

        for skill in self.always_loaded_bodies() {
            if skill.meta.name == IGNORE_LLM_SKILL {
                continue;
            }
            out.push_str(&format!("## Skill: {}\n\n", skill.meta.name));
            out.push_str(&skill.body);
            out.push_str("\n\n");
        }
        out
    }
}

fn parse_skill(path: &Path, text: &str) -> Result<Skill> {
    let (meta, body) = if text.starts_with("---") {
        let rest = &text[3..];
        let end = rest
            .find("\n---")
            .context("missing closing YAML frontmatter delimiter")?;
        let yaml = &rest[..end];
        let body = rest[end + 4..].trim_start_matches('\n').to_string();
        let meta: SkillMeta = serde_yaml::from_str(yaml).context("parsing skill frontmatter")?;
        (meta, body)
    } else {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        (
            SkillMeta {
                name,
                description: String::new(),
                always_load: false,
            },
            text.to_string(),
        )
    };
    Ok(Skill {
        meta,
        body,
        path: path.to_path_buf(),
    })
}

/// Extract command basenames from the ignore-list skill body.
pub fn parse_ignore_llm_commands(body: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for raw in body.lines() {
        let mut line = raw.trim();
        if line.starts_with("```") {
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            line = rest.trim();
        }
        // Strip inline code ticks: `top`
        if line.starts_with('`') && line.ends_with('`') && line.len() >= 2 {
            line = &line[1..line.len() - 1];
        }
        // One token only (basename).
        let Some(token) = line.split_whitespace().next() else {
            continue;
        };
        let token = token.trim_matches('`');
        if token.is_empty() {
            continue;
        }
        if !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.'))
        {
            continue;
        }
        // Reject path-like tokens.
        if token.contains('/') {
            continue;
        }
        set.insert(token.to_string());
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let text = "---\nname: shell\ndescription: Basic shell\nalways_load: true\n---\n\n# Hello\n";
        let skill = parse_skill(Path::new("shell.md"), text).unwrap();
        assert_eq!(skill.meta.name, "shell");
        assert!(skill.meta.always_load);
        assert!(skill.body.contains("# Hello"));
    }

    #[test]
    fn parses_ignore_list() {
        let body = r#"
# Bypass

top
htop

- mc
* vim

`nano`

not a command because spaces
/usr/bin/top
"#;
        let set = parse_ignore_llm_commands(body);
        assert!(set.contains("top"));
        assert!(set.contains("htop"));
        assert!(set.contains("mc"));
        assert!(set.contains("vim"));
        assert!(set.contains("nano"));
        assert!(!set.contains("/usr/bin/top"));
        assert!(!set.iter().any(|s| s.contains(' ')));
    }
}
