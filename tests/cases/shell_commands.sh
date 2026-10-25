#!/usr/bin/env bash
# Exhaustive simple-shell command tests against the Shomsky container.
# Requires IMAGE (default shomskylinux:dev). Does not need an LLM.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../lib.sh
source "$ROOT/tests/lib.sh"

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

run_mode_suite() {
  local mode="$1"
  echo "==> shell suite (mode=${mode})"

  local out

  out="$(run_once "$mode" 'pwd')"
  check_matches "$out" '/(home/shomsky|root|opt/shomsky)' "pwd ($mode)"

  out="$(run_once "$mode" 'uname')"
  check_contains "$out" "Linux" "uname ($mode)"

  out="$(run_once "$mode" 'uname -a')"
  check_contains "$out" "Linux" "uname -a ($mode)"

  out="$(run_once "$mode" 'echo hello-shomsky')"
  check_contains "$out" "hello-shomsky" "echo ($mode)"

  out="$(run_once "$mode" 'echo -n nonl')"
  check_contains "$out" "nonl" "echo -n ($mode)"

  out="$(run_once "$mode" 'ls /etc/engram')"
  check_contains "$out" "engram.toml" "ls /etc/engram ($mode)"
  check_contains "$out" "models.toml" "ls models.toml listed ($mode)"

  out="$(run_once "$mode" 'ls -la /etc/engram')"
  check_contains "$out" "engram.toml" "ls -la /etc/engram ($mode)"
  check_matches "$out" '\.' "ls -la shows entries ($mode)"

  out="$(run_once "$mode" 'ls -1 /etc/engram')"
  check_contains "$out" "engram.toml" "ls -1 ($mode)"

  out="$(run_once "$mode" 'cat /etc/os-release')"
  check_contains "$out" "ID=shomsky" "cat os-release ($mode)"
  check_contains "$out" "ID_LIKE=alpine" "cat ID_LIKE ($mode)"
  check_contains "$out" "NAME=\"Shomsky Linux\"" "cat NAME ($mode)"

  out="$(run_once "$mode" 'head -n 1 /etc/os-release')"
  check_contains "$out" "NAME=" "head ($mode)"

  out="$(run_once "$mode" 'tail -n 1 /etc/os-release')"
  check_matches "$out" '.+' "tail ($mode)"

  out="$(run_once "$mode" 'wc -l /etc/os-release')"
  check_matches "$out" '[0-9]+' "wc -l ($mode)"

  out="$(run_once "$mode" 'wc -c /etc/os-release')"
  check_matches "$out" '[0-9]+' "wc -c ($mode)"

  out="$(run_once "$mode" 'grep ID=shomsky /etc/os-release')"
  check_contains "$out" "ID=shomsky" "grep ($mode)"

  out="$(run_once "$mode" 'grep -c ID= /etc/os-release')"
  check_matches "$out" '^[0-9]+' "grep -c ($mode)"

  out="$(run_once "$mode" 'which busybox')"
  check_contains "$out" "busybox" "which busybox ($mode)"

  out="$(run_once "$mode" 'command -v busybox')"
  check_contains "$out" "busybox" "command -v ($mode)"

  out="$(run_once "$mode" 'test -f /etc/os-release && echo exists')"
  check_contains "$out" "exists" "test -f ($mode)"

  out="$(run_once "$mode" 'test -d /etc/engram && echo dir-ok')"
  check_contains "$out" "dir-ok" "test -d ($mode)"

  out="$(run_once "$mode" 'true && echo true-ok')"
  check_contains "$out" "true-ok" "true ($mode)"

  out="$(run_once "$mode" 'false || echo false-ok')"
  check_contains "$out" "false-ok" "false ($mode)"

  out="$(run_once "$mode" 'dirname /etc/engram/engram.toml')"
  check_contains "$out" "/etc/engram" "dirname ($mode)"

  out="$(run_once "$mode" 'basename /etc/engram/engram.toml')"
  check_contains "$out" "engram.toml" "basename ($mode)"

  out="$(run_once "$mode" 'echo a b c | wc -w')"
  check_matches "$out" '(^|[ ])3([ ]|$)' "pipeline wc -w ($mode)"

  out="$(run_once "$mode" 'echo hello | tr a-z A-Z')"
  check_contains "$out" "HELLO" "pipeline tr ($mode)"

  out="$(run_once "$mode" 'printf hello')"
  check_contains "$out" "hello" "printf ($mode)"

  out="$(run_once "$mode" 'busybox echo busybox-echo')"
  check_contains "$out" "busybox-echo" "busybox echo ($mode)"

  out="$(run_once "$mode" 'date +%Y')"
  check_matches "$out" '20[0-9]{2}' "date ($mode)"

  out="$(run_once "$mode" 'id')"
  check_matches "$out" 'uid=' "id ($mode)"

  out="$(run_once "$mode" 'hostname')"
  check_matches "$out" '.+' "hostname ($mode)"

  out="$(run_once "$mode" 'ls /no/such/path/zzz')"
  check_matches "$out" 'No such file|cannot|not found|No such|exit' "ls missing path ($mode)"

  # File create / read / copy / move / remove in one container
  out="$(run_seq "$mode" \
    'mkdir -p /tmp/shomsky-t/sub' \
    'echo hello-file > /tmp/shomsky-t/a.txt' \
    'cat /tmp/shomsky-t/a.txt' \
    'ls /tmp/shomsky-t' \
    'cp /tmp/shomsky-t/a.txt /tmp/shomsky-t/b.txt' \
    'cat /tmp/shomsky-t/b.txt' \
    'mv /tmp/shomsky-t/b.txt /tmp/shomsky-t/c.txt' \
    'ls /tmp/shomsky-t' \
    'touch /tmp/shomsky-t/touched' \
    'ls /tmp/shomsky-t' \
    'rm /tmp/shomsky-t/c.txt' \
    'test ! -e /tmp/shomsky-t/c.txt && echo rm-ok' \
    'find /tmp/shomsky-t -name a.txt' \
    'cat /tmp/shomsky-t/sub/../a.txt')"
  check_contains "$out" "hello-file" "redirect write + cat ($mode)"
  check_contains "$out" "a.txt" "ls after write ($mode)"
  check_contains "$out" "touched" "touch ($mode)"
  check_contains "$out" "rm-ok" "rm ($mode)"
  check_contains "$out" "a.txt" "find ($mode)"

  out="$(run_seq "$mode" \
    'mkdir -p /tmp/shomsky-perm' \
    'echo secret > /tmp/shomsky-perm/s.txt' \
    'chmod 600 /tmp/shomsky-perm/s.txt' \
    'ls -l /tmp/shomsky-perm/s.txt')"
  check_matches "$out" 'rw-|rwx' "chmod ($mode)"

  out="$(run_once "$mode" 'echo one && echo two')"
  check_contains "$out" "one" "&& first ($mode)"
  check_contains "$out" "two" "&& second ($mode)"

  out="$(run_once "$mode" 'echo aaa | sed s/a/b/')"
  check_contains "$out" "b" "sed ($mode)"

  out="$(run_once "$mode" 'echo -e line1\nline2 | sort')"
  # busybox echo -e may vary; also try printf
  out="$(run_once "$mode" "printf 'b\na\n' | sort")"
  check_contains "$out" "a" "sort ($mode)"

  out="$(run_once "$mode" "printf 'a\na\nb\n' | uniq")"
  check_contains "$out" "a" "uniq ($mode)"

  out="$(run_once "$mode" "printf 'x:y:z\n' | cut -d: -f2")"
  check_contains "$out" "y" "cut ($mode)"

  out="$(run_once "$mode" 'env | grep PATH')"
  check_contains "$out" "PATH" "env ($mode)"

  # Persistent cwd: cd then relative commands in one session
  out="$(run_seq "$mode" \
    'cd /etc' \
    'pwd' \
    'ls engram' \
    'cd /tmp' \
    'pwd')"
  check_matches "$out" '^/etc$' "cd /etc then pwd ($mode)"
  check_contains "$out" "engram.toml" "relative ls after cd ($mode)"
  check_matches "$out" '^/tmp$' "cd /tmp then pwd ($mode)"

  out="$(run_seq "$mode" \
    'cd /no/such/engram/path' \
    'pwd')"
  check_matches "$out" '^/(engram/user|home/shomsky|root|opt/shomsky)(/|$)' "failed cd keeps cwd ($mode)"
}

