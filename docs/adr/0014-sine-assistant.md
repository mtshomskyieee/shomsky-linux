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
| Open | `:sine` or exact bare `sine`; optional remainder is the first request. Editor also via `:edit [path]` (picker when omitted), `:menu` → Editor (recent + clear), or SINE `e` |
| Layout | Left top (brand/art from `docs/sine_image.txt`), left bottom (agents / loops / files), right top (agent), right bottom (chat), status + command line. Tab cycles focus: input → files → agent → chat. On files: ↑↓ select a touched file, Enter opens a full-screen numbered/wrapped viewer (Esc closes); `e` opens the editor overlay for that path. On agent/chat: ↑↓ and PageUp/PageDown scroll that pane |
| Editor | Full-screen left/right overlay (`src/sine_editor.rs`): numbered editable buffer + file-scoped AI conversation. Entry: `run_for_path` from `:edit`/menu (owns raw mode + alt screen; creates missing file; records `.edit_history.json` unless hallucination) or `run_embedded` from SINE `e` (parent already owns the TTY). Picker in `src/edit_picker.rs`, history in `src/edit_history.rs`. Esc flushes autosave and returns. Operator and model can change lines (whole-file `content` rewrite preferred; or line `edits`). Model calls use one system + one user payload (current numbered file is source of truth; prior chat is prose context only — no duplicated ask / no UI “rewrote file” assistant turns). `content` wins over `edits`; a multi-line `replace` of line 1 that looks like a whole file is coerced to `content` so the buffer is not duplicated. When the file is truncated in the model prompt (>12k chars, ±40-line window), whole-file `content` is refused and only line `edits` are allowed. Loose/triple-quoted envelopes and fenced JSON are unwrapped to the inner file body (never write the `say`/`content` wrapper into the buffer). Version ring checkpoints before AI applies; Ctrl-Z / Ctrl-Y / typed `undo`/`redo` walk it. Autosave (~800ms after edits) writes the real path and pushes a version ring under `/engram/user/.sine/edits/<encoded-path>/` (`ring.json`, `chat.json`; last 16 distinct snapshots, at least 3 once that many exist). Ctrl-Z / Ctrl-Y undo/redo. Ctrl-K clears the editor chat context (file kept); Ctrl-A / `compact` compactifies that chat into one summary turn (bare `c` is always a typed letter). Type `auto` to toggle `editor_auto`. Chat header shows `ctx N tok · est P%` (chars/4 over system + chat + file slice + pending suggestion; same `ENGRAM_CONTEXT_TOKENS` window as `:chat`). Hallucination mode keeps undo in memory and does not write the path or meta dir |
| `editor_auto` | Separate from plan `auto`. Defaults off. Type `auto` in the editor composer to toggle it (Ctrl-A is compactify). Off: model runs only on send; changes in the reply apply immediately when the operator asked. On: replies with changes are proposals (Enter accepts, Ctrl-K dismisses); after ~20s quiet, one unsolicited suggestion may appear. The model cannot flip `editor_auto` |
| Plan | One local `chat_text_max` JSON plan; show it; user `approve` / `edit` / `cancel` |
| `auto` | Shell flag stored in the session; still shows the plan, then runs without a second confirm. The model cannot flip it. Distinct from editor `editor_auto` |
| Idle | While the screen is open and quiet, read top commands, one short model call, rewrite the playbook |
| Session | `/engram/user/.sine/` (`session.json`, `playbook.md`); restored on the next open. Esc saves and returns. No background work after exit |
| Stub | `ENGRAM_SINE_STUB=1` for CI (plans, idle, and editor replies) |

Allowed dependency surface (code implements only this list):

| Upstream | SINE may use |
|----------|--------------|
| ADR-0001 shell/modes | Run inside `engram-shell`. The shell executes steps. Hallucination shows the plan and does not run it or write the session. Root may run shell steps and skips the model. The model cannot change mode, turn on `auto`, or add a verb |
| ADR-0003 local model | Planning, idle, and the editor conversation each use one `LlmClient::chat_text_max` call to the in-container OpenAI-compatible URL. No cloud client. Editor replies are JSON `{say, edits[]}` with `replace` / `delete` / `insert_after` (1-based lines) |
| ADR-0004 command ledger | Idle and the `top_commands` verb read `/engram/user/.command_freq.json` the way `:top` does. No new MCP server. SINE does not call `mcp__cmdfreq__top_commands` itself |
| ADR-0007 crew | Left-bottom roster and the `shomagent` verb use `shomagent::daemon::rpc` on `/run/shomagent/ctl.sock`. Allowed requests: roster, ask (Talk), pause, resume, stop. SINE does not supervise processes and does not speak dream, refine, or web unless a later update to this ADR adds them |
| ADR-0013 scheduler | Left-bottom **shomloops:** shows enabled runs from `shomloop::list_runs()` whose next fire (local time via `shomloop::next_fire`) is within 24 hours, sorted soonest-first. Full ledger remains `shomloop list`. No crontab rewrite. enable/disable/delete/edit stay out until this ADR says otherwise |
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
- Modules: `src/split_frame.rs` (generic UI), `src/sine_tui.rs` (assistant),
  `src/sine_editor.rs` (file editor overlay). No new workspace crate.
