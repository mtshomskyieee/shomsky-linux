#!/usr/bin/env bash
# Pick agent or hallucination mode, then launch the matching compose wrapper.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/envfile.sh
source "$ROOT/scripts/envfile.sh"

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

# First launch asks. Later launches reuse .env unless SHOMSKY_CHOOSE_WORKDIR=1.
# A non-interactive start with no saved choice uses ./engram/user.
choose_workdir() {
  local env_file="$ROOT/.env"
  ensure_env_file "$env_file" "$ROOT/.env.example"

  local saved_dir saved_kind
  saved_dir="$(env_get SHOMSKY_USER_DIR "$env_file")"
  saved_kind="$(env_get SHOMSKY_WORKDIR_KIND "$env_file")"
  if [[ "${SHOMSKY_CHOOSE_WORKDIR:-}" != "1" && -n "$saved_dir" && -n "$saved_kind" ]]; then
    echo "  Using SHOMSKY_USER_DIR=${saved_dir} (${saved_kind})"
    echo "  Re-ask with SHOMSKY_CHOOSE_WORKDIR=1"
    echo
    return
  fi

  if [[ ! -t 0 ]]; then
    apply_workdir_choice 2 "$HOME" "$env_file"
    echo "  Non-interactive; using ./engram/user"
    echo
    return
  fi

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
  echo "  Saved SHOMSKY_USER_DIR=$(env_get SHOMSKY_USER_DIR "$env_file")"
  echo "  Saved SHOMSKY_WORKDIR_KIND=$(env_get SHOMSKY_WORKDIR_KIND "$env_file")"
  echo
}

# Use a local image. Pull only a registry reference (name contains /).
# Compile the image only when SHOMSKY_BUILD=1.
ensure_image() {
  local ref="${SHOMSKY_IMAGE:-shomskylinux:dev}"
  if docker image inspect "$ref" >/dev/null 2>&1; then
    echo "Using local image ${ref}"
    return
  fi
  case "$ref" in
    */*)
      echo "Pulling ${ref}"
      docker pull "$ref"
      return
      ;;
  esac
  if [[ "${SHOMSKY_BUILD:-}" == "1" ]]; then
    local image_name image_tag
    image_name="${ref%%:*}"
    image_tag="${ref#*:}"
    if [[ "$image_tag" == "$ref" ]]; then
      image_tag=latest
    fi
    echo "Building ${ref}"
    IMAGE="$image_name" TAG="$image_tag" bash "$ROOT/build.sh"
    return
  fi
  echo "Image ${ref} is not local." >&2
  echo "Pull a published image, for example:" >&2
  echo "  SHOMSKY_IMAGE=ghcr.io/mtshomskyieee/shomsky-linux:latest $0" >&2
  echo "Or build this tree with: SHOMSKY_BUILD=1 $0" >&2
  exit 1
}

main() {
# First run: no .env model yet. Later starts reuse SHOMSKY_MODEL from .env.
chosen_model=""
if [[ -f "$ROOT/.env" ]]; then
  chosen_model="$(env_get SHOMSKY_MODEL "$ROOT/.env")"
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

ensure_image

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
