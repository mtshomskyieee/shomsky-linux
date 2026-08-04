---
name: shell
description: Basic shell commands and filesystem tool use for BusyBox ash on Shomsky Linux
always_load: true
---

# Shell skill

You are running inside **Shomsky Linux**, an Alpine-derived container. The real
shell is BusyBox ash. Prefer the provided tools over inventing output.

## Tools

| Tool | Purpose |
|------|---------|
| `shell_exec` | Run a BusyBox ash command; returns stdout/stderr and exit status |
| `read_file` | Read a file as text |
| `write_file` | Create or overwrite a file (agent-mode only) |
| `list_dir` | List directory entries |
| `load_skill` | Load another skill from `/engram/skills` by name |
| `mcp__cmdfreq__top_commands` | Ranked most-used commands (default `limit` **10**) |
| `mcp__cmdfreq__record_command` | Record a command (usually done automatically by the shell) |

### Top 10 commands

When the user asks for their most-used or top commands, call:

```
mcp__cmdfreq__top_commands
{"limit": 10}
```

Text style: `TOOL mcp__cmdfreq__top_commands {"limit":10}`

Users can also run the meta-command `:top` / `:top 10` without involving you.

Interactive TUI programs (`top`, `mc`, `vim`, …) are listed in
`ignore_llm_for_these_commands` and are run by the shell on the real TTY —
never call `shell_exec` for them.

## BusyBox essentials

```
ls [-la] [path]     list files
pwd                 print working directory
cd PATH             change directory (via shell_exec of a compound command)
cat FILE            print file (prefer read_file tool)
echo TEXT           print text
mkdir [-p] DIR      create directory
rm [-rf] PATH       remove (use carefully)
cp SRC DST          copy
mv SRC DST          move/rename
find PATH -name P   search
grep -n PATTERN F   search file contents
head -n N FILE      first N lines
tail -n N FILE      last N lines
wc -l FILE          line count
uname -a            kernel / OS info
cat /etc/os-release distribution identity
apk add PKG         install package (needs network + root)
```

## Filesystem layout (Shomsky)

```
/engram/            Engram root (skills, models, shell history)
/engram/user/       Default working directory (host-mounted workspace)
/engram/skills/     Markdown skills (this file lives here)
/engram/models/     GGUF model volume (downloaded on first run)
/etc/engram/        Distro config (engram.toml, models.toml)
/etc/os-release     ID=shomsky, ID_LIKE=alpine
/bin/busybox        Real shell binary
```

Prefer creating and editing files under `/engram/user` (agent-mode home;
`cd ~` lands here). In root mode, `cd ~` goes to `/root`. In agent and root
modes, `cd` changes the session working directory for later commands.

## Tool-use rules

1. When the user asks what is on disk, call `list_dir` or `shell_exec` with `ls`.
2. When the user asks to read a file, prefer `read_file`.
3. When the user asks to create or edit a file, use `write_file` (or
   `shell_exec` with a heredoc only if necessary).
4. Chain tools: explore with `list_dir` before guessing paths.
5. Keep commands BusyBox-compatible — no bash arrays, no GNU-only flags.
6. Do not pretend a command succeeded if a tool returned an error.
7. Use built-in tools (`shell_exec`, `read_file`, `write_file`, `list_dir`,
   `load_skill`) and connected MCP tools (`mcp__…`). Never invent other names.
   (Hallucination-mode does not load this skill — the model simulates a shell
   from conversation history instead of calling tools.)

## Text tool protocol (small models)

If native API tool calling is unavailable, emit an allow-listed pattern the
shell can grep:

```
<tool_call>
{"name":"list_dir","arguments":{"path":"/etc/engram"}}
</tool_call>
```

or a single line:

```
TOOL read_file path=/etc/os-release
TOOL shell_exec command=uname -a
```

The shell executes the tool, appends `<tool_result>…</tool_result>` to the
context, and continues. Keep replies short so the context window stays small.

## Compound commands

`shell_exec` runs each invocation in a fresh ash process. Working directory and
exported variables do **not** persist across calls. For multi-step work, combine
with `&&` in a single command, or use absolute paths.
