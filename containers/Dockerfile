# syntax=docker/dockerfile:1.7
# Shomsky Linux — Alpine-derived LLM host (CPU / musl)
#
# Multi-stage:
#   builder  — alpine:3.24 + rust/cargo, builds mistral.rs + engram-shell
#   runtime  — slim alpine with binaries, rootfs overlay, /engram skills

ARG ALPINE_VERSION=3.24
ARG MISTRALRS_REF=v0.9.0

# ---------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION} AS builder

ARG MISTRALRS_REF
# Cap parallelism — mistral.rs is memory-hungry during compile.
ARG CARGO_BUILD_JOBS=2

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

WORKDIR /build

# --- engram-shell + engram-cmdfreq + shomagent -----------------------------
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    mkdir -p /out \
 && cargo build --release -p engram-shell -p engram-cmdfreq -p shomagent -p shomsky \
 && cp target/release/engram-shell /out/engram-shell \
 && cp target/release/engram-cmdfreq /out/engram-cmdfreq \
 && cp target/release/shomagent /out/shomagent \
 && cp target/release/shomagentd /out/shomagentd \
 && cp target/release/shomsky /out/shomsky

# --- mistral.rs (CPU, no default extras) -----------------------------------
# Clone into a fixed path, then build with registry+target cache mounts.
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
    cargo build --release -p mistralrs-cli --no-default-features \
 && test -x target/release/mistralrs \
 && cp target/release/mistralrs /out/mistralrs \
 && strip /out/engram-shell /out/engram-cmdfreq /out/shomagent /out/shomagentd /out/shomsky /out/mistralrs
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
COPY --from=builder /out/engram-shell /usr/local/bin/engram-shell
COPY --from=builder /out/engram-cmdfreq /usr/local/bin/engram-cmdfreq
COPY --from=builder /out/shomagent /usr/local/bin/shomagent
COPY --from=builder /out/shomagentd /usr/local/bin/shomagentd
COPY --from=builder /out/shomsky /usr/local/bin/shomsky
COPY --from=builder /out/mistralrs /usr/local/bin/mistralrs
COPY containers/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY containers/shomsky-tests.sh /usr/local/bin/shomsky-tests

RUN chmod 755 /usr/local/bin/engram-shell \
              /usr/local/bin/engram-cmdfreq \
              /usr/local/bin/shomagent \
              /usr/local/bin/shomagentd \
              /usr/local/bin/shomsky \
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
