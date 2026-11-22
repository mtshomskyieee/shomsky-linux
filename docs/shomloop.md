# shomloop

Schedule a shell command, keep the plan in `~/.loop/planned_runs.csv`, and
install a crontab line that runs it. The CSV is the source of truth. Container
start runs `shomloop restore`, which rewrites the marked crontab block and
starts `crond` if it is not already running. When at least one loop is put
back, startup prints the count and points at `shomloop list`.

See [ADR-0013](adr/0013-shomloop.md).

The default container process is root, so `~` is `/root` for both the shell
and the entrypoint. There is no second ledger under `/engram/user`.

## Usage

Quote the frequency when it contains spaces. The rest of the line is the command.

```bash
shomloop "daily at 5pm" echo hello
shomloop hourly apk update
shomloop list
shomloop logs
shomloop logs 1
shomloop disable 1
shomloop enable 1
shomloop edit 1 "weekly on monday at 5pm" echo hello
shomloop delete 1
```

`:menu` → **shomloop** lists planned runs. **new loop** prefills `shomloop `.
**logs** runs `shomloop logs`. Enter on a run, then `[e]` edit, `[d]`isable,
`[n]` enable, `[x]` delete, or `[l]`ogs.

## Frequencies

| Phrase | Cron |
|--------|------|
| `hourly` | `0 * * * *` |
| `daily` | `0 0 * * *` |
| `daily at 5pm`, `daily at 5:30pm`, `daily at 17:00` | minute and hour that day |
| `weekly` | Sunday 00:00 (`0 0 * * 0`) |
| `weekly on monday`, `weekly on monday at 5pm` | that weekday |
| `monthly`, `monthly at 5pm` | day 1 of the month |
| `every 15 minutes`, `every 2 hours` | step expression |
| five fields of digits, `*`, `/`, `-`, `,` | passed through |

Anything else is an error. The phrase list is printed on stderr. The local
model is not asked to invent a cron expression.

## Ledger and logs

`~/.loop/planned_runs.csv` columns: `id,enabled,frequency,cron,command,created_at`.

Each enabled row becomes a crontab line inside `# BEGIN shomloop` /
`# END shomloop`. Other crontab lines stay. Disabled rows stay in the CSV and
are left out of crontab. Cron calls `shomloop run <id>`, which appends a
stamped block (start, command, stdout, stderr, exit) to `/tmp/shomloop/<id>.log`.

`shomloop logs` prints every job log. `/tmp` is empty on a fresh container.
