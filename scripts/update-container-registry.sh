#!/usr/bin/env bash
# Build the next Shomsky Linux image and push it to GHCR.
#
# Usage:
#   ./scripts/update-container-registry.sh [patch|minor|major|X.Y.Z] [--dry-run]
#
# Reads the highest X.Y.Z already published on
# ghcr.io/${GHCR_OWNER}/shomsky-linux and on git tags (vX.Y.Z), bumps it,
# builds shomskylinux:<version>, and pushes that tag and :latest.
# Default bump is patch. An explicit X.Y.Z is used as-is when it is not
# already published.
#
# Env:
#   GHCR_OWNER          GitHub user or org (default: mtshomskyieee)
#   GHCR_USER           docker login user (default: gh login, else GHCR_OWNER)
#   GHCR_TOKEN          registry token; falls back to GITHUB_TOKEN, then `gh auth token`
#   IMAGE               local image name (default: shomskylinux)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OWNER="${GHCR_OWNER:-mtshomskyieee}"
IMAGE="${IMAGE:-shomskylinux}"
REMOTE="ghcr.io/${OWNER}/shomsky-linux"
BUMP="patch"
EXPLICIT=""
DRY_RUN=0

usage() {
  sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    patch|minor|major) BUMP="$1" ;;
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

# Print one X.Y.Z per line from the GHCR tag list. Follows Link rel=next.
ghcr_versions() {
  local token url headers body next
  token="$(curl -fsS --retry 2 \
    "https://ghcr.io/token?service=ghcr.io&scope=repository:${OWNER}/shomsky-linux:pull" \
    | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
  [[ -n "$token" ]] || return 1
  url="https://ghcr.io/v2/${OWNER}/shomsky-linux/tags/list?n=1000"
  while [[ -n "$url" ]]; do
    headers="$(mktemp)"
    body="$(curl -fsS --retry 2 -D "$headers" -H "Authorization: Bearer ${token}" "$url")" \
      || { rm -f "$headers"; return 1; }
    grep -oE '[0-9]+\.[0-9]+\.[0-9]+' <<<"$body" || true
    next="$(grep -i '^link:' "$headers" | tr ',' '\n' | grep 'rel="next"' \
      | sed -n 's/.*<\([^>]*\)>.*/\1/p' | head -n 1 || true)"
    rm -f "$headers"
    url="$next"
  done
}

# Print one X.Y.Z per line from local and origin git tags named vX.Y.Z.
git_versions() {
  git tag -l 'v[0-9]*' | sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$/\1/p'
  git ls-remote --tags origin 'refs/tags/v*' 2>/dev/null \
    | awk '{print $2}' \
    | sed -nE 's#^refs/tags/v([0-9]+\.[0-9]+\.[0-9]+)(\^\{\})?$#\1#p' \
    || true
}

highest_version() {
  grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' | sort -uV | tail -n 1
}

bump_version() {
  local version="$1" kind="$2"
  local major minor patch
  IFS=. read -r major minor patch <<<"$version"
  case "$kind" in
    major) echo "$((major + 1)).0.0" ;;
    minor) echo "${major}.$((minor + 1)).0" ;;
    patch) echo "${major}.${minor}.$((patch + 1))" ;;
    *) echo "Unknown bump: $kind" >&2; return 1 ;;
  esac
}

ensure_registry_login() {
  local token="${GHCR_TOKEN:-${GITHUB_TOKEN:-}}"
  local user="${GHCR_USER:-}"
  if [[ -z "$token" ]] && command -v gh >/dev/null 2>&1; then
    token="$(gh auth token 2>/dev/null || true)"
    if [[ -n "$token" && -z "$user" ]]; then
      user="$(gh api user -q .login 2>/dev/null || true)"
    fi
  fi
  if [[ -z "$user" ]]; then
    user="$OWNER"
  fi
  if [[ -n "$token" ]]; then
    echo "Logging in to ghcr.io as ${user}"
    printf '%s' "$token" | docker login ghcr.io -u "$user" --password-stdin
    return
  fi
  if [[ -f "${HOME}/.docker/config.json" ]] && grep -q 'ghcr.io' "${HOME}/.docker/config.json"; then
    echo "Using existing docker credentials for ghcr.io"
    return
  fi
  echo "No GHCR credentials. Set GHCR_TOKEN or GITHUB_TOKEN, run \`gh auth login\`, or \`docker login ghcr.io\`." >&2
  exit 1
}

known=""
echo "Reading tags from ${REMOTE}"
if ghcr="$(ghcr_versions)"; then
  known+="${ghcr}"$'\n'
else
  echo "Could not list ${REMOTE} tags; continuing with git tags." >&2
fi
known+="$(git_versions)"$'\n'

current="$(printf '%s\n' "$known" | highest_version || true)"
if [[ -z "$current" ]]; then
  echo "No published X.Y.Z tag found on GHCR or git." >&2
  exit 1
fi

if [[ -n "$EXPLICIT" ]]; then
  version="$EXPLICIT"
  if printf '%s\n' "$known" | grep -qx "$version"; then
    echo "${version} is already published. Pick a newer tag." >&2
    exit 1
  fi
else
  version="$(bump_version "$current" "$BUMP")"
fi

echo "Current: ${current}"
echo "Next:    ${version}"
echo "Push:    ${REMOTE}:${version}"
echo "Push:    ${REMOTE}:latest"

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "Dry run; image was not built or pushed."
  exit 0
fi

ensure_registry_login

IMAGE="$IMAGE" TAG="$version" "$ROOT/scripts/build.sh"
GHCR_OWNER="$OWNER" IMAGE="$IMAGE" "$ROOT/scripts/publish.sh" "$version"

echo "OK  ${REMOTE}:${version}"
