#!/usr/bin/env bash
# Learn verb suite (stub LLM): intent → promote → publish; bats-fail gate.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

HELPER="$ROOT/tests/helpers/learn_full.sh"
chmod +x "$HELPER" 2>/dev/null || true

if [ "$TEST_DRIVER" = "local" ]; then
  if [ ! -x /usr/local/bin/engram-shell ] || [ ! -x /usr/local/bin/shomsky ]; then
    echo "SKIP learn suite (binaries not in /usr/local/bin)"
    exit 0
  fi
  export ENGRAM_SHELL=/usr/local/bin/engram-shell
  export SHOMSKY_BIN=/usr/local/bin/shomsky
  exec /bin/sh "$HELPER"
fi

if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
  echo "SKIP learn suite (image ${IMAGE} not present)"
  exit 0
fi

docker run --rm \
  -e SKIP_LLM=1 \
  -e SKIP_MODEL_FETCH=1 \
  -e SKIP_SHOMAGENT=1 \
  -e SHOMSKY_MODEL_PATH=/etc/os-release \
  -e ENGRAM_LEARN_STUB=1 \
  -e ENGRAM_LEARN_AUTO=1 \
  -e ENGRAM_SHELL=/usr/local/bin/engram-shell \
  -e SHOMSKY_BIN=/usr/local/bin/shomsky \
  -v "$HELPER:/tmp/learn_full.sh:ro" \
  "$IMAGE" \
  /bin/sh /tmp/learn_full.sh
