# ADR-0002: Rust language and coding style

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

The linux shell and first-party MCP helpers must compile on Alpine/musl with
the distro `rust`/`cargo` toolchain, stay size-conscious for the container
image, and remain readable for contributors (including AI-assisted work that
must follow [ADR-0000](0000-what-is-an-adr.md) and
[ADR-0006](0006-mit-as-is-and-ai-contributions.md)).

## Decision

### Language and workspace

- **Rust edition 2021**, `rust-version = "1.74"` (see root `Cargo.toml`).
- Cargo **workspace** with:
  - root package / binary **`engram-shell`** (`src/main.rs`)
  - member crate **`engram-cmdfreq`** (`crates/engram-cmdfreq/`) — stdio MCP +
    shared command-frequency store used by Completer / `:top`
  - member crate **`shomagent`** (`crates/shomagent/`) — `shomagentd` supervisor
    + `shomagent` CLI (see [ADR-0007](0007-shomagent-background-crew.md))
  - member crate **`shomsky`** (`crates/shomsky/`) — learn registry / apk publish
    + `shomsky` CLI (see [ADR-0008](0008-learn-userland-verb.md))
  - member crate **`quickyn`** (`crates/quickyn/`) — yes/no + confidence CLI
    (see [ADR-0009](0009-quickyn-yes-no.md))
- Prefer MIT / Apache-2.0 dependencies consistent with the distro stack.

### Runtime and libraries

- **Tokio** multi-thread runtime for the shell (`#[tokio::main]`).
- **clap** with derive + env for CLI / overrides.
- **serde** + TOML/YAML/JSON for config, skills frontmatter, and MCP framing.
- **reqwest** (rustls) for the OpenAI-compatible LLM client.
- **rustyline** for the interactive prompt.
- Errors: **`anyhow`** (+ `Context` / `bail!`) for application paths. Keep
  `thiserror` available for typed library errors when a crate boundary needs
  them; do not introduce a second ad-hoc error framework.

### Module layout (`engram-shell`)

Keep responsibilities split under `src/`:

| Area | Modules |
|------|---------|
| Entry / CLI | `main.rs` (`--once`, `--script`, `--self-test`) |
| Config / modes | `config.rs`, `modes.rs` |
| REPL hub | `repl.rs` |
| LLM | `llm.rs` |
| Tools | `tools/` (builtins, shell detect, text tool parse) |
| MCP client | `mcp/` |
| Skills / UI | `skills.rs`, `ui.rs`, `spinner.rs`, `completer.rs` |
| Learn | `learn/` (intent prefix, plan, promote; uses `shomsky` lib) |
| Hallucination | `halluc_fs.rs`, `halluc_session.rs` |

Mode policy and tool execution stay in the shell (client-side), never delegated
to the model server. See [ADR-0001](0001-alpine-llm-linux-shell.md).

### Build profile

Release builds optimize for **size** (LTO, `codegen-units = 1`, strip,
`opt-level = "z"`) so the musl image stays small.

### Testing style

- Unit tests co-located with modules (`#[cfg(test)]`) where they clarify logic.
- Regression and golden paths live in the bash harness under `tests/` (see
  [ADR-0005](0005-ci-integration-steering.md)). Do not replace that suite with
  a parallel incompatible harness without a superseding ADR.

## Consequences

- New shell features should land in the existing module map unless a new
  boundary clearly warrants a module or workspace crate.
- Size-focused release settings constrain dependency bloat.
- Contributors targeting Alpine must keep the musl/CPU build path working.