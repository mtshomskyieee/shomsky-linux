# shomagent

Background agents you can hold a conversation with. Long-running work should
not block your terminal, and should not spam it either.

`shomagentd` runs in the container. The `shomagent` CLI talks to it. Members
get numeric ids; **create a member before `ask` / `talk` / `write`.**

## Command reference

| Command | Purpose |
|---------|---------|
| `shomagent` / `roster` | List members |
| `watch "<cmd>" --tell-me "…"` | Supervise a process stream |
| `spawn …` | Alias of `watch` |
| `read <path>` | Attach document **read-only** |
| `readwrite <path>` | Attach document **read/write** |
| `write <id> "<text>"` | Append to a readwrite document |
| `write <id> --replace "…"` | Overwrite document |
| `ask <id> "<q>"` | One-shot Q&A; answer is `#N > …` |
| `talk <id>` | Interactive chat (`:q` to leave) |
| `dream [path]` | Generative dream journal (default `dream.md`; `quit` to leave) |
| `dream seed [path]` | Same, but pause for a user-written seed before the first beat |
| `refine idea [path]` | Reason, then rewrite an idea file from a standing task (default `idea.md`; `quit` to leave) |
| `pause` / `resume` / `stop <id>` | Lifecycle |

## Quick use

### Documents

```bash
shomagent readwrite notes.md
shomagent write 1 "paris is in france"
shomagent ask 1 "where is paris"
# #1 > …answer from the document…

shomagent read details.md          # read-only
shomagent ask 2 "summarize this"
# shomagent write 2 "…" → rejected
```

### Process watchers

```bash
shomagent watch "npm run dev" --tell-me "when a request 500s, with the stack trace root cause"
# ⟡ shomagent#1 attached (pid …)

shomagent spawn "sleep 3600" --name idle --tell-me "nothing"
shomagent                 # roster
shomagent ask 1 "is it only orders?"
shomagent talk 1
shomagent pause 1 && shomagent resume 1 && shomagent stop 1
```

Inside `engram-shell`, findings interrupt between prompts:

```text
⟡ shomagent#1 ─ 3 × 500 on POST /api/orders
  root cause: …
```

Meta: `:shomagent` or `:agents` lists the roster; `:shomagent talk N` peeks.
Inside the shell, `:menu` → **shomagent** offers list agents, `readwrite` /
`dream seed` / `refine idea` (type a filename), `ask` / `talk` (pick an opened
document), and `stop` / end (pick any agent — documents detach and keep the file).

## Typical use

A session attaches a member, then you keep working. Create the member before `ask`, `talk`, or `write`. Findings interrupt between prompts; `ask` and `talk` dig into that member.

### Watch a long-running command

```bash
shomagent watch "npm run dev" --tell-me "when a request 500s, with the stack trace root cause"
# ⟡ shomagent#1 attached (pid …)
```

The command runs in the background. stdout and stderr stay in a ring buffer. The summarizer checks new lines against `--tell-me`. The same class of finding is not reported again for an hour.

Return to the shell. A match shows up between prompts:

```text
⟡ shomagent#1 ─ 3 × 500 on POST /api/orders
  root cause: …
```

Ask one question, or open a chat (`:q` leaves it):

```bash
shomagent ask 1 "is it only orders?"
# #1 > …

shomagent talk 1
```

Inside the shell, `:shomagent` or `:agents` lists the roster. `:shomagent talk N` peeks at member N.

Pause freezes the process group with SIGSTOP; resume continues it with SIGCONT. Stop kills it. Resume of a stopped or exited member starts the command again.

Pause, resume, or stop when you are done:

```bash
shomagent pause 1 && shomagent resume 1 && shomagent stop 1
```

`shomagent watch … --persist` writes `./shomagent_crew.d/<name>.toml` so the watcher is declared for later. Drop-ins below cover restart policy and overrides.

### Ask a document

```bash
shomagent readwrite notes.md
shomagent write 1 "paris is in france"
shomagent ask 1 "where is paris"
# #1 > …answer from the document…

shomagent write 1 --replace "new contents"

shomagent read details.md          # read-only
shomagent ask 2 "summarize this"
# shomagent write 2 "…" → rejected
```

`read` is ask and talk only. `readwrite` also accepts `write` (append) and `write --replace`.

### Dream

A persistent foreground session. Seeds a random bash or Python topic, then
keeps appending ReAct beats to a markdown journal and the terminal. Invented
commands are shown and saved; they are not executed.

```bash
shomagent dream              # journal → dream.md
shomagent dream ideas.md     # journal → ideas.md
shomagent dream seed         # wait for your seed, then dream → dream.md
shomagent dream seed ideas.md
```

With `dream seed`, the session pauses first: type or paste the opening seed,
then a line containing only `.` (or Ctrl-D) to plant it and begin dreaming.
Type `quit` before planting to leave without writing.

On a color TTY each beat crystallizes through the same green character rain as
the splash (`ENGRAM_MATRIX`, `ENGRAM_MATRIX_SECS`, `ENGRAM_MATRIX_GLYPHS`).
Tall beats are clipped to the terminal height for the rain; overflow prints
underneath. Default craft is bash/python, but steering may leave that freely.
Each LLM turn stays small-model friendly: a short recent excerpt plus a hidden
one-line gist of the last beat (not the whole journal, and not shown as a
`CARRY:` line). Before the first beat, the session says so: past about 1,000
characters the next beat auto-compacts to that excerpt and dreaming continues.
The full journal stays in the file. While the model works, a spinner on the `thinking…` line
(`[/] … 12s`, same `ENGRAM_SPINNER` gate as the shell) shows the session is
alive. A visible `steer>` prompt accepts normal typed input (echoed) at any
time — including while thinking. Press Enter to send; type `quit` to leave.
The roster glyph for a live dream member is `~`.

