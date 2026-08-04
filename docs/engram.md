# Engram

`/engram` is the Shomsky-specific directory for durable agent state that is
not part of the base OS.

```
/engram/
  user/                          Agent working tree (Compose bind mount)
    .command_freq.json           Command-frequency store (MCP cmdfreq / :top)
  skills/                        Markdown skills (tool manuals, procedures)
  models/                        GGUF weights (Docker named volume)
  .hallucination_shell_history/  Hallucination session JSON (Compose bind mount)
  .shell_history                 readline history for engram-shell
```

Compose persists models via the `shomsky-models` volume, the agent working
directory via a host bind mount (default `./engram/user` → `/engram/user`;
override with `SHOMSKY_USER_DIR`), and hallucination sessions via
`./.hallucination_shell_history` → `/engram/.hallucination_shell_history`
(override with `SHOMSKY_HALLUC_HISTORY`). Without those mounts, files live
only in the container layer and disappear with `docker compose run --rm`.

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

## Interactive bypass list

[`engram/skills/ignore_llm_for_these_commands.md`](../engram/skills/ignore_llm_for_these_commands.md)
lists TUI / ncurses programs (`top`, `mc`, `vim`, …). In **agent** and **root**
modes, those commands run on the real TTY (stdin/stdout/stderr inherited) and
never involve the LLM. Edit the file (one basename per line) to grow the list;
restart the shell (or rebuild the image) to reload.
