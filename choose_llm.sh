#!/usr/bin/env bash
# Pick a model alias from rootfs/etc/engram/models.toml and save it in .env.
# Weights are downloaded later by the container entrypoint.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
MODELS_TOML="$ROOT/rootfs/etc/engram/models.toml"
ENV_FILE="$ROOT/.env"
ENV_EXAMPLE="$ROOT/.env.example"

[ -f "$MODELS_TOML" ] || {
  echo "missing model registry: $MODELS_TOML" >&2
  exit 1
}

current=""
if [[ -f "$ENV_FILE" ]]; then
  current="$(awk -F= '/^SHOMSKY_MODEL=/{print $2; exit}' "$ENV_FILE")"
fi

# alias<TAB>description<TAB>size_hint, in file order.
mapfile_out="$(awk '
  /^\[models\./ {
    if (alias != "") print alias "\t" desc "\t" size
    alias=$0
    sub(/^\[models\./, "", alias)
    sub(/\]$/, "", alias)
    desc=""
    size=""
    next
  }
  /^\[/ { if (alias != "") { print alias "\t" desc "\t" size; alias="" } }
  alias != "" && /^description[[:space:]]*=/ {
    sub(/^[^=]*=[[:space:]]*/, "")
    gsub(/"/, "")
    desc=$0
  }
  alias != "" && /^size_hint[[:space:]]*=/ {
    sub(/^[^=]*=[[:space:]]*/, "")
    size=$0
  }
  END { if (alias != "") print alias "\t" desc "\t" size }
' "$MODELS_TOML")"

aliases=()
descs=()
sizes=()
while IFS=$'\t' read -r alias desc size; do
  [[ -n "$alias" ]] || continue
  aliases+=("$alias")
  descs+=("$desc")
  sizes+=("$size")
done <<< "$mapfile_out"

if [[ ${#aliases[@]} -eq 0 ]]; then
  echo "no models found in $MODELS_TOML" >&2
  exit 1
fi

human_size() {
  local bytes="${1:-0}"
  if [[ ! "$bytes" =~ ^[0-9]+$ ]] || [[ "$bytes" -eq 0 ]]; then
    printf '?'
    return
  fi
  if [[ "$bytes" -ge 1000000000 ]]; then
    awk -v b="$bytes" 'BEGIN { printf "%.1f GB", b/1000000000 }'
  else
    awk -v b="$bytes" 'BEGIN { printf "%.0f MB", b/1000000 }'
  fi
}

echo
echo "  Choose an LLM"
echo "  ─────────────"
if [[ -n "$current" ]]; then
  echo "  Current: $current"
  echo
fi

i=0
while [[ "$i" -lt ${#aliases[@]} ]]; do
  n=$((i + 1))
  mark=" "
  if [[ "${aliases[$i]}" == "$current" ]]; then
    mark="*"
  fi
  printf '  %s%2d) %-12s  %-8s  %s\n' \
    "$mark" "$n" "${aliases[$i]}" "$(human_size "${sizes[$i]}")" "${descs[$i]}"
  i=$((i + 1))
done
echo

while true; do
  read -r -p "  Select model [1-${#aliases[@]}]: " choice
  if [[ "$choice" =~ ^[0-9]+$ ]] && [[ "$choice" -ge 1 ]] && [[ "$choice" -le ${#aliases[@]} ]]; then
    break
  fi
  echo "  Please enter a number from 1 to ${#aliases[@]}."
done

selected="${aliases[$((choice - 1))]}"
if [[ "$selected" == "tiny" ]]; then
  tool_style="text"
else
  tool_style="auto"
fi

if [[ ! -f "$ENV_FILE" ]]; then
  if [[ -f "$ENV_EXAMPLE" ]]; then
    cp "$ENV_EXAMPLE" "$ENV_FILE"
  else
    : > "$ENV_FILE"
  fi
fi

set_env_key() {
  local key="$1" val="$2" file="$3" tmp
  tmp="$(mktemp)"
  if grep -q "^${key}=" "$file"; then
    awk -v k="$key" -v v="$val" '
      $0 ~ "^" k "=" { print k "=" v; next }
      { print }
    ' "$file" > "$tmp"
    mv "$tmp" "$file"
  else
    rm -f "$tmp"
    printf '%s=%s\n' "$key" "$val" >> "$file"
  fi
}

set_env_key SHOMSKY_MODEL "$selected" "$ENV_FILE"
set_env_key ENGRAM_TOOL_STYLE "$tool_style" "$ENV_FILE"

echo
echo "  Saved SHOMSKY_MODEL=${selected}"
echo "  Saved ENGRAM_TOOL_STYLE=${tool_style}"
echo "  File: ${ENV_FILE}"
echo "  The image downloads this GGUF on the next start."
echo