### Refine idea

A persistent foreground session. You write a refinement task (end with `.`),
then each pass **reasons** in one sentence and **acts** by rewriting the idea
file. The reason is shown in the terminal and remembered, so the next pass
makes a different change. The file on disk stays the document. Type a new line
at `task>` to replace the standing task; type `quit` to leave. Default path
is `idea.md`.

```bash
shomagent refine idea              # → idea.md
shomagent refine idea notes.md
```

Unlike dream (which appends beats), refine replaces the whole file when the
pass still reads as the idea. Before that replacement, the current file is
copied to `name.v0`, then `name.v1`, and so on (`example.md` becomes
`example.md.v0` on the first rewrite). A file longer than 6,000 characters is
clipped for the model. The pass continues, the terminal names the `.vN` file
that holds the full text from before the clip, and the rewrite is saved as usual.
A substantial document (about 120 words or more)
ends the session instead of being saved when the rewrite is nearly empty,
drops most of the piece, or no longer reads as a document. An empty rewrite of
any existing file ends the same way. A pass that only repeats the current
text, including a punctuation-only edit, also ends the session. The previous
text stays on disk. A `**reason:**` line still counts when a preamble comes
before it; that preamble is not written into the file. Roster glyph for a live
refine member is `✦`.

## Drop-ins (`shomagent_crew.d`)

```toml
# /etc/shomsky/shomagent_crew.d/dev-watch.toml
# or ./shomagent_crew.d/dev-watch.toml in a repo
name = "dev-watch"
command = "npm"
args = ["run", "dev"]
tell_me = "when a request 500s, with the stack trace root cause"
restart = "on-failure"   # never | on-failure | always
autostart = false         # local files also need SHOMAGENT_LOCAL_AUTOSTART=1
budget_tokens = 512
ring_lines = 2000
```

System drop-ins load first; cwd `./shomagent_crew.d` overrides the same `name`.
A cwd drop-in with `autostart = true` does not run at daemon startup unless
`SHOMAGENT_LOCAL_AUTOSTART=1`. System autostart still runs. One failed autostart
is logged and the daemon keeps going. `restart = always` or `on-failure` waits
1s, then 2s, doubling up to 60s. A process that lived 30s resets that delay.
After 8 fast exits the member stays stopped until `resume`.
`shomagent watch … --persist` writes a local drop-in.

A watch that is running or paused is also saved under `$SHOMAGENT_RUN_DIR/members`.
The next daemon start brings running watches back and leaves paused ones paused
until `resume`. `stop` removes that file, so a stopped watch is not restored.
Dream, refine, and document sessions are not saved. A dream or refine member
with no step for 3 minutes is detached; the file on disk stays. A second
readwrite, dream, or refine attach to a path that already has a writer is rejected.
Two read-only attaches of the same path are allowed.

## How it works

1. **`shomagentd`** supervises member processes (restart policy, pids) and document attachments.
2. Process members keep a **ring buffer** of stdout/stderr.
3. A **summarizer** (local mistral.rs, or stub when `SHOMAGENT_STUB_LLM=1`) reads new
   lines against `tell_me` on a token budget.
4. A **novelty filter** fingerprints findings so the same class is not
   re-reported within an hour. Repeat counts (`3 ×`, `x4`) collapse; CVE ids,
   ports, and versions stay distinct.
5. Novel findings land in **`/run/shomagent/notify`** (JSONL) — one lane into
   the shell.
6. Document members (`read` / `readwrite`) use file contents as `ask`/`talk` context;
   `write <id>` appends (or `--replace`) only when the member is readwrite.

## Environment

| Variable | Default | Meaning |
|----------|---------|---------|
| `SHOMAGENT_RUN_DIR` | `/run/shomagent` | Socket, notify lane, member state. Directory is `0700`, socket is `0600`. |
| `SHOMAGENT_SYSTEM_CREW` | `/etc/shomsky/shomagent_crew.d` | System drop-ins |
| `SHOMAGENT_LOCAL_AUTOSTART` | unset | `1` allows `./shomagent_crew.d` autostart |
| `SHOMAGENT_LLM_URL` / `ENGRAM_LLM_URL` | `http://127.0.0.1:1234` | OpenAI-compatible API |
| `SHOMAGENT_STUB_LLM` | unset | `1` = heuristic / no real LLM (CI when `SKIP_LLM=1`) |
| `SKIP_SHOMAGENT` | `0` | Entrypoint skips starting the daemon |

## Tests

- Unit: `cargo test -p shomagent` (ring, novelty, notify, crew merge, stub LLM).
- Integration: `tests/cases/shomagent_smoke.sh` → `tests/helpers/shomagent_full.sh`
  covers roster, read, readwrite, write, ask, talk, watch, spawn, pause, resume,
  stop, persist, and novelty (stub LLM).

See [ADR-0007](adr/0007-shomagent-background-crew.md).
