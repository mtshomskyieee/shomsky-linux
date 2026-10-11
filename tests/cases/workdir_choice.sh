#!/usr/bin/env bash
# Host-side check of the working-directory writer.
# No Docker. Skips when the launcher is not in this tree (image-only tests).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LAUNCHER="$ROOT/start-shomsky-linux-shell.sh"

if [[ ! -f "$LAUNCHER" ]]; then
  echo "skip: $LAUNCHER not in this tree"
  exit 0
fi

# shellcheck source=../../start-shomsky-linux-shell.sh
source "$LAUNCHER"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

home_file="$tmp/home.env"
: > "$home_file"
apply_workdir_choice 1 "/tmp/fake-home" "$home_file"
grep -qx 'SHOMSKY_USER_DIR="/tmp/fake-home"' "$home_file"
grep -qx 'SHOMSKY_WORKDIR_KIND="home"' "$home_file"
[[ "$(env_get SHOMSKY_USER_DIR "$home_file")" == "/tmp/fake-home" ]]

docker_file="$tmp/docker.env"
printf 'SHOMSKY_MODEL=tiny\n' > "$docker_file"
apply_workdir_choice 2 "/tmp/ignored" "$docker_file"
grep -qx 'SHOMSKY_MODEL=tiny' "$docker_file"
grep -qx 'SHOMSKY_USER_DIR="./engram/user"' "$docker_file"
grep -qx 'SHOMSKY_WORKDIR_KIND="engram"' "$docker_file"

# Second write replaces the keys and keeps the unrelated line.
apply_workdir_choice 1 "/Users/example" "$docker_file"
grep -qx 'SHOMSKY_MODEL=tiny' "$docker_file"
grep -qx 'SHOMSKY_USER_DIR="/Users/example"' "$docker_file"
grep -qx 'SHOMSKY_WORKDIR_KIND="home"' "$docker_file"

space_file="$tmp/space.env"
: > "$space_file"
apply_workdir_choice 1 "/Users/Ada Lovelace" "$space_file"
[[ "$(env_get SHOMSKY_USER_DIR "$space_file")" == "/Users/Ada Lovelace" ]]

if apply_workdir_choice 9 "/tmp/fake-home" "$home_file"; then
  echo "FAIL choice 9 should be rejected" >&2
  exit 1
fi

echo "OK workdir_choice"
