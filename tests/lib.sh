#!/usr/bin/env bash
# Shared helpers for Shomsky Linux tests.
#
# TEST_DRIVER=docker  — spawn containers via docker run (host default)
# TEST_DRIVER=local   — call engram-shell in this process (compose `tests` service)
#
# Auto: if /usr/local/bin/engram-shell exists and docker is unavailable → local
# shellcheck disable=SC2034

: "${IMAGE:=shomskylinux:dev}"

if [ -z "${TEST_DRIVER:-}" ]; then
  if [ -x /usr/local/bin/engram-shell ] && ! command -v docker >/dev/null 2>&1; then
    TEST_DRIVER=local
  elif [ -x /usr/local/bin/engram-shell ] && [ "${SHOMSKY_IN_CONTAINER:-0}" = "1" ]; then
    TEST_DRIVER=local
  else
    TEST_DRIVER=docker
  fi
fi
export TEST_DRIVER

_engram_env() {
  # Common env for every engram-shell invocation under test.
  export ENGRAM_DIRECT_SHELL=1
  export ENGRAM_TOOL_STYLE=text
  export ENGRAM_CONFIG="${ENGRAM_CONFIG:-/etc/engram/engram.toml}"
  export ENGRAM_SKILLS_DIR="${ENGRAM_SKILLS_DIR:-/engram/skills}"
  # Hallucination sessions: always fresh under a temp dir (no interactive prompt).
  export ENGRAM_HALLUC_RESUME="${ENGRAM_HALLUC_RESUME:-fresh}"
  export ENGRAM_HALLUC_HISTORY_DIR="${ENGRAM_HALLUC_HISTORY_DIR:-/tmp/shomsky-halluc-history}"
}

# Run a single engram-shell --once invocation. Prints stdout+stderr.
# Usage: run_once <mode> <command>
run_once() {
  local mode="$1"
  local cmd="$2"
  if [ "$TEST_DRIVER" = "local" ]; then
    _engram_env
    ENGRAM_MODE="$mode" /usr/local/bin/engram-shell --once "$cmd" 2>&1
  else
    docker run --rm \
      -e SKIP_LLM=1 \
      -e SKIP_MODEL_FETCH=1 \
      -e SHOMSKY_MODEL_PATH=/etc/os-release \
      -e "ENGRAM_MODE=${mode}" \
      -e ENGRAM_DIRECT_SHELL=1 \
      -e ENGRAM_TOOL_STYLE=text \
      "$IMAGE" \
      /usr/local/bin/engram-shell --once "$cmd" 2>&1
  fi
}

# Multi-command in one process (shared cwd / session state).
# Usage: run_seq mode 'cmd1' 'cmd2' ...
run_seq() {
  local mode="$1"
  shift
  local script
  # Colima shares the home directory, not macOS /var/folders temp paths.
  script="$(mktemp "${PWD}/.engram-test-script.XXXXXX")"
  local c
  for c in "$@"; do
    printf '%s\n' "$c" >>"$script"
  done
  if [ "$TEST_DRIVER" = "local" ]; then
    _engram_env
    ENGRAM_MODE="$mode" /usr/local/bin/engram-shell --script "$script" 2>&1
  else
    # shellcheck disable=SC2086
    docker run --rm \
      -e SKIP_LLM=1 \
      -e SKIP_MODEL_FETCH=1 \
      -e SHOMSKY_MODEL_PATH=/etc/os-release \
      -e "ENGRAM_MODE=${mode}" \
      -e ENGRAM_DIRECT_SHELL=1 \
      -e ENGRAM_TOOL_STYLE=text \
      ${SHOMSKY_WORKDIR_KIND:+-e SHOMSKY_WORKDIR_KIND=$SHOMSKY_WORKDIR_KIND} \
      -v "$script:/tmp/engram-test-script:ro" \
      "$IMAGE" \
      /usr/local/bin/engram-shell --script /tmp/engram-test-script 2>&1
  fi
  local st=$?
  rm -f "$script"
  return "$st"
}

