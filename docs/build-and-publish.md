# Build and publish

## Prerequisites

- Docker 24+ with BuildKit
- ~20–40 minutes and several GB of RAM for the first mistral.rs compile
- Network access to crates.io, GitHub, and (at run time) Hugging Face

On small builders, lower parallelism:

```bash
export CARGO_BUILD_JOBS=1
```

## Run with Compose

```bash
# Interactive shell (recommended) — commits apk installs to shomskylinux:installed
./scripts/shell.sh
# or
./agent_sh.sh
```

Do **not** rely on bare `docker compose up` for the shell: Compose’s helper
menu (`w Enable Watch`) captures keystrokes. If you use `up`, disable it:

```bash
COMPOSE_MENU=false docker compose up --menu=false
```

Defaults come from [docker-compose.yml](../docker-compose.yml). Copy
[.env.example](../.env.example) to `.env` to change `SHOMSKY_MODEL`, mode,
tool style, or memory limits without editing the compose file.

Produces `shomskylinux:dev` and `shomskylinux:latest`.

## Build version

The product build number is the single line in [`VERSION`](../VERSION)
(`MAJOR.MINOR.PATCH`). Every pull request into `main` must increase it
(patch `+1` by default; minor or major when the change warrants it).
[`rootfs/etc/os-release`](../rootfs/etc/os-release) `VERSION_ID` and
`PRETTY_NAME` must match. The splash footer prints `Build:X.Y.Z`; `:sys` prints `version=X.Y.Z`.
See [ADR-0010](adr/0010-build-version.md).

CI job `version-bump` runs `./scripts/check-version-bump.sh` on pull
requests to `main`. `./scripts/update-container-registry.sh` publishes
that `VERSION` to GHCR and refuses if the tag already exists.

## Smoke test

```bash
./tests/run.sh
# or
./scripts/smoke.sh
```

## Pull published images

```bash
docker pull ghcr.io/mtshomskyieee/shomsky-linux:latest
docker pull ghcr.io/mtshomskyieee/shomsky-linux:0.1.0
```

See the [README quick start](../README.md#quick-start) for a full `docker run`
example. Package page:
https://github.com/mtshomskyieee/shomsky-linux/pkgs/container/shomsky-linux

## Publish to GHCR (canonical)

Published images live on **GitHub Container Registry** (not Docker Hub):

```text
ghcr.io/mtshomskyieee/shomsky-linux:0.1.0
ghcr.io/mtshomskyieee/shomsky-linux:latest
```

Tagged releases (`v*`) are built and pushed by
[`.github/workflows/release.yml`](../.github/workflows/release.yml). After the
first publish, set the package visibility to **Public** under GitHub →
Packages → `shomsky-linux` → Package settings.

Manual push (optional):

```bash
echo "$GITHUB_TOKEN" | docker login ghcr.io -u mtshomskyieee --password-stdin
GHCR_OWNER=mtshomskyieee ./scripts/publish.sh 0.1.0
```

GitHub Actions (see [`.github/workflows/`](../.github/workflows/)) also:

- **ci.yml** — build image, run root-mode + tiny-model smoke on PRs / main
- **release.yml** — on tag `v*`, push `ghcr.io/mtshomskyieee/shomsky-linux:{version,latest}`

## Useful overrides

| Variable | Default | Meaning |
|----------|---------|---------|
| `SHOMSKY_MODEL` | `mistral` | Model alias from `models.toml` |
| `ENGRAM_MODE` | `agent` | `agent` / `hallucination` / `root` |
| `SKIP_LLM` | `0` | Skip starting mistral.rs |
| `SKIP_MODEL_FETCH` | `0` | Skip download; requires `SHOMSKY_MODEL_PATH` |
| `MISTRALRS_EXTRA_ARGS` | empty | Extra flags appended to `mistralrs serve` |
| `CARGO_BUILD_JOBS` | `2` | Compile parallelism in the builder stage |
