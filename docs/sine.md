# SINE

Semi-persistent plan-then-approve assistant in `engram-shell`.

Normative dependency surface: [ADR-0014](adr/0014-sine-assistant.md).
This page is the how-to for the screen. It does not restate the closed verb list.

## Open

- `:sine` or bare `sine` (exact first token)
- Optional remainder is the first request: `:sine list top commands`
- Non-TTY prints `sine: interactive only` and returns
- Esc or Ctrl-C saves (agent/root) and returns to the prompt

## Layout

Generic chrome: `src/split_frame.rs`. SINE fills it as:

| Pane | Content |
|------|---------|
| Left top | Brand avatar (`:menu` → Theme → avatar), independent of chrome color: **navi eye**, **coffee mug**, **laptop**, **mana flame**, **leaf** (img2ascii), or **braille bunny** ([`sine_image.txt`](sine_image.txt)). Sources under `assets/brand/` + `assets/sine_image.txt`. Import / rest+busy: [import-sine-image.md](import-sine-image.md), [ADR-0015](adr/0015-sine-brand-avatars.md) |
| Left bottom | agents / loops / files — roster, enabled shomloops due within 24h (with relative next-fire), then files this session touched; Tab to files, ↑↓ select, Enter opens a full-screen numbered/wrapped viewer (Esc closes; `e` opens the editor), empty draft + `e` opens the editor for the selected file |
| Right top | Plans, step results, idle notes (line-wrapped); header shows live `ctx N tok · est P%`; Tab to agent, ↑↓ / PgUp/PgDn scroll |
| Right bottom | Chat history (line-wrapped); same context meter in the chat header; Tab to chat, ↑↓ / PgUp/PgDn scroll |
| Status | Mode, `auto` on/off, phase, context meter, `focus:input\|files\|agent\|chat` |
| Command line | Multi-line composer (grows 1–3 visible rows; scrolls if longer) |
| Footer | Key hints; shows `^A auto:on|off` |

Context uses the same chars/4 estimate and `ENGRAM_CONTEXT_TOKENS` window as `:chat`.
The meter counts **session** working memory only: chat, agent log, pending plan,
and in-memory playbook (not the fixed plan system prompt). After **Ctrl-K** /
`clear`, that is empty → **`ctx 0 tok`**.
The status line also shows the last LLM sample as `N.N tok/s` (output
chars/4 over wall time) so slow local models set expectations; `— tok/s`
until the first plan or idle call finishes.

Keys: Esc exit · **Ctrl-S** send · Enter send on a one-line draft, insert a
newline when the draft already has newlines, or send again on a trailing blank
line (or **open** the selected file when focus is **files** and the draft is empty) ·
**e** (files focus, empty draft) open the **editor** for the selected file ·
**Ctrl-A** toggle auto · **Ctrl-K** clear context (not Ctrl-L — hosts often
eat Ctrl-L as form-feed) · **Ctrl-R** clear the files list (drop prior-session
paths and reset the touch baseline; session/playbook/plan reappear if present) ·
**Tab** cycles focus (`input` → `files` → `agent` → `chat`); the focused
section uses the theme selection bar (`▸` on pane headers, or the composer when
focus is input) · ↑↓ walk input history on input, select a file on files, or
scroll agent/chat · PageUp/PageDown scroll agent/chat (or jump files) ·
Ctrl-Left/Right vertical split · Ctrl-Up/Down pane split (left pair when
files-focused, right pair otherwise).
Bracketed paste keeps newlines in the composer (does not auto-send). Typed
characters always go to the command line (except bare `e` / `E` on files with
an empty draft). You can also type `auto`, `clear`,
or `files clear` / `refresh files` and press Enter / Ctrl-S.

## Editor

Full-screen overlay (left = numbered editable file, right = AI conversation).
Open with **`:edit [path]`** from the shell (no path → cwd picker with **New file**),
**`:menu` → Editor** (recent paths + clear history), or **e** from the SINE files
list / viewer. Missing paths are created empty. Opens record under
`/engram/user/.edit_history.json` (skipped in hallucination). Esc autosaves and
returns. The chat header shows the same style of context meter as `:chat` / SINE
(`ctx N tok · est P%`): chars/4 over the editor system prompt, chat transcript,
pending suggestion, and the file slice attached on the next model call, against
`ENGRAM_CONTEXT_TOKENS`. **Ctrl-K** / compactify drop that meter the same way
they shrink the transcript.

