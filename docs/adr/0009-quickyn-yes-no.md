# ADR-0009: quickyn yes/no CLI

- **Status:** Accepted
- **Date:** 2026-10-17

## Context

Shomsky Linux is LLM + linux + commandline. Beside the REPL and the
`shomagent` crew, operators need a **composable one-shot decision** tool: ask
the current local model a yes/no question, get a confidence, and use the exit
status in scripts. Optional file context (chain-of-thought pre-prompt) must stay
on the CLI without growing a daemon.

Related: [ADR-0001](0001-alpine-llm-linux-shell.md),
[ADR-0002](0002-rust-language-and-style.md),
[ADR-0007](0007-shomagent-background-crew.md),
[docs/quickyn.md](../quickyn.md).

## Decision

Ship a first-party **`quickyn`** binary at the same layer as `shomagent`:

| Surface | Behavior |
|---------|----------|
| `quickyn "<question>"` | Yes/no + confidence from the configured local model |
| `quickyn "<question>" <file>` | File body is pre-prompt context before the question |
| `:quickyn` (shell) | Split-screen TUI over the same `decide` function (question + file left; check/X + verdict right) |
| stdout | One line: `yes 0.91` or `no 0.42` |
| exit | `0` = yes, `1` = no, `2` = usage / missing file / unparseable |
| stderr | Size warning when file `chars().count() > 1000`; errors |

- Workspace crate **`crates/quickyn/`** (lib + CLI). Thin OpenAI-compatible chat
  client; **no** dependency on `shomagent`. Shared **`decide(question, file_body)`**
  used by the binary and by `engram-shell`’s `:quickyn` TUI (`src/quickyn_tui.rs`,
  ANSI + BusyBox `stty` like `:menu`).
- Env: `ENGRAM_LLM_URL` / `ENGRAM_LLM_MODEL` (defaults match the shell). Stub:
  `QUICKYN_STUB_LLM=1` (yes unless question or file contains `stub-no`).
- Image installs `/usr/local/bin/quickyn`.
- Agent mode lists `quickyn` in direct-shell detection so the verb is not
  swallowed by chat.

## Consequences

- Scripts can branch on `quickyn` exit status without parsing prose.
- Large files warn but are not truncated; callers own latency.
- Tiny models may drift from the strict two-line format; the parser also
  accepts a looser yes/no + float shape.
