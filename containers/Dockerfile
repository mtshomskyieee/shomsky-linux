# syntax=docker/dockerfile:1.7
# Shomsky Linux — Alpine-derived LLM host (CPU / musl)
#
# Multi-stage:
#   toolchain — alpine + rust/cargo (shared base)
#   mistral   — builds mistral.rs only (cached until MISTRALRS_REF changes)
#   workspace — builds engram-shell + crates (invalidates on src/ changes)
#   runtime   — slim alpine with binaries, rootfs overlay, /engram skills
#
# Editing src/ must NOT rebuild mistral.rs. Keep those stages independent.

ARG ALPINE_VERSION=3.24
ARG MISTRALRS_REF=v0.9.0
ARG CARGO_BUILD_JOBS=2

# ---------------------------------------------------------------------------
# Shared Rust toolchain
# ---------------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION} AS toolchain

ARG CARGO_BUILD_JOBS

RUN apk add --no-cache \
      build-base \
      cargo \
      rust \
      git \
      curl \
      openssl-dev \
      openssl-libs-static \
      pkgconf \
      cmake \
      clang-dev \
      llvm-dev \
      python3 \
      linux-headers \
      ca-certificates

ENV CARGO_HOME=/usr/local/cargo \
    CARGO_TERM_COLOR=always \
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} \
    RUSTFLAGS="-C target-feature=-crt-static"

# ---------------------------------------------------------------------------
# mistral.rs (CPU, no default extras)
# Only rebuilds when MISTRALRS_REF / toolchain / this stage changes.
# ---------------------------------------------------------------------------
FROM toolchain AS mistral

ARG MISTRALRS_REF

WORKDIR /src
# musl's libc::Ioctl is unsigned; mistral.rs v0.9.0 passes SIOCSIFFLAGS uncast.
RUN git clone --depth 1 --branch "${MISTRALRS_REF}" \
      https://github.com/EricLBuehler/mistral.rs.git mistral.rs \
 && sed -i \
      's/libc::ioctl(sock, libc::SIOCSIFFLAGS, \&ifr)/libc::ioctl(sock, libc::SIOCSIFFLAGS as _, \&ifr)/' \
      mistral.rs/mistralrs-sandbox/src/linux/namespaces.rs

WORKDIR /src/mistral.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/mistral.rs/target \
    mkdir -p /out \
 && cargo build --release -p mistralrs-cli --no-default-features \
 && test -x target/release/mistralrs \
 && cp target/release/mistralrs /out/mistralrs \
 && strip /out/mistralrs

# ---------------------------------------------------------------------------
# Workspace crates (engram-shell, shomagent, …)
# This is the only stage that invalidates on day-to-day src/ edits.
# ---------------------------------------------------------------------------
FROM toolchain AS workspace

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY VERSION ./VERSION
COPY src ./src
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    mkdir -p /out \
 && cargo build --release -p engram-shell -p engram-cmdfreq -p shomagent -p shomsky -p quickyn \
 && cp target/release/engram-shell /out/engram-shell \
 && cp target/release/engram-cmdfreq /out/engram-cmdfreq \
 && cp target/release/shomagent /out/shomagent \
 && cp target/release/shomagentd /out/shomagentd \
 && cp target/release/shomsky /out/shomsky \
 && cp target/release/quickyn /out/quickyn \
 && strip /out/engram-shell /out/engram-cmdfreq /out/shomagent /out/shomagentd /out/shomsky /out/quickyn

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION} AS runtime

LABEL org.opencontainers.image.title="Shomsky Linux" \
      org.opencontainers.image.description="Alpine-derived LLM host with engram-shell" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/mtshomskyieee/shomsky-linux"

RUN apk add --no-cache \
      busybox \
      bash \
      bats \
      ca-certificates \
      curl \
      libgcc \
      libstdc++ \
      openssl \
      && update-ca-certificates

# Branding + config overlay
COPY rootfs/ /
COPY engram/skills /engram/skills
COPY tests /opt/shomsky/tests
COPY NOTICE /usr/share/doc/shomsky/NOTICE
COPY LICENSE /usr/share/doc/shomsky/LICENSE
COPY third_party/docker-alpine /usr/share/doc/shomsky/third_party/docker-alpine

# Binaries (MCP servers are stdio children — never published as ports)
COPY --from=workspace /out/engram-shell /usr/local/bin/engram-shell
COPY --from=workspace /out/engram-cmdfreq /usr/local/bin/engram-cmdfreq
COPY --from=workspace /out/shomagent /usr/local/bin/shomagent
COPY --from=workspace /out/shomagentd /usr/local/bin/shomagentd
COPY --from=workspace /out/shomsky /usr/local/bin/shomsky
COPY --from=workspace /out/quickyn /usr/local/bin/quickyn
COPY --from=mistral /out/mistralrs /usr/local/bin/mistralrs
COPY containers/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY containers/shomsky-tests.sh /usr/local/bin/shomsky-tests

RUN chmod 755 /usr/local/bin/engram-shell \
              /usr/local/bin/engram-cmdfreq \
              /usr/local/bin/shomagent \
              /usr/local/bin/shomagentd \
              /usr/local/bin/shomsky \
              /usr/local/bin/quickyn \
              /usr/local/bin/mistralrs \
              /usr/local/bin/entrypoint.sh \
              /usr/local/bin/shomsky-tests \
 && mkdir -p /engram/models /engram/skills /engram/user /opt/shomsky \
              /engram/user/bin /engram/user/share/man/man1 \
              /etc/shomsky/shomagent_crew.d /run/shomagent \
 && chmod 755 /engram /engram/models /engram/skills /engram/user \
 && if ! grep -q '/usr/local/bin/engram-shell' /etc/shells 2>/dev/null; then \
      echo '/usr/local/bin/engram-shell' >> /etc/shells; \
    fi \
 && sed -i 's|^root:x:0:0:root:/root:.*|root:x:0:0:root:/root:/usr/local/bin/engram-shell|' /etc/passwd \
 && adduser -D -h /home/shomsky -s /usr/local/bin/engram-shell shomsky \
 && chown -R shomsky:shomsky /engram

ENV SHOMSKY_MODEL=mistral \
    ENGRAM_MODE=agent \
    ENGRAM_CONFIG=/etc/engram/engram.toml \
    ENGRAM_SKILLS_DIR=/engram/skills \
    PATH="/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

VOLUME ["/engram/models"]
WORKDIR /home/shomsky
EXPOSE 1234

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
# Default: interactive engram-shell (entrypoint execs it when no args)
CMD []
