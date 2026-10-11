#!/usr/bin/env bash
# Upsert KEY=VAL in a dotenv file. Values are double-quoted so spaces, #, $,
# and quotes survive Compose interpolation. Safe to source from tests.
set -euo pipefail

# Print a double-quoted dotenv value. `$` is written as `$$` for Compose.
env_quote() {
  local v="$1"
  v="${v//\\/\\\\}"
  v="${v//\"/\\\"}"
  v="${v//\$/\$\$}"
  printf '"%s"' "$v"
}

# Print the raw value of KEY from a dotenv file (first `=` only).
env_get() {
  local key="$1" file="$2" raw val
  [[ -f "$file" ]] || return 0
  raw="$(awk -v k="$key" 'index($0, k "=") == 1 { print substr($0, length(k) + 2); exit }' "$file")"
  val="$raw"
  if [[ ${#val} -ge 2 && "${val:0:1}" == '"' && "${val: -1}" == '"' ]]; then
    val="${val:1:${#val}-2}"
    val="${val//\\\\/\\}"
    val="${val//\\\"/\"}"
    val="${val//\$\$/\$}"
  fi
  printf '%s' "$val"
}

set_env_key() {
  local key="$1" val="$2" file="$3" tmp quoted
  quoted="$(env_quote "$val")"
  tmp="$(mktemp)"
  if grep -q "^${key}=" "$file"; then
    awk -v k="$key" -v v="$quoted" '
      index($0, k "=") == 1 { print k "=" v; next }
      { print }
    ' "$file" > "$tmp"
    mv "$tmp" "$file"
  else
    rm -f "$tmp"
    printf '%s=%s\n' "$key" "$quoted" >> "$file"
  fi
}
