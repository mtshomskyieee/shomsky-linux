# Tests

Regression tests for Shomsky Linux.

## Run (no LLM)

```bash
docker compose run --rm tests
```

Uses the `tests` service (same image, skips model download / mistralrs,
`TEST_DRIVER=local`).

On the host:

```bash
./tests/run.sh
```

## Run (with LLM)

CPU-only by default. Expect **slow cold starts**: model load can take several
minutes before `/v1/models` answers, and the **first chat completion** may take
another long stretch. Later turns are usually faster but still CPU-bound.
Override wait with `LLM_READY_TRIES` (seconds; default **600**).

Starts the tiny model via the normal entrypoint, then runs agent + hallucination
checks (including the golden path):

```bash
docker compose run --rm tests-llm
# optional: LLM_READY_TRIES=900 docker compose run --rm tests-llm
```

Or with an already-running mistralrs:

```bash
RUN_LLM_TESTS=1 TEST_DRIVER=local ./tests/run.sh
```

After editing `engram-shell` / the hallucination prompt, rebuild the fast image:

```bash
docker build -f containers/Dockerfile.shell -t shomskylinux:dev .
docker compose run --rm tests-llm
```

## Layout

| Path | Role |
|------|------|
| `run.sh` | Entry point — self-test + all `cases/*.sh` |
| `lib.sh` | `run_once` / `run_seq` / assert helpers (incl. ASCII + script markers) |
| `cases/shell_commands.sh` | Exhaustive simple shell checks (agent/root, no LLM) |
| `cases/shomagent_smoke.sh` | Full shomagent verb suite via `helpers/shomagent_full.sh` (stub LLM) |
| `helpers/shomagent_full.sh` | roster, read, readwrite, write, ask, talk, watch, spawn, pause/resume/stop, persist, novelty |
| `cases/learn_smoke.sh` | Learn `,` intent → promote ~/bin → `shomsky publish` apk (stub LLM) |
| `helpers/learn_full.sh` | plan/run/save, bats-fail gate, apk contents |
| `cases/llm_chat.sh` | Agent chat + loose hallucination checks (`RUN_LLM_TESTS=1`) |
| `cases/llm_hallucination_golden.sh` | Exact 4-step golden path ×3 (`RUN_LLM_TESTS=1`) |

### Golden path

`ls -a` → `echo hi > hi.txt` → `ls -a` → `cat hi.txt` must yield exactly
`""`, `""`, `hi.txt`, `hi`, ASCII-only, no real disk writes (3×).

Hallucination builtins are implemented by an in-memory virtual FS
(`src/halluc_fs.rs`) so this path stays repeatable; unknown commands still
go to the LLM under the hallucination system prompt.
