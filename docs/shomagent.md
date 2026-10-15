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
```

On a color TTY each beat crystallizes through the same green character rain as
the splash (`ENGRAM_MATRIX`, `ENGRAM_MATRIX_SECS`, `ENGRAM_MATRIX_GLYPHS`).
Type a line to steer the next beat; type `quit` to leave. The roster glyph for
a live dream member is `~`.

## Drop-ins (`shomagent_crew.d`)

```toml
# /etc/shomsky/shomagent_crew.d/dev-watch.toml
# or ./shomagent_crew.d/dev-watch.toml in a repo
name = "dev-watch"
command = "npm"
args = ["run", "dev"]
tell_me = "when a request 500s, with the stack trace root cause"
restart = "on-failure"   # never | on-failure | always
autostart = false
budget_tokens = 512
ring_lines = 2000
```

System drop-ins load first; cwd `./shomagent_crew.d` overrides the same `name`.
`shomagent watch … --persist` writes a local drop-in.

## How it works

1. **`shomagentd`** supervises member processes (restart policy, pids) and document attachments.
2. Process members keep a **ring buffer** of stdout/stderr.
3. A **summarizer** (local mistral.rs, or stub when `SHOMAGENT_STUB_LLM=1`) reads new
   lines against `tell_me` on a token budget.
4. A **novelty filter** fingerprints findings so the same class is not
   re-reported within an hour.
5. Novel findings land in **`/run/shomagent/notify`** (JSONL) — one lane into
   the shell.
6. Document members (`read` / `readwrite`) use file contents as `ask`/`talk` context;
   `write <id>` appends (or `--replace`) only when the member is readwrite.

## Environment

| Variable | Default | Meaning |
|----------|---------|---------|
| `SHOMAGENT_RUN_DIR` | `/run/shomagent` | Socket, notify lane, member state |
| `SHOMAGENT_SYSTEM_CREW` | `/etc/shomsky/shomagent_crew.d` | System drop-ins |
| `SHOMAGENT_LLM_URL` / `ENGRAM_LLM_URL` | `http://127.0.0.1:1234` | OpenAI-compatible API |
| `SHOMAGENT_STUB_LLM` | unset | `1` = heuristic / no real LLM (CI when `SKIP_LLM=1`) |
| `SKIP_SHOMAGENT` | `0` | Entrypoint skips starting the daemon |

## Tests

- Unit: `cargo test -p shomagent` (ring, novelty, notify, crew merge, stub LLM).
- Integration: `tests/cases/shomagent_smoke.sh` → `tests/helpers/shomagent_full.sh`
  covers roster, read, readwrite, write, ask, talk, watch, spawn, pause, resume,
  stop, persist, and novelty (stub LLM).

See [ADR-0007](adr/0007-shomagent-background-crew.md).
