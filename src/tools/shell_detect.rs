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
}
