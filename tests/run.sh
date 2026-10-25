#!/usr/bin/env bash
# Run all Shomsky Linux tests.
#
# Host:
#   ./tests/run.sh
#   IMAGE=shomskylinux:dev ./tests/run.sh
#
# Compose (in-container, no nested Docker):
#   docker compose run --rm tests
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export IMAGE="${IMAGE:-shomskylinux:dev}"

# Prefer local driver when already inside the image / compose tests service.
if [ -z "${TEST_DRIVER:-}" ]; then
  if [ "${SHOMSKY_IN_CONTAINER:-0}" = "1" ] || { [ -x /usr/local/bin/engram-shell ] && [ ! -S /var/run/docker.sock ] && [ ! -S "${DOCKER_HOST#unix://}" ]; }; then
    # Heuristic: engram-shell present and no obvious docker socket → local
    if [ -x /usr/local/bin/engram-shell ] && ! docker info >/dev/null 2>&1; then
      export TEST_DRIVER=local
    fi
  fi
fi
# Explicit compose flag
if [ "${SHOMSKY_IN_CONTAINER:-0}" = "1" ]; then
  export TEST_DRIVER=local
fi

# shellcheck source=lib.sh
source "$ROOT/tests/lib.sh"

echo "========================================"
echo " Shomsky Linux tests"
echo " IMAGE=$IMAGE"
echo " TEST_DRIVER=$TEST_DRIVER"
echo "========================================"

if [ "$TEST_DRIVER" = "docker" ]; then
  if ! command -v docker >/dev/null; then
    echo "error: docker not found (or set TEST_DRIVER=local)" >&2
    exit 2
  fi
  if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "error: image '$IMAGE' not found" >&2
    echo "  build with: docker compose build" >&2
    exit 2
  fi
fi

# 1) Built-in binary self-test
echo
echo "==> engram-shell --self-test"
if [ "$TEST_DRIVER" = "local" ]; then
  /usr/local/bin/engram-shell --self-test | tee /tmp/shomsky-selftest.out
else
  docker run --rm \
    -e SKIP_LLM=1 \
    -e SKIP_MODEL_FETCH=1 \
    -e SHOMSKY_MODEL_PATH=/etc/os-release \
    -e ENGRAM_SKIP_PACKAGE_RESTORE=1 \
    "$IMAGE" \
    /usr/local/bin/engram-shell --self-test | tee /tmp/shomsky-selftest.out
fi
grep -q 'self-test passed' /tmp/shomsky-selftest.out

# 2) Case suites
FAILED=0
for case in "$ROOT"/tests/cases/*.sh; do
  [ -f "$case" ] || continue
  echo
  echo "======== case: $(basename "$case") ========"
  if bash "$case"; then
    echo "OK $(basename "$case")"
  else
    echo "FAIL $(basename "$case")"
    FAILED=1
  fi
done

echo
if [ "$FAILED" -ne 0 ]; then
  echo "==== TESTS FAILED ===="
  exit 1
fi
echo "==== ALL TESTS PASSED ===="
