#!/usr/bin/env bash
# Thin wrapper — full suite lives in tests/run.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export IMAGE="${IMAGE:-shomskylinux:dev}"
exec "$ROOT/tests/run.sh"
