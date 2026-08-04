#!/usr/bin/env bash
# Pick agent or hallucination mode, then launch the matching compose wrapper.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"

echo
echo "  Shomsky Linux"
echo "  ─────────────"
echo "  1) agent mode"
echo "  2) hallucination mode"
echo

while true; do
  read -r -p "  Select mode [1/2]: " choice
  case "$choice" in
    1|a|A|agent)
      exec bash "$ROOT/agent_sh.sh"
      ;;
    2|h|H|hallucination|halluc)
      exec bash "$ROOT/halluc_sh.sh"
      ;;
    *)
      echo "  Please enter 1 (agent) or 2 (hallucination)."
      ;;
  esac
done
