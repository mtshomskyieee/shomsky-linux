# Engram

`/engram` is the Shomsky-specific directory for durable agent state that is
not part of the base OS.

```
/engram/
  user/                          Agent working tree (Compose bind mount)
    .command_freq.json           Command-frequency store (MCP cmdfreq / :top)
    .packages.json               apk add / apk del ledger (:packages / :menu)
  skills/                        Markdown skills (tool manuals, procedures)
  models/                        GGUF weights (Docker named volume)
  .hallucination_shell_history/  Hallucination session JSON (Compose bind mount)
  .shell_history                 readline history for engram-shell
```

Compose persists models via the `shomsky-models` volume, the agent working
directory via a host bind mount (default `./engram/user` → `/engram/user`;
override with `SHOMSKY_USER_DIR`), and hallucination sessions via
`./.hallucination_shell_history` → `/engram/.hallucination_shell_history`
(override with `SHOMSKY_HALLUC_HISTORY`). Interactive shells
(`./agent_sh.sh`, `./scripts/shell.sh`) commit apk database changes onto a
local `shomskylinux:installed` image so installed packages survive the next
start. Each start also reinstalls ledger packages that `apk info -e` reports
missing (including after a rebuild). Tests use `docker compose run --rm` and
stay ephemeral.

## Skill format

Skills are Markdown files with optional YAML frontmatter:

```markdown
---
name: shell
description: Basic shell commands and filesystem tool use
always_load: true
---

# Body

Instructions the model should follow when this skill is active…
```

| Field | Meaning |
|-------|---------|
| `name` | Stable identifier used by `load_skill` |
| `description` | Shown in the skill index |
| `always_load` | If `true`, the full body is inlined into the system prompt |

Skills without `always_load` appear only in the index. The model pulls them
in with the `load_skill` tool so the context window stays small as the
catalog grows.

## Shipping skills

Skills in [`engram/skills/`](../engram/skills/) are copied into the image at
build time. At runtime you can bind-mount additional skills:

```bash
docker run -v "$PWD/my-skills:/engram/skills:ro" …
```

## First skill: `shell.md`

[`engram/skills/shell.md`](../engram/skills/shell.md) documents BusyBox
essentials and the filesystem layout. It is `always_load: true` so every
session starts with a shared shell vocabulary.

## Packages: `packages.md`

[`engram/skills/packages.md`](../engram/skills/packages.md) walks install and
uninstall with Alpine `apk` (search when the binary name differs from the
package). It is `always_load: true`. In agent mode the model resolves the
package name and runs `apk add` / `apk del` via `shell_exec` when UID is 0.
Successful mutations are recorded in `/engram/user/.packages.json` and shown
under `:menu` → Packages or `:packages`. Interactive launches commit those
packages into `shomskylinux:installed`; a new container reinstalls any ledger
entry that is missing from the image.

## Interactive bypass list

[`engram/skills/ignore_llm_for_these_commands.md`](../engram/skills/ignore_llm_for_these_commands.md)
lists TUI / ncurses programs (`top`, `mc`, `vim`, …). In **agent** and **root**
modes, those commands run on the real TTY (stdin/stdout/stderr inherited) and
never involve the LLM. Edit the file (one basename per line) to grow the list;
restart the shell (or rebuild the image) to reload.
