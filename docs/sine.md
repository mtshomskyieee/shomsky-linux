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
| Left bottom | shomagent roster, then shomloop list (about 1s refresh) |
| Right top | Plans, step results, idle notes; header shows live `ctx N tok · est P%` |
| Right bottom | Chat history; same context meter in the chat header |
| Status | Mode, `auto` on/off, phase, context meter |
| Command line | User input |
| Footer | Key hints; shows `^A auto:on|off` |

Context uses the same chars/4 estimate and `ENGRAM_CONTEXT_TOKENS` window as `:chat`.
It counts the plan system prompt, chat turns, pending plan, and playbook.

Keys: Esc exit · Enter send · **Ctrl-A** toggle auto · **Ctrl-L** clear
context · Tab focus column · Ctrl-Left/Right vertical split · Ctrl-Up/Down
pane split. Typed characters always go to the command line. You can also type
`auto` or `clear` and press Enter.

While planning or running, a spinner (`[-] planning…` / `running…`) appears
under the SINE art. The agents/loops list refreshes after each plan step and
about twice a minute otherwise.

## Flow

1. Type a request. SINE plans with one local model call (JSON).
2. The plan appears in the agent pane before anything runs.
3. `approve` / `a`, `edit` / `e`, or `cancel` / `q`.
   - **edit / e** loads the plan into the command line so you can change it
     and press Enter to re-plan.
4. **Ctrl-A** (or type `auto`) toggles autonomous mode. Footer shows
   `^A auto:on|off`. The plan is still shown, then runs without a second
   confirm. The model cannot flip `auto`.
5. **Ctrl-L** (or type `clear`) clears chat, agent log, and any pending plan.

Root mode skips the model and wraps the line as a shell step. Hallucination
mode shows the plan and does not execute or write the session.

## Session

Directory `/engram/user/.sine/` (override `ENGRAM_SINE_DIR`):

| File | Role |
|------|------|
| `session.json` | Chat, agent log, input history, split ratios, `auto`, pending plan |
| `playbook.md` | Idle notes |

Restored on the next open. Model calls and idle run only while the screen is
open.

## Idle

After about 60 seconds quiet (empty input, no job, no pending plan), SINE
reads the top commands from `/engram/user/.command_freq.json` (same path as
`:top`), asks the model for a short playbook, and publishes it. One pass per
quiet stretch. Not in hallucination mode.

## Stub

`ENGRAM_SINE_STUB=1` returns a canned plan and idle playbook (CI).

## Related

- [ADR-0014](adr/0014-sine-assistant.md)
- [sine-usecase.md](sine-usecase.md) — HN top-10 headlines once-immediate scenario
- [chat.md](chat.md) — simpler framed chat without tools
- [shomagent.md](shomagent.md), [shomloop.md](shomloop.md), [mcp.md](mcp.md)
