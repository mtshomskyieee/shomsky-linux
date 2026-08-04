# ADR-0000: What is an ADR

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

Shomsky Linux is a coherent product (Alpine/musl container, local LLM, agent
linux shell, MCP, CI). Decisions about architecture, language, integrations,
and contribution policy need a durable place that is easy for humans and
AI-assisted contributors to load before changing code.

Long-form how-to docs (`docs/mcp.md`, `docs/models.md`, …) remain useful for
operations. They are not a substitute for recording *why* a constraint exists
and when it may change.

## Decision

We use **Architecture Decision Records (ADRs)** under [`docs/adr/`](./).

An ADR is a short, numbered markdown file that captures one significant
decision (or a tightly related pair of decisions) using this shape:

| Section | Purpose |
|---------|---------|
| **Status** | `Proposed`, `Accepted`, `Deprecated`, or `Superseded by ADR-NNNN` |
| **Date** | ISO date of acceptance (or last status change) |
| **Context** | Forces, constraints, and alternatives considered |
| **Decision** | What we chose, stated imperatively |
| **Consequences** | Trade-offs, follow-ons, and what becomes harder or easier |
| **Steering** (optional) | Rules for future PRs when the ADR is normative guidance |

### Numbering and filenames

- Zero-padded four-digit ids: `0000`, `0001`, …
- Filenames: `NNNN-kebab-case-title.md`
- Index: [`README.md`](README.md)

### When to write an ADR

Write or update an ADR when a change:

- Alters a cross-cutting boundary (sandbox, MCP transport, CI gates, license posture)
- Introduces a new integration or removes an Accepted one
- Would surprise a contributor who only read the code and how-to docs

Do **not** use ADRs for routine bugfixes, renames, or doc-only clarifications
that do not change a decision.

### Changing an Accepted ADR

1. Prefer a new ADR that **supersedes** the old one, and set the old Status to
   `Superseded by ADR-NNNN`.
2. Or amend the Accepted ADR in place only for factual corrections that do not
   change the decision.
3. Code that knowingly violates an Accepted ADR must land with the superseding
   ADR (or Status update) in the **same** change set. See
   [ADR-0006](0006-mit-as-is-and-ai-contributions.md).

### AI-assisted work

Before generating or editing project code with an AI tool, load at least this
index ([`README.md`](README.md)) and every Accepted ADR relevant to the change.
Full contribution policy (including AS-IS / MIT posture) is in
[ADR-0006](0006-mit-as-is-and-ai-contributions.md).

## Consequences

- Contributors have a single directory for normative decisions.
- How-to docs stay detailed; ADRs stay short and decisive.
- Reviewers can reject PRs that contradict Accepted ADRs without a supersession.