#!/bin/sh
# SINE non-TTY / stub smoke (no model required).
set -eu

SHELL_BIN="${ENGRAM_SHELL:-/usr/local/bin/engram-shell}"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

echo "==> :sine refuses non-TTY"
out="$(ENGRAM_SINE_STUB=1 "$SHELL_BIN" --once ':sine' 2>&1 || true)"
echo "$out" | grep -q 'sine: interactive only' || fail "expected interactive-only for :sine; got: $out"

echo "==> bare sine refuses non-TTY"
out="$(ENGRAM_SINE_STUB=1 "$SHELL_BIN" --once 'sine' 2>&1 || true)"
echo "$out" | grep -q 'sine: interactive only' || fail "expected interactive-only for sine; got: $out"

echo "==> :sine with request still refuses non-TTY"
out="$(ENGRAM_SINE_STUB=1 "$SHELL_BIN" --once ':sine list agents' 2>&1 || true)"
echo "$out" | grep -q 'sine: interactive only' || fail "expected interactive-only for :sine request; got: $out"

echo "==> :sine does not call the LLM under stub"
out="$(ENGRAM_SINE_STUB=1 "$SHELL_BIN" --once ':sine' 2>&1 || true)"
echo "$out" | grep -qi 'llm error' && fail "unexpected llm error: $out"
echo "$out" | grep -qi 'connection refused' && fail "unexpected connect: $out"

echo "OK sine smoke"
