#!/usr/bin/env bash
# Parameter / option selector for Shomsky Linux — run on the host BEFORE
# starting the shell. Choices are saved to .env and applied on the next
# ./agent_sh.sh / ./start-shomsky-linux-shell.sh (not typed at the REPL).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/envfile.sh
source "$ROOT/scripts/envfile.sh"
ENV_FILE="$ROOT/.env"
ENV_EXAMPLE="$ROOT/.env.example"

ensure_env_file() {
  if [[ ! -f "$ENV_FILE" ]]; then
    if [[ -f "$ENV_EXAMPLE" ]]; then
      cp "$ENV_EXAMPLE" "$ENV_FILE"
    else
      : > "$ENV_FILE"
    fi
  fi
}

# Show current .env value or a display default when unset.
show_val() {
  local key="$1" fallback="$2" cur
  cur="$(env_get "$key" "$ENV_FILE")"
  if [[ -n "$cur" ]]; then
    printf '%s' "$cur"
  else
    printf '%s' "$fallback"
  fi
}

# Prompt; Enter keeps current. Writes KEY=val, or removes KEY when val is empty
# and allow_empty=1 (meaning "use built-in default / unset").
ask() {
  local key="$1" prompt="$2" current="$3" allow_empty="${4:-0}"
  local reply
  read -r -p "  ${prompt} [${current}]: " reply
  if [[ -z "$reply" ]]; then
    reply="$current"
  fi
  if [[ -z "$reply" && "$allow_empty" == "1" ]]; then
    remove_env_key "$key" "$ENV_FILE"
  else
    set_env_key "$key" "$reply" "$ENV_FILE"
  fi
  printf '%s' "$reply"
}

ask_choice() {
  local key="$1" prompt="$2" current="$3"
  shift 3
  local opts=("$@") opt reply matched
  local listed
  listed="$(IFS='|'; echo "${opts[*]}")"
  while true; do
    read -r -p "  ${prompt} (${listed}) [${current}]: " reply
    if [[ -z "$reply" ]]; then
      reply="$current"
    fi
    matched=0
    for opt in "${opts[@]}"; do
      if [[ "$reply" == "$opt" ]]; then
        matched=1
        break
      fi
    done
    if [[ "$matched" == 1 ]]; then
      set_env_key "$key" "$reply" "$ENV_FILE"
      printf '%s' "$reply"
      return
    fi
    echo "  Please enter one of: ${listed}"
  done
}

ensure_env_file

echo
echo "  Shomsky Linux — parameter / option selector"
echo "  ───────────────────────────────────────────"
echo "  Run this BEFORE starting the shell."
echo "  It does not launch the shell; it only picks launch options and"
echo "  stores them in ${ENV_FILE} for the next start."
echo
echo "  After you finish here, start with:"
echo "    ./start-shomsky-linux-shell.sh"
echo "    or  ./agent_sh.sh"
echo "  (These are not REPL commands — do not type them inside engram-shell.)"
echo
echo "  Current:"
echo "    ENGRAM_MATRIX=$(show_val ENGRAM_MATRIX auto)"
echo "    ENGRAM_MATRIX_SECS=$(show_val ENGRAM_MATRIX_SECS 5)"
echo "    ENGRAM_MATRIX_GLYPHS=$(show_val ENGRAM_MATRIX_GLYPHS '(katakana)')"
echo "    ENGRAM_SPINNER=$(show_val ENGRAM_SPINNER auto)"
echo "    ENGRAM_LLM_READY_TRIES=$(show_val ENGRAM_LLM_READY_TRIES 600)"
echo "    NO_COLOR=$(show_val NO_COLOR '(colors on)')"
echo "    MISTRALRS_LOG_TO_TTY=$(show_val MISTRALRS_LOG_TO_TTY 0)"
echo

matrix="$(ask_choice ENGRAM_MATRIX "Matrix intro" "$(show_val ENGRAM_MATRIX auto)" auto always off)"
echo "  → ENGRAM_MATRIX=${matrix}"

while true; do
  secs_cur="$(show_val ENGRAM_MATRIX_SECS 5)"
  secs="$(ask ENGRAM_MATRIX_SECS "Matrix duration seconds (0-10; 0=skip)" "$secs_cur")"
  if [[ "$secs" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    # Clamp like the shell: 0–10
    secs="$(awk -v s="$secs" 'BEGIN {
      if (s < 0) s = 0
      if (s > 10) s = 10
      printf "%g", s
    }')"
    set_env_key ENGRAM_MATRIX_SECS "$secs" "$ENV_FILE"
    break
  fi
  echo "  Please enter a number from 0 to 10."
done
echo "  → ENGRAM_MATRIX_SECS=${secs}"

glyphs_cur="$(env_get ENGRAM_MATRIX_GLYPHS "$ENV_FILE")"
[[ -n "$glyphs_cur" ]] || glyphs_cur="katakana"
glyphs="$(ask_choice ENGRAM_MATRIX_GLYPHS "Matrix glyphs" "$glyphs_cur" katakana ascii)"
if [[ "$glyphs" == "katakana" ]]; then
  remove_env_key ENGRAM_MATRIX_GLYPHS "$ENV_FILE"
  echo "  → ENGRAM_MATRIX_GLYPHS=(default katakana)"
else
  echo "  → ENGRAM_MATRIX_GLYPHS=ascii"
fi

spinner="$(ask_choice ENGRAM_SPINNER "Spinner" "$(show_val ENGRAM_SPINNER auto)" auto always off)"
echo "  → ENGRAM_SPINNER=${spinner}"

while true; do
  tries_cur="$(show_val ENGRAM_LLM_READY_TRIES 600)"
  tries="$(ask ENGRAM_LLM_READY_TRIES "LLM ready wait (seconds)" "$tries_cur")"
  if [[ "$tries" =~ ^[0-9]+$ ]] && [[ "$tries" -ge 1 ]]; then
    set_env_key ENGRAM_LLM_READY_TRIES "$tries" "$ENV_FILE"
    break
  fi
  echo "  Please enter a positive integer."
done
echo "  → ENGRAM_LLM_READY_TRIES=${tries}"

color_cur="$(env_get NO_COLOR "$ENV_FILE")"
if [[ -n "$color_cur" ]]; then
  color_cur="off"
else
  color_cur="on"
fi
colors="$(ask_choice NO_COLOR "Splash/prompt colors" "$color_cur" on off)"
if [[ "$colors" == "off" ]]; then
  set_env_key NO_COLOR "1" "$ENV_FILE"
  echo "  → NO_COLOR=1"
else
  remove_env_key NO_COLOR "$ENV_FILE"
  echo "  → NO_COLOR=(unset; colors on)"
fi

log_cur="$(show_val MISTRALRS_LOG_TO_TTY 0)"
case "$log_cur" in
  1|true|yes|on) log_cur=1 ;;
  *) log_cur=0 ;;
esac
log_tty="$(ask_choice MISTRALRS_LOG_TO_TTY "Stream mistralrs logs to TTY" "$log_cur" 0 1)"
echo "  → MISTRALRS_LOG_TO_TTY=${log_tty}"

echo
echo "  Options saved to ${ENV_FILE}."
echo "  Now start the shell (still on the host):"
echo "    ./start-shomsky-linux-shell.sh"
echo "    or  ./agent_sh.sh"
echo
