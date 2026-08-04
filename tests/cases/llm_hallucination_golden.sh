#!/usr/bin/env bash
# Golden hallucination path — exact multi-turn virtual shell.
# Commands: ls -a, echo hi > hi.txt, ls -a, cat hi.txt
# Expected:  "", "", "hi.txt", "hi"
#
# Requires live LLM. Run via:
#   docker compose run --rm tests-llm
# or: RUN_LLM_TESTS=1 TEST_DRIVER=local ./tests/run.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

if [ "${RUN_LLM_TESTS:-0}" != "1" ]; then
  echo "SKIP llm_hallucination_golden (set RUN_LLM_TESTS=1 or use: docker compose run --rm tests-llm)"
  exit 0
fi

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
bad() { FAIL=$((FAIL + 1)); }

export TEST_DRIVER=local
export ENGRAM_DIRECT_SHELL=1
export ENGRAM_TOOL_STYLE=text
export ENGRAM_SCRIPT_MARKERS=1
export ENGRAM_HALLUC_RESUME=fresh
export ENGRAM_HALLUC_HISTORY_DIR="${ENGRAM_HALLUC_HISTORY_DIR:-/tmp/shomsky-halluc-history-$$}"

EXPECTED=("" "" "hi.txt" "hi")
CMDS=$'ls -a\necho hi > hi.txt\nls -a\ncat hi.txt\n'

run_golden_once() {
  local round="$1"
  local script raw turns
  script="$(mktemp)"
  printf '%s' "$CMDS" >"$script"

  # stdout only (markers); progress lines go to stderr
  raw="$(
    ENGRAM_MODE=hallucination \
    ENGRAM_LLM_URL="${ENGRAM_LLM_URL:-http://127.0.0.1:1234}" \
    ENGRAM_DIRECT_SHELL=1 \
    ENGRAM_TOOL_STYLE=text \
    ENGRAM_SCRIPT_MARKERS=1 \
    ENGRAM_HALLUC_RESUME=fresh \
    ENGRAM_HALLUC_HISTORY_DIR="${ENGRAM_HALLUC_HISTORY_DIR}" \
      /usr/local/bin/engram-shell --script "$script" 2>/dev/null
  )"
  rm -f "$script"

  if printf '%s' "$raw" | grep -F 'llm error' >/dev/null; then
    echo "  FAIL  round $round: llm error"
    printf '%s\n' "$raw" | sed 's/^/        | /'
    bad
    return 1
  fi
  if printf '%s' "$raw" | grep -E 'no real process|Simulated successful|hallucination-mode' >/dev/null; then
    echo "  FAIL  round $round: stub banner leaked"
    bad
    return 1
  fi

  turns=()
  parse_script_turns_into turns "$raw"

  if [ "${#turns[@]}" -ne 4 ]; then
    echo "  FAIL  round $round: expected 4 turns, got ${#turns[@]}"
    echo "        raw:"
    printf '%s\n' "$raw" | sed 's/^/        | /'
    bad
    return 1
  fi

  local i exp got
  for i in 0 1 2 3; do
    exp="${EXPECTED[$i]}"
    got="${turns[$i]}"
    # trim a single trailing newline if model added one and we already stripped via read
    got="${got%"$'\r'"}"

    if ! assert_ascii_english "$got" "round $round step $((i + 1)) ASCII"; then
      bad
      return 1
    fi
    ok

    if [ "$got" = "$exp" ]; then
      echo "  PASS  round $round step $((i + 1)): exact match $(printf %q "$exp")"
      ok
    else
      echo "  FAIL  round $round step $((i + 1)): expected $(printf %q "$exp"), got $(printf %q "$got")"
      bad
      return 1
    fi
  done

  if [ -f hi.txt ] || [ -f /root/hi.txt ] || [ -f /home/shomsky/hi.txt ]; then
    echo "  FAIL  round $round: hi.txt appeared on real disk"
    bad
    return 1
  fi
  echo "  PASS  round $round: no real hi.txt on disk"
  ok
  return 0
}

require_llm

echo "==> hallucination golden path (3 rounds)"
ROUNDS="${GOLDEN_ROUNDS:-3}"
r=1
while [ "$r" -le "$ROUNDS" ]; do
  echo "-- round $r/$ROUNDS"
  run_golden_once "$r" || true
  r=$((r + 1))
done

echo
echo "==== golden results: ${PASS} passed, ${FAIL} failed ===="
[ "$FAIL" -eq 0 ] || exit 1
echo "OK llm_hallucination_golden passed"
