# ADR-0014: SINE assistant

- **Status:** Accepted
- **Date:** 2026-11-22

## Context

Operators want a semi-persistent, plan-then-approve assistant that can see the
crew roster, scheduled loops, and command ledger without inventing a second
control plane. Those surfaces already have decisions:
[ADR-0001](0001-alpine-llm-linux-shell.md),
[ADR-0003](0003-runtime-integrations.md),
[ADR-0004](0004-mcp-integration-steering.md),
[ADR-0007](0007-shomagent-background-crew.md),
[ADR-0013](0013-shomloop.md). The learn confirm loop
([ADR-0008](0008-learn-userland-verb.md)) is a UX ancestor only.

SINE must track those ecosystems through this ADR. The split-pane chrome is
local UI and is not part of this contract.

## Decision

Ship **SINE** as a foreground `engram-shell` screen (`:sine` / bare `sine`):

| Surface | Behavior |
|---------|----------|
| Open | `:sine` or exact bare `sine`; optional remainder is the first request |
| Layout | Left top (brand/art from `docs/sine_image.txt`), left bottom (agents / loops / files), right top (agent), right bottom (chat), status + command line. Tab-focus left, ↑↓ select a touched file, Enter opens a full-screen numbered/wrapped viewer (Esc closes) |
| Plan | One local `chat_text_max` JSON plan; show it; user `approve` / `edit` / `cancel` |
| `auto` | Shell flag stored in the session; still shows the plan, then runs without a second confirm. The model cannot flip it |
| Idle | While the screen is open and quiet, read top commands, one short model call, rewrite the playbook |
| Session | `/engram/user/.sine/` (`session.json`, `playbook.md`); restored on the next open. Esc saves and returns. No background work after exit |
| Stub | `ENGRAM_SINE_STUB=1` for CI |

Allowed dependency surface (code implements only this list):

| Upstream | SINE may use |
|----------|--------------|
| ADR-0001 shell/modes | Run inside `engram-shell`. The shell executes steps. Hallucination shows the plan and does not run it or write the session. Root may run shell steps and skips the model. The model cannot change mode, turn on `auto`, or add a verb |
| ADR-0003 local model | Planning and idle each use one `LlmClient::chat_text_max` call to the in-container OpenAI-compatible URL. No cloud client |
| ADR-0004 command ledger | Idle and the `top_commands` verb read `/engram/user/.command_freq.json` the way `:top` does. No new MCP server. SINE does not call `mcp__cmdfreq__top_commands` itself |
| ADR-0007 crew | Left-bottom roster and the `shomagent` verb use `shomagent::daemon::rpc` on `/run/shomagent/ctl.sock`. Allowed requests: roster, ask (Talk), pause, resume, stop. SINE does not supervise processes and does not speak dream, refine, or web unless a later update to this ADR adds them |
| ADR-0013 scheduler | Roster loop list via `shomloop::list_runs()` on `~/.loop/planned_runs.csv`. No crontab rewrite. enable/disable/delete/edit stay out until this ADR says otherwise |
| ADR-0008 learn | UX ancestor for plan-then-confirm only. SINE does not call `learn::promote` and does not install scripts |
| ADR-0009 quickyn | Plan verb `quickyn` calls `quickyn::decide` in-process for yes/no + confidence. Optional `file` arg is pre-prompt context. Prefer this over inventing a shell yes/no |

Closed plan verbs the shell may execute after approval: `shell`, `shomagent`,
`top_commands`, `say`, `quickyn`. Unknown verbs are refused.

Touched files appear under **files:** in the left-bottom pane and persist in
`session.json`. Sources: shell redirects / path-like args / `tee`, files
created or mtime-updated in cwd (and `/engram/user`) during a shell step
(catches `yc_headlines_$(date …).csv`), `/engram/…` paths printed in shell
output, session.json, playbook.md, **plan.md** (written when a plan is
presented; kept across Ctrl-K so a small local model can recover after
compaction), and quickyn file args.

Planning prompt steers toward first-party CLIs. Webpage / JS work should be
planned as `shell` steps that run `shomagent web`, then a roster/`shomagent list`
step, then `ask` / `stop` — not as `curl` HTML scrapers. Plans must not invent
member ids from chat history; they use id `0` as a placeholder after web+roster.
At run time SINE binds `0` (and any stale planned id missing from the live
roster) to the last attached member still on the roster. Native plan verb
`shomagent` remains limited to ask/pause/resume/stop/roster (`list` normalizes
to roster); opening a URL is always via `shell`.

When the operator asks for recurring work (and optionally “also right now”),
the preferred plan is: write a self-contained job script under `/engram/user/`,
`chmod +x`, run it once for the test, then a `shell` step
`shomloop "<frequency>" /engram/user/<job>.sh` ([ADR-0013](0013-shomloop.md)).
The script parses its own member ids; SINE’s id-`0` binding does not apply
inside `crond`. There is still no native `shomloop` plan verb — create/list/
enable go through the CLI via `shell`.

## Consequences

- A change to ADR-0001, 0003, 0004, 0007, or 0013 that alters a surface listed
  above updates this ADR in the same change set. SINE's code then follows 0014.
- A pull request that teaches SINE a new verb, RPC, or ledger path includes that
  update here. The how-to (`docs/sine.md`) explains the screen; it is not a
  second contract.
- If an upstream ADR and this ADR disagree, the upstream ADR wins and this file
  is corrected. `docs/llm.txt` points at 0014 and does not restate the verb list.
- Modules: `src/split_frame.rs` (generic UI), `src/sine_tui.rs` (assistant).
  No new workspace crate.
