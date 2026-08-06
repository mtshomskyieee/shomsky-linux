#!/bin/sh
# Learn verb smoke (stub LLM): intent → promote → publish apk; bats-fail refuses install.
set -eu

PASS=0
FAIL=0

ok() { echo "  OK  $*"; PASS=$((PASS + 1)); }
bad() { echo "  FAIL $*"; FAIL=$((FAIL + 1)); }

need() {
  # need "$haystack" "$needle" "$label"
  case "$1" in
    *"$2"*) ok "$3" ;;
    *) bad "$3 (missing '$2' in: $1)" ;;
  esac
}

ENGRAM_SHELL="${ENGRAM_SHELL:-/usr/local/bin/engram-shell}"
SHOMSKY_BIN="${SHOMSKY_BIN:-/usr/local/bin/shomsky}"

if [ ! -x "$ENGRAM_SHELL" ]; then
  echo "SKIP learn suite (engram-shell missing)"
  exit 0
fi
if [ ! -x "$SHOMSKY_BIN" ]; then
  echo "SKIP learn suite (shomsky missing)"
  exit 0
fi
if ! command -v bats >/dev/null 2>&1; then
  echo "SKIP learn suite (bats not installed)"
  exit 0
fi

WORKDIR="$(mktemp -d /tmp/learn-smoke-XXXXXX)"
export ENGRAM_USER_DIR="$WORKDIR/user"
export HOME="$ENGRAM_USER_DIR"
export ENGRAM_LEARN_STUB=1
export ENGRAM_LEARN_AUTO=1
export ENGRAM_MODE=agent
export SKIP_LLM=1
export PATH="$ENGRAM_USER_DIR/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
mkdir -p "$ENGRAM_USER_DIR"

echo "==> learn smoke (stub + auto)"

out="$("$ENGRAM_SHELL" --once ', say hello from learn' 2>&1)" || true
echo "$out" | head -40
need "$out" "⟡ plan:" "shows plan"
need "$out" "hello-from-learn" "ran stub command"
need "$out" "✓ wrote" "promote installed"

# Suggested slug from "say hello from learn"
CMD_NAME=""
for cand in say-hello-from hello-from-learn learned-cmd say-hello; do
  if [ -x "$ENGRAM_USER_DIR/bin/$cand" ]; then
    CMD_NAME=$cand
    break
  fi
done
# Fall back: first executable in bin
if [ -z "$CMD_NAME" ]; then
  CMD_NAME="$(find "$ENGRAM_USER_DIR/bin" -maxdepth 1 -type f -executable 2>/dev/null | head -1 | xargs -n1 basename 2>/dev/null || true)"
fi
if [ -n "$CMD_NAME" ] && [ -x "$ENGRAM_USER_DIR/bin/$CMD_NAME" ]; then
  ok "binary $CMD_NAME present"
else
  bad "no learned binary under $ENGRAM_USER_DIR/bin (got '$CMD_NAME')"
  ls -la "$ENGRAM_USER_DIR/bin" 2>/dev/null || true
fi

if [ -n "$CMD_NAME" ]; then
  [ -f "$ENGRAM_USER_DIR/share/man/man1/$CMD_NAME.1" ] && ok "man page" || bad "man page missing"
  [ -f "$ENGRAM_USER_DIR/bin/tests/$CMD_NAME.bats" ] && ok "bats installed" || bad "bats missing"
  [ -f "$ENGRAM_USER_DIR/bin/.learn/$CMD_NAME.toml" ] && ok "registry" || bad "registry missing"
  run_out="$("$ENGRAM_USER_DIR/bin/$CMD_NAME" -h 2>&1)" || true
  need "$run_out" "usage:" "learned --help"

  pub_out="$("$SHOMSKY_BIN" publish "$CMD_NAME" --home "$ENGRAM_USER_DIR" 2>&1)" || true
  need "$pub_out" "✓ wrote" "shomsky publish"
  apk_path="$(echo "$pub_out" | sed -n 's/.*✓ wrote //p' | head -1)"
  if [ -n "$apk_path" ] && [ -f "$apk_path" ]; then
    ok "apk file exists"
    tar -tzf "$apk_path" 2>/dev/null | grep -q "usr/local/bin/$CMD_NAME" && ok "apk contains binary" \
      || bad "apk missing usr/local/bin/$CMD_NAME"
  else
    bad "apk path missing ($apk_path)"
  fi
fi

# BATS gate: failing tests must not install
fail_out="$("$ENGRAM_SHELL" --once ', FAILBATS broken promote' 2>&1)" || true
need "$fail_out" "not installing" "bats fail refuses install"
fail_slug="$(find "$ENGRAM_USER_DIR/bin" -maxdepth 1 -type f -name '*fail*' 2>/dev/null | head -1 || true)"
if [ -z "$fail_slug" ]; then
  ok "no broken binary installed"
else
  bad "unexpected binary from FAILBATS: $fail_slug"
fi

echo "learn smoke: ${PASS} passed, ${FAIL} failed"
rm -rf "$WORKDIR"
[ "$FAIL" -eq 0 ]
