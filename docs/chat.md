# :chat

Immersive framed conversation with the current local model. Same ANSI + BusyBox
`stty` overlay as [`:quickyn`](quickyn.md): alternate screen, no TUI crate.

`:chat` starts a **new session**. It does not read or write REPL history, and it
does not run tools.

## Usage

Inside `engram-shell`, type `:chat`. Off a TTY (or if raw mode fails): prints
`chat: interactive only` and returns.

```text
┌ chat ── ctx 842 tok · est 3% ─────────────────┐
│ you                                            │
│   hello                                        │
│ assistant                                      │
│   hi                                           │
├────────────────────────────────────────────────┤
│ > input                                        │
└ Enter send · Tab [c] compactify · ↑↓ · Esc ────┘
```

The title shows estimated context size and percent of the window. Counters
update after each turn and after compactify.

| Key | Effect |
|-----|--------|
| Enter | Send the input line (or compactify when that control is focused) |
| Tab | Focus the compactify control |
| `c` | Compactify (empty input, or when the control is focused) |
| ↑ / ↓ | Scroll the transcript one line |
| PgUp / PgDn | Scroll a page |
| ← / → | Move in the input |
| Backspace / ^U | Edit / clear the input |
| Esc | Cancel an in-flight reply or compactify; exit when idle |

New replies stick to the bottom. After you scroll up, the view stays until you
scroll back to the end.

## Context counters

Tokens are `chars().count() / 4` over every message that will be sent, including
the short chat system prompt. Percent is `tokens * 100 / window` (integer).

| Variable | Default | Role |
|----------|---------|------|
| `ENGRAM_CONTEXT_TOKENS` | `32768` | Window used for the percent |

`:chat` does **not** trim with `max_context_messages`. The counters are the
signal; compactify is how the user shrinks the prompt.

## Compactify

`c` asks the current model to summarize the dialogue so far (decisions, names,
open questions), then replaces the turns with that summary as a single user
message under the same system prompt. Counters are recomputed. An empty
transcript shows `nothing to compactify` and does not call the model.

`ENGRAM_CHAT_STUB=1` skips HTTP: replies are a canned line, and compactify
substitutes a short canned summary.

## Save

One file per visit, named at open and rewritten after each completed turn and
on Esc:

```text
saved_chat_<YYYYMMDDTHHMMSSZ>.md
```

Written in the shell cwd. Markdown is a `# chat` title, then `## user` /
`## assistant` sections. A compacted summary is a user section. The system
prompt is not written out.

After the frame closes: `chat: saved <path>`.

Hallucination mode does not write (no disk writes). The frame still runs; on
exit: `chat: transcript not saved (hallucination mode)`.

## Tests

- TUI state: `cargo test chat_tui`

Also listed under `:menu` → System.
