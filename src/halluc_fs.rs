//! Deterministic virtual filesystem for hallucination-mode guided shell.
//! Simple builtins stay repeatable; unknown lines fall through to the LLM.

use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct HallucFs {
    /// Filenames under the virtual home (`/root`).
    files: BTreeMap<String, String>,
}

impl HallucFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_files(files: BTreeMap<String, String>) -> Self {
        Self { files }
    }

    pub fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    /// Try to execute a simple shell line against the virtual FS.
    /// Returns `Some(stdout)` when handled; `None` to fall through to the LLM.
    pub fn try_exec(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return Some(String::new());
        }

        if let Some((content, path, append)) = parse_echo_redirect(line) {
            let body = if content.ends_with('\n') {
                content
            } else {
                format!("{content}\n")
            };
            if append {
                self.files
                    .entry(path)
                    .and_modify(|cur| cur.push_str(&body))
                    .or_insert(body);
            } else {
                self.files.insert(path, body);
            }
            return Some(String::new());
        }

        if let Some(rest) = line.strip_prefix("echo") {
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                let args = rest.trim_start();
                let (n_flag, text) = strip_echo_n(args);
                let text = unescape_echo_args(&text);
                if n_flag {
                    return Some(text);
                }
                return Some(format!("{text}\n"));
            }
        }

        if line == "ls" || line.starts_with("ls ") {
            let args = line.strip_prefix("ls").unwrap_or("").trim();
            let path_arg = args
                .split_whitespace()
                .find(|a| !a.starts_with('-'))
                .unwrap_or("");
            if !path_arg.is_empty() && path_arg != "." && path_arg != "./" {
                let name = path_arg.trim_start_matches("./");
                if self.files.contains_key(name) {
                    return Some(format!("{name}\n"));
                }
                return Some(format!("ls: {name}: No such file or directory\n"));
            }
            if self.files.is_empty() {
                return Some(String::new());
            }
            let mut out = self
                .files
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            out.push('\n');
            return Some(out);
        }

        if let Some(rest) = line.strip_prefix("cat") {
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                let path = rest.trim().trim_start_matches("./");
                if path.is_empty() {
                    return Some("cat: invalid option\n".into());
                }
                return Some(match self.files.get(path) {
                    Some(content) => {
                        let mut s = content.clone();
                        if s.ends_with('\n') {
                            s.pop();
                        }
                        if s.is_empty() {
                            String::new()
                        } else {
                            format!("{s}\n")
                        }
                    }
                    None => format!("cat: can't open '{path}': No such file or directory\n"),
                });
            }
        }

        if line == "pwd" {
            return Some("/root\n".into());
        }

        None
    }
}

fn parse_echo_redirect(line: &str) -> Option<(String, String, bool)> {
    let echo = line.strip_prefix("echo")?;
    if !echo.is_empty() && !echo.starts_with(' ') && !echo.starts_with('\t') {
        return None;
    }
    let rest = echo.trim_start();
    if let Some(idx) = rest.find(">>") {
        let content = rest[..idx].trim();
        let path = rest[idx + 2..].trim();
        if path.is_empty() || path.contains(char::is_whitespace) {
            return None;
        }
        let (_, text) = strip_echo_n(content);
        return Some((unescape_echo_args(&text), path.to_string(), true));
    }
    if let Some(idx) = rest.find('>') {
        let content = rest[..idx].trim();
        let path = rest[idx + 1..].trim();
        if path.is_empty() || path.contains(char::is_whitespace) {
            return None;
        }
        let (_, text) = strip_echo_n(content);
        return Some((unescape_echo_args(&text), path.to_string(), false));
    }
    None
}

fn strip_echo_n(args: &str) -> (bool, String) {
    let args = args.trim();
    if let Some(a) = args.strip_prefix("-n") {
        if a.is_empty() || a.starts_with(' ') {
            return (true, a.trim_start().to_string());
        }
    }
    (false, args.to_string())
}

fn unescape_echo_args(args: &str) -> String {
    let args = args.trim();
    if args.len() >= 2
        && ((args.starts_with('"') && args.ends_with('"'))
            || (args.starts_with('\'') && args.ends_with('\'')))
    {
        return args[1..args.len() - 1].to_string();
    }
    args.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_path() {
        let mut fs = HallucFs::new();
        assert_eq!(fs.try_exec("ls -a").unwrap(), "");
        assert_eq!(fs.try_exec("echo hi > hi.txt").unwrap(), "");
        assert_eq!(fs.try_exec("ls -a").unwrap(), "hi.txt\n");
        assert_eq!(fs.try_exec("cat hi.txt").unwrap(), "hi\n");
    }
}
