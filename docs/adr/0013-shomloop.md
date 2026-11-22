# ADR-0013: shomloop scheduled commands

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

Operators want a command to repeat on a schedule (`daily at 5pm`, `weekly`,
`hourly`) without hand-editing crontab. The container does not keep `crond`
across a restart, so the plan has to live somewhere the entrypoint can
reinstall from.

Related: [ADR-0001](0001-alpine-llm-linux-shell.md),
[ADR-0002](0002-rust-language-and-style.md),
[docs/shomloop.md](../shomloop.md).

## Decision

Ship a first-party **`shomloop`** binary at the same layer as `quickyn`:

| Surface | Behavior |
|---------|----------|
| `shomloop "<frequency>" <command>` | Parse the phrase, append `~/.loop/planned_runs.csv`, rewrite crontab |
| `shomloop list` | Print planned runs |
| `shomloop logs [id]` | Print `/tmp/shomloop` job logs |
| `shomloop enable\|disable\|delete\|edit` | Update the row and rewrite crontab |
| `shomloop restore` | Entrypoint: rewrite the marked crontab block and start `crond` |
| `:menu` → shomloop | New loop, logs, and per-run edit / disable / enable / delete |

- Workspace crate **`crates/shomloop/`** (lib + CLI). The shell menu reads the
  ledger through the lib. Crontab changes stay in the binary.
- Frequencies are a closed phrase list plus a raw five-field cron expression.
  Unknown wording is an error. No model call.
- Cron runs `shomloop run <id>`. The user command is not stored on the crontab
  line. Disabled rows stay in the CSV and are omitted from crontab.
- The ledger path is `$HOME/.loop/planned_runs.csv`. The default container is
  root, so that is `/root/.loop/planned_runs.csv`.
- Agent mode lists `shomloop` in direct-shell detection so the verb is not
  swallowed by chat.

## Consequences

- A container restart rebuilds crontab from the CSV and starts `crond` again.
- Logs under `/tmp/shomloop` do not survive a fresh container.
- Lines outside the `# BEGIN shomloop` / `# END shomloop` markers are left in
  place.
- Phrases outside the list are rejected rather than guessed.
