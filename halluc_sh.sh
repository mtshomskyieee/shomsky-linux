#!/usr/bin/env bash
# Interactive hallucination-mode shell. apk changes are committed into the
# local shomskylinux:installed image (tests keep docker compose run --rm).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export ENGRAM_MODE=hallucination
exec "$ROOT/scripts/run-shell.sh" "$@"
