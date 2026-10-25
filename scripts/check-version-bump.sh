#!/usr/bin/env bash
# Enforce ADR-0010: VERSION is MAJOR.MINOR.PATCH, matches os-release, and is
# strictly greater than origin/main when that file exists.
#
# Env:
#   BASE_REF   git ref to compare against (default: origin/main)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BASE_REF="${BASE_REF:-origin/main}"
VERSION_FILE="$ROOT/VERSION"
OS_RELEASE="$ROOT/rootfs/etc/os-release"

die() { echo "error: $*" >&2; exit 1; }

[[ -f "$VERSION_FILE" ]] || die "missing ${VERSION_FILE}"
version="$(tr -d '[:space:]' < "$VERSION_FILE")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
  die "VERSION must be MAJOR.MINOR.PATCH, got: ${version}"

[[ -f "$OS_RELEASE" ]] || die "missing ${OS_RELEASE}"
id="$(sed -n 's/^VERSION_ID=//p' "$OS_RELEASE" | tr -d '"')"
pretty="$(sed -n 's/^PRETTY_NAME=//p' "$OS_RELEASE" | tr -d '"')"
[[ "$id" == "$version" ]] || \
  die "os-release VERSION_ID=${id} does not match VERSION=${version}"
[[ "$pretty" == "Shomsky Linux ${version}" ]] || \
  die "os-release PRETTY_NAME=${pretty} does not match Shomsky Linux ${version}"

ensure_base_ref() {
  if git rev-parse --verify --quiet "$BASE_REF" >/dev/null; then
    return 0
  fi
  local remote_name="${BASE_REF#origin/}"
  if [[ "$BASE_REF" == origin/* ]] && git fetch --no-tags --depth=1 origin "$remote_name" 2>/dev/null; then
    git rev-parse --verify --quiet "$BASE_REF" >/dev/null
  else
    return 1
  fi
}

base_version=""
if ensure_base_ref; then
  if git cat-file -e "${BASE_REF}:VERSION" 2>/dev/null; then
    base_version="$(git show "${BASE_REF}:VERSION" | tr -d '[:space:]')"
  fi
fi

if [[ -n "$base_version" ]]; then
  [[ "$base_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
    die "base VERSION is not MAJOR.MINOR.PATCH: ${base_version}"
  if [[ "$version" == "$base_version" ]]; then
    die "VERSION ${version} is not greater than ${BASE_REF} (${base_version})"
  fi
  last="$(printf '%s\n%s\n' "$base_version" "$version" | sort -V | tail -n 1)"
  [[ "$last" == "$version" ]] || \
    die "VERSION ${version} is not greater than ${BASE_REF} (${base_version})"
fi

echo "OK  VERSION=${version}"
