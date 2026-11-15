#!/bin/sh
# Full shomagent verb coverage (stub LLM). Invoked by tests/cases/shomagent_smoke.sh
# (local driver) or docker-mounted as this file. Includes dream (piped).
set -eu

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok: $*"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL: $*" >&2; }

need() {
  # need <haystack> <needle_regex> <label>
  if printf '%s' "$1" | grep -Eq "$2"; then ok "$3"; else bad "$3 (got: $1)"; fi
}
need_not() {
  if printf '%s' "$1" | grep -Eq "$2"; then bad "$3 (unexpected in: $1)"; else ok "$3"; fi
}
expect_fail() {
  # expect_fail <label> -- command...
  label="$1"
  shift
  if "$@" >/tmp/shomagent-expect.out 2>&1; then
    bad "$label (expected failure): $(cat /tmp/shomagent-expect.out)"
  else
    ok "$label"
  fi
}

WORKDIR="$(mktemp -d /tmp/shomagent-work-XXXXXX)"
RUN_DIR="$(mktemp -d /tmp/shomagent-run-XXXXXX)"
export SHOMAGENT_RUN_DIR="$RUN_DIR"
export SHOMAGENT_STUB_LLM=1
export SHOMAGENT_STUB_WEB=1
export SHOMAGENT_SYSTEM_CREW="$RUN_DIR/empty-crew"
mkdir -p "$SHOMAGENT_SYSTEM_CREW"
cd "$WORKDIR"

SHOMAGENT="${SHOMAGENT:-shomagent}"
SHOMAGENTD="${SHOMAGENTD:-shomagentd}"

cleanup() {
  if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$WORKDIR" "$RUN_DIR"
}
trap cleanup EXIT

echo "==> shomagent full suite (stub LLM)"
"$SHOMAGENTD" &
DAEMON_PID=$!
sleep 0.5

# --- roster (empty) ---
out="$("$SHOMAGENT" 2>&1 || true)"
need "$out" "no shomagent" "roster empty"

# --- read (read-only document) ---
out="$("$SHOMAGENT" read details.md 2>&1)"
need "$out" "shomagent#1 read" "read attach"
[ -f details.md ] && ok "read created details.md" || bad "details.md missing"
out="$("$SHOMAGENT" 2>&1)"
need "$out" "details.md" "roster shows details.md"
need "$out" "◇" "roster read glyph"

# write must fail on read-only
expect_fail "write rejected on read member" \
  "$SHOMAGENT" write 1 "should not land"

# seed file and ask
printf '%s\n' "paris is in france" >> details.md
out="$("$SHOMAGENT" ask 1 "where is paris" 2>&1)"
need "$out" "paris is in france|from the document" "ask read-only document"

# --- readwrite + write + ask ---
out="$("$SHOMAGENT" readwrite notes.md 2>&1)"
need "$out" "shomagent#2 readwrite" "readwrite attach"
out="$("$SHOMAGENT" write 2 "france is in europe" 2>&1)"
need "$out" "wrote|appended" "write append"
grep -q "france is in europe" notes.md && ok "notes.md contains append" || bad "append missing"
out="$("$SHOMAGENT" write 2 "capital: paris" 2>&1)"
need "$out" "wrote|appended" "write second append"
out="$("$SHOMAGENT" ask 2 "where is france" 2>&1)"
need "$out" "france|from the document" "ask readwrite document"

# --replace
out="$("$SHOMAGENT" write 2 --replace "# notes
only this line
" 2>&1)"
need "$out" "replaced|wrote" "write --replace"
grep -q "only this line" notes.md && ok "replace content" || bad "replace content"
need_not "$(cat notes.md)" "france is in europe" "replace cleared old lines"

# --- talk ---
talk="$(printf '%s\n' 'where is paris' ':q' | "$SHOMAGENT" talk 2 2>&1 || true)"
need "$talk" "from the document|only this" "talk session"
need "$talk" "ctx [0-9]+/[0-9]+ tok" "talk context tokens and buffer"
need "$talk" "est [0-9]+%" "talk context percent"

# --- watch / spawn / pause / resume / stop (process) ---
out="$("$SHOMAGENT" spawn "sleep 120" --name sleeper --tell-me "nothing" 2>&1)"
need "$out" "shomagent#" "spawn sleep"
# find sleeper id from roster
roster="$("$SHOMAGENT" 2>&1)"
need "$roster" "sleeper" "roster sleeper"
# sleeper should be #3
out="$("$SHOMAGENT" pause 3 2>&1)"
need "$out" "paused" "pause #3"
roster="$("$SHOMAGENT" 2>&1)"
need "$roster" "⏸" "roster paused glyph"
out="$("$SHOMAGENT" resume 3 2>&1)"
need "$out" "resumed" "resume #3"
out="$("$SHOMAGENT" stop 3 2>&1)"
need "$out" "stopped" "stop process #3"

