#!/bin/sh
# Shomsky Linux container entrypoint:
#   1. Resolve SHOMSKY_MODEL against /etc/engram/models.toml
#   2. Download GGUF into /engram/models if missing (optional sha256 check)
#   3. Start mistralrs serve on 127.0.0.1:1234
#   4. Wait for /v1/models health
#   5. Exec engram-shell (or a provided command)

set -eu

MODELS_TOML="${MODELS_TOML:-/etc/engram/models.toml}"
MODELS_DIR="${MODELS_DIR:-/engram/models}"
SKILLS_DIR="${SKILLS_DIR:-/engram/skills}"
LLM_HOST="${LLM_HOST:-127.0.0.1}"
LLM_PORT="${LLM_PORT:-1234}"
SHOMSKY_MODEL="${SHOMSKY_MODEL:-mistral}"
SKIP_MODEL_FETCH="${SKIP_MODEL_FETCH:-0}"
SKIP_LLM="${SKIP_LLM:-0}"
HF_BASE="${HF_BASE:-https://huggingface.co}"

log() { printf 'shomsky: %s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }

# Minimal TOML table reader for [models.<alias>] keys.
# Prints: repo|filename|sha256|description|tok_model_id
lookup_model() {
  alias="$1"
  file="$2"
  awk -v alias="$alias" '
    BEGIN { in_table=0; repo=""; filename=""; sha=""; desc=""; tok="" }
    /^\[models\./ {
      name=$0
      sub(/^\[models\./,"",name)
      sub(/\]$/,"",name)
      in_table = (name == alias)
      next
    }
    /^\[/ { in_table=0 }
    in_table && /^repo[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/"/,""); repo=$0
    }
    in_table && /^filename[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/"/,""); filename=$0
    }
    in_table && /^sha256[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/"/,""); sha=$0
    }
    in_table && /^description[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/"/,""); desc=$0
    }
    in_table && /^tok_model_id[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/"/,""); tok=$0
    }
    END {
      if (repo == "" || filename == "") exit 1
      printf "%s|%s|%s|%s|%s\n", repo, filename, sha, desc, tok
    }
  ' "$file"
}

HALLUC_HIST_DIR="${ENGRAM_HALLUC_HISTORY_DIR:-/engram/.hallucination_shell_history}"
USER_DIR="${ENGRAM_USER_DIR:-/engram/user}"
export ENGRAM_HALLUC_HISTORY_DIR="$HALLUC_HIST_DIR"
export ENGRAM_USER_DIR="$USER_DIR"
mkdir -p "$MODELS_DIR" "$SKILLS_DIR" "$HALLUC_HIST_DIR" "$USER_DIR" \
  "$USER_DIR/bin" "$USER_DIR/bin/tests" "$USER_DIR/bin/completions" \
  "$USER_DIR/bin/.learn" "$USER_DIR/bin/packages" \
  "$USER_DIR/share/man/man1"
# Learned commands on PATH; man pages under user MANPATH.
export PATH="$USER_DIR/bin:${PATH:-/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"
export MANPATH="$USER_DIR/share/man${MANPATH:+:$MANPATH}"
# Named/bind mounts often appear as root:root; keep writable for the shell user.
# A host-home bind (SHOMSKY_WORKDIR_KIND=home) keeps the host owner's files.
if [ -d /engram ]; then
  chown -R shomsky:shomsky /engram/models "$HALLUC_HIST_DIR" 2>/dev/null || true
  if [ "${SHOMSKY_WORKDIR_KIND:-engram}" != "home" ]; then
    chown -R shomsky:shomsky "$USER_DIR" 2>/dev/null || true
  fi
fi

if [ "$SKIP_MODEL_FETCH" != "1" ]; then
  [ -f "$MODELS_TOML" ] || die "missing models registry: $MODELS_TOML"
  info="$(lookup_model "$SHOMSKY_MODEL" "$MODELS_TOML")" \
    || die "unknown SHOMSKY_MODEL='$SHOMSKY_MODEL' (check $MODELS_TOML)"
  # repo|filename|sha256|description|tok_model_id
  repo=$(printf '%s' "$info" | cut -d'|' -f1)
  filename=$(printf '%s' "$info" | cut -d'|' -f2)
  sha256=$(printf '%s' "$info" | cut -d'|' -f3)
  desc=$(printf '%s' "$info" | cut -d'|' -f4)
  tok_model_id=$(printf '%s' "$info" | cut -d'|' -f5)

  dest="$MODELS_DIR/$filename"
  export SHOMSKY_MODEL_PATH="$dest"
  export SHOMSKY_MODEL
  # Optional tokenizer HF id when GGUF lacks an embedded tokenizer
  if [ -n "$tok_model_id" ]; then
    export TOK_MODEL_ID="$tok_model_id"
  fi

  if [ -f "$dest" ]; then
    log "model present: $dest"
  else
    url="$HF_BASE/$repo/resolve/main/$filename"
    log "downloading $SHOMSKY_MODEL — $desc"
    log "from $url"
    tmp="$dest.partial"
    if command -v curl >/dev/null 2>&1; then
      curl -fL --progress-bar -o "$tmp" "$url" || {
        rm -f "$tmp"
        die "download failed"
      }
    else
      wget -O "$tmp" "$url" || {
        rm -f "$tmp"
        die "download failed"
      }
    fi
    if [ -n "$sha256" ]; then
      log "verifying sha256..."
      echo "$sha256  $tmp" | sha256sum -c - || {
        rm -f "$tmp"
        die "sha256 mismatch"
      }
    fi
    mv "$tmp" "$dest"
    log "saved $dest"
  fi
else
  # Caller must set SHOMSKY_MODEL_PATH
  : "${SHOMSKY_MODEL_PATH:?SKIP_MODEL_FETCH=1 requires SHOMSKY_MODEL_PATH}"
  dest="$SHOMSKY_MODEL_PATH"
  filename="$(basename "$dest")"
  # If the file lives outside MODELS_DIR, point -m at its parent.
  MODELS_DIR="$(dirname "$dest")"
fi

cleanup() {
  if [ -n "${SHOMAGENT_PID:-}" ] && kill -0 "$SHOMAGENT_PID" 2>/dev/null; then
    log "stopping shomagentd (pid $SHOMAGENT_PID)"
    kill "$SHOMAGENT_PID" 2>/dev/null || true
    wait "$SHOMAGENT_PID" 2>/dev/null || true
  fi
  if [ -n "${LLM_PID:-}" ] && kill -0 "$LLM_PID" 2>/dev/null; then
    log "stopping mistralrs (pid $LLM_PID)"
    kill "$LLM_PID" 2>/dev/null || true
    wait "$LLM_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

if [ "$SKIP_LLM" != "1" ]; then
  command -v mistralrs >/dev/null 2>&1 || die "mistralrs binary not found in PATH"
  [ -f "$dest" ] || die "model file missing: $dest"

  log "starting mistralrs serve on ${LLM_HOST}:${LLM_PORT}"
  log "CPU-only: model load and first inference can take several minutes — be patient"
  MISTRALRS_LOG="${MISTRALRS_LOG:-/engram/mistralrs.log}"
  export MISTRALRS_LOG
  mkdir -p "$(dirname "$MISTRALRS_LOG")"
  # Keep the interactive TTY clean. Use :logs inside engram-shell, or:
  #   MISTRALRS_LOG_TO_TTY=1 docker compose run ...
  # shellcheck disable=SC2086
  if [ "${MISTRALRS_LOG_TO_TTY:-0}" = "1" ]; then
    RUST_LOG="${RUST_LOG:-info}" mistralrs serve \
      -m "$MODELS_DIR" \
      --format gguf \
      -f "$filename" \
      --cpu \
      --port "$LLM_PORT" \
      --host "$LLM_HOST" \
      --no-ui \
      --disable-access-log \
      ${TOK_MODEL_ID:+--tok-model-id "$TOK_MODEL_ID"} \
      ${MISTRALRS_EXTRA_ARGS:-} &
  else
    RUST_LOG="${RUST_LOG:-error}" mistralrs serve \
      -m "$MODELS_DIR" \
      --format gguf \
      -f "$filename" \
      --cpu \
      --port "$LLM_PORT" \
      --host "$LLM_HOST" \
      --no-ui \
      --disable-access-log \
      ${TOK_MODEL_ID:+--tok-model-id "$TOK_MODEL_ID"} \
      ${MISTRALRS_EXTRA_ARGS:-} \
      >>"$MISTRALRS_LOG" 2>&1 &
  fi
  LLM_PID=$!
  export ENGRAM_LLM_URL="http://${LLM_HOST}:${LLM_PORT}"
  log "mistralrs logs → $MISTRALRS_LOG  (type :logs in the shell)"

  # Interactive linux shell: skip the health wait so engram-shell can show the
  # splash + progress bar while the model warms. Non-interactive / scripted
  # runs still block here so --once / tests see a ready LLM.
  interactive_shell=0
  if [ "$#" -eq 0 ] && [ -t 1 ]; then
    interactive_shell=1
  fi

  if [ "$interactive_shell" -eq 1 ]; then
    log "LLM warming in background — splash UI will wait"
  else
    # Wait for health (CPU cold start is slow; default 10 minutes)
    url="http://${LLM_HOST}:${LLM_PORT}/v1/models"
    tries=0
    max_tries="${LLM_READY_TRIES:-600}"
    until curl -fsS "$url" >/dev/null 2>&1; do
      if ! kill -0 "$LLM_PID" 2>/dev/null; then
        die "mistralrs exited before becoming ready"
      fi
      tries=$((tries + 1))
      if [ "$tries" -ge "$max_tries" ]; then
        die "timed out waiting for LLM at $url after ${max_tries}s (CPU load can be slow; raise LLM_READY_TRIES)"
      fi
      if [ $((tries % 30)) -eq 0 ]; then
        log "still waiting for LLM at $url (${tries}s / ${max_tries}s)…"
      fi
      sleep 1
    done
    log "LLM ready at $url (first chat reply on CPU may still take a while)"
  fi
fi

# Prefer stub LLM only when there is no real inference server (CI / SKIP_LLM).
if [ -z "${SHOMAGENT_STUB_LLM:-}" ] && [ "${SKIP_LLM:-0}" = "1" ]; then
  export SHOMAGENT_STUB_LLM=1
fi
if [ -z "${ENGRAM_LEARN_STUB:-}" ] && [ "${SKIP_LLM:-0}" = "1" ]; then
  export ENGRAM_LEARN_STUB=1
fi

# Background crew supervisor (ring buffers + novelty-filtered findings).
SKIP_SHOMAGENT="${SKIP_SHOMAGENT:-0}"
if [ "$SKIP_SHOMAGENT" != "1" ] && command -v shomagentd >/dev/null 2>&1; then
  mkdir -p /run/shomagent /etc/shomsky/shomagent_crew.d
  export SHOMAGENT_RUN_DIR="${SHOMAGENT_RUN_DIR:-/run/shomagent}"
  log "starting shomagentd"
  shomagentd &
  SHOMAGENT_PID=$!
fi

# If the user passed a command, run it; otherwise start the linux shell.
if [ "$#" -gt 0 ]; then
  exec "$@"
fi

if [ -x /usr/local/bin/engram-shell ]; then
  exec /usr/local/bin/engram-shell
fi
if command -v engram-shell >/dev/null 2>&1; then
  exec engram-shell
fi
die "engram-shell not found"
