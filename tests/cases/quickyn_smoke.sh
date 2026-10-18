#!/usr/bin/env bash
# quickyn stub-LLM suite: yes/no exits, size warning, missing file.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

HELPER="$ROOT/tests/helpers/quickyn_full.sh"
chmod +x "$HELPER" 2>/dev/null || true

if [ "$TEST_DRIVER" = "local" ]; then
  if [ ! -x /usr/local/bin/quickyn ]; then
    echo "SKIP quickyn suite (binary not in /usr/local/bin)"
    exit 0
  fi
  export QUICKYN=/usr/local/bin/quickyn
  exec /bin/sh "$HELPER"
fi

# Host / docker driver
if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
  echo "SKIP quickyn suite (image ${IMAGE} not present)"
  exit 0
fi

docker run --rm \
  -e SKIP_LLM=1 \
  -e SKIP_MODEL_FETCH=1 \
  -e SKIP_SHOMAGENT=1 \
  -e SHOMSKY_MODEL_PATH=/etc/os-release \
  -e QUICKYN_STUB_LLM=1 \
  -e QUICKYN=/usr/local/bin/quickyn \
  -v "$HELPER:/tmp/quickyn_full.sh:ro" \
  "$IMAGE" \
  /bin/sh /tmp/quickyn_full.sh
