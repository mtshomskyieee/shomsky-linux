# Shell modes

`engram-shell` has three operating modes. Mode is enforced on the client side —
the model cannot escalate itself.

## agent-mode (default)

The LLM is offered real tools:

| Tool | Effect |
|------|--------|
| `shell_exec` | Runs BusyBox ash (`busybox ash -c …`) |
| `read_file` | Reads a file from disk |
| `write_file` | Creates/overwrites a file |
| `list_dir` | Lists directory entries |
| `load_skill` | Returns the body of a skill from `/engram/skills` |

Shell-shaped lines (`ls`, `echo`, redirects, pipelines, …) usually run
**immediately** via BusyBox (`direct_shell`, on by default) and never wait on
the LLM. Natural-language questions still go to the model.

The process working directory persists across turns in agent and root modes
(so `cd /etc` then `ls` lists `/etc`). `cd ~` / bare `cd` use a mode home:
`/engram/user` in agent mode, `/root` in root mode. Hallucination mode does
not change the real cwd — it stays on the virtual filesystem.

Interactive / TUI programs listed in
`/engram/skills/ignore_llm_for_these_commands.md` (`top`, `mc`, editors, …)
always run on the inherited TTY in agent and root modes — the LLM is skipped
and output is not captured into chat history.

## hallucination-mode

Guided virtual shell: common builtins (`ls`, `echo`, redirects, `cat`, `pwd`)
run against an **in-memory filesystem** so the experience is repeatable and
ASCII-only. Unknown lines still go to the LLM (no tool schemas; quiet no-ops
if a tool call appears). Nothing touches the real disk.

### Session memory

Each hallucination session is auto-saved after every turn as conversation-style
JSON under `/engram/.hallucination_shell_history/` (override with
`ENGRAM_HALLUC_HISTORY_DIR`). With Compose, that path is bind-mounted to
`./.hallucination_shell_history` on the host so sessions survive `--rm`:

```text
hallucination_shell_20260802T181045Z.json
```

The file stores chat messages plus the virtual filesystem snapshot. When you
enter hallucination mode (startup or `:mode hallucination`), you are offered:

- **resume** the latest session (`r`)
- **start fresh** (`f`)

Non-interactive / CI: set `ENGRAM_HALLUC_RESUME=fresh` or `resume`.

`:clear` in hallucination mode starts a new timestamped session file.
Leaving hallucination mode (or exiting the shell) writes a final save.

## root-mode

Bypasses the LLM entirely. Each input line is executed with BusyBox ash.
This is the escape hatch when you need a real shell. The working directory
persists across turns (`cd /tmp` then `pwd` stays in `/tmp`); the prompt
shows the current path.

Root-mode is gated by `allow_root_mode` in `/etc/engram/engram.toml`
(default `true`). Set it to `false` (or omit granting it) for locked-down
deployments.

## Switching modes

At the prompt:

```
:mode                  # show current
:mode agent
:mode hallucination
:mode root
```

Or open the **feature menu** with `:menu` / `:m` and pick a mode from the Modes
category (TTY only; slips out cleanly back to the prompt).

## Feature menu

On a color TTY, `:menu` / `:m` opens a slip-in navigation hub:

| Category | Examples |
|----------|----------|
| Modes | Switch agent / hallucination / root |
| Learn | Prefill `, ` or `?? `; save when a promote is pending |
| shomagent | Roster, or prefill `watch` / `ask` / `talk` / `read` |
| Help | Run `:help` |
| System | `:sys`, `:model`, `:skills`, `:top`, `:clear`, exit |

Arrow keys / `hjkl` / Tab navigate; Enter selects; Esc backs up a level; Esc Esc
(or `q`) leaves the menu. Under pipes, `--once`, and `--script` the overlay is
skipped (`:menu` prints a one-line hint).

Or at start:

```bash
docker run -e ENGRAM_MODE=hallucination …
# or
engram-shell --mode root
```

## Tool styles

Small models often cannot emit native OpenAI `tool_calls`. Engram-shell
supports three strategies (`tool_style` in `engram.toml`, or `:tools` /
`ENGRAM_TOOL_STYLE`):

| Style | Behavior |
|-------|----------|
| `auto` (default) | Use native `tool_calls` when present; otherwise grep the reply for allow-listed text patterns |
| `native` | OpenAI tool schemas only |
| `text` | Text patterns only — no schemas sent (default for `SHOMSKY_MODEL=tiny`) |

### Text pattern protocol

Allow-listed names only: `shell_exec`, `read_file`, `write_file`, `list_dir`,
`load_skill`.

```
<tool_call>
{"name":"list_dir","arguments":{"path":"/etc/engram"}}
</tool_call>
```

or:

```
TOOL read_file path=/etc/os-release
```

Flow (agent mode):

1. Restrict history to `max_context_messages` (default 24)
2. Grep the assistant reply for allow-listed patterns
3. Execute matching tools client-side (mode policy still applies)
4. Append `<tool_result>…</tool_result>` to the context
5. Continue the loop or show the final answer

Unknown tool names (e.g. invented `egremain`) are **ignored**, not executed.

Hallucination mode does not use this protocol — the model answers as a shell.

## Direct shell lines

In **agent** mode, lines that look like shell commands run **immediately** via
BusyBox. Chatty questions still go to the model.

In **hallucination** mode, every line goes to the LLM (virtual shell).

## Status spinner

On an interactive TTY, engram-shell draws an ASCII spinner on stderr while
waiting on the LLM, tools, or model readiness, for example:

```text
[/] thinking... 12s
[-] running shell_exec... 3s
[|] waiting for model... 45s
```

It is silent under pipes, `--once`, and `--script` so tests that capture
`stdout`/`stderr` stay clean. Override with:

| Variable | Values |
|----------|--------|
| `ENGRAM_SPINNER` | `auto` (default), `always`, `off` / `0` / `false` |
| `ENGRAM_LLM_READY_TRIES` | Startup poll budget in seconds (default `600`) |

## Intro animation

Interactive startup plays a Matrix-style green digital rain in the block of
rows the splash is about to occupy, which crystallizes into the Cool S splash
itself — the art is then already on screen, so the handoff to the normal startup
output is seamless rather than a redraw. It runs for ~2 seconds, any keypress
skips it (landing on the same finished art), and scrollback is left holding
exactly the splash. It is off wherever the splash is off (pipes,
`--once`, `--script`, `NO_COLOR=1`, `TERM=dumb`).

| Variable | Values |
|----------|--------|
| `ENGRAM_MATRIX` | `auto` (default), `always`, `off` / `0` / `false` |
| `ENGRAM_MATRIX_SECS` | Duration in seconds, `0`–`10` (default `2`; `0` skips) |
| `ENGRAM_MATRIX_GLYPHS` | `ascii` to avoid halfwidth katakana |

## Quieting mistral.rs logs

Server logs default to `/engram/mistralrs.log` (not the TTY). In the shell:

```
:logs
:logs 100
```

To stream logs to the terminal instead:

```bash
MISTRALRS_LOG_TO_TTY=1 docker compose run --rm shomsky
```
