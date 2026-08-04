#!/usr/bin/env bash
# Interactive Shomsky Linux session (proper TTY — use this instead of bare `up`).
set -euo pipefail
cd "$(dirname "$0")/.."
export COMPOSE_MENU="${COMPOSE_MENU:-false}"
exec docker compose run --rm --build shomsky "$@"
