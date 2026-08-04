#!/usr/bin/env bash
# Build the Shomsky Linux CPU (Alpine/musl) image.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

IMAGE="${IMAGE:-shomskylinux}"
TAG="${TAG:-dev}"
ALPINE_VERSION="${ALPINE_VERSION:-3.24}"
MISTRALRS_REF="${MISTRALRS_REF:-v0.9.0}"
# Lower on small builders to avoid OOM during mistral.rs compile.
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
PLATFORM="${PLATFORM:-linux/amd64}"

echo "Building ${IMAGE}:${TAG}"
echo "  alpine=${ALPINE_VERSION}  mistralrs=${MISTRALRS_REF}  jobs=${CARGO_BUILD_JOBS}"

docker build \
  --platform "$PLATFORM" \
  -f containers/Dockerfile \
  --build-arg "ALPINE_VERSION=${ALPINE_VERSION}" \
  --build-arg "MISTRALRS_REF=${MISTRALRS_REF}" \
  --build-arg "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}" \
  -t "${IMAGE}:${TAG}" \
  -t "${IMAGE}:latest" \
  .

echo "OK  ${IMAGE}:${TAG}"
