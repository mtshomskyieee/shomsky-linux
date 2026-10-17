#!/bin/sh
# quickyn coverage (stub LLM). Invoked by tests/cases/quickyn_smoke.sh
set -eu

PASS=0
FAIL=0

ok() {
  PASS=$((PASS + 1))
  echo "  ok: $1"
}

bad() {
  FAIL=$((FAIL + 1))
  echo "  FAIL: $1"
}

need() {
  # need <haystack> <needle> <label>
  case "$1" in
    *"$2"*) ok "$3" ;;
    *) bad "$3 (missing '$2' in: $1)" ;;
  esac
}

QUICKYN="${QUICKYN:-quickyn}"
WORKDIR="$(mktemp -d /tmp/quickyn-work-XXXXXX)"
cd "$WORKDIR"
export QUICKYN_STUB_LLM=1

echo "==> quickyn suite (stub LLM)"

# Yes path
out="$("$QUICKYN" "is paris in france?" 2>&1)" || code=$?
code=${code:-0}
need "$out" "yes " "stub yes stdout"
[ "$code" = "0" ] && ok "exit 0 for yes" || bad "exit for yes was $code"

# No path via stub-no marker
set +e
out="$("$QUICKYN" "please stub-no this" 2>&1)"
code=$?
set -e
need "$out" "no " "stub-no stdout"
[ "$code" = "1" ] && ok "exit 1 for no" || bad "exit for no was $code"

# File context + size warning (>1000 characters)
awk 'BEGIN{for(i=0;i<1001;i++)printf "x"}' > big.txt
set +e
out="$("$QUICKYN" "is this large?" big.txt 2>&1)"
code=$?
set -e
need "$out" "may not be a quick decision" "size warning"
need "$out" "yes " "decision still printed with large file"
[ "$code" = "0" ] && ok "exit 0 with large file" || bad "exit with large file was $code"

# File containing stub-no
echo "context stub-no here" > ctx.txt
set +e
out="$("$QUICKYN" "anything" ctx.txt 2>&1)"
code=$?
set -e
need "$out" "no " "file stub-no"
[ "$code" = "1" ] && ok "exit 1 for file stub-no" || bad "exit for file stub-no was $code"

# Missing file → exit 2
set +e
out="$("$QUICKYN" "q" /no/such/file.txt 2>&1)"
code=$?
set -e
[ "$code" = "2" ] && ok "exit 2 for missing file" || bad "exit for missing file was $code"

echo "quickyn suite: ${PASS} passed, ${FAIL} failed"
rm -rf "$WORKDIR"
[ "$FAIL" -eq 0 ]
