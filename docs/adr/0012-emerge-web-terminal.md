# ADR-0012: :emerge web terminal

- **Status:** Accepted
- **Date:** 2026-11-17

## Context

The shell runs inside a Docker container on a TTY. Operators also want to
continue that same session from a browser on the machine that started the
container, without a second product or a published MCP port.

[ADR-0004](0004-mcp-integration-steering.md) keeps MCP on stdio and forbids
publishing MCP ports. A terminal page is a different listen: one local HTTP
port for the human, not a tool transport.

## Decision

- `:emerge` is an interactive meta-command. Under `--script`, pipes, and
  `--once` it refuses. `ENGRAM_EMERGE=off` (also `0`, `false`, `no`) disables
  it; unset leaves it available.
- The parent shell writes a snapshot (mode, tool style, working directory,
  conversation, and in hallucination mode the virtual filesystem) and binds
  `0.0.0.0:1337` inside the container (`ENGRAM_EMERGE_PORT` overrides).
- `GET /` serves one offline page (vendored xterm.js, MIT). `GET /ws` is the
  terminal socket. Text frames resize the PTY. Binary frames are keyboard
  input. PTY output is binary frames.
- The first connection starts one child: this same `engram-shell` binary with
  `--emerge-resume`. A refresh reattaches to that PTY while the child is
  alive. The container TTY only shows the URL and waits.
- Ctrl-C on the container TTY stops the listener, kills the child, deletes
  the snapshot, and returns to the prompt. A second `:emerge` while the port
  is open prints the URL and does not start another server.
- Compose and the interactive launcher publish **`127.0.0.1:1337:1337`**
  only. There is no auth token. Do not publish 1337 on a non-loopback
  address.
- MCP stays stdio-only. This listen does not change ADR-0004.

## Consequences

- The page is a new terminal view. Scrollback from before `:emerge` stays on
  the container TTY. The child keeps the conversation, mode, cwd, and
  hallucination files, so the next turn continues that session.
- The image does not gain a terminal package. xterm.js is vendored under
  `assets/emerge/` and its MIT notice stays in `NOTICE`.
- A loopback publish is enough for a browser on the Docker host. It is not a
  remote-access feature.

## Steering

- Do not add a second listen, a token gate, or a non-loopback publish without
  a superseding ADR.
- Do not route MCP, or any model tool, through this port.
