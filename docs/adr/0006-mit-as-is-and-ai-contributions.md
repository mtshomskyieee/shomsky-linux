# ADR-0006: MIT, use-at-your-own-risk, and AI-assisted contributions

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

Shomsky Linux original code is already under the MIT License. Agent-mode can
run real shell commands and write files inside the container sandbox
([ADR-0001](0001-alpine-llm-linux-shell.md)). The project also expects
AI-assisted patches. Contributors need an explicit posture on warranty and on
how AI tools must use the ADR set ([ADR-0000](0000-what-is-an-adr.md)).

SPDX / Alpine aggregate / NOTICE detail remains in
[docs/licensing.md](../licensing.md) and root `LICENSE` / `NOTICE`. This ADR
does not replace those files.

## Decision

### 1. MIT and use at your own risk

- Original Shomsky Linux source stays **MIT**. Do not invent a second license
  or warranty text that conflicts with MIT for that code.
- The software and container are provided **AS IS**, without warranty of any
  kind. Operators and users run it **at their own risk**.
- That risk includes agent-mode executing real commands and mutating the
  filesystem **inside** the configured sandbox (typically the container and
  bind-mounted paths). Safer modes (`hallucination`) and operator hardening
  (non-root, read-only rootfs, `allow_root_mode = false`) reduce but do not
  eliminate responsibility for how the image is run.
- Image aggregates (Alpine packages, BusyBox GPL-2.0, model licenses, etc.)
  remain governed by upstream terms documented in
  [docs/licensing.md](../licensing.md).

### 2. AI-assisted contributions

- AI-assisted contributions are **allowed and expected**.
- Before generating or editing project code with an AI tool, contributors
  **must** point the tool at [`docs/adr/`](./) — at least
  [`README.md`](README.md) and every Accepted ADR relevant to the change —
  and require adherence to those decisions.
- Human submitters remain responsible for correctness, security impact, and
  license compliance of AI-generated patches.
- Pull requests that knowingly violate an **Accepted** ADR must include a
  superseding ADR (or Status change) in the **same** change set
  ([ADR-0000](0000-what-is-an-adr.md)).

## Consequences

- Reviewers can cite this ADR for both “no warranty / own risk” and “AI must
  follow ADRs.”
- `docs/licensing.md` stays the legal inventory; this ADR stays the policy
  decision.
- No separate `CONTRIBUTING.md` is required for these rules (may be split
  later without changing the decisions here).