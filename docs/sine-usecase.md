# SINE usecase: HN top 10 headlines (now + daily 4am)

How-to scenario for driving SINE. Normative surface: [ADR-0014](adr/0014-sine-assistant.md).
Screen reference: [sine.md](sine.md). Scheduler: [shomloop.md](shomloop.md).

## Goal

1. Visit `https://news.ycombinator.com`
2. Take the **latest 10 headlines**
3. Write them to a CSV named `yc_headlines_<datetime>.csv`
4. Do this **once daily at 4am on a loop** and **also right now** so we can test

Suggested path for the CSV: `/engram/user/yc_headlines_<datetime>.csv`
(agent home). Datetime form: `YYYYMMDDTHHMMSSZ` (UTC), same spirit as
`:chat` save stamps. Each run (test and cron) should mint a **new** stamped
filename so days do not overwrite each other.

Job script (durable, for cron): `/engram/user/yc_headlines_job.sh` — written by
the plan, then pointed at by `shomloop`. Prefer this over a giant inline
`sh -c` on the crontab line. Keep it under `/engram/user/` (not `/tmp`) so the
daily run still finds the file after restarts.

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
| Closed verbs | After approval SINE may run `shell`, `shomagent` (roster/ask/pause/resume/stop), `top_commands`, `say`, `quickyn` only |
| Prefer shomagent | SINE’s planner is steered to use `shomagent web` for pages/JS; not `curl` HTML/RSS scrapers |
| Schedule = script + shomloop | A “daily / loop / at 4am” ask must **not** stop at an interactive one-shot plan. Prefer: write job script → chmod → run once (if testing now) → `shomloop "daily at 4am" /engram/user/<job>.sh` |
| No SINE binding in cron | The job script parses `shomagent#id` (or `shomagent list`) itself. Id `0` rewrite is only for interactive in-plan steps |
| Network | Container needs Chromium for `shomagent web`. HN must be reachable. `crond` must be up (`shomloop restore` at entrypoint) |

## Transfer: SINE ask → job script → shomloop

When the operator asks for a schedule (e.g. “daily at 4am”) **and** “also right
now”, the plan should look like this — **not** five interactive web/ask steps
with no `shomloop`:

1. **Write** `/engram/user/yc_headlines_job.sh` — serial `shomagent web` → parse
   live id → `ask` → write stamped CSV → `stop`.
2. **`chmod +x`** that script.
3. **Run the script once** (the “right now” test).
4. **Register** `shomloop "daily at 4am" /engram/user/yc_headlines_job.sh`.

Illustrative job script body:

```bash
#!/bin/sh
set -e
out=$(shomagent web https://news.ycombinator.com)
id=$(printf '%s\n' "$out" | sed -n 's/.*shomagent#\([0-9][0-9]*\).*/\1/p' | tail -n 1)
test -n "$id"
ts=$(date -u +%Y%m%dT%H%M%SZ)
shomagent ask "$id" "list top 10 story titles and urls as CSV with header rank,title,url" \
  > /engram/user/yc_headlines_$ts.csv
shomagent stop "$id"
```

Then:

```bash
chmod +x /engram/user/yc_headlines_job.sh
/engram/user/yc_headlines_job.sh
shomloop "daily at 4am" /engram/user/yc_headlines_job.sh
```

Frequency phrase `daily at 4am` → cron `0 4 * * *`. Confirm with `shomloop list`
(or SINE left **shomloops:**). Logs: `shomloop logs <id>`.

A plan that only does interactive `web` → `roster` → `ask 0` → `stop` is
**wrong for this usecase** — it never schedules the daily run.

## Ways to get SINE to do it

### 1. Natural-language request (preferred)

Open SINE, then type something explicit — or **paste** the block below into the
composer (multi-line paste stays in the draft; finish with **Ctrl-S**, or Enter
on a trailing blank line):

```text
Visit https://news.ycombinator.com with shomagent web (headless browser).
Ask that member for the top 10 story headlines with urls. Write
/engram/user/yc_headlines_<UTC datetime>.csv with columns rank,title,url.
Do this once daily at 4am on a loop and also right now so we can test.
Write a job script under /engram/user/ (web; parse live shomagent#id; ask;
write stamped CSV; stop). chmod +x, run that script once now, then
shomloop "daily at 4am" pointing at that script. Do not use curl. Do not
emit a one-shot-only plan without shomloop.
```

Or open with the request already attached:

```text
:sine HN top 10 to yc_headlines_<datetime>.csv — write job script, run once now, shomloop daily at 4am
```

Then:

1. Read the plan in the **agent** pane.
2. Confirm it **writes a job script**, runs it once, and has
   `shomloop "daily at 4am" /engram/user/…` — **not** curl, and **not**
   interactive-only web/ask with no schedule.
