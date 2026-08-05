# Shomsky Linux

Shomsky Linux is an Alpine-derived Linux distribution distributed as a Docker
container. Its linux shell is an LLM agent that can use real tools against the
filesystem, or run in safer modes that only simulate writes.

## Quick start

Pull the published image from GitHub Container Registry:

```bash
docker pull ghcr.io/mtshomskyieee/shomsky-linux:latest
# or pin a release:  ghcr.io/mtshomskyieee/shomsky-linux:0.1.0
```

Run it (use `tiny` if you want a small model for a first smoke test):

```bash
mkdir -p engram/user .hallucination_shell_history
docker run -it --rm \
  -v shomsky-models:/engram/models \
  -v "$PWD/engram/user:/engram/user" \
  -v "$PWD/.hallucination_shell_history:/engram/.hallucination_shell_history" \
  -w /engram/user \
  -e SHOMSKY_MODEL=tiny \
  ghcr.io/mtshomskyieee/shomsky-linux:latest
```

First run downloads a GGUF into the `shomsky-models` volume, starts
[mistral.rs](https://github.com/EricLBuehler/mistral.rs), then drops you into
`engram-shell`. Exit with `:exit` or Ctrl+D.

Package page: https://github.com/mtshomskyieee/shomsky-linux/pkgs/container/shomsky-linux

### Your home directory under /engram is /engram/user
Agent-mode files start in `/engram/user`, bind-mounted to `./engram/user`
on the host (override with `SHOMSKY_USER_DIR`). Hallucination session JSON is
bind-mounted to `./.hallucination_shell_history` (override with
`SHOMSKY_HALLUC_HISTORY`).

### From this repo (Compose)

```bash
# optional: cp .env.example .env   # then set SHOMSKY_MODEL=tiny for low RAM
./start-shomsky-linux-shell.sh     # pick agent or hallucination mode
# or: ./agent_sh.sh / ./halluc_sh.sh
# or: ./scripts/shell.sh
# or: SHOMSKY_MODEL=tiny ENGRAM_TOOL_STYLE=text docker compose run --rm shomsky
```

Use **`run`**, not bare `up`. Compose’s attached helper menu (`w Enable Watch`)
steals the keyboard, so you never get an `engram-shell` prompt. `run --rm`
gives a real TTY. `./start-shomsky-linux-shell.sh` wraps that and prompts for
agent or hallucination mode.

If you prefer `up`:

```bash
COMPOSE_MENU=false SHOMSKY_MODEL=tiny docker compose up --menu=false
```

## Modes

| Mode | Behavior |
|------|----------|
| `agent` (default) | LLM with real tools; shell-shaped lines hit BusyBox immediately; `cd` persists |
| `hallucination` | Guided virtual shell (builtins in-memory) + LLM fallback; no disk writes |
| `root` | Bypass the LLM; lines go straight to BusyBox ash with persistent cwd (config-gated) |

Switch at the prompt with `:mode agent|hallucination|root`.

## shomagent

Background agents you can hold a conversation with. Long-running watches stay
off your TTY; they only interrupt when a novelty filter decides the finding is
new. Documents can be attached read-only or read/write and queried with `ask`.
Full reference: [docs/shomagent.md](docs/shomagent.md).

`shomagentd` starts with the container. Members are listed by id (`#1`, `#2`, …).
**You must create a member before `ask` / `talk` / `write`.**

### Command reference

| Command | Purpose |
|---------|---------|
| `shomagent` / `shomagent roster` | List members (`▶` running, `⏸` paused, `◇` read, `◆` readwrite) |
| `shomagent watch "<cmd>" --tell-me "…"` | Run a command; watch its stream for the brief |
| `shomagent spawn "<cmd>" …` | Same as `watch` |
| `shomagent read <path>` | Attach a **read-only** document member |
| `shomagent readwrite <path>` | Attach a **read/write** document member |
| `shomagent write <id> "<text>"` | Append text to a **readwrite** document |
| `shomagent write <id> --replace "<text>"` | Replace the whole file |
| `shomagent ask <id> "<question>"` | One-shot question (answer prints as `#N > …`) |
| `shomagent talk <id>` | Interactive chat (`:q` to leave) |
| `shomagent pause \| resume \| stop <id>` | Control a process member; `stop` on a doc detaches (file kept) |

Flags on `watch` / `spawn` / `read` / `readwrite`: `--name`, `--tell-me`.
`watch` / `spawn` also support `--persist` (writes `./shomagent_crew.d/<name>.toml`).

In `engram-shell`: `:shomagent` / `:agents` for roster peek.

### Example: ask a document

```bash
# 1) Attach a writable notes file (creates it if missing)
shomagent readwrite notes.md
# ⟡ shomagent#1 readwrite (…/notes.md)

# 2) Save facts into the file
shomagent write 1 "paris is in france"
shomagent write 1 "capital: paris"

# 3) Ask — the answer is the next line starting with #1 >
shomagent ask 1 "where is paris"
# #1 > …answer from the document…

# Read-only variant (cannot write):
shomagent read details.md
shomagent ask 2 "summarize the key points"
# shomagent write 2 "…"  → error (read-only)
```

### Example: watch a process

```bash
shomagent watch "npm run dev" \
  --tell-me "when a request 500s, with the stack trace root cause"
# ⟡ shomagent#1 attached (pid …)

shomagent                 # roster
shomagent ask 1 "is it only orders?"
shomagent talk 1          # interactive; :q to leave

shomagent pause 1
shomagent resume 1
shomagent stop 1
```

Novel findings interrupt the shell between prompts (no log spam):

```text
⟡ shomagent#1 ─ 3 × 500 on POST /api/orders
  root cause: …
```

### Repo drop-ins

Ship watchers under `./shomagent_crew.d/*.toml` (overrides
`/etc/shomsky/shomagent_crew.d/` for the same `name`):

```toml
# ./shomagent_crew.d/dev-watch.toml
name = "dev-watch"
command = "npm"
args = ["run", "dev"]
tell_me = "when a request 500s, with the stack trace root cause"
restart = "on-failure"
autostart = false
```

Or: `shomagent watch "…" --name test-loop --tell-me "…" --persist`

## Models

Set `SHOMSKY_MODEL` to one of the aliases in `/etc/engram/models.toml`:

- `mistral` (default) — Mistral 7B Instruct v0.3 GGUF Q4_K_M
- `qwen` — Qwen2.5 7B Instruct GGUF Q4_K_M
- `deepseek` — DeepSeek distill GGUF
- `tiny` — Qwen2.5 0.5B Instruct GGUF (CI / smoke tests)

## Learn

Prefix a line with `,` or `??` to treat it as **intent**. The shell shows a
real shell plan, you confirm, it runs, then you can **save** it as a command
under `~/bin` (with man page + BATS). Promote only installs if the test
passes. Package for others with `shomsky publish <name>` (apk →
`/usr/local/bin`). Full reference: [docs/learn.md](docs/learn.md).

```text
$ , show the five largest files here as a table
  ⟡ plan:
      find . -type f -printf '%s\t%p\n' | sort -rn | head -5 | column -t
  [r]un / [e]dit / [q]uit?
```

## Skills

Markdown skills live under `/engram/skills`. Skills with `always_load: true`
are inlined into the system prompt; others are loaded on demand via the
`load_skill` tool. See [docs/engram.md](docs/engram.md).

## Test

```bash
docker compose run --rm tests          # shell + shomagent suites (no LLM)
docker compose run --rm tests-llm      # agent + hallucination LLM checks (tiny)
# or on the host against a built image:
./tests/run.sh
RUN_LLM_TESTS=1 ./tests/run.sh         # when mistralrs is already up
# crate-level unit tests:
#   cargo test -p shomagent
```

CPU-only: first start (model load + first tokens) can take several minutes.
See [tests/README.md](tests/README.md) and [docs/models.md](docs/models.md).

## Run

```bash
docker compose up
# first time / after code changes:
docker compose up --build
```

See [docs/build-and-publish.md](docs/build-and-publish.md) and
[docs/adr/0001-alpine-llm-linux-shell.md](docs/adr/0001-alpine-llm-linux-shell.md)
(Architecture Decision Records: [docs/adr/](docs/adr/)).

## License

Shomsky Linux original code is MIT, provided AS IS — use at your own risk
([ADR-0006](docs/adr/0006-mit-as-is-and-ai-contributions.md)). The container
is built from the [official Alpine Linux Docker image](https://github.com/alpinelinux/docker-alpine)
(MIT; Copyright (c) 2019 Natanael Copa — see
[third_party/docker-alpine/](third_party/docker-alpine/)) and aggregates Alpine
packages under their upstream licenses (including GPL-2.0 BusyBox). See
[LICENSE](LICENSE), [NOTICE](NOTICE), and [docs/licensing.md](docs/licensing.md).

Shomsky Linux is based on [Alpine Linux](https://alpinelinux.org/) and is not
affiliated with or endorsed by the Alpine Linux project.

AI-assisted contributors should load [docs/adr/](docs/adr/) and adhere to
Accepted ADRs before changing code (see ADR-0006).
