# ADR-0007: shomagent background crew

- **Status:** Accepted
- **Date:** 2026-08-04

## Context

Long-running watches should not block the interactive TTY or spam it.
Users want supervised background agents that share one notification lane and
support stream-scoped chat. Alpine idioms suggest s6/runit; the container
today uses `entrypoint.sh` as PID 1 with a trap, and adding a full service
manager would reshape the image.

## Decision

Ship **`shomagentd`** (Rust supervisor) and **`shomagent`** (CLI) as first-party
binaries. Members are declared in **`shomagent_crew.d`**:

| Path | Role |
|------|------|
| `/etc/shomsky/shomagent_crew.d/*.toml` | Distro / system drop-ins |
| `./shomagent_crew.d/*.toml` | Repo-local (cwd); overrides same `name` |

Each member has a ring buffer of stdout/stderr, an LLM summarizer on a token
budget, and a **novelty filter** so the same error class does not re-interrupt.
Findings append to `/run/shomagent/notify` (JSONL). `engram-shell` drains that
lane between prompts — the first shared notification lane (future “oops”
reuses the same format).

Supervision is in-process (spawn/reap/restart policy), not s6/runit yet.
Control plane is a Unix socket at `/run/shomagent/ctl.sock`.

CLI surface: `shomagent watch|spawn|read|readwrite|write|ask|talk|pause|resume|stop`
and bare `shomagent` for roster. REPL meta: `:shomagent` / `:agents`.

Document members:

- `read <path>` — attach read-only; `ask`/`talk` use file contents
- `readwrite <path>` — attach writable; plus `write <id> <text>` (append) or
  `write <id> --replace <text>`

## Consequences

- Entrypoint starts `shomagentd` alongside mistralrs; `SKIP_SHOMAGENT=1` disables.
- Local models via existing OpenAI-compatible URL; `SHOMAGENT_STUB_LLM=1` for CI.
- Real s6/runit and Ctrl-G “apply suggestion” are deferred.
- Config tree `/etc/shomsky/` is introduced beside `/etc/engram/`.
