//! Heuristics for treating a line as a direct shell command vs conversation.

/// Return true when the line should execute as a real/simulated shell command
/// without waiting for the LLM to invent a tool call.
pub fn looks_like_shell_command(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with(':') {
        return false;
    }

    let lower = t.to_ascii_lowercase();
    const CHATTY: &[&str] = &[
        "what ",
        "why ",
        "how ",
        "please ",
        "can you",
        "could you",
        "would you",
        "explain ",
        "tell me",
        "write me",
        "help me",
        "help ",
        "describe ",
        "summarize ",
        "who are",
        "hello",
        "hi ",
        "hey ",
        "thanks",
        "thank you",
    ];
    for p in CHATTY {
        if lower.starts_with(p) || lower == p.trim() {
            return false;
        }
    }

    // Shell metacharacters almost always mean a command pipeline.
    if t.contains('|')
        || t.contains('>')
        || t.contains('<')
        || t.contains("&&")
        || t.contains("||")
        || t.contains(';')
        || t.contains("$(")
        || t.contains('`')
    {
        return true;
    }

    let first = t.split_whitespace().next().unwrap_or("");
    // Strip simple path prefixes: /bin/ls, ./script
    let cmd = first.rsplit('/').next().unwrap_or(first);

    if learned_command(cmd) {
        return true;
    }

    const CMDS: &[&str] = &[
        "ls", "cd", "pwd", "cat", "echo", "mkdir", "rm", "cp", "mv", "touch", "head",
        "tail", "grep", "find", "wc", "uname", "whoami", "id", "env", "export", "history",
        "df", "du", "ps", "top", "apk", "chmod", "chown", "ln", "file", "which", "type",
        "command", "hash", "alias", "ulimit", "umask", "readonly", "unset", "shift",
        "date", "clear", "printf", "test", "[", "stat", "realpath", "basename", "dirname",
        "sed", "awk", "sort", "uniq", "cut", "tr", "tee", "sleep", "true", "false",
        "busybox", "sh", "ash", "hostname", "free", "uptime", "mount", "umount", "kill",
        "wget", "curl", "tar", "gzip", "gunzip", "sha256sum", "md5sum", "diff", "patch",
        "seq", "yes", "od", "hexdump", "strings", "cmp", "comm", "expand", "unexpand",
        "fold", "fmt", "nl", "rev", "paste", "join", "split", "csplit", "xargs",
        "dirname", "readlink", "sync", "install", "dd", "df", "sync",
    ];
    CMDS.contains(&cmd)
}

/// A name installed by learn into `~/bin`, so typing it runs the script.
fn learned_command(cmd: &str) -> bool {
    if cmd.is_empty()
        || !cmd
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return false;
    }
    shomsky::LearnPaths::from_env().command_path(cmd).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_commands() {
        assert!(looks_like_shell_command("ls"));
        assert!(looks_like_shell_command("ls -la"));
        assert!(looks_like_shell_command("echo hi > file.txt"));
        assert!(looks_like_shell_command("cat file.txt"));
        assert!(!looks_like_shell_command("what is in /etc?"));
        assert!(!looks_like_shell_command("please list the files"));
    }

    #[test]
    fn saved_command_runs_as_shell() {
        let _guard = crate::modes::engram_env_lock();
        let home = std::env::temp_dir().join(format!(
            "shomsky-detect-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let paths = shomsky::LearnPaths::new(&home);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.command_path("list-sort-by-size"), "#!/bin/sh\nls -l | sort -n\n")
            .unwrap();
        let prev = std::env::var_os("ENGRAM_USER_DIR");
        std::env::set_var("ENGRAM_USER_DIR", &home);
        assert!(looks_like_shell_command("list-sort-by-size"));
        assert!(looks_like_shell_command("list-sort-by-size --help"));
        assert!(!looks_like_shell_command("not-saved"));
        match prev {
            Some(v) => std::env::set_var("ENGRAM_USER_DIR", v),
            None => std::env::remove_var("ENGRAM_USER_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
