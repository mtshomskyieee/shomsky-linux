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
| Left top | Header `SINE`, art from [`sine_image.txt`](sine_image.txt) (packaged as `assets/sine_image.txt` for the image build) |
| Left bottom | agents / loops / files — roster, shomloop list, then files this session touched; Tab left, ↑↓ select, Enter opens a full-screen numbered/wrapped viewer (Esc closes) |
| Right top | Plans, step results, idle notes (line-wrapped); header shows live `ctx N tok · est P%` |
| Right bottom | Chat history (line-wrapped); same context meter in the chat header |
| Status | Mode, `auto` on/off, phase, context meter |
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
line (or **open** the selected file when focus is left and the draft is empty) ·
**Ctrl-A** toggle auto · **Ctrl-K** clear context (not Ctrl-L — hosts often
eat Ctrl-L as form-feed) · Tab focus column (left = file list) · ↑↓ select file
when left-focused (or walk input history) · Ctrl-Left/Right vertical split ·
Ctrl-Up/Down pane split. Bracketed paste keeps newlines in the composer (does
not auto-send). Typed characters always go to the command line. You can also
type `auto` or `clear` and press Enter / Ctrl-S.

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
- [sine-usecase.md](sine-usecase.md) — HN top-10 headlines once-immediate scenario
- [chat.md](chat.md) — simpler framed chat without tools
- [shomagent.md](shomagent.md), [shomloop.md](shomloop.md), [mcp.md](mcp.md)
