# ADR-0003: Runtime integrations

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

The shell needs inference, a real userspace for agent-mode tools, persistent
model weights, and host-visible working trees for development — without baking
weights into the image or introducing cloud LLM vendors as a hard dependency.

Operational detail: [models](../models.md), [engram layout](../engram.md),
[build and publish](../build-and-publish.md), `.env.example`.

## Decision

### Local OpenAI-compatible LLM only

- Default inference is **in-container mistral.rs** on `127.0.0.1:1234`.
- `engram-shell` talks OpenAI-compatible HTTP (`/v1/models`,
  `/v1/chat/completions`) via `ENGRAM_LLM_URL` / `llm_url` in `engram.toml`.
- **No** first-party clients for cloud Anthropic/OpenAI/Ollama as the product
  path. Alternate OpenAI-compatible URLs are an extension point for operators,
  not a second supported product stack.

### Model weights

- Weights are **not** shipped in the image or git tree.
- Entrypoint resolves `SHOMSKY_MODEL` against `/etc/engram/models.toml`,
  downloads GGUF into `/engram/models` (named volume), then starts mistral.rs.
- Aliases include `mistral` (default), `qwen`, `deepseek`, and `tiny` (CI /
  smoke). Prefer `tiny` + `ENGRAM_TOOL_STYLE=text` for constrained hosts and
  automated LLM checks.

### OS tools and skills

- Agent-mode real execution uses **BusyBox ash** inside the image.
- Markdown skills under `/engram/skills` (YAML frontmatter; `always_load` vs
  on-demand `load_skill`) document tool behavior for the agent.

### Docker / Compose contract

- Flagship packaging is Docker (Compose services `shomsky`, `tests`,
  `tests-llm`).
- Persist models on volume `shomsky-models` → `/engram/models`.
- Bind-mount agent worktree (`/engram/user`) and hallucination session history
  for host visibility during development.
- Entrypoint may skip fetch/LLM (`SKIP_MODEL_FETCH`, `SKIP_LLM`) for tests that
  do not need inference.

### MCP and remote tools

- Extra tools beyond builtins go through **local stdio MCP** only
  ([ADR-0004](0004-mcp-integration-steering.md)). Do not add Compose-published
  MCP/HTTP tool backends in v1.

## Consequences

- First-run UX depends on network + Hugging Face (optional `HF_TOKEN`).
- CI and laptops with little RAM lean on `tiny` and non-LLM test paths.
- Changing the default inference stack or adding a mandatory cloud provider
  requires superseding this ADR.