# Models

Model weights are **not** baked into the image. On first `docker run`, the
entrypoint resolves `SHOMSKY_MODEL` against `/etc/engram/models.toml`,
downloads the GGUF into `/engram/models`, optionally verifies `sha256`, and
starts `mistralrs serve`.

## Aliases

| Alias | Approx size | Notes |
|-------|-------------|-------|
| `mistral` (default) | ~4.4 GB | Mistral 7B Instruct v0.2 Q4_K_M |
| `qwen` | ~4.7 GB | Qwen2.5 7B Instruct Q4_K_M |
| `deepseek` | ~4.7 GB | DeepSeek-R1-Distill-Qwen-7B Q4_K_M |
| `tiny` | ~400 MB | Qwen2.5 0.5B Instruct — CI / smoke |

```bash
docker pull ghcr.io/mtshomskyieee/shomsky-linux:latest
docker run -it --rm \
  -v shomsky-models:/engram/models \
  -e SHOMSKY_MODEL=tiny \
  ghcr.io/mtshomskyieee/shomsky-linux:latest
```

## Registry format

See [`rootfs/etc/engram/models.toml`](../rootfs/etc/engram/models.toml):

```toml
[models.tiny]
description = "…"
repo = "Qwen/Qwen2.5-0.5B-Instruct-GGUF"
filename = "qwen2.5-0.5b-instruct-q4_k_m.gguf"
sha256 = ""          # optional pin
tok_model_id = ""    # optional HF tokenizer id
license = "Apache-2.0"
size_hint = 400000000
```

## Serving

```text
mistralrs serve -m /engram/models --format gguf -f <filename> --cpu \
  --host 127.0.0.1 --port 1234 --no-ui
```

`engram-shell` then uses `http://127.0.0.1:1234/v1`.

## Memory guidance (CPU)

| Model | Suggested host RAM |
|-------|--------------------|
| `tiny` | ≥ 2 GB |
| 7B Q4_K_M | ≥ 8 GB |

This builder host has limited free RAM; prefer `tiny` for local smoke tests.

### Timing (CPU-only)

Expect long waits on a cold start:

1. **Model load** — mistralrs may take several minutes before `GET /v1/models` succeeds (`LLM_READY_TRIES`, default 600s).
2. **First completion** — the first `/v1/chat/completions` after ready can also take a long time on CPU; subsequent calls are usually quicker but still far from GPU latency.

`engram-shell` uses a 900s HTTP client timeout for chat requests for this reason.

## GPU track

CUDA cannot target Alpine/musl. See
[`containers/Dockerfile.cuda`](../containers/Dockerfile.cuda) for a separate
glibc + NVIDIA image that reuses `engram-shell` and `/engram` skills. That
image is explicitly **not** Shomsky-on-Alpine; it is a companion runtime.
