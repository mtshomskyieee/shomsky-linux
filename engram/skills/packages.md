---
name: packages
description: Install and remove Alpine software with apk. `shominstall PKG` means the user wants that package installed. Use when the user asks to install, add, uninstall, or remove a program (neovim, curl, …).
always_load: true
---

# Packages

Install and remove software with **apk** on Shomsky Linux (Alpine / musl).
Use `shell_exec` in **agent** mode. Hallucination mode does not touch the disk.
`:mode root` only bypasses the LLM — it is not required for installs.

## Ledger

The shell records successful `apk add` / `apk del` into
`/engram/user/.packages.json`. Surface it with `:packages` or `:menu` →
**Packages** (Enter on an installed package → [u]ninstall runs `apk del`;
removed → [r]einstall). Prefer those over inventing a list. Do not edit the
ledger by hand unless the user asks.

Interactive launches (`./agent_sh.sh`, `./scripts/shell.sh`) commit apk
database changes onto a local `shomskylinux:installed` image so the next
session already contains those packages. Each container start also compares
the ledger to `apk info -e` and `apk add`s names still marked installed that
are missing (rebuild, deleted installed image, partial install). A repair
does not rewrite the ledger. Tests keep `docker compose run --rm` and stay
ephemeral.

## Rules

1. **`shominstall PKG` is installed by the shell** as `apk add PKG` before
   the line reaches you. Bare `shominstall` (no name) means ask which Alpine
   package to install. Do not execute `shominstall`, and do not run BusyBox
   `install`.
2. **apk only.** Never run `apt`, `apt-get`, `dnf`, or `pacman`. Never run
   BusyBox `install` to add software — that applet copies files. A clear
   request (`install git`, `please install git`, `apt-get install -y git`,
   or a tool call of `shominstall git`) is `apk add`. When you call
   `shell_exec`, the command must be `apk add PKG` (or `apk del`).
3. **UID 0 and network.** Run `id -u` first; it must be `0`. The image has no
   `doas` or `sudo`. If not root, or if apk cannot reach the network, stop and
   report the error (do not send the user to `:mode root` for privileges).
4. **Musl Alpine only.** Do not enable `edge`, pipe `curl` into a shell, or
   install glibc / `gcompat` binaries.
5. **Not learn.** `~/bin` commands and `shomsky publish` are the `learn` skill.

## Install

Via `shell_exec`, one BusyBox command per call. Example text-tool lines:

```
TOOL shell_exec command=id -u
TOOL shell_exec command=apk search -q cmd:nvim
TOOL shell_exec command=apk add neovim
```

Do not wrap the command in quotes (`command="apk …"`). Prefer Form A JSON
with `"command":"apk add neovim"` when using `<tool_call>`.

1. Resolve the package name (user words are often binaries: nvim → neovim):
   - apk update if the index is missing or stale
   - apk search -q cmd:NAME then apk search -q NAME
   - **One match** → use it
   - **Several** → list them and ask which to install
   - **None** → read the branch from /etc/apk/repositories, then
     curl pkgs.alpinelinux.org for that name. Still absent → say so
2. apk add PKG
3. Verify: apk info -e PKG and command -v the binary

Editors and other TUI names in `ignore_llm_for_these_commands` (`nvim`,
`vim`, …) are typed by the user on the real TTY — never `shell_exec` them.

## Uninstall

1. Resolve what is installed: `apk info -W /path/to/binary` or
   `apk info -e PKG` / `apk search -q NAME`
2. Refuse to remove image essentials unless the user explicitly insists after
   you say the image needs them: `busybox`, `bash`, `bats`,
   `ca-certificates`, `curl`, `libgcc`, `libstdc++`, `openssl`
3. `apk del PKG` (do not use `--rdepends`)
4. Confirm with `apk info -e PKG` (expect missing) and `command -v`
