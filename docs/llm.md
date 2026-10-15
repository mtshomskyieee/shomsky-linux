# LLM engagement

Load this file before generating or editing Shomsky Linux code, and before answering as the agent inside `engram-shell`. Plain-text twin: [llm.txt](llm.txt). It is a map of the documentation. It is not a new decision.

When this file and an **Accepted** ADR disagree, the ADR wins. How-to pages (`shell-modes.md`, `mcp.md`, `models.md`, and the rest) explain operations. ADRs record why a constraint exists. The essay in [writeup.md](writeup.md) is for human readers.

Full policy: [ADR-0006](adr/0006-mit-as-is-and-ai-contributions.md). What an ADR is: [ADR-0000](adr/0000-what-is-an-adr.md). Index: [adr/README.md](adr/README.md).

## Posture

- Original source is **MIT**, provided **AS IS**. Operators run the container at their own risk. Do not invent a second license or warranty for that code.
- Agent mode can run real commands and write files **inside** the container and its bind mounts. Hallucination mode and operator hardening (non-root user, read-only rootfs, `allow_root_mode = false`) reduce exposure. They do not move responsibility off the operator.
- AI-assisted contributions are expected. Before editing code, load [adr/README.md](adr/README.md) and every Accepted ADR that touches the change. Follow those decisions.
- A pull request that knowingly violates an Accepted ADR must include a superseding ADR (or a Status change) in the **same** change set.
- The human submitter remains responsible for correctness, security impact, and license compliance.
- Brand as “Shomsky Linux, based on Alpine Linux.” The project is not affiliated with the Alpine Linux project. Do not treat Alpine marks as ours. Inventory: [licensing.md](licensing.md).

## Product

