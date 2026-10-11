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
# On a Mac, match the machine so rustc is not emulated (QEMU SIGSEGV).
# Elsewhere keep linux/amd64 unless PLATFORM is set.
if [[ -z "${PLATFORM:-}" ]]; then
  if [[ "$(uname -s)" == "Darwin" ]]; then
    case "$(uname -m)" in
      arm64|aarch64) PLATFORM=linux/arm64 ;;
      *) PLATFORM=linux/amd64 ;;
    esac
  else
    PLATFORM=linux/amd64
  fi
fi

# Colima provides the Docker daemon on macOS in this setup. Start it when
# the CLI is present and the VM is stopped so `docker build` can connect.
if [[ "$(uname -s)" == "Darwin" ]] && command -v colima >/dev/null 2>&1; then
  if ! colima status >/dev/null 2>&1; then
    echo "Colima is not running; starting it..."
    colima start
  fi
fi

# Dockerfile RUN --mount lines need BuildKit. Newer Docker CLIs only provide
# that through the buildx plugin.
if ! docker buildx version >/dev/null 2>&1; then
  echo "Docker Buildx is required to build this image." >&2
  echo "Install it with: brew install docker-buildx" >&2
  echo "Then link the plugin:" >&2
  echo "  mkdir -p ~/.docker/cli-plugins" >&2
  echo "  ln -sfn \"\$(brew --prefix docker-buildx)/bin/docker-buildx\" ~/.docker/cli-plugins/docker-buildx" >&2
  exit 1
fi

export DOCKER_BUILDKIT=1

echo "Building ${IMAGE}:${TAG}"
echo "  alpine=${ALPINE_VERSION}  mistralrs=${MISTRALRS_REF}  jobs=${CARGO_BUILD_JOBS}  platform=${PLATFORM}"

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
