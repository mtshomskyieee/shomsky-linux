#!/usr/bin/env bash
# Tag and push to GHCR.
# Usage: GHCR_OWNER=<user_or_org> ./scripts/publish.sh [tag]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

IMAGE_LOCAL="${IMAGE:-shomskylinux}"
TAG="${1:-${TAG:-dev}}"
OWNER="${GHCR_OWNER:?set GHCR_OWNER to your GitHub user or org}"
REMOTE="ghcr.io/${OWNER}/shomsky-linux"

docker tag "${IMAGE_LOCAL}:${TAG}" "${REMOTE}:${TAG}"
docker tag "${IMAGE_LOCAL}:${TAG}" "${REMOTE}:latest"

echo "Pushing ${REMOTE}:${TAG}"
docker push "${REMOTE}:${TAG}"
docker push "${REMOTE}:latest"
echo "OK  ${REMOTE}:${TAG}"