# watch + novelty (duplicate errors → few findings)
out="$("$SHOMAGENT" watch \
  'for i in 1 2; do echo "ERROR 500 on POST /api/orders pool exhausted"; sleep 0.2; done; sleep 6' \
  --tell-me "when a request 500s" --name err-watch 2>&1)"
need "$out" "shomagent#" "watch err-watch"
sleep 7
roster="$("$SHOMAGENT" 2>&1)"
need "$roster" "err-watch" "roster err-watch"
if [ -f "$RUN_DIR/notify" ]; then
  count="$(grep -c '"title"' "$RUN_DIR/notify" || true)"
  if [ "$count" -ge 1 ] && [ "$count" -le 3 ]; then
    ok "novelty findings=$count"
  else
    bad "novelty findings=$count"
  fi
else
  bad "notify lane missing"
fi

# --- persist drop-in ---
out="$("$SHOMAGENT" watch "true" --name persist-demo --tell-me "x" --persist 2>&1)"
need "$out" "shomagent#" "watch --persist"
[ -f "$WORKDIR/shomagent_crew.d/persist-demo.toml" ] && ok "persist toml" || bad "persist toml missing"

# --- stop document detaches, file kept ---
out="$("$SHOMAGENT" stop 1 2>&1)"
need "$out" "detached|stopped" "stop document #1"
[ -f details.md ] && ok "details.md kept after stop" || bad "details.md deleted"
roster="$("$SHOMAGENT" 2>&1)"
need_not "$roster" "details.md" "details.md gone from roster"

# --- unknown member ---
expect_fail "ask unknown member" "$SHOMAGENT" ask 99 "hi"
expect_fail "write unknown member" "$SHOMAGENT" write 99 "hi"

# --- dream (piped: no rain; stub LLM) ---
dream_out="$(printf 'quit\n' | "$SHOMAGENT" dream dream-test.md 2>&1 || true)"
need "$dream_out" "dream|seed|generators" "dream LLM-seed session"
[ -f dream-test.md ] && ok "dream-test.md created" || bad "dream-test.md missing"
need "$(cat dream-test.md)" "seed|generators|python" "dream file has seed"

dream_out="$(printf '%s\n' 'pipes as patience' 'quit' | "$SHOMAGENT" dream dream-steer.md 2>&1 || true)"
need "$dream_out" "pipes as patience|steered" "dream steered beat on terminal"
need "$(cat dream-steer.md)" "pipes as patience" "dream file contains steering"

# User-planted seed: pause for text, end with `.`, then quit before more beats.
dream_out="$(printf '%s\n' '**bash** — user planted doorway' 'a short note about patience' '.' 'quit' | "$SHOMAGENT" dream seed dream-plant.md 2>&1 || true)"
need "$dream_out" "user planted doorway|seed" "dream seed plant session"
[ -f dream-plant.md ] && ok "dream-plant.md created" || bad "dream-plant.md missing"
need "$(cat dream-plant.md)" "user planted doorway" "dream file has user seed"
need_not "$(cat dream-plant.md)" "generators as polite laziness" "user seed skipped LLM seed"

roster="$("$SHOMAGENT" 2>&1)"
need_not "$roster" "dream-test.md|dream-steer.md|dream-plant.md" "dream members ended after quit"

# --- refine idea (piped: overwrite; stub LLM) ---
rm -f refine-new.md refine-old.md refine-loop.md
# Quit before planting: file must not appear.
refine_out="$(printf 'quit\n' | "$SHOMAGENT" refine idea refine-new.md 2>&1 || true)"
need "$refine_out" "quit|leaving refine" "refine quit before task"
[ ! -f refine-new.md ] && ok "refine-new.md absent after early quit" || bad "refine-new.md created on early quit"

# Missing file: task + . + quit creates and contains the task.
refine_out="$(printf '%s\n' 'clarify the pitch' '.' 'quit' | "$SHOMAGENT" refine idea refine-new.md 2>&1 || true)"
need "$refine_out" "refine|clarify the pitch" "refine creates missing file session"
[ -f refine-new.md ] && ok "refine-new.md created" || bad "refine-new.md missing"
need "$(cat refine-new.md)" "clarify the pitch" "refine file contains task"
need "$(cat refine-new.md)" "^# refined" "refine stub header"

