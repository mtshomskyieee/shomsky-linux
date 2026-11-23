#!/usr/bin/env bash
# SINE stub suite: non-TTY :sine / bare sine refuse without calling the LLM.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

HELPER="$ROOT/tests/helpers/sine_full.sh"
chmod +x "$HELPER" 2>/dev/null || true

if [ "$TEST_DRIVER" = "local" ]; then
  if [ ! -x /usr/local/bin/engram-shell ]; then
    echo "SKIP sine suite (engram-shell not in /usr/local/bin)"
    exit 0
  fi
  export ENGRAM_SHELL=/usr/local/bin/engram-shell
  exec /bin/sh "$HELPER"
fi

if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
  echo "SKIP sine suite (image ${IMAGE} not present)"
  exit 0
fi

docker run --rm \
  -e SKIP_LLM=1 \
  -e SKIP_MODEL_FETCH=1 \
  -e SKIP_SHOMAGENT=1 \
  -e ENGRAM_SKIP_PACKAGE_RESTORE=1 \
  -e SHOMSKY_MODEL_PATH=/etc/os-release \
  -e ENGRAM_SINE_STUB=1 \
  -e ENGRAM_SHELL=/usr/local/bin/engram-shell \
  -v "$HELPER:/tmp/sine_full.sh:ro" \
  "$IMAGE" \
  /bin/sh /tmp/sine_full.sh
