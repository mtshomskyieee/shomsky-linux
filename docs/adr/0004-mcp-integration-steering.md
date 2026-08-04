# ADR-0004: MCP integration steering

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

`engram-shell` needs a way to discover and call tools beyond the five builtins
(`shell_exec`, `read_file`, `write_file`, `list_dir`, `load_skill`) without
opening network attack surface or bypassing mode policy. The
[Model Context Protocol](https://modelcontextprotocol.io/) is the chosen
extension mechanism.

How-to and examples: [docs/mcp.md](../mcp.md). Reference server:
`engram-cmdfreq` (`crates/engram-cmdfreq/`).

## Decision

### Transport and trust

- MCP servers are **stdio children** of `engram-shell` only (anonymous pipes).
- Config accepts local exec via `[[mcp_servers]]` (`name`, `command`, `args`,
  optional `env`). There is **no** `url` field.
- Refuse commands that look like network URLs (`://`) at connect time.
- Do **not** bind TCP/UDP for MCP; HTTP / SSE / Streamable HTTP are out of
  scope for v1.
- Docker Compose must **not** publish MCP ports. Nothing outside the container
  dials MCP except by the same trust path as `docker exec` into the container.

### Naming and lifecycle

- Tools appear to the LLM as OpenAI functions named `mcp__{server}__{tool}`.
- The shell spawns configured servers on REPL start and shuts them down on exit.
- Empty `mcp_servers` disables MCP; the shell still works.

### Mode policy

| Mode | MCP behavior |
|------|----------------|
| `agent` | Schemas advertised; tools execute |
| `root` | Tools may execute if invoked; shell lines still recorded where applicable |
| `hallucination` | No MCP schemas; calls are quiet no-ops (like other real tools) |

Mode enforcement stays in the shell ([ADR-0001](0001-alpine-llm-linux-shell.md)).

### First-party example

Ship `engram-cmdfreq` as the reference stdio MCP (`record_command`,
`top_commands`). Completer / `:top` may read the same JSON store file directly
(no MCP RPC per Tab key).

## Steering

When adding or changing MCP support:

1. Keep new servers **in-image** (or under `/engram`) and stdio-only.
2. Document connect expectations in [docs/mcp.md](../mcp.md); update this ADR
   only if the security model or transport changes.
3. Do not introduce host-reachable MCP listeners “for convenience.”
4. Preserve hallucination quiet no-ops for MCP tools.
5. Prefer tool names and schemas that small models can call under both
   `native` and `text` tool styles.

## Consequences

- Extension surface is process-local and container-bound.
- Contributors cannot “just expose” an MCP HTTP port without superseding this
  ADR.
- Operators get automatic cmdfreq when the image ships the binary +
  `[[mcp_servers]]` entry.