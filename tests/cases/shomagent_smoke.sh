#!/usr/bin/env bash
# Full shomagent verb suite (stub LLM): roster, read, readwrite, write, ask,
# talk, watch, spawn, pause, resume, stop, persist, novelty.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

HELPER="$ROOT/tests/helpers/shomagent_full.sh"
chmod +x "$HELPER" 2>/dev/null || true

if [ "$TEST_DRIVER" = "local" ]; then
  if [ ! -x /usr/local/bin/shomagentd ] || [ ! -x /usr/local/bin/shomagent ]; then
    echo "SKIP shomagent suite (binaries not in /usr/local/bin)"
    exit 0
  fi
  export SHOMAGENT=/usr/local/bin/shomagent
  export SHOMAGENTD=/usr/local/bin/shomagentd
  exec /bin/sh "$HELPER"
fi

# Host / docker driver
if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
  echo "SKIP shomagent suite (image ${IMAGE} not present)"
  exit 0
fi

docker run --rm \
  -e SKIP_LLM=1 \
  -e SKIP_MODEL_FETCH=1 \
  -e SKIP_SHOMAGENT=1 \
  -e SHOMSKY_MODEL_PATH=/etc/os-release \
  -e SHOMAGENT_STUB_LLM=1 \
  -e SHOMAGENT=/usr/local/bin/shomagent \
  -e SHOMAGENTD=/usr/local/bin/shomagentd \
  -v "$HELPER:/tmp/shomagent_full.sh:ro" \
  "$IMAGE" \
  /bin/sh /tmp/shomagent_full.sh
