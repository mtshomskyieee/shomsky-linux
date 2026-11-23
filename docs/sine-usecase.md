# SINE usecase: HN top 10 headlines (once, immediately)

How-to scenario for driving SINE. Normative surface: [ADR-0014](adr/0014-sine-assistant.md).
Screen reference: [sine.md](sine.md).

## Goal

1. Visit `https://news.ycombinator.com`
2. Take the **latest 10 headlines**
3. Write them to a CSV named `yc_headlines_<datetime>.csv`
4. Do this **once** and **immediately** (not on a schedule, not a loop)

Suggested path for the file: `/engram/user/yc_headlines_<datetime>.csv`
(agent home). Datetime form: `YYYYMMDDTHHMMSSZ` (UTC), same spirit as
`:chat` save stamps.

Example CSV shape:

```csv
rank,title,url
1,"Example title","https://example.com/item"
...
10,"Tenth title","https://..."
```

## Constraints (today)

| Constraint | Meaning for this usecase |
|------------|--------------------------|
| Plan-then-approve | SINE shows a plan; you `approve` / `a` (or leave `auto` on) before anything runs |
| Closed verbs | After approval SINE may run `shell`, `shomagent` (roster/ask/pause/resume/stop), `top_commands`, `say` only |
| Web via shell | ADR-0014 has no native `web` plan verb. Opening a page is a **`shell`** step: `shomagent web <url>` (headless Chromium). Then `shomagent ask <id> "…"` (shell or the `shomagent` ask verb) |
| Prefer shomagent | SINE’s planner is steered to use `shomagent web` for pages/JS; not `curl` HTML/RSS scrapers |
| Once, immediately | Do **not** use `shomloop` for this scenario. One plan, one run, done |
| Network | Container needs Chromium for `shomagent web` (image ships it). HN must be reachable |

## Ways to get SINE to do it

### 1. Natural-language request (preferred)

Open SINE, then type something explicit — or **paste** the block below into the
composer (multi-line paste stays in the draft; finish with **Ctrl-S**, or Enter
on a trailing blank line):

```text
Visit https://news.ycombinator.com with shomagent web (headless browser).
Ask that member for the top 10 story headlines with urls. Write
/engram/user/yc_headlines_<UTC datetime>.csv with columns rank,title,url.
Do this once immediately. Do not use curl. Do not schedule a loop.
Stop the web member when done.
```

Or open with the request already attached:

```text
:sine Use shomagent web on news.ycombinator.com; top 10 headlines to yc_headlines_<datetime>.csv once now
```

Then:

1. Read the plan in the **agent** pane.
2. Confirm it uses `shomagent web` (shell), then ask/stop — **not** `curl` and not `shomloop`.
3. `approve` / `a`, or **Ctrl-A** if you want `auto` for this session.
4. Check the agent pane for output and that the CSV exists.

If the plan is wrong (e.g. curl/RSS), `cancel` / `q` and restate “use shomagent web”.

Expected plan shape (illustrative):

```text
1. shell {"command":"shomagent web https://news.ycombinator.com"}
2. shell {"command":"shomagent ask 1 \"list top 10 headlines with urls as CSV rank,title,url\""}
3. shell {"command":"… write /engram/user/yc_headlines_….csv …"}
4. shell {"command":"shomagent stop 1"}
```

(Ids come from the roster after web attaches; SINE may combine steps.)

### 2. Steer harder toward shomagent web

If a small model still proposes curl:

```text
Reject curl/wget/RSS. Plan only with shomagent web https://news.ycombinator.com
then shomagent ask <id> for the top 10 titles and urls, write the CSV under
/engram/user/, then shomagent stop <id>. Once, immediately.
```

### 3. Operator-edited shell (when the model plan is weak)

1. Ask SINE as in (1) so the intent is in chat history.
2. If the plan uses curl: `cancel`, then “run exactly these shell commands: …”
   with `shomagent web` / `ask` / write / `stop`.
3. Or leave SINE (Esc), run those at the agent prompt, then return and note
   the CSV path in chat.

### 4. `auto` for a hands-off single run

1. **Ctrl-A** until the footer shows `^A auto:on` (or type `auto`).
2. Submit the natural-language request from (1) or (2).
3. SINE still **prints** the plan, then runs it without a second confirm.
4. **Ctrl-A** again to turn `auto` off when you are done.

Use `auto` only when you are willing for that plan to execute as shown.

### 5. Fallback: curl (only if Chromium/web is unavailable)

If `shomagent web` fails (no Chromium, stub, network policy), cancel and ask
for a BusyBox `curl` + parse fallback. That is secondary to the shomagent path.

## What success looks like

- Agent pane shows an approved plan and the shell output (or a clear error).
- Chat has your request and SINE’s short “done” / result note.
- File exists, e.g. `/engram/user/yc_headlines_20261123T011500Z.csv`, with a
  header and 10 data rows.
- No new `shomloop` row for this job.

## Failure modes

| Symptom | What to try |
|---------|-------------|
| Plan uses `shomloop` or “daily” | Cancel; restate “once, immediately, no schedule” |
| Plan uses `curl`/RSS instead of shomagent | Cancel; restate “use shomagent web, not curl” |
| Plan invents a bare `web` verb | Cancel; ask for `shell` with `shomagent web …` |
| Empty or wrong CSV | Approve a follow-up: show first lines of the file; tighten the ask |
| Chromium / web attach error | `apk add chromium` if missing; or fall back to curl only then |
| TLS / network error | Check container network; retry once; do not invent offline headlines |
| Hallucination mode | Plan appears but does not run or write; switch to agent mode |

## Related

- [sine.md](sine.md) — screen, keys, session
- [ADR-0014](adr/0014-sine-assistant.md) — allowed verbs
- [shomagent.md](shomagent.md) / [ADR-0011](adr/0011-shomagent-web.md) — browser members (outside SINE’s native verbs)
- [shomloop.md](shomloop.md) — schedules (not used for this once-immediate goal)