3. `approve` / `a`, or **Ctrl-A** if you want `auto` for this session.
4. Check the agent pane for the test CSV, the script on disk, and left
   **shomloops:** / `shomloop list`.

If the plan is wrong (one-shot only, curl/RSS, or schedule with no script),
`cancel` / `q` and restate: “write job script, run once now, shomloop daily at 4am”.

Expected plan shape (illustrative):

```text
1. shell {"command":"cat > /engram/user/yc_headlines_job.sh <<'EOF' … EOF"}
2. shell {"command":"chmod +x /engram/user/yc_headlines_job.sh"}
3. shell {"command":"/engram/user/yc_headlines_job.sh"}
4. shell {"command":"shomloop \"daily at 4am\" /engram/user/yc_headlines_job.sh"}
```

(The script itself owns web → parse id → ask → CSV → stop. Do not hardcode
member numbers in the script.)

### 2. Steer harder toward script + shomloop

If a small model still proposes a one-shot interactive plan or curl:

```text
Reject curl/wget/RSS. Reject one-shot-only web→ask plans.
Plan must: (1) cat a /engram/user/yc_headlines_job.sh that does shomagent web,
parses shomagent#id, asks for top 10 CSV, writes stamped file, stops;
(2) chmod +x; (3) run the script once now; (4) shomloop "daily at 4am"
/engram/user/yc_headlines_job.sh.
```

### 3. Operator-edited shell (when the model plan is weak)

1. Ask SINE as in (1) so the intent is in chat history.
2. If the plan omits the script or shomloop: `cancel`, then paste the four
   shell steps from the expected shape (or leave SINE and run them at the
   agent prompt).
3. Return and note the CSV path / loop id in chat.

### 4. `auto` for a hands-off dual run

1. **Ctrl-A** until the footer shows `^A auto:on` (or type `auto`).
2. Submit the natural-language request from (1) or (2).
3. SINE still **prints** the plan, then runs it without a second confirm
   (write script, test run, **and** loop register).
4. **Ctrl-A** again to turn `auto` off when you are done.

Use `auto` only when you are willing for that plan (including the new shomloop
row and job script) to execute as shown.

### 5. Fallback: curl (only if Chromium/web is unavailable)

If `shomagent web` fails (no Chromium, stub, network policy), cancel and ask
for a BusyBox `curl` + parse job script and matching `shomloop`. That is
secondary to the shomagent path.

## What success looks like

- Agent pane shows an approved plan that includes **script write + shomloop**,
  not only interactive ask steps.
- `/engram/user/yc_headlines_job.sh` exists and is executable.
- Test CSV exists, e.g. `/engram/user/yc_headlines_20261123T011500Z.csv`, with a
  header and 10 data rows.
- A new enabled `shomloop` row for `daily at 4am` points at that script
  (`shomloop list` / left **shomloops:**).
- After 4am (or `shomloop run <id>`), another stamped CSV appears;
  `shomloop logs <id>` shows the run.

## Failure modes

| Symptom | What to try |
|---------|-------------|
| Plan is interactive web→ask only; no script/shomloop | Cancel; restate “write job script, run once, shomloop daily at 4am” |
| Plan schedules inline `sh -c` with no file | Prefer rewrite to `/engram/user/…_job.sh` then `shomloop … that path` |
| Plan schedules but never runs once now | Cancel; restate “run the job script once immediately **and** schedule” |
| Script hardcodes `ask 1` / `ask 5` | Cancel; require parsing `shomagent#id` inside the script |
| Plan uses `curl`/RSS instead of shomagent | Cancel; restate “use shomagent web, not curl” |
| Plan invents a bare `web` or `schedule` verb | Cancel; ask for `shell` with script write / `shomloop "daily at 4am" …` |
| `unknown member #N` inside the script | Fix id parse (`shomagent#N` from web output); re-run script |
| Empty or wrong CSV | Tighten the ask text in the script; re-run once |
| Chromium / web attach error | `apk add chromium` if missing; or fall back to curl only then |
| Loop registered but never fires | `shomloop list`; check script path still exists; `crond`; `shomloop restore`; logs |
| TLS / network error | Check container network; retry once; do not invent offline headlines |
| Hallucination mode | Plan appears but does not run or write; switch to agent mode |

## Related

- [sine.md](sine.md) — screen, keys, session
- [ADR-0014](adr/0014-sine-assistant.md) — allowed verbs
- [shomagent.md](shomagent.md) / [ADR-0011](adr/0011-shomagent-web.md) — browser members
- [shomloop.md](shomloop.md) / [ADR-0013](adr/0013-shomloop.md) — schedules (`daily at 4am`, ledger, crontab)