run_home_tilde_suite() {
  echo "==> cd ~ home by mode"
  local out

  out="$(run_seq agent \
    'cd /tmp' \
    'cd ~' \
    'pwd')"
  check_matches "$out" '^/engram/user$' "agent cd ~ → /engram/user"

  out="$(run_seq agent \
    'cd /tmp' \
    'cd' \
    'pwd')"
  check_matches "$out" '^/engram/user$' "agent bare cd → /engram/user"

  out="$(run_seq root \
    'cd /tmp' \
    'cd ~' \
    'pwd')"
  check_matches "$out" '^/root$' "root cd ~ → /root"

  out="$(run_seq root \
    'cd /tmp' \
    'cd' \
    'pwd')"
  check_matches "$out" '^/root$' "root bare cd → /root"

  # Workspace label must not change the real home path.
  out="$(SHOMSKY_WORKDIR_KIND=home run_seq agent \
    'cd /tmp' \
    'cd ~' \
    'pwd')"
  check_matches "$out" '^/engram/user$' "agent cd ~ stays /engram/user when kind=home"
}

run_ignore_llm_suite() {
  echo "==> ignore_llm interactive list"
  local out

  # Under --once there is no TTY: should refuse without hanging / involving LLM.
  out="$(run_once agent 'top')"
  check_contains "$out" "interactive TTY" "agent top needs TTY under --once"
  check_not "$out" "thinking" "agent top does not call LLM"

  out="$(run_once root 'mc')"
  check_contains "$out" "interactive TTY" "root mc needs TTY under --once"
}

