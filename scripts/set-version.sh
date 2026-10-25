#!/usr/bin/env bash
# Set the Shomsky Linux build version (ADR-0010).
#
# Usage:
#   ./scripts/set-version.sh              # suggest patch +1; type another X.Y.Z
#   ./scripts/set-version.sh 1.2.0        # set that version without a prompt
#   ./scripts/set-version.sh [--dry-run] [X.Y.Z]
#
# Updates repo-root VERSION and rootfs/etc/os-release VERSION_ID / PRETTY_NAME.
# Crate versions in Cargo.toml stay independent.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION_FILE="$ROOT/VERSION"
OS_RELEASE="$ROOT/rootfs/etc/os-release"
DRY_RUN=0
EXPLICIT=""

usage() {
  sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
}

die() { echo "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *)
      if [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        EXPLICIT="$1"
      else
        echo "Unknown argument: $1" >&2
        usage >&2
        exit 1
      fi
      ;;
  esac
  shift
done

read_semver() {
  local file="$1"
  [[ -f "$file" ]] || return 1
  tr -d '[:space:]' < "$file"
}

is_semver() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

# True when $1 is strictly greater than $2 (both MAJOR.MINOR.PATCH).
version_gt() {
  local a="$1" b="$2"
  [[ "$a" != "$b" ]] || return 1
  [[ "$(printf '%s\n%s\n' "$b" "$a" | sort -V | tail -n 1)" == "$a" ]]
}

bump_patch() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$1"
  echo "${major}.${minor}.$((patch + 1))"
}

os_field() {
  sed -n "s/^${1}=//p" "$OS_RELEASE" | tr -d '"'
}

current=""
if current="$(read_semver "$VERSION_FILE")" && is_semver "$current"; then
  :
else
  current=""
fi

id=""
pretty=""
if [[ -f "$OS_RELEASE" ]]; then
  id="$(os_field VERSION_ID)"
  pretty="$(os_field PRETTY_NAME)"
fi

if [[ -n "$current" ]]; then
  suggested="$(bump_patch "$current")"
else
  suggested="0.1.0"
fi

echo "Current VERSION:     ${current:-"(missing)"}"
if [[ -f "$OS_RELEASE" ]]; then
  echo "os-release ID:       ${id:-"(missing)"}"
  echo "os-release pretty:   ${pretty:-"(missing)"}"
  if [[ -n "$current" && ( "$id" != "$current" || "$pretty" != "Shomsky Linux ${current}" ) ]]; then
    echo "warning: os-release does not match VERSION (ADR-0010 lockstep)."
  fi
else
  echo "warning: missing ${OS_RELEASE}" >&2
fi
echo "Suggested next:      ${suggested}"

if [[ -n "$EXPLICIT" ]]; then
  version="$EXPLICIT"
elif [[ -t 0 ]]; then
  read -r -p "Next version [${suggested}]: " typed
  version="${typed:-$suggested}"
else
  die "no TTY; pass X.Y.Z as an argument (see --help)"
fi

version="${version#v}"
is_semver "$version" || die "VERSION must be MAJOR.MINOR.PATCH, got: ${version}"

if [[ -n "$current" && "$version" == "$current" && "$id" == "$current" && "$pretty" == "Shomsky Linux ${current}" ]]; then
  echo "Already at ${version}; nothing to update."
  exit 0
fi

if git rev-parse --verify --quiet origin/main >/dev/null; then
  if git cat-file -e origin/main:VERSION 2>/dev/null; then
    base="$(git show origin/main:VERSION | tr -d '[:space:]')"
    if is_semver "$base" && ! version_gt "$version" "$base"; then
      echo "warning: ${version} is not greater than origin/main (${base}); CI will reject a PR into main." >&2
    fi
  fi
fi

echo
echo "Will set:"
echo "  VERSION                        -> ${version}"
echo "  os-release VERSION_ID          -> ${version}"
echo "  os-release PRETTY_NAME         -> Shomsky Linux ${version}"

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "Dry run; files were not written."
  exit 0
fi

[[ -f "$OS_RELEASE" ]] || die "missing ${OS_RELEASE}"

printf '%s\n' "$version" > "$VERSION_FILE"

if grep -q '^VERSION_ID=' "$OS_RELEASE"; then
  sed -i "s/^VERSION_ID=.*/VERSION_ID=${version}/" "$OS_RELEASE"
else
  die "${OS_RELEASE} has no VERSION_ID= line"
fi
if grep -q '^PRETTY_NAME=' "$OS_RELEASE"; then
  sed -i "s/^PRETTY_NAME=.*/PRETTY_NAME=\"Shomsky Linux ${version}\"/" "$OS_RELEASE"
else
  die "${OS_RELEASE} has no PRETTY_NAME= line"
fi

echo "OK  VERSION=${version}"
echo "    ${VERSION_FILE}"
echo "    ${OS_RELEASE}"
