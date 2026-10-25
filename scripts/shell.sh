#!/usr/bin/env bash
# Interactive Shomsky Linux session (proper TTY — use this instead of bare `up`).
# apk changes are committed into the local shomskylinux:installed image
# (tests keep docker compose run --rm).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export COMPOSE_MENU="${COMPOSE_MENU:-false}"
export SHOMSKY_COMPOSE_RUN_ARGS="${SHOMSKY_COMPOSE_RUN_ARGS:---build}"
exec "$ROOT/scripts/run-shell.sh" "$@"