run_hallucination_isolation() {
  echo "==> hallucination-mode (deferred to LLM suite)"
  echo "  SKIP  hallucination UX needs the LLM — run: docker compose run --rm tests-llm"
}

run_reinstall_missing() {
  local json="$1"
  shift
  local file out
  file="$(mktemp "${PWD}/.engram-test-pkgs.XXXXXX")"
  printf '%s\n' "$json" >"$file"
  if [ "$TEST_DRIVER" = "local" ]; then
    _engram_env
    out="$(env ENGRAM_PACKAGES_PATH="$file" "$@" /usr/local/bin/engram-shell --reinstall-missing 2>&1)" || true
  else
    local docker_e=()
    local kv
    for kv in "$@"; do
      docker_e+=(-e "$kv")
    done
    out="$(docker run --rm \
      -e SKIP_LLM=1 \
      -e SKIP_MODEL_FETCH=1 \
      -e SHOMSKY_MODEL_PATH=/etc/os-release \
      -e ENGRAM_PACKAGES_PATH=/tmp/packages.json \
      "${docker_e[@]}" \
      -v "$file:/tmp/packages.json:ro" \
      "$IMAGE" \
      /usr/local/bin/engram-shell --reinstall-missing 2>&1)" || true
  fi
  rm -f "$file"
  printf '%s\n' "$out"
}

run_package_restore_suite() {
  echo "==> missing-package restore"
  local out json

  json='{"packages":{"busybox":{"status":"installed","updated_at":"1"}},"updated_at":"1"}'
  out="$(run_reinstall_missing "$json")"
  check_not "$out" "reinstalling missing packages" "busybox already present is silent"

  json='{"packages":{"htop":{"status":"removed","updated_at":"1"}},"updated_at":"1"}'
  out="$(run_reinstall_missing "$json")"
  check_not "$out" "reinstalling missing packages" "removed packages are not reinstalled"

  json='{"packages":{"zz-shomsky-no-such-pkg":{"status":"installed","updated_at":"1"}},"updated_at":"1"}'
  out="$(run_reinstall_missing "$json")"
  check_contains "$out" "reinstalling missing packages: zz-shomsky-no-such-pkg" \
    "missing ledger package is reinstalled"
  check_contains "$out" "warning:" "failed apk add is a warning"

  json='{"packages":{"zz-shomsky-no-such-pkg":{"status":"installed","updated_at":"1"}},"updated_at":"1"}'
  out="$(run_reinstall_missing "$json" ENGRAM_SKIP_PACKAGE_RESTORE=1)"
  check_not "$out" "reinstalling missing packages" "ENGRAM_SKIP_PACKAGE_RESTORE skips restore"
}

run_agent_vs_root_parity() {
  echo "==> agent/root parity on read-only commands"
  local a r
  a="$(run_once agent 'cat /etc/os-release' | grep '^ID=')"
  r="$(run_once root 'cat /etc/os-release' | grep '^ID=')"
  if [ "$a" = "$r" ]; then
    echo "  PASS  agent/root ID= parity"
    ok
  else
    echo "  FAIL  agent/root ID= parity ($a vs $r)"
    bad
  fi
}

# --- exports for summary ---
shell_commands_pass() { echo "$PASS"; }
shell_commands_fail() { echo "$FAIL"; }

# When executed directly:
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "IMAGE=$IMAGE  TEST_DRIVER=$TEST_DRIVER"
  if [ "${TEST_DRIVER:-docker}" = "docker" ]; then
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
      echo "error: image '$IMAGE' not found — build first" >&2
      exit 2
    fi
  fi
  run_mode_suite agent
  run_mode_suite root
  run_home_tilde_suite
  run_ignore_llm_suite
  run_hallucination_isolation
  run_package_restore_suite
  run_agent_vs_root_parity
  echo
  echo "==== results: ${PASS} passed, ${FAIL} failed ===="
  [ "$FAIL" -eq 0 ] || exit 1
  echo "OK shell_commands passed"
fi
