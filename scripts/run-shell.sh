#!/usr/bin/env bash
# Interactive Shomsky Linux session.
#
# apk add / apk del land in the container writable layer. This launcher keeps
# that layer by committing onto a local `shomskylinux:installed` image when the
# apk database changed. Tests keep `docker compose run --rm` and stay ephemeral.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# shellcheck source=scripts/envfile.sh
source "$ROOT/scripts/envfile.sh"

LIVE="${SHOMSKY_LIVE_CONTAINER:-shomsky-live}"
INSTALLED="${SHOMSKY_INSTALLED_IMAGE:-shomskylinux:installed}"

# Shell env overrides .env, matching Compose interpolation.
if [[ -z "${SHOMSKY_IMAGE:-}" && -f "$ROOT/.env" ]]; then
  SHOMSKY_IMAGE="$(env_get SHOMSKY_IMAGE "$ROOT/.env")"
fi
BASE_IMAGE="${SHOMSKY_IMAGE:-shomskylinux:dev}"

export COMPOSE_MENU="${COMPOSE_MENU:-false}"

image_id() {
  docker image inspect -f '{{.Id}}' "$1" 2>/dev/null || true
}

image_base_label() {
  docker image inspect -f '{{index .Config.Labels "shomsky.base-id"}}' "$1" 2>/dev/null || true
}

container_exists() {
  docker inspect "$1" >/dev/null 2>&1
}

container_running() {
  local s
  s="$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null || true)"
  [[ "$s" == "true" ]]
}

apk_db_changed() {
  docker diff "$1" 2>/dev/null | grep -Eq '^[ACD] /lib/apk/db(/|$)'
}

# Commit when apk changed; always remove the container unless commit fails.
commit_apk_changes() {
  local name="$1" dest="$2" base_id="$3"
  if ! container_exists "$name"; then
    return 0
  fi
  if ! apk_db_changed "$name"; then
    docker rm -f "$name" >/dev/null
    return 0
  fi
  echo "  Saving apk packages to ${dest}" >&2
  if [[ -z "$base_id" || "$base_id" == "<no value>" ]]; then
    base_id="$(docker inspect -f '{{.Image}}' "$name" 2>/dev/null || true)"
  fi
  if docker commit --change "LABEL shomsky.base-id=${base_id}" "$name" "$dest" >/dev/null; then
    docker rm -f "$name" >/dev/null
    return 0
  fi
  echo "warning: docker commit failed; leaving container ${name} for the next launch" >&2
  return 1
}

# Base id stored on a leftover container: its image's shomsky.base-id, or the
# image id itself when the session started from the base tag.
leftover_base_id() {
  local img_id label
  img_id="$(docker inspect -f '{{.Image}}' "$1" 2>/dev/null || true)"
  [[ -n "$img_id" ]] || return 0
  label="$(image_base_label "$img_id")"
  if [[ -n "$label" && "$label" != "<no value>" ]]; then
    printf '%s' "$label"
  else
    printf '%s' "$img_id"
  fi
}

if container_exists "$LIVE"; then
  if container_running "$LIVE"; then
    echo "error: container ${LIVE} is already running" >&2
    exit 1
  fi
  echo "  Committing leftover ${LIVE}" >&2
  commit_apk_changes "$LIVE" "$INSTALLED" "$(leftover_base_id "$LIVE")"
fi

# --build must tag the base image, never shomskylinux:installed.
compose_run_args="${SHOMSKY_COMPOSE_RUN_ARGS:-}"
if [[ " $compose_run_args " == *" --build "* ]]; then
  echo "  Building ${BASE_IMAGE}" >&2
  SHOMSKY_IMAGE="$BASE_IMAGE" docker compose build shomsky
  compose_run_args="${compose_run_args//--build/}"
fi

if [[ "$BASE_IMAGE" == "$INSTALLED" ]]; then
  BASE_ID="$(image_base_label "$INSTALLED")"
  if [[ -z "$BASE_ID" || "$BASE_ID" == "<no value>" ]]; then
    BASE_ID="$(image_id "$INSTALLED")"
  fi
  export SHOMSKY_IMAGE="$INSTALLED"
else
  BASE_ID="$(image_id "$BASE_IMAGE")"
  installed_base="$(image_base_label "$INSTALLED")"
  if [[ -n "$BASE_ID" && "$installed_base" == "$BASE_ID" ]]; then
    export SHOMSKY_IMAGE="$INSTALLED"
    echo "  Using ${INSTALLED} (packages from prior sessions)" >&2
  else
    export SHOMSKY_IMAGE="$BASE_IMAGE"
  fi
fi

CLEANED=0
cleanup() {
  if [[ "$CLEANED" == 1 ]]; then
    return
  fi
  CLEANED=1
  commit_apk_changes "$LIVE" "$INSTALLED" "$BASE_ID" || true
}
trap cleanup EXIT

status=0
# Remaining args are the command inside the container.
# shellcheck disable=SC2086
docker compose run --name "$LIVE" ${compose_run_args} shomsky "$@" || status=$?
exit "$status"
