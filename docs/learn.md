# Learn — crystallize intent into userland

When you ask in natural language with a `,` or `??` prefix, Shomsky plans a
real shell command, runs it after you confirm, and can **promote** the result
into `~/bin` with a man page and a BATS test. The distro grows commands as you
use it.

See [ADR-0008](adr/0008-learn-userland-verb.md).

## Intent

```text
$ , find the biggest files that changed this week and show them as a table
  ⟡ plan:
      find . -mtime -7 -type f -printf '%s\t%p\n' | sort -rn | head -20 | column -t
  [r]un / [e]dit / [q]uit?
```

- `, …` or `?? …` — this line is intent, not syntax.
- Agent mode only (hallucination/root skip with a message).
- Plans are always shown as real shell.

## Confirm and save

After `[r]un`:

```text
  ⟡ ran. 20 lines.
  [s]ave as command?
$ s
  ⟡ name? (suggested: recent-fat)
$ recent-fat
  ✓ wrote ~/bin/recent-fat
  ✓ wrote ~/share/man/man1/recent-fat.1
  ✓ wrote ~/bin/tests/recent-fat.bats  → passing
  ✓ added tab-completion
```

If BATS fails, learn retries the LLM once, then gives up **without** installing.

## Paths

| Artifact | Location |
|----------|----------|
| Command | `~/bin/<name>` (`/engram/user/bin` in agent mode) |
| Man page | `~/share/man/man1/<name>.1` |
| Test | `~/bin/tests/<name>.bats` |
| Completion | `~/bin/completions/<name>` |
| Registry | `~/bin/.learn/<name>.toml` |

`PATH` and `MANPATH` are wired by the container entrypoint.

## Publish

```bash
shomsky publish recent-fat
# → ~/bin/packages/recent-fat-0.1.0-r0.apk
apk add --allow-untrusted ~/bin/packages/recent-fat-0.1.0-r0.apk
# installs /usr/local/bin/recent-fat + system man page
```

## CI / automation

| Env | Effect |
|-----|--------|
| `ENGRAM_LEARN_STUB=1` | Canned plan/promote JSON (no LLM) |
| `ENGRAM_LEARN_AUTO=1` | Auto `[r]un` and save with suggested name |

## Related

- [Shell modes](shell-modes.md)
- [shomagent](shomagent.md)
- [ADR-0001](adr/0001-alpine-llm-linux-shell.md)