# Existing file: overwrite (OLD UNIQUE not first line; prior: preserved).
printf '%s\n' 'OLD UNIQUE' 'body line' > refine-old.md
refine_out="$(printf '%s\n' 'make it sharper' '.' 'quit' | "$SHOMAGENT" refine idea refine-old.md 2>&1 || true)"
need "$refine_out" "make it sharper|refined" "refine overwrite session"
need "$(cat refine-old.md)" "^# refined" "refine overwrote first line"
need "$(cat refine-old.md)" "prior: OLD UNIQUE" "refine read prior content"
need_not "$(head -n1 refine-old.md)" "OLD UNIQUE" "refine first line is not OLD UNIQUE"

# Two tasks then quit: file reflects the second task.
refine_out="$(printf '%s\n' 'first pass task' '.' 'second pass task' 'quit' | "$SHOMAGENT" refine idea refine-loop.md 2>&1 || true)"
need "$refine_out" "second pass task|refined" "refine two-pass session"
need "$(cat refine-loop.md)" "second pass task" "refine file has second task"

roster="$("$SHOMAGENT" 2>&1)"
need_not "$roster" "refine-new.md|refine-old.md|refine-loop.md" "refine members ended after quit"

# --- web (stub browser) ---
out="$("$SHOMAGENT" web https://example.com 2>&1)"
need "$out" "shomagent#[0-9]+ web" "web attach"
web_id="$(printf '%s' "$out" | sed -n 's/.*shomagent#\([0-9][0-9]*\) web.*/\1/p' | head -n1)"
[ -n "$web_id" ] || bad "web attach id missing"
roster="$("$SHOMAGENT" 2>&1)"
need "$roster" "example.com" "roster shows web url"
need "$roster" "◎" "roster web glyph"

out="$("$SHOMAGENT" ask "$web_id" "what domain" 2>&1)"
need "$out" "Example Domain|illustrative examples|domain" "ask web page"

out="$("$SHOMAGENT" navigate "$web_id" "https://news.example/latest" 2>&1)"
need "$out" "navigated|news.example" "navigate web member"
out="$("$SHOMAGENT" ask "$web_id" "news.example latest" 2>&1)"
need "$out" "news.example" "ask after navigate"

out="$("$SHOMAGENT" click "$web_id" "Learn more" 2>&1)"
need "$out" "clicked" "click Learn more"
out="$("$SHOMAGENT" ask "$web_id" "stub page" 2>&1)"
need "$out" "Stub page after clicking|Learn More|More information" "ask after click"

expect_fail "click unknown label" \
  "$SHOMAGENT" click "$web_id" "No Such Button"

out="$("$SHOMAGENT" type "$web_id" Search hello 2>&1)"
need "$out" "typed" "type into Search"
out="$("$SHOMAGENT" ask "$web_id" "Search field" 2>&1)"
need "$out" "hello|Search" "ask shows typed value"

expect_fail "type unknown field" \
  "$SHOMAGENT" type "$web_id" "NoSuchField" nope

# click/type rejected on a document member
out="$("$SHOMAGENT" read web-reject.md 2>&1)"
need "$out" "shomagent#" "read for web reject"
doc_id="$(printf '%s' "$out" | sed -n 's/.*shomagent#\([0-9][0-9]*\) read.*/\1/p' | head -n1)"
expect_fail "click rejected on read member" \
  "$SHOMAGENT" click "$doc_id" "Learn more"
expect_fail "type rejected on read member" \
  "$SHOMAGENT" type "$doc_id" Search hi
"$SHOMAGENT" stop "$doc_id" >/dev/null 2>&1 || true

"$SHOMAGENT" stop "$web_id" >/dev/null 2>&1 || true
roster="$("$SHOMAGENT" 2>&1)"
need_not "$roster" "example.com" "web member gone after stop"

# foreground --browser: question, click, quit
browser_out="$(printf '%s\n' 'what is the title' 'click Learn more' 'quit' | "$SHOMAGENT" web https://example.com --browser 2>&1 || true)"
need "$browser_out" "Example Domain|page|web" "browser session shows page"
need "$browser_out" "clicked|Learn more|quit" "browser click and quit"
roster="$("$SHOMAGENT" 2>&1)"
need_not "$roster" "example.com" "browser quit detached member"

echo
echo "shomagent full suite: ${PASS} passed, ${FAIL} failed"
[ "$FAIL" -eq 0 ]