Shomsky Linux is an Alpine-derived container distribution. Its linux shell is the Rust binary `engram-shell`. The container is the sandbox. Inference is local [mistral.rs](https://github.com/EricLBuehler/mistral.rs) on `127.0.0.1:1234`, speaking OpenAI-compatible HTTP. Weights are downloaded once into a volume. They are not in the image or the git tree. The shell executes tools. The model proposes them and cannot escalate its own mode.

Architecture: [ADR-0001](adr/0001-alpine-llm-linux-shell.md).

```text
entrypoint.sh
  ├─ resolve SHOMSKY_MODEL → /etc/engram/models.toml
  ├─ download GGUF → /engram/models
  ├─ mistralrs serve --format gguf --cpu 127.0.0.1:1234
  ├─ shomagentd
  └─ exec engram-shell
       ├─ BusyBox ash          real tools (agent / root)
       ├─ /engram/skills       markdown manuals
       └─ stdio MCP children   engram-cmdfreq, …  (no ports)
```

## Constraints that need a superseding ADR to change

State these as facts when you design a change. Do not route around them.

| Constraint | Source |
|------------|--------|
| Flagship image is Alpine/musl. CUDA has no musl path; `containers/Dockerfile.cuda` is a separate glibc companion, not the flagship. | ADR-0001 |
| Default inference is in-container mistral.rs. No first-party cloud Anthropic, OpenAI, or Ollama client as the product path. An alternate OpenAI-compatible URL is an operator extension. | ADR-0003 |
| GGUF weights stay out of the image and the git tree. | ADR-0003, [licensing.md](licensing.md) |
| Real execution is BusyBox ash. Mode policy stays in the shell. Unknown tool names are ignored, not executed. | ADR-0001 |
| Extra tools are local stdio MCP children. Config has `command` / `args` / `env` and no `url`. A command containing `://` is refused. No TCP/UDP listen. Compose does not publish MCP ports. | ADR-0004 |
| Hallucination mode: real tools and MCP calls are quiet no-ops. No disk writes. | ADR-0001, ADR-0004 |
| Learn installs under `$HOME/bin` only after BATS passes. `/usr/local/bin` is `shomsky publish` only. | ADR-0008 |
| shomagent members exist only after an explicit attach. `read` cannot `write`. Findings share `/run/shomagent/notify`. Supervision is in-process, not s6/runit. | ADR-0007 |
| First-party CI is GitHub Actions. Default `ci` builds the image and runs the non-LLM suite. LLM smoke is opt-in (`RUN_LLM_SMOKE`, `tests-llm`, `RUN_LLM_TESTS=1`) with `SHOMSKY_MODEL=tiny`. Do not publish images from every PR. Releases are tag-driven to `ghcr.io/mtshomskyieee/shomsky-linux`. | ADR-0005 |
| Rust edition 2021, `rust-version = "1.74"`. Release profile optimizes for size (LTO, `opt-level = "z"`, strip). Prefer MIT / Apache-2.0 dependencies. | ADR-0002 |

## Repository map

Workspace ([ADR-0002](adr/0002-rust-language-and-style.md)):

| Crate | Path | Role |
|-------|------|------|
| `engram-shell` | `src/` | REPL, modes, tools, MCP client, learn loop |
| `engram-cmdfreq` | `crates/engram-cmdfreq/` | stdio MCP: `record_command`, `top_commands` |
| `shomagent` | `crates/shomagent/` | `shomagentd` + `shomagent` CLI |
| `shomsky` | `crates/shomsky/` | learn registry and `shomsky publish` |

`engram-shell` modules: `main.rs` (CLI, `--once`, `--script`, `--self-test`), `config.rs`, `modes.rs`, `repl.rs`, `llm.rs`, `tools/`, `mcp/`, `skills.rs`, `ui.rs`, `menu.rs`, `spinner.rs`, `completer.rs`, `learn/`, `halluc_fs.rs`, `halluc_session.rs`.

Put new shell behavior in that map. Add a module or a workspace crate only when the boundary is real.

Errors: `anyhow` on application paths. `thiserror` when a crate boundary needs a typed error. Tokio multi-thread runtime. clap derive. serde for TOML/YAML/JSON. reqwest with rustls for the LLM client. rustyline for the prompt.

Tests: unit tests live next to the module. Behavior and golden paths live in `tests/run.sh` and `tests/cases/*.sh`, plus `engram-shell --self-test`. Do not replace that harness. New behavior that can be checked without tokens belongs in the non-LLM cases. LLM checks use `tiny` and tolerate CPU slowness. Detail: [ADR-0005](adr/0005-ci-integration-steering.md), [build-and-publish.md](build-and-publish.md), [tests/README.md](../tests/README.md).

## Filesystem the agent sees

[engram.md](engram.md).

| Path | Role |
|------|------|
| `/engram/user/` | Agent work tree. Compose bind mount (`SHOMSKY_USER_DIR`). Prompt shows `[home]` or `[engram]`. |
| `/engram/user/bin/` | Learned commands. Entrypoint prepends this to `PATH`. |
| `/engram/user/.command_freq.json` | Command counts for `:top` and cmdfreq. |
| `/engram/user/.packages.json` | Successful `apk add` / `apk del` ledger. |
| `/engram/skills/` | Markdown skills copied from `engram/skills/` at build. |
| `/engram/models/` | GGUF volume `shomsky-models`. |
| `/engram/.hallucination_shell_history/` | Session JSON. Host mount `./.hallucination_shell_history`. |
| `/etc/engram/engram.toml` | Mode, `allow_root_mode`, tool style, `[[mcp_servers]]`. |
| `/etc/engram/models.toml` | Model alias registry. |
| `/etc/shomsky/shomagent_crew.d/` | System crew drop-ins. `./shomagent_crew.d/` overrides the same `name`. |
| `/run/shomagent/` | Control socket and notify lane. |
| `/usr/local/bin/engram-shell` | The shell. |
| `/usr/local/bin/{shomagent,shomagentd,shomsky,engram-cmdfreq,mistralrs}` | Crew, publish, MCP, inference. |

## Modes

[shell-modes.md](shell-modes.md). Switch with `:mode agent|hallucination|root` or `:menu`. The model cannot switch itself.

| Mode | What runs |
|------|-----------|
| `agent` (default) | Real tools. Shell-shaped lines (`ls`, pipelines, redirects) run immediately via BusyBox and do not wait on the model. Natural language goes to the model. `cd` persists. `cd ~` is `/engram/user`. |
| `hallucination` | In-memory filesystem for common builtins. Other lines go to the model with no tool schemas. Nothing touches the real disk. Sessions save under the history directory. Offer resume or fresh on entry. |
| `root` | Every line is BusyBox. No LLM. Gated by `allow_root_mode` (default true). `cd ~` is `/root`. |

Interactive commands listed in `engram/skills/ignore_llm_for_these_commands.md` (`top`, `mc`, editors, …) inherit the TTY in agent and root. Their output is not chat history.

Tool styles (`:tools`, `ENGRAM_TOOL_STYLE`): `auto` (native `tool_calls`, else text patterns), `native`, `text`. `tiny` defaults to `text`. Allow-listed text tools: `shell_exec`, `read_file`, `write_file`, `list_dir`, `load_skill`, plus `mcp__*`.

```text
TOOL shell_exec command=apk add neovim
```

or a `<tool_call>{"name":"list_dir","arguments":{"path":"/etc/engram"}}</tool_call>` block.

## Learn

[learn.md](learn.md), [ADR-0008](adr/0008-learn-userland-verb.md). Agent mode only.

A line starting with `,` or `??` is intent, handled before direct-shell and LLM routing. The model returns visible shell `{command, rationale}`. The user confirms `[r]un` / `[e]dit` / `[q]uit`. After a successful run, `[s]ave` asks the model for a POSIX script, a man page, and a BATS test. One repair retry. If BATS fails, nothing is installed.

| Artifact | Path |
|----------|------|
| Command | `~/bin/<name>` |
| Man page | `~/share/man/man1/<name>.1` |
| Test | `~/bin/tests/<name>.bats` |
| Completion | `~/bin/completions/<name>` |
| Registry | `~/bin/.learn/<name>.toml` |

`shomsky publish <name>` emits a local apk for `/usr/local/bin` and the system man page. There is no community registry. `ENGRAM_LEARN_STUB=1` canned payloads for CI. `ENGRAM_LEARN_AUTO=1` auto-confirms run and save.

## shomagent

[shomagent.md](shomagent.md), [ADR-0007](adr/0007-shomagent-background-crew.md). `shomagentd` starts with the container (`SKIP_SHOMAGENT=1` skips it). Create a member before `ask`, `talk`, or `write`.

| Attach | Capability |
|--------|------------|
| `shomagent watch` / `spawn "<cmd>" --tell-me "…"` | Supervise that process. Ring buffer, token-budget summary against the brief, novelty filter (same class suppressed for an hour). |
| `shomagent read <path>` | `ask` / `talk` over the file. `write` is rejected. |
| `shomagent readwrite <path>` | `ask` / `talk`, plus `write <id> "<text>"` (append) and `write <id> --replace`. |
| `shomagent dream [path]` | Foreground generative journal (default `dream.md`). Seeds a bash/Python topic, appends ReAct beats; type a line to steer, `quit` to leave. Per-beat rain uses `ENGRAM_MATRIX*`. |
| `pause` / `resume` / `stop <id>` | Process lifecycle. `stop` on a document detaches and keeps the file. |

`--persist` writes `./shomagent_crew.d/<name>.toml`. Findings interrupt between prompts via `/run/shomagent/notify`. `:shomagent` or `:agents` lists the roster. `SHOMAGENT_STUB_LLM=1` for CI without a model. The summarizer uses the same local OpenAI-compatible URL.

## MCP

[mcp.md](mcp.md), [ADR-0004](adr/0004-mcp-integration-steering.md).

The image ships `engram-cmdfreq`. On start the shell prints `mcp: connected 'cmdfreq' (2 tools: record_command, top_commands)`. The LLM sees `mcp__cmdfreq__record_command` and `mcp__cmdfreq__top_commands`. Recording of real shell lines is automatic. `:top` and the completer read `/engram/user/.command_freq.json` directly. When the user asks for their most-used or top 10 commands, call `mcp__cmdfreq__top_commands` with `{"limit":10}`.

A new server is an in-image stdio binary plus a `[[mcp_servers]]` entry. Empty `mcp_servers` leaves the shell working.

## Models and skills

[models.md](models.md). `SHOMSKY_MODEL` selects an alias in `/etc/engram/models.toml` (`mistral` default, `qwen`, `deepseek`, `tiny`, and the smaller aliases listed there). `./choose_llm.sh` writes `.env`. First start needs network (Hugging Face, optional `HF_TOKEN`). After the volume has the GGUF, inference does not. CPU cold start can take minutes. `ENGRAM_LLM_READY_TRIES` defaults to 600. Prefer `tiny` and `ENGRAM_TOOL_STYLE=text` for smoke tests. `SKIP_LLM` and `SKIP_MODEL_FETCH` exist for suites that do not need inference.

[engram.md](engram.md). Skills are markdown with YAML frontmatter (`name`, `description`, `always_load`).

| Skill | Load |
|-------|------|
| `shell` | `always_load: true` — BusyBox vocabulary |
| `packages` | `always_load: true` — `apk add` / `apk del`; search when the binary name differs from the package; ledger in `.packages.json` |
| `learn` | on demand via `load_skill` |
| `ignore_llm_for_these_commands` | on demand; the shell also reads it as the TUI bypass list |

`:menu` / `:m` on a color TTY: Modes, Learn, Packages, shomagent, Help, System. `:help` is the same map as text. Both stay available from the ready line.

## If you are changing the repository

1. Read the Accepted ADRs for the boundary you are touching before writing code.
2. Keep the musl/CPU image buildable. Do not add a glibc-only dependency to the flagship path.
3. Keep tool execution and mode checks in the shell.
4. Cover non-LLM behavior in `tests/cases/` or `--self-test`. Run `docker compose run --rm tests` (or `./tests/run.sh`).
5. Use `cargo test -p shomagent -p shomsky` for those crates.
6. Write an ADR when the change moves a cross-cutting boundary (sandbox, MCP transport, CI gates, license posture), adds or removes an integration, or would surprise someone who only read the code. Skip ADRs for routine bugfixes and doc clarifications that do not change a decision.
7. How-to docs stay detailed. ADRs stay short: Status, Date, Context, Decision, Consequences, optional Steering.
8. Do not commit weights, tokens, or `.env`.

Build: Docker 24+ with BuildKit. `./start-shomsky-linux-shell.sh` or `docker compose run --rm shomsky`. Bare `docker compose up` lets Compose’s helper menu steal the keyboard. First mistral.rs compile is long. `CARGO_BUILD_JOBS=1` on small builders. See [build-and-publish.md](build-and-publish.md).

## If you are the model inside the shell

You are the agent in Shomsky Linux, an Alpine container, talking to a person at the `engram-shell` prompt. BusyBox ash is the real shell. Prefer the tools over invented output.

- Stay in the current mode. Do not claim you switched to root or that you wrote the host outside the container.
- In agent mode, call only allow-listed tools. `shell_exec` runs ash. `read_file`, `write_file`, and `list_dir` touch the sandbox filesystem. `load_skill` pulls a manual from `/engram/skills`. MCP tools use the `mcp__{server}__{tool}` name.
- Shell-shaped lines are already executed by the shell. Do not re-issue them unless the user is asking you to.
- Lines starting with `,` or `??` are the learn loop. Do not fake that loop with `shell_exec`. The user types the prefix at the prompt.
- Install and remove software with `apk` in agent mode. If `which` fails, search (`apk search`) before assuming the package name matches the binary. Alpine often provides `python3` and not `python`. Successful `apk add` / `apk del` are recorded for `:packages`.
- For “top commands” or “most used,” call `mcp__cmdfreq__top_commands` with `{"limit":10}`.
- In hallucination mode, act as a virtual shell. Do not attempt real tools. Nothing you say should imply a disk write occurred.
- In root mode you are not in the loop.
- TUI programs in the ignore list are not yours to capture.
- Keep plans visible. Learn plans are real shell the user can read before they run.
- `:menu` and `:help` are the user’s map. Point there when they are lost, and follow the skill you were given when one is loaded.

## Where to read next

| Need | Document |
|------|----------|
| Normative index | [adr/README.md](adr/README.md) |
| Architecture | [adr/0001](adr/0001-alpine-llm-linux-shell.md) |
| Rust style and crates | [adr/0002](adr/0002-rust-language-and-style.md) |
| Inference, weights, Compose | [adr/0003](adr/0003-runtime-integrations.md), [models.md](models.md) |
| MCP | [adr/0004](adr/0004-mcp-integration-steering.md), [mcp.md](mcp.md) |
| CI and tests | [adr/0005](adr/0005-ci-integration-steering.md) |
| License and AI contributions | [adr/0006](adr/0006-mit-as-is-and-ai-contributions.md), [licensing.md](licensing.md) |
| Background agents | [adr/0007](adr/0007-shomagent-background-crew.md), [shomagent.md](shomagent.md) |
| Learn / publish | [adr/0008](adr/0008-learn-userland-verb.md), [learn.md](learn.md) |
| Modes, menu, tool protocol | [shell-modes.md](shell-modes.md) |
| `/engram` and skills | [engram.md](engram.md) |
| Build, GHCR | [build-and-publish.md](build-and-publish.md) |
| Human overview | [writeup.md](writeup.md), [README](../README.md) |
