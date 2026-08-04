# MCP integration (local / in-container)

Shomsky Linux supports the [Model Context Protocol](https://modelcontextprotocol.io/) so `engram-shell` can discover and call **extra tools** beyond the five built-ins (`shell_exec`, `read_file`, `write_file`, `list_dir`, `load_skill`).

This document uses the first shipped MCP server, **`engram-cmdfreq`**, as the end-to-end example.

## Enabling by default (dev image)

MCP starts automatically when the **image** contains:

1. `/usr/local/bin/engram-cmdfreq`
2. `[[mcp_servers]]` in `/etc/engram/engram.toml` (ships in [`rootfs/etc/engram/engram.toml`](../rootfs/etc/engram/engram.toml))

`./agent_sh.sh` only runs the existing `shomskylinux:dev` image — it does **not** rebuild.
After pulling MCP changes, rebuild once:

```bash
./build.sh          # fast path: containers/Dockerfile.shell
# or:
docker compose build shomsky
```

On shell start you should see:

```text
mcp: connected 'cmdfreq' (2 tools: record_command, top_commands)
```

Then `:top` / `show my top 10 commands` work. No extra env vars or Compose ports are required.

## Security model

| Rule | Detail |
|------|--------|
| Stdio only | Each MCP server is a **child process** of `engram-shell`. IPC is anonymous pipes (stdin/stdout). |
| No listen sockets | MCP servers must not bind TCP/UDP. HTTP / SSE / Streamable HTTP are **not** configured in v1. |
| Not published | Docker Compose does **not** publish any MCP port. Nothing outside the container can dial an MCP server. |
| Container boundary | Servers run inside the same Shomsky container as the shell. Host processes cannot reach them except by `docker exec` into that container (same trust model as the shell itself). |
| Mode policy | MCP tools run in **agent** (and root if tools are invoked). In **hallucination** mode they are quiet no-ops, like other real tools. |

Config accepts only local exec:

```toml
[[mcp_servers]]
name = "cmdfreq"
command = "/usr/local/bin/engram-cmdfreq"
args = []
```

There is no `url` field. A `command` containing `://` is refused at connect time.

## Architecture

```
engram-shell  --stdio pipes-->  engram-cmdfreq (child)
      |                                |
      |                         /engram/user/.command_freq.json
      +-- rustyline Completer --^
      +-- :top ----------------^
```

- The LLM sees MCP tools as OpenAI functions named `mcp__{server}__{tool}`.
- Tab completion and `:top` read the **same JSON store file** (no MCP RPC per Tab key).

## Config reference

In `/etc/engram/engram.toml` (defaults ship in the image):

```toml
cmdfreq_path = "/engram/user/.command_freq.json"

[[mcp_servers]]
name = "cmdfreq"
command = "/usr/local/bin/engram-cmdfreq"
args = []
# optional env overrides; ENGRAM_CMDFREQ_PATH is injected from cmdfreq_path
```

| Key | Meaning |
|-----|---------|
| `name` | Server id used in tool names (`mcp__cmdfreq__…`) |
| `command` | Absolute path to a binary **inside the container** |
| `args` | argv after the command |
| `env` | Extra environment for the child |
| `cmdfreq_path` | Frequency JSON path (Completer / `:top` / server) |

Override the store path with `ENGRAM_CMDFREQ_PATH`.

Empty `mcp_servers` disables MCP; the shell still works.

## Tool naming

| MCP server | MCP tool | Name seen by the LLM |
|------------|----------|----------------------|
| `cmdfreq` | `record_command` | `mcp__cmdfreq__record_command` |
| `cmdfreq` | `top_commands` | `mcp__cmdfreq__top_commands` |

## Example: `engram-cmdfreq`

Tracks how often shell commands run (agent direct shell, `shell_exec`, root mode) and exposes:

- **`record_command`** — `{ "command": "ls -la" }` increment count
- **`top_commands`** — `{ "limit": 10, "prefix": "" }` ranked list (default limit **10**)

The binary is stdio-only MCP. It is **not** started by the entrypoint as a daemon; the shell spawns it on REPL start and kills it on exit.

Store file (default): `/engram/user/.command_freq.json` (persists via the Compose bind mount for `/engram/user`).

### Get your top 10 commands

1. Use **agent** mode (the default): `ENGRAM_MODE=agent` or `:mode agent`.
2. Run some real shell commands so counts accumulate (`ls`, `pwd`, …). Recording is automatic.
3. Then either:

**A. Meta-command (no LLM):**

```text
:top
```

or:

```text
:top 10
```

**B. Natural language (agent mode):**

```text
show my top 10 commands
```

or:

```text
what are my most used commands?
```

The agent should call:

```text
mcp__cmdfreq__top_commands
{"limit": 10}
```

**C. Text tool style** (tiny / `ENGRAM_TOOL_STYLE=text`):

```text
TOOL mcp__cmdfreq__top_commands {"limit":10}
```

**D. Optional prefix filter:**

```json
{"limit": 10, "prefix": "git"}
```

**Sample output:**

```text
 1.    12  ls -la
 2.     8  pwd
 3.     5  cat /etc/os-release
 ...
```

If nothing is recorded yet, `:top` explains how to accumulate usage.

## Adding another local MCP

1. Build or copy a **stdio** MCP server binary into the image (e.g. `/usr/local/bin/my-mcp`) or under `/engram`.
2. Add an entry to `engram.toml`:

```toml
[[mcp_servers]]
name = "mytools"
command = "/usr/local/bin/my-mcp"
args = []
```

3. Rebuild / restart the container. On shell start you should see `mcp: connected 'mytools' (…)`.
4. Tools appear as `mcp__mytools__{toolname}` for the LLM.

Do **not** expose the server on a host port. Keep the process as a stdio child of `engram-shell`.

## Modes

| Mode | MCP behavior |
|------|----------------|
| `agent` | Schemas advertised; tools execute |
| `root` | Tools can execute if the model path invokes them; command lines are also recorded |
| `hallucination` | No MCP schemas; calls are quiet no-ops |

## See also

- [Architecture (ADR-0001)](adr/0001-alpine-llm-linux-shell.md)
- [MCP steering (ADR-0004)](adr/0004-mcp-integration-steering.md)
- [Shell modes](shell-modes.md)
- [Engram (skills)](engram.md)