| Control | Action |
|---------|--------|
| Tab | Focus file ↔ chat composer |
| Typing / arrows / PgUp/PgDn | Edit or move in the focused side |
| Ctrl-S / Enter (composer has text) | Send to the model |
| Ctrl-A | **Compactify** the editor chat into one summary turn (also: type `compact`) |
| type `auto` | Toggle **editor** auto (independent of plan `auto`; letter `c` is always typed) |
| Ctrl-Z / Ctrl-Y | Undo / redo through the version ring (also: type `undo` / `redo`) |
| Enter (empty composer, pending suggestion) | Accept proposed edits |
| Ctrl-K | Dismiss pending suggestion, otherwise **clear** the editor chat context (file kept) |
| type `clear` | Same as Ctrl-K clear |

**Non-auto (default):** the model runs only after you send. Line edits in its
JSON reply apply immediately and autosave.

**Auto:** edits stay as proposals until you accept them. After about 20 seconds
of quiet, one unsolicited suggestion may appear.

Autosave writes the real path about 800ms after the last edit (and on applied
AI edits / Esc). Versions live under `/engram/user/.sine/edits/<encoded-path>/`
(`ring.json` keeps the last 16 distinct snapshots; `chat.json` is the editor
transcript). Hallucination mode does not write the path or meta dir.

**shomloops:** lists enabled loops whose next fire is within 24 hours (local
time), soonest first, with a coarse relative label (`in 2h`). Use
`shomloop list` for the full ledger.

Plan verb **`quickyn`** asks a yes/no with confidence (ADR-0009) when the
step is a boolean gate.

While planning or running, a spinner (`[-] planning…` / `running…`) appears
under the SINE art and the bunny's `o` eyes glance around the face. After a
plan or step error the eyes settle on `x` until the next request. The
agents/loops list refreshes during long shell steps and about twice a minute
otherwise. One-shot webpage plans use `web` → `shomagent list`/`roster` →
`ask`/`stop` with id `0` (SINE binds stale/zero ids to the live roster). When
the ask includes a schedule (daily / loop / “at 4am”), the plan should write a
job script under `/engram/user/`, run it once if testing now, then
`shomloop "<frequency>" /engram/user/<job>.sh` — not an interactive-only plan.
If a shell step fails (wrong member id, or `shomagent ask` overwhelms the local
model on a large page), SINE **halts** the plan, skips remaining steps, and
prints recourse in the agent pane.

## Flow

1. Type a request. SINE plans with one local model call (JSON).
2. The plan appears in the agent pane before anything runs.
3. `approve` / `a`, `edit` / `e`, or `cancel` / `q`.
   - **edit / e** loads the plan into the command line so you can change it
     and press Enter or **Ctrl-S** to re-plan.
4. **Ctrl-A** (or type `auto`) toggles autonomous mode. Footer shows
   `^A auto:on|off`. The plan is still shown, then runs without a second
   confirm. The model cannot flip `auto`.
5. **Ctrl-K** (or type `clear`) clears session working memory (chat, agent log,
   pending plan, in-memory playbook) so agent/chat headers show **`ctx 0 tok`**.
   The command line draft and ↑↓ history are kept (not in context until Enter).
   **`plan.md` / `playbook.md` on disk are kept** for recovery.

Root mode skips the model and wraps the line as a shell step. Hallucination
mode shows the plan and does not execute or write the session.

## Session

Directory `/engram/user/.sine/` (override `ENGRAM_SINE_DIR`):

| File | Role |
|------|------|
| `session.json` | Chat, agent log, input history, split ratios, `auto`, pending plan |
| `playbook.md` | Idle notes |
| `plan.md` | Last plan (markdown + embedded JSON). Written on present; status updates on approve/cancel/done. Survives Ctrl-K |
| `edits/<encoded-path>/` | Per-file editor version ring (`ring.json`) and conversation (`chat.json`) |

Restored on the next open. Model calls and idle run only while the screen is
open.

## Idle

After about 60 seconds of true quiet (empty input, phase idle, no in-flight
job), SINE reads the top commands from `/engram/user/.command_freq.json`
(same path as `:top`), asks the model for a short playbook, and publishes it.
The quiet timer restarts when a request is fielded and again when planning or
running finishes, so slow local models do not trip idle mid-request or the
instant a reply returns. One pass per quiet stretch. Not in hallucination mode.

## Stub

`ENGRAM_SINE_STUB=1` returns a canned plan and idle playbook (CI).

## Related

- [ADR-0014](adr/0014-sine-assistant.md)
- [ADR-0015](adr/0015-sine-brand-avatars.md) / [import-sine-image.md](import-sine-image.md) — brand avatars
- [sine-usecase.md](sine-usecase.md) — HN top-10 headlines once-immediate scenario
- [chat.md](chat.md) — simpler framed chat without tools
- [shomagent.md](shomagent.md), [shomloop.md](shomloop.md), [mcp.md](mcp.md)
