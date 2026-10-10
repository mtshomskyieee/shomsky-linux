#!/usr/bin/env bash
# Pick agent or hallucination mode, then launch the matching compose wrapper.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"

# Upsert KEY=VAL in an env file. Safe to source for tests.
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

# choice 1 = host home, choice 2 = container workspace ./engram/user.
apply_workdir_choice() {
  local choice="$1" home_dir="$2" env_file="$3"
  case "$choice" in
    1|h|H|home)
      set_env_key SHOMSKY_USER_DIR "$home_dir" "$env_file"
      set_env_key SHOMSKY_WORKDIR_KIND home "$env_file"
      ;;
    2|d|D|docker|engram)
      set_env_key SHOMSKY_USER_DIR "./engram/user" "$env_file"
      set_env_key SHOMSKY_WORKDIR_KIND engram "$env_file"
      ;;
    *)
      return 1
      ;;
  esac
}

ensure_env_file() {
  local env_file="$1" example="$2"
  if [[ ! -f "$env_file" ]]; then
    if [[ -f "$example" ]]; then
      cp "$example" "$env_file"
    else
      : > "$env_file"
    fi
  fi
}

# Every launch: home directory or the container engram workspace.
choose_workdir() {
  local env_file="$ROOT/.env"
  ensure_env_file "$env_file" "$ROOT/.env.example"

  echo
  echo "  Working directory"
  echo "  ─────────────────"
  echo "  1) home directory          ($HOME)"
  echo "  2) docker's home directory (./engram/user)"
  echo
  echo "  Hallucination mode ignores this and keeps its virtual filesystem."
  echo

  local choice
  while true; do
    read -r -p "  Select directory [1/2]: " choice
    if apply_workdir_choice "$choice" "$HOME" "$env_file"; then
      break
    fi
    echo "  Please enter 1 (home) or 2 (docker)."
  done
  echo
  echo "  Saved $(awk -F= '/^SHOMSKY_USER_DIR=/{print; exit}' "$env_file")"
  echo "  Saved $(awk -F= '/^SHOMSKY_WORKDIR_KIND=/{print; exit}' "$env_file")"
  echo
}

main() {
# First run: no .env model yet. Later starts reuse SHOMSKY_MODEL from .env.
chosen_model=""
if [[ -f "$ROOT/.env" ]]; then
  chosen_model="$(awk -F= '/^SHOMSKY_MODEL=/{print $2; exit}' "$ROOT/.env")"
fi
if [[ -z "$chosen_model" ]]; then
  bash "$ROOT/choose_llm.sh"
fi

# Colima provides the Docker daemon on macOS in this setup.
if [[ "$(uname -s)" == "Darwin" ]] && command -v colima >/dev/null 2>&1; then
  if ! colima status >/dev/null 2>&1; then
    echo "Colima is not running; starting it..."
    colima start
  fi
fi

# Use a local image when one exists. Otherwise pull, and build with ./build.sh
# if the registry does not have this image.
IMAGE_REF="${SHOMSKY_IMAGE:-shomskylinux:dev}"
if docker image inspect "$IMAGE_REF" >/dev/null 2>&1; then
  echo "Using local image ${IMAGE_REF}"
elif docker pull "$IMAGE_REF"; then
  echo "Pulled ${IMAGE_REF}"
else
  echo "Could not pull ${IMAGE_REF}; building it locally..."
  image_name="${IMAGE_REF%%:*}"
  image_tag="${IMAGE_REF#*:}"
  if [[ "$image_tag" == "$IMAGE_REF" ]]; then
    image_tag=latest
  fi
  IMAGE="$image_name" TAG="$image_tag" bash "$ROOT/build.sh"
fi

choose_workdir

echo
echo "  Shomsky Linux"
echo "  ─────────────"
echo "  1) agent mode"
echo "  2) hallucination mode"
echo

while true; do
  read -r -p "  Select mode [1/2]: " choice
  case "$choice" in
    1|a|A|agent)
      exec bash "$ROOT/agent_sh.sh"
      ;;
    2|h|H|hallucination|halluc)
      exec bash "$ROOT/halluc_sh.sh"
      ;;
    *)
      echo "  Please enter 1 (agent) or 2 (hallucination)."
      ;;
  esac
done
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
