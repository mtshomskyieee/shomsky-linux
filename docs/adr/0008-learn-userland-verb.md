# ADR-0008: Learn — a Linux that writes its own userland

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

Natural-language agent turns today execute and vanish. The product thesis needs
a path where repeated intent crystallizes into real userland: a command under
the agent's home, a man page, a test, and later an apk others can install.
Without a gated promote path, "save this" would ship untested scripts into
`PATH`.

Related: [ADR-0001](0001-alpine-llm-linux-shell.md) (shell + filesystem),
[ADR-0002](0002-rust-language-and-style.md) (module / crate layout),
[docs/learn.md](../learn.md).

## Decision

Ship an interactive **learn** loop in `engram-shell` and a **`shomsky`** CLI
for packaging.

### Intent prefix

Lines starting with `,` or `??` are **intent**, not shell syntax. They are
handled before direct-shell / LLM routing. Agent mode only; other modes print
a clear skip.

### Two-phase trust

1. LLM produces a plan as **visible real shell** (JSON `{command, rationale}`;
   no hidden tool steps).
2. User confirms (`[r]un` / `[e]dit` / `[q]uit`), then the shell executes via
   existing `shell_exec`.
3. After a successful run, offer `[s]ave as command?`.

### Promote gate

On save, the LLM generalizes constants into flags, emits a POSIX script, man
page, and BATS test. **BATS must pass** (one repair retry on failure) before
install. Failures do not install.

Install target (learn):

| Path | Role |
|------|------|
| `$HOME/bin/<name>` | Executable (`HOME` = agent `/engram/user`) |
| `$HOME/share/man/man1/<name>.1` | Man page |
| `$HOME/bin/tests/<name>.bats` | BATS suite |
| `$HOME/bin/completions/<name>` | Bash completion snippet |
| `$HOME/bin/.learn/<name>.toml` | Registry for publish |

Entrypoint prepends `$ENGRAM_USER_DIR/bin` to `PATH` and sets `MANPATH`.

### Publish

`shomsky publish <name>` reads the registry and emits a local **apk** that
installs to `/usr/local/bin` and `/usr/share/man/man1`. No community registry
in this decision — packages are traded by hand.

### Stub

`ENGRAM_LEARN_STUB=1` returns canned plan/promote payloads for CI without an
LLM. `ENGRAM_LEARN_AUTO=1` auto-confirms run/save with the suggested name for
non-interactive scripts.

## Consequences

- Image gains `bats` and `/usr/local/bin/shomsky`.
- Workspace gains member crate `shomsky` (lib + CLI).
- Completer lists `$HOME/bin` executables in addition to cmdfreq.
- Promote never writes `/usr/local/bin` directly; that path is publish-only.
