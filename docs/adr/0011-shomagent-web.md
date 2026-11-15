# ADR-0011: shomagent web (headless browser members)

- **Status:** Accepted
- **Date:** 2026-11-15

## Context

[ADR-0007](0007-shomagent-background-crew.md) defines process and document
members. Users also need to open a URL, read the visible text, and act on
labels (click a link or button, type into a field) without leaving the
shomagent roster model.

Embedding a full browser automation stack (Playwright, chromiumoxide) would
bloat the Alpine/musl image and fight the Rust 1.74 pin. CI must not require a
real browser binary.

## Decision

- Add **`shomagent web <url>`**: attach a live **web** member. The daemon
  opens the URL in headless Chromium and keeps the browser for the member’s
  life. Roster state is `web`; glyph `◎`.
- Interaction is **text-label only**:
  - `ask` / `talk` — refresh the page snapshot, answer from that text
  - `click <id> <visible text>` — click a link or button by label
  - `type <id> <label> <text>` — fill a field by label / placeholder /
    accessible name (does not submit)
- **`shomagent web <url> --browser`** — same member, foreground loop (page
  text on screen; questions, `click`, `type`, `quit`).
- Only `http` and `https`. No `--persist`. Pause/resume freeze Chromium via
  SIGSTOP/SIGCONT when a process exists.
- Page text is untrusted data, fenced like document excerpts.
- Runtime image installs **`chromium`**. CDP is a small
  `tokio-tungstenite` client (no chromiumoxide).
- Tests set **`SHOMAGENT_STUB_WEB=1`** for an in-memory fixture page so CI
  does not need Chromium.

## Consequences

- Image size grows with Chromium.
- Root containers need Chromium `--no-sandbox`.
- Extends ADR-0007’s CLI surface; how-to lives in [shomagent.md](../shomagent.md).
