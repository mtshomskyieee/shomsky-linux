#!/usr/bin/env bash
# Propose the next build VERSION, confirm or override, then call set-version.sh.
#
# Usage:
#   ./scripts/set-next-version.sh
#   ./scripts/set-next-version.sh --dry-run
#
# Reads repo-root VERSION and proposes patch +1 (ADR-0010 default: the next
# minor revision). Asks whether that is correct. Type n to enter another
# X.Y.Z, or type a version at the confirm prompt to override immediately.
# Then runs ./scripts/set-version.sh so VERSION and os-release stay in lockstep.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SET_VERSION="$ROOT/scripts/set-version.sh"
VERSION_FILE="$ROOT/VERSION"

die() { echo "error: $*" >&2; exit 1; }

usage() {
  sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
}

is_semver() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

bump_patch() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$1"
  echo "${major}.${minor}.$((patch + 1))"
}

trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf '%s' "$s"
}

DRY_RUN=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=(--dry-run) ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
  shift
done

[[ -x "$SET_VERSION" || -f "$SET_VERSION" ]] || die "missing ${SET_VERSION}"
[[ -f "$VERSION_FILE" ]] || die "missing ${VERSION_FILE}"

current="$(tr -d '[:space:]' < "$VERSION_FILE")"
is_semver "$current" || die "VERSION must be MAJOR.MINOR.PATCH, got: ${current}"
proposed="$(bump_patch "$current")"

echo "Current VERSION:  ${current}"
echo "Proposed next:    ${proposed}"

read -r -p "Is ${proposed} correct? [Y/n or X.Y.Z] " answer \
  || die "no answer; run interactively or pipe Y / n / X.Y.Z"
answer="$(trim "${answer:-}")"
answer="${answer#v}"

version=""
if [[ -z "$answer" || "$answer" =~ ^[Yy]([Ee][Ss])?$ ]]; then
  version="$proposed"
elif [[ "$answer" =~ ^[Nn][Oo]?$ ]]; then
  read -r -p "Next version: " typed
  typed="$(trim "${typed:-}")"
  typed="${typed#v}"
  [[ -n "$typed" ]] || die "no version entered"
  version="$typed"
else
  version="$answer"
fi

is_semver "$version" || die "VERSION must be MAJOR.MINOR.PATCH, got: ${version}"

exec "$SET_VERSION" "${DRY_RUN[@]}" "$version"