# Hallucination write isolation: ensure shell_exec write does not touch disk.
run_halluc_isolation_raw() {
  if [ "$TEST_DRIVER" = "local" ]; then
    _engram_env
    local marker="/tmp/halluc-isolated-$$.txt"
    rm -f "$marker"
    local sim
    sim="$(ENGRAM_MODE=hallucination /usr/local/bin/engram-shell --once "echo should-not-persist > $marker" 2>&1)"
    printf '%s\n' "$sim"
    if [ -f "$marker" ]; then
      echo LEAKED
      cat "$marker" || true
    else
      echo ISOLATED-OK
    fi
  else
    docker run --rm \
      -e SKIP_LLM=1 \
      -e SKIP_MODEL_FETCH=1 \
      -e SHOMSKY_MODEL_PATH=/etc/os-release \
      -e ENGRAM_MODE=hallucination \
      -e ENGRAM_DIRECT_SHELL=1 \
      "$IMAGE" \
      /bin/sh -c '
        /usr/local/bin/engram-shell --once "echo should-not-persist > /tmp/halluc-isolated.txt"
        if [ -f /tmp/halluc-isolated.txt ]; then
          echo LEAKED
          cat /tmp/halluc-isolated.txt
          exit 1
        fi
        echo ISOLATED-OK
      ' 2>&1
  fi
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local name="$3"
  if printf '%s' "$haystack" | grep -F -- "$needle" >/dev/null; then
    echo "  PASS  $name"
    return 0
  fi
  echo "  FAIL  $name"
  echo "        expected to contain: $needle"
  echo "        got:"
  printf '%s\n' "$haystack" | sed 's/^/        | /'
  return 1
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local name="$3"
  if printf '%s' "$haystack" | grep -F -- "$needle" >/dev/null; then
    echo "  FAIL  $name"
    echo "        expected NOT to contain: $needle"
    echo "        got:"
    printf '%s\n' "$haystack" | sed 's/^/        | /'
    return 1
  fi
  echo "  PASS  $name"
  return 0
}

assert_matches() {
  local haystack="$1"
  local regex="$2"
  local name="$3"
  if printf '%s' "$haystack" | grep -E -- "$regex" >/dev/null; then
    echo "  PASS  $name"
    return 0
  fi
  echo "  FAIL  $name"
  echo "        expected to match: $regex"
  echo "        got:"
  printf '%s\n' "$haystack" | sed 's/^/        | /'
  return 1
}

# Fail if text contains non-ASCII (or UTF-8 junk). Allows empty;
# printable ASCII + tab/LF/CR only.
assert_ascii_english() {
  local haystack="$1"
  local name="$2"
  local leftover
  leftover="$(printf '%s' "$haystack" | LC_ALL=C tr -d '\11\12\15\40-\176')"
  if [ -n "$leftover" ] || printf '%s' "$haystack" | grep -F '�' >/dev/null 2>&1; then
    echo "  FAIL  $name (non-ASCII / unreadable terminal bytes)"
    echo "        got (hex dump of first 120 bytes):"
    printf '%s' "$haystack" | head -c 120 | od -An -tx1 | sed 's/^/        /'
    return 1
  fi
  echo "  PASS  $name (ASCII)"
  return 0
}

# Wait for mistralrs /v1/models. CPU-only hosts are slow on cold start.
# Override with LLM_READY_TRIES (seconds, default 600).
require_llm() {
  local url="${ENGRAM_LLM_URL:-http://127.0.0.1:1234}"
  local max="${LLM_READY_TRIES:-600}"
  local i=0
  echo "Waiting for LLM at $url (CPU cold start can take several minutes; limit ${max}s)…"
  while [ "$i" -lt "$max" ]; do
    if curl -fsS "$url/v1/models" >/dev/null 2>&1; then
      echo "LLM ready at $url (first completion may still be slow on CPU)"
      export ENGRAM_LLM_URL="$url"
      return 0
    fi
    i=$((i + 1))
    if [ $((i % 30)) -eq 0 ]; then
      echo "  … still waiting (${i}s / ${max}s)"
    fi
    sleep 1
  done
  echo "error: LLM not reachable at $url/v1/models after ${max}s" >&2
  echo "hint: raise LLM_READY_TRIES; check /engram/mistralrs.log" >&2
  exit 2
}

# Parse ENGRAM_SCRIPT_MARKERS stdout into a bash array (nameref).
# parse_script_turns_into ARRNAME "$raw"
parse_script_turns_into() {
  local -n _out="$1"
  local raw="$2"
  _out=()
  local turn="" in=0
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      '<<<ENGRAM_TURN '*)
        in=1
        turn=""
        ;;
      '<<<ENGRAM_END>>>')
        _out+=("$turn")
        in=0
        turn=""
        ;;
      *)
        if [ "$in" -eq 1 ]; then
          if [ -z "$turn" ]; then
            turn="$line"
          else
            turn+=$'\n'"$line"
          fi
        fi
        ;;
    esac
  done <<< "$raw"
}
