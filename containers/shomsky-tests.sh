#!/bin/sh
# Entrypoint for `docker compose run tests`
set -eu
export SHOMSKY_IN_CONTAINER=1
export TEST_DRIVER=local
cd /opt/shomsky
# Prefer bash when present (Alpine community/main ships it when installed).
if command -v bash >/dev/null 2>&1; then
  exec bash ./tests/run.sh "$@"
fi
# Fallback: scripts are mostly bash — install bash if missing (dev/test only).
if command -v apk >/dev/null 2>&1; then
  apk add --no-cache bash >/dev/null
  exec bash ./tests/run.sh "$@"
fi
echo "error: bash is required to run tests" >&2
exit 2
