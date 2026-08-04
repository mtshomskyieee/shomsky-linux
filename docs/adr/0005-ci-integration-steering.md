# ADR-0005: CI integration steering

- **Status:** Accepted
- **Date:** 2026-08-02

## Context

Shomsky Linux is validated primarily as a **Docker image**: Rust builds happen
inside Alpine builder stages, and behavior is exercised by a bash harness and
optional tiny-model LLM checks. Contributors need a clear, cheap default CI
path and an explicit opt-in for slow LLM smoke.

Operational detail: [tests/README.md](../../tests/README.md),
[build-and-publish.md](../build-and-publish.md),
[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml),
[`.github/workflows/release.yml`](../../.github/workflows/release.yml).

## Decision

### Platform

- **GitHub Actions only** for first-party CI/CD (no GitLab/Forge/Jenkins as the
  product path).
- Two workflows:
  - **`ci`** — on push/PR to `main`: build image (`./scripts/build.sh`), run
    `docker compose run --rm tests` (non-LLM shell suite).
  - **`release`** — on tag `v*`: build and push to GHCR
    (`ghcr.io/mtshomskyieee/shomsky-linux` with semver + `latest` when appropriate).

### Default vs optional LLM

- Default CI **must not** require a full LLM chat path.
- Optional tiny-model smoke may run when repo variable `RUN_LLM_SMOKE == '1'`
  (see `ci.yml` / `./scripts/smoke.sh`).
- Local / Compose LLM suites use `tests-llm` or `RUN_LLM_TESTS=1` with
  `SHOMSKY_MODEL=tiny` and typically `ENGRAM_TOOL_STYLE=text`.

### Harness expectations

- Entry: `tests/run.sh` (and Compose `tests` / `tests-llm` services).
- Include `engram-shell --self-test` and `cases/*.sh`.
- Non-LLM coverage: agent/root BusyBox behavior (`shell_commands.sh`).
- LLM coverage (opt-in): chat checks + hallucination golden path
  (`llm_hallucination_golden.sh`).
- Prefer documenting timeouts / ready waits for CPU-only hosts rather than
  shortening golden expectations.

### Release pins

Release builds pin Alpine and mistral.rs refs via Dockerfile build-args (see
`release.yml`). Changing those pins is a release/engineering change; changing
*whether* CI builds the image or drops non-LLM tests requires superseding this
ADR.

## Steering

When touching CI or tests:

1. Keep PR/`main` green without LLM unless `RUN_LLM_SMOKE` is explicitly enabled.
2. Do not replace Compose/`tests/run.sh` with an undocumented parallel gate.
3. New shell behavior that can be checked without tokens should land in
   non-LLM cases or `--self-test`.
4. LLM assertions should tolerate CPU slowness; use `tiny` for automation.
5. Publish path remains tag-driven GHCR push; do not silently publish from every
   PR build.

## Consequences

- Contributors iterate quickly on shell/MCP changes via `tests` service.
- LLM flakiness and long cold starts stay off the critical PR path by default.
- Release quality still depends on image build success and tagged publish.