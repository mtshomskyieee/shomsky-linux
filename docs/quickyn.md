# quickyn

One-shot **yes/no** decision from the current local model, with a confidence
score. Same CLI layer as `shomagent`: a binary on `PATH`, usable inside
`engram-shell` agent mode and from ordinary scripts.

See [ADR-0009](adr/0009-quickyn-yes-no.md).

## Usage

```bash
quickyn "is paris in france?"
# yes 0.91

quickyn "ship this change?" notes.md
# (stderr if notes.md > 1000 chars:)
# quickyn: notes.md is 4821 characters; this may not be a quick decision
# no 0.64
```

Quote the question when it contains spaces. The optional file is **chain-of-thought
context**: its body is placed in the user message *before* the question. The file
is not printed back on stdout.

## Contract

| Channel | Content |
|---------|---------|
| stdout | One line: `yes 0.91` or `no 0.42` (confidence in `0.00`–`1.00`) |
| stderr | Size warning; errors; unparseable raw model reply |
| exit `0` | Decision is **yes** |
| exit `1` | Decision is **no** |
| exit `2` | Usage error, missing file, LLM failure, or unparseable reply |

Files longer than **1000 characters** (`chars().count() > 1000`) warn on stderr;
the call still runs and the file is **not** truncated.

## Model / env

Uses the same OpenAI-compatible endpoint as the shell:

| Variable | Default | Role |
|----------|---------|------|
| `ENGRAM_LLM_URL` | `http://127.0.0.1:1234` | Base URL (no trailing `/v1`) |
| `ENGRAM_LLM_MODEL` | `default` | Model name |
| `QUICKYN_STUB_LLM` | unset | `1` / `true` / `yes` → canned decision (CI) |

Stub: **yes** unless the question or file body contains the substring `stub-no`.

## Scripting

```bash
if quickyn "are the tests green?" report.txt; then
  echo "proceed"
else
  echo "hold"
fi
```

## Agent mode

`quickyn` is a known direct-shell command in agent mode, so the line runs the
binary immediately instead of going to chat. See `:help`.

## Interactive (`:quickyn`)

Inside `engram-shell`, `:quickyn` opens a split-screen TUI over the same
`decide` path as the binary:

| Pane | Content |
|------|---------|
| Left | Question editor + optional file path |
| Right top | Text checkmark (yes) or X (no); `···` while waiting |
| Right bottom | `yes 0.91` / `no 0.42`, size warning, or error |

Keys: **Enter** ask · **Tab** question/file · **^O** browse · **^U** clear
focused field · **Esc** back (or cancel browse / in-flight ask).

Off a TTY (or if raw mode fails): prints
`quickyn: interactive only — use quickyn "question" [file]` and returns.

Also listed under `:menu` → System.

## Tests

- Unit: `cargo test -p quickyn`
- TUI state: `cargo test quickyn_tui`
- Smoke: `tests/cases/quickyn_smoke.sh` (stub LLM; skips if binary missing)
