---
name: learn
description: Crystallize natural-language intent into ~/bin commands via , / ?? prefixes
always_load: false
---

# Learn

Lines starting with `,` or `??` are **intent**, not shell. The shell plans
visible BusyBox/ash commands, confirms, runs them, and can promote to
`~/bin` with man + BATS. See `/usr/share/doc` docs or ask `:help`.

Do **not** call `shell_exec` to invent a learn flow — the user types `, …`
at the prompt. After promote, the new binary is on `PATH` under `~/bin`.

Publish: `shomsky publish <name>` emits an apk for `/usr/local/bin`.
