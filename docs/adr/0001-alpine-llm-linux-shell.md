# ADR-0001: Alpine + LLM Linux shell architecture

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

We need a small, coherent Linux distribution shipped as a Docker image whose
linux shell is an LLM agent. The product differentiator is a musl-native stack
(not glibc binaries wrapped with `gcompat`). Model weights are large and must
not bloat the image. Tool execution and mode policy must stay under the shell’s
control so the model cannot escalate itself.

Related operational detail: [shell modes](../shell-modes.md),
[MCP](../mcp.md), [models](../models.md),
[build and publish](../build-and-publish.md).

## Decision

Shomsky Linux is an **Alpine-derived container distribution** whose linux shell
is the Rust binary **`engram-shell`**. The image stays small; GGUF weights are
fetched on first run into a named volume. Inference is local
[mistral.rs](https://github.com/EricLBuehler/mistral.rs) serving an
OpenAI-compatible HTTP API. The **container is the sandbox**.

### Components

```
┌─────────────────────────────────────────────────────────────┐
│  Docker container (Shomsky Linux)                           │
│                                                             │
│  entrypoint.sh                                              │
│    ├─ resolve SHOMSKY_MODEL → models.toml                   │
│    ├─ download GGUF → /engram/models (volume)               │
│    ├─ mistralrs serve --format gguf --cpu :1234             │
│    └─ exec engram-shell  (linux shell)                      │
│                                                             │
│  /engram/skills/*.md   ← markdown skills (tool manuals)     │
│  /etc/engram/*.toml    ← distro config + model registry     │
│  BusyBox ash           ← real OS tools used by agent-mode   │
└─────────────────────────────────────────────────────────────┘
```

### Why Alpine + mistral.rs

- Alpine 3.24 ships musl-native `rust` / `cargo`.
- mistral.rs is MIT, pure Rust, and compiles against that toolchain without a
  glibc shim.
- The resulting image stays a coherent musl distro.

### Processes at runtime

| Process | Role |
|---------|------|
| `entrypoint.sh` | PID 1 wrapper: fetch, start server, start shomagentd, exec shell |
| `mistralrs` | OpenAI-compatible HTTP server on `127.0.0.1:1234` |
| `shomagentd` | Background crew supervisor (ring buffers, novelty findings) |
| `engram-shell` | Interactive REPL; owns tool execution, modes, and MCP client |
| `engram-cmdfreq` (etc.) | Optional **stdio child** MCP servers spawned by the shell — no ports |
| `shomagent` | CLI client to `shomagentd` (watch / roster / talk) |

`engram-shell` talks to mistral.rs over `/v1/chat/completions`. Tool discovery
is configurable:

- **native** — OpenAI `tool_calls` with schemas
- **text** — grep allow-listed patterns in the assistant reply (for small models)
- **auto** — native if present, else text

Configured MCP servers (see [MCP](../mcp.md) and
[ADR-0004](0004-mcp-integration-steering.md)) add tools named
`mcp__{server}__{tool}`. They use stdio pipes only and are **not** published
from Docker Compose.

The shell — not the model server — executes tools, so mode policy cannot be
talked around by the LLM. Unknown tool names are ignored.

### Filesystem contract

| Path | Purpose |
|------|---------|
| `/engram/user/` | Agent working tree (Compose bind mount) |
| `/engram/skills/` | Markdown skills with YAML frontmatter |
| `/engram/models/` | GGUF volume (persistent) |
| `/etc/engram/engram.toml` | Shell defaults (`mode`, `allow_root_mode`, …) |
| `/etc/engram/models.toml` | Model alias registry |
| `/etc/shomsky/shomagent_crew.d/` | System shomagent member drop-ins |
| `/run/shomagent/` | Control socket + notification lane |
| `/etc/os-release` | `ID=shomsky`, `ID_LIKE=alpine` |
| `/usr/local/bin/engram-shell` | Linux shell |
| `/usr/local/bin/engram-cmdfreq` | First-party command-frequency MCP (stdio child) |
| `/usr/local/bin/shomagent` / `shomagentd` | Background crew CLI + daemon |
| `/engram/user/.command_freq.json` | Command frequency store (Completer / `:top` / MCP) |
| `/usr/local/bin/mistralrs` | Inference server |

### Image build

Multi-stage Dockerfile in [`containers/Dockerfile`](../../containers/Dockerfile):

1. **builder** — `alpine:3.24` + `rust`/`cargo`/`build-base` compiles
   `engram-shell` and `mistralrs-cli` (`--no-default-features`, CPU).
2. **runtime** — slim Alpine with BusyBox, curl, CA certs, binaries, rootfs
   overlay, and skills.

### GPU

CUDA has no supported musl/Alpine userspace path. The flagship image is
CPU-only. A separate glibc-based CUDA image is stubbed in
[`containers/Dockerfile.cuda`](../../containers/Dockerfile.cuda) — see comments
there and [models.md](../models.md).

### Security boundary

Agent-mode can run real shell commands and write files **inside** the
container. Recommendations for operators:

- Run as the non-root `shomsky` user
- `--read-only` rootfs with a writable `/engram` volume
- Drop Linux capabilities (`--cap-drop=ALL`)
- Disable root-mode with `allow_root_mode = false` in `engram.toml`

## Consequences

- Product identity is musl/Alpine + local LLM + shell-owned tools.
- Image builds stay heavy (mistral.rs compile) but runtime images stay lean.
- GPU remains a non-flagship / stub path until a musl story exists.
- Operators must treat the container as the trust boundary; see also
  [ADR-0006](0006-mit-as-is-and-ai-contributions.md) (AS-IS / use at your own risk).
