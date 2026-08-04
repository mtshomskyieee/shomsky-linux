#!/usr/bin/env bash
# LLM tests — agent conversational replies + hallucination shell simulation.
# Requires a live mistralrs at ENGRAM_LLM_URL (default http://127.0.0.1:1234).
#
# Run via:
#   docker compose run --rm tests-llm
# or: RUN_LLM_TESTS=1 TEST_DRIVER=local ./tests/run.sh  (with LLM already up)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

if [ "${RUN_LLM_TESTS:-0}" != "1" ]; then
  echo "SKIP llm_chat (set RUN_LLM_TESTS=1 or use: docker compose run --rm tests-llm)"
  exit 0
fi

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
bad() { FAIL=$((FAIL + 1)); }

check_contains() {
  local out="$1" needle="$2" name="$3"
  if assert_contains "$out" "$needle" "$name"; then ok; else bad; fi
}
check_matches() {
  local out="$1" regex="$2" name="$3"
  if assert_matches "$out" "$regex" "$name"; then ok; else bad; fi
}
check_not() {
  local out="$1" needle="$2" name="$3"
  if assert_not_contains "$out" "$needle" "$name"; then ok; else bad; fi
}

export TEST_DRIVER=local
export ENGRAM_DIRECT_SHELL=1
export ENGRAM_TOOL_STYLE=text
export ENGRAM_HALLUC_RESUME=fresh
export ENGRAM_HALLUC_HISTORY_DIR="${ENGRAM_HALLUC_HISTORY_DIR:-/tmp/shomsky-halluc-history-$$}"

llm_once() {
  local mode="$1"
  local cmd="$2"
  ENGRAM_MODE="$mode" \
  ENGRAM_LLM_URL="${ENGRAM_LLM_URL:-http://127.0.0.1:1234}" \
  ENGRAM_DIRECT_SHELL=1 \
  ENGRAM_TOOL_STYLE=text \
    /usr/local/bin/engram-shell --once "$cmd" 2>&1
}

llm_script() {
  local mode="$1"
  local file="$2"
  ENGRAM_MODE="$mode" \
  ENGRAM_LLM_URL="${ENGRAM_LLM_URL:-http://127.0.0.1:1234}" \
  ENGRAM_DIRECT_SHELL=1 \
  ENGRAM_TOOL_STYLE=text \
    /usr/local/bin/engram-shell --script "$file" 2>&1
}

require_llm

echo "==> LLM agent conversational"
out="$(llm_once agent 'Reply with exactly this single token and nothing else: PONG')"
check_matches "$out" 'PONG|pong' "agent replies PONG"
check_not "$out" "llm error" "agent no llm error"
check_not "$out" "Connection refused" "agent connected"

out="$(llm_once agent 'What is 2+2? Reply with only the digit.')"
check_matches "$out" '[0-9]' "agent answers arithmetic"
check_not "$out" "llm error" "agent arithmetic no error"

out="$(llm_once agent 'In one short sentence, what is a moose?')"
check_matches "$out" '[Mm]oose|animal|deer|elk|mammal|Antler' "agent answers moose question"
check_not "$out" "llm error" "agent moose no error"
check_not "$out" "no real process" "agent moose not halluc stub"

echo "==> LLM hallucination shell simulation"
out="$(llm_once hallucination 'echo hello-halluc')"
check_contains "$out" "hello-halluc" "halluc echo prints text"
if assert_ascii_english "$out" "halluc echo ASCII"; then ok; else bad; fi
check_not "$out" "hallucination-mode" "halluc no stub banner"
check_not "$out" "no real process" "halluc no disabled warning"
check_not "$out" "Simulated successful" "halluc no simulated banner"
check_not "$out" "llm error" "halluc echo no llm error"

script="$(mktemp)"
trap 'rm -f "$script"' EXIT
cat >"$script" <<'EOF'
# Multi-turn virtual FS
echo hello > notes.txt
ls
cat notes.txt
EOF

out="$(llm_script hallucination "$script")"
check_matches "$out" 'notes\.txt|notes' "halluc ls shows virtual file after write"
check_contains "$out" "hello" "halluc cat returns virtual content"
if assert_ascii_english "$out" "halluc multi-turn ASCII"; then ok; else bad; fi
check_not "$out" "no real process" "halluc multi-turn no stub"
check_not "$out" "Simulated successful" "halluc multi-turn no simulated banner"
check_not "$out" "llm error" "halluc multi-turn no llm error"

# Disk isolation: redirect must not create a real file
marker="/tmp/should-not-touch-disk-$$.txt"
rm -f "$marker"
out="$(llm_once hallucination "echo x > $marker")"
if [ -f "$marker" ]; then
  echo "  FAIL  halluc echo-redirect left a real file"
  bad
else
  echo "  PASS  halluc echo-redirect did not touch real disk"
  ok
fi
check_not "$out" "no real process" "halluc redirect no stub warning"

echo
echo "==== LLM results: ${PASS} passed, ${FAIL} failed ===="
[ "$FAIL" -eq 0 ] || exit 1
echo "OK llm tests passed"
