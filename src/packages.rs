//! Ledger of packages installed / removed via `apk add` / `apk del`.
//!
//! Persisted under `/engram/user` so Compose bind-mounts keep history.
//! Surfaced in `:menu` → Packages and `:packages`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Soft cap on unique package names tracked.
pub const MAX_ENTRIES: usize = 500;

/// Image essentials — menu warns before uninstall; skill refuses without insist.
pub const ESSENTIAL_PACKAGES: &[&str] = &[
    "busybox",
    "bash",
    "bats",
    "ca-certificates",
    "curl",
    "libgcc",
    "libstdc++",
    "openssl",
];

pub fn is_essential(name: &str) -> bool {
    ESSENTIAL_PACKAGES.iter().any(|p| *p == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageStatus {
    Installed,
    Removed,
}

impl PackageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageStatus::Installed => "installed",
            PackageStatus::Removed => "removed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub status: PackageStatus,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageStore {
    pub packages: BTreeMap<String, PackageEntry>,
    #[serde(default)]
    pub updated_at: String,
}

impl PackageStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        Ok(())
    }

    pub fn record_install(&mut self, name: &str) {
        self.set(name, PackageStatus::Installed);
    }

    pub fn record_remove(&mut self, name: &str) {
        self.set(name, PackageStatus::Removed);
    }

    fn set(&mut self, name: &str, status: PackageStatus) {
        let name = name.trim();
        if name.is_empty() || name.starts_with('-') {
            return;
        }
        // Strip apk version pin: neovim=0.9.5-r0 → neovim
        let name = name.split('=').next().unwrap_or(name).trim();
        if name.is_empty() {
            return;
        }
        let now = now_rfc3339();
        self.packages.insert(
            name.to_string(),
            PackageEntry {
                status,
                updated_at: now.clone(),
            },
        );
        self.updated_at = now;
        self.evict_if_needed();
    }

    fn evict_if_needed(&mut self) {
        while self.packages.len() > MAX_ENTRIES {
            let victim = self
                .packages
                .iter()
                .min_by(|a, b| {
                    a.1.updated_at
                        .cmp(&b.1.updated_at)
                        .then_with(|| a.0.cmp(b.0))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = victim {
                self.packages.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Installed first, then removed; each group by name.
    pub fn list_sorted(&self) -> Vec<(&str, &PackageEntry)> {
        let mut items: Vec<(&str, &PackageEntry)> = self
            .packages
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        items.sort_by(|a, b| {
            status_rank(a.1.status)
                .cmp(&status_rank(b.1.status))
                .then_with(|| a.0.cmp(b.0))
        });
        items
    }
}

fn status_rank(s: PackageStatus) -> u8 {
    match s {
        PackageStatus::Installed => 0,
        PackageStatus::Removed => 1,
    }
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Compact UTC-ish stamp without chrono dependency.
    format!("{secs}")
}

/// Apply mutations parsed from a successful shell command line.
pub fn record_from_command(path: &Path, command: &str) {
    let mutations = parse_apk_mutations(command);
    if mutations.is_empty() {
        return;
    }
    let mut store = PackageStore::load(path).unwrap_or_default();
    for (status, name) in mutations {
        match status {
            PackageStatus::Installed => store.record_install(&name),
            PackageStatus::Removed => store.record_remove(&name),
        }
    }
    let _ = store.save(path);
}

/// A `shominstall` line: the user wants packages installed, and the agent
/// should do it. Package names may be empty (`shominstall` alone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shominstall {
    pub packages: Vec<String>,
}

/// Recognize `shominstall git` and `please shominstall git`.
///
/// Returns `None` for questions, shell pipelines, and any other verb.
/// BusyBox `install` and `apk` do not match.
pub fn parse_shominstall(line: &str) -> Option<Shominstall> {
    let original = line.trim();
    if original.is_empty() || original.starts_with(':') || is_install_question(original) {
        return None;
    }
    if has_shell_operator(original) {
        return None;
    }
    let normalized = normalize_install_line(original);
    let (_polite, rest) = strip_request_prefix(&normalized);
    let tokens = install_tokens(&rest);
    let (kind, verb_at) = find_install_verb(&tokens)?;
    if kind != InstallVerb::Shominstall {
        return None;
    }
    let mut packages = Vec::new();
    for tok in tokens.iter().skip(verb_at + 1) {
        let lower = tok.to_ascii_lowercase();
        if is_install_filler(&lower) || tok.starts_with('-') {
            continue;
        }
        if tok.contains('/') || tok.starts_with('.') || !is_package_token(tok) {
            if packages.is_empty() {
                return None;
            }
            break;
        }
        packages.push(tok.clone());
    }
    Some(Shominstall { packages })
}

/// User message for the agent. `shominstall` is not a binary; the agent
/// installs with `apk` after resolving the name.
pub fn shominstall_agent_prompt(line: &str) -> Option<String> {
    let req = parse_shominstall(line)?;
    if req.packages.is_empty() {
        return Some(
            "The user typed `shominstall` with no package name. \
             Ask which Alpine package to install. \
             `shominstall` means install a package with apk. \
             It is not BusyBox `install` and it is not a binary."
                .into(),
        );
    }
    let pkgs = req.packages.join(" ");
    Some(format!(
        "The user typed `shominstall {pkgs}`. \
         Install those Alpine packages in this container. \
         Follow the packages skill: resolve each name, then `apk add`. \
         Do not run BusyBox `install`. \
         `shominstall` is not a binary — do not execute it."
    ))
}

/// When `line` is a request to install software, return the `apk add` command.
///
/// Shomsky is Alpine, so `install git`, `please install git`, and
/// `apt-get install git` all mean `apk add`. BusyBox `install` is left alone:
/// any flag, any path, or two-or-more operands (`install SOURCE DEST`) returns
/// `None`.
pub fn package_install_command(line: &str) -> Option<String> {
    let original = line.trim();
    if original.is_empty() || original.starts_with(':') || is_install_question(original) {
        return None;
    }
    if has_shell_operator(original) {
        return None;
    }

    let normalized = normalize_install_line(original);
    let (polite, rest) = strip_request_prefix(&normalized);
    let tokens = install_tokens(&rest);
    if tokens.is_empty() {
        return None;
    }

    let (kind, verb_at) = find_install_verb(&tokens)?;
    if kind == InstallVerb::ApkAdd {
        return None;
    }

    let mut pkgs = Vec::new();
    let mut saw_package_word = false;
    for tok in tokens.iter().skip(verb_at + 1) {
        let lower = tok.to_ascii_lowercase();
        if is_install_filler(&lower) {
            if lower == "package" || lower == "packages" {
                saw_package_word = true;
            }
            continue;
        }
        if tok.starts_with('-') {
            if kind == InstallVerb::Apt || kind == InstallVerb::Shominstall {
                continue;
            }
            // BusyBox install flags: -m, -d, -o, -t, …
            return None;
        }
        if tok.contains('/') || tok.starts_with('.') {
            return None;
        }
        if !is_package_token(tok) {
            if pkgs.is_empty() {
                return None;
            }
            break;
        }
        pkgs.push(tok.clone());
    }

    if pkgs.is_empty() {
        return None;
    }
    // `install SRC DEST` is BusyBox. Multiple names are packages only when the
    // line is clearly a request (politeness, apt, or the word "package").
    if kind == InstallVerb::Install && !polite && !saw_package_word && pkgs.len() >= 2 {
        return None;
    }

    Some(format!("apk add {}", pkgs.join(" ")))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallVerb {
    /// Already `apk add` — do not rewrite.
    ApkAdd,
    /// Bare `install NAME`.
    Install,
    /// `apt` / `apt-get install`.
    Apt,
    /// `shominstall NAME` — user verb for a package install.
    Shominstall,
}

fn is_install_question(line: &str) -> bool {
    if line.contains('?') {
        return true;
    }
    let lower = line.trim().to_ascii_lowercase();
    const QUESTIONS: &[&str] = &[
        "how ", "what ", "why ", "when ", "where ", "who ", "which ", "explain ",
    ];
    QUESTIONS.iter().any(|p| lower.starts_with(p))
}

fn has_shell_operator(line: &str) -> bool {
    let rest = line.trim().trim_start_matches('$').trim_start();
    rest.contains('|')
        || rest.contains('>')
        || rest.contains('<')
        || rest.contains('&')
        || rest.contains(';')
        || rest.contains('$')
        || rest.contains('`')
        || rest.contains('\n')
}

fn normalize_install_line(line: &str) -> String {
    let trimmed = line.trim().trim_start_matches('$').trim_start();
    trimmed
        .chars()
        .map(|c| match c {
            ',' | ';' | ':' | '!' | '?' => ' ',
            _ => c,
        })
        .collect()
}

fn strip_request_prefix(line: &str) -> (bool, String) {
    let mut rest = line.trim().to_string();
    let mut polite = false;
    const POLITE: &[&str] = &[
        "could you please ",
        "can you please ",
        "would you please ",
        "will you please ",
        "could you ",
        "can you ",
        "would you ",
        "will you ",
        "i want you to ",
        "i need you to ",
        "i'd like you to ",
        "id like you to ",
        "i want to ",
        "i need to ",
        "i'd like to ",
        "id like to ",
        "help me ",
        "please ",
        "pls ",
        "kindly ",
    ];
    loop {
        let lower = rest.to_ascii_lowercase();
        let mut ate = false;
        for prefix in POLITE {
            if lower.starts_with(prefix) {
                rest = rest[prefix.len()..].trim_start().to_string();
                polite = true;
                ate = true;
                break;
            }
        }
        if !ate {
            break;
        }
    }
    (polite, rest)
}

fn install_tokens(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn find_install_verb(tokens: &[String]) -> Option<(InstallVerb, usize)> {
    let head = tokens.first()?.to_ascii_lowercase();
    if head == "sudo" || head == "doas" {
        return find_install_verb(&tokens[1..]).map(|(k, i)| (k, i + 1));
    }
    if head == "apk" {
        let action = tokens.get(1).map(|s| s.to_ascii_lowercase());
        if action.as_deref() == Some("add") {
            return Some((InstallVerb::ApkAdd, 1));
        }
        return None;
    }
    if head == "apt" || head == "apt-get" {
        let install_at = tokens.iter().skip(1).position(|t| {
            let l = t.to_ascii_lowercase();
            l == "install" || l == "add"
        })?;
        return Some((InstallVerb::Apt, install_at + 1));
    }
    if head == "install" {
        return Some((InstallVerb::Install, 0));
    }
    if head == "shominstall" {
        return Some((InstallVerb::Shominstall, 0));
    }
    None
}

fn is_install_filler(lower: &str) -> bool {
    matches!(
        lower,
        "the"
            | "a"
            | "an"
            | "some"
            | "me"
            | "my"
            | "package"
            | "packages"
            | "for"
            | "now"
            | "thanks"
            | "thank"
            | "you"
            | "and"
            | "plus"
            | "also"
            | "to"
            | "just"
            | "software"
            | "program"
            | "programs"
            | "tool"
            | "app"
            | "application"
            | "using"
            | "with"
            | "via"
    )
}

fn is_package_token(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    const NOT_PKG: &[&str] = &[
        "it", "them", "this", "that", "these", "those", "something", "one", "please",
    ];
    if NOT_PKG.contains(&lower.as_str()) {
        return false;
    }
    let mut chars = tok.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '.' | '-'))
}

/// Parse `apk add` / `apk del` package names from a BusyBox ash command line.
pub fn parse_apk_mutations(command: &str) -> Vec<(PackageStatus, String)> {
    let mut out = Vec::new();
    for segment in split_shell_segments(command) {
        let tokens = shell_tokens(segment);
        let Some(apk_idx) = tokens.iter().position(|t| t == "apk") else {
            continue;
        };
        let Some(action) = tokens.get(apk_idx + 1).map(|s| s.as_str()) else {
            continue;
        };
        let status = match action {
            "add" => PackageStatus::Installed,
            "del" | "delete" | "remove" => PackageStatus::Removed,
            _ => continue,
        };
        for tok in tokens.iter().skip(apk_idx + 2) {
            if tok.starts_with('-') {
                continue;
            }
            // Stop at redirections
            if tok == ">" || tok == ">>" || tok == "<" || tok.starts_with('>') {
                break;
            }
            out.push((status, tok.clone()));
        }
    }
    out
}

fn split_shell_segments(command: &str) -> Vec<&str> {
    let mut segs = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b';' || bytes[i] == b'\n' {
            segs.push(&command[start..i]);
            start = i + 1;
            i += 1;
            continue;
        }
        if i + 1 < bytes.len()
            && ((bytes[i] == b'&' && bytes[i + 1] == b'&')
                || (bytes[i] == b'|' && bytes[i + 1] == b'|'))
        {
            segs.push(&command[start..i]);
            start = i + 2;
            i += 2;
            continue;
        }
        // Do not split on single `|` (pipeline) — apk rarely piped; treat whole as one.
        i += 1;
    }
    segs.push(&command[start..]);
    segs
}

fn shell_tokens(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn format_list(store: &PackageStore) -> String {
    let items = store.list_sorted();
    if items.is_empty() {
        return "No packages tracked yet. Successful `apk add` / `apk del` are recorded."
            .to_string();
    }
    let mut lines = Vec::new();
    lines.push(format!(
        "{:<20} {:<10} {}",
        "PACKAGE", "STATUS", "UPDATED"
    ));
    for (name, entry) in items {
        lines.push(format!(
            "{:<20} {:<10} {}",
            name,
            entry.status.as_str(),
            entry.updated_at
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_path(prefix: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("engram-packages-{prefix}-{n}.json"))
    }

    #[test]
    fn parse_apk_add_simple() {
        let m = parse_apk_mutations("apk add neovim");
        assert_eq!(m, vec![(PackageStatus::Installed, "neovim".into())]);
    }

    #[test]
    fn parse_apk_add_flags_and_multi() {
        let m = parse_apk_mutations("apk add --no-cache neovim htop");
        assert_eq!(
            m,
            vec![
                (PackageStatus::Installed, "neovim".into()),
                (PackageStatus::Installed, "htop".into()),
            ]
        );
    }

    #[test]
    fn parse_apk_del() {
        let m = parse_apk_mutations("apk del neovim");
        assert_eq!(m, vec![(PackageStatus::Removed, "neovim".into())]);
    }

    #[test]
    fn parse_compound() {
        let m = parse_apk_mutations("apk update && apk add neovim");
        assert_eq!(m, vec![(PackageStatus::Installed, "neovim".into())]);
    }

    #[test]
    fn parse_ignores_search() {
        assert!(parse_apk_mutations("apk search -q neovim").is_empty());
    }

    #[test]
    fn shominstall_steers_the_agent() {
        let req = parse_shominstall("shominstall git").unwrap();
        assert_eq!(req.packages, vec!["git".to_string()]);
        let prompt = shominstall_agent_prompt("please shominstall git").unwrap();
        assert!(prompt.contains("shominstall git"));
        assert!(prompt.contains("apk add"));
        assert!(prompt.contains("BusyBox"));
        assert_eq!(parse_shominstall("install git"), None);
        assert_eq!(parse_shominstall("apk add git"), None);
        assert_eq!(parse_shominstall("shominstall"), Some(Shominstall { packages: vec![] }));
        assert_eq!(
            package_install_command("shominstall git curl"),
            Some("apk add git curl".into())
        );
    }

    #[test]
    fn install_name_becomes_apk_add() {
        assert_eq!(
            package_install_command("install git"),
            Some("apk add git".into())
        );
        assert_eq!(
            package_install_command("  $ install git "),
            Some("apk add git".into())
        );
    }

    #[test]
    fn natural_language_install() {
        assert_eq!(
            package_install_command("please install git"),
            Some("apk add git".into())
        );
        assert_eq!(
            package_install_command("can you install git"),
            Some("apk add git".into())
        );
        assert_eq!(
            package_install_command("please install the git package"),
            Some("apk add git".into())
        );
        assert_eq!(
            package_install_command("please install git and curl"),
            Some("apk add git curl".into())
        );
        assert_eq!(
            package_install_command("help me install neovim"),
            Some("apk add neovim".into())
        );
    }

    #[test]
    fn apt_get_becomes_apk_add() {
        assert_eq!(
            package_install_command("apt-get install git"),
            Some("apk add git".into())
        );
        assert_eq!(
            package_install_command("apt-get install -y git"),
            Some("apk add git".into())
        );
        assert_eq!(
            package_install_command("please apt install git curl"),
            Some("apk add git curl".into())
        );
        assert_eq!(
            package_install_command("sudo apt-get install git"),
            Some("apk add git".into())
        );
    }

    #[test]
    fn busybox_install_is_unchanged() {
        assert_eq!(package_install_command("install -m 755 src dest"), None);
        assert_eq!(package_install_command("install -d /tmp/foo"), None);
        assert_eq!(package_install_command("install src dest"), None);
        assert_eq!(package_install_command("install ./git /usr/local/bin/git"), None);
        assert_eq!(package_install_command("install git /usr/bin/git"), None);
        assert_eq!(package_install_command("install"), None);
        assert_eq!(package_install_command("apk add git"), None);
    }

    #[test]
    fn install_questions_are_not_commands() {
        assert_eq!(package_install_command("how do I install git?"), None);
        assert_eq!(package_install_command("what is install git"), None);
        assert_eq!(package_install_command("please install it"), None);
        assert_eq!(package_install_command("install git && echo done"), None);
    }

    #[test]
    fn parse_version_pin() {
        let m = parse_apk_mutations("apk add neovim=0.9.5-r0");
        assert_eq!(m, vec![(PackageStatus::Installed, "neovim=0.9.5-r0".into())]);
        let mut store = PackageStore::default();
        store.record_install("neovim=0.9.5-r0");
        assert!(store.packages.contains_key("neovim"));
    }

    #[test]
    fn round_trip_save() {
        let path = tmp_path("rt");
        let mut store = PackageStore::default();
        store.record_install("neovim");
        store.record_remove("htop");
        store.save(&path).unwrap();
        let loaded = PackageStore::load(&path).unwrap();
        assert_eq!(
            loaded.packages["neovim"].status,
            PackageStatus::Installed
        );
        assert_eq!(loaded.packages["htop"].status, PackageStatus::Removed);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn record_from_command_writes() {
        let path = tmp_path("cmd");
        record_from_command(&path, "apk add neovim");
        let loaded = PackageStore::load(&path).unwrap();
        assert_eq!(
            loaded.packages["neovim"].status,
            PackageStatus::Installed
        );
        record_from_command(&path, "apk del neovim");
        let loaded = PackageStore::load(&path).unwrap();
        assert_eq!(loaded.packages["neovim"].status, PackageStatus::Removed);
        let _ = fs::remove_file(&path);
    }
}
