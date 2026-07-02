# Build a fully static musl binary, ship it on scratch: no distro, no shell,
# no libc, no package manager. TLS roots are compiled into the binary
# (rustls + webpki-roots), so not even CA certificates are needed.
#
# Multi-arch: buildx sets TARGETARCH per requested platform and runs this stage
# emulated on that arch, so we build for the matching native musl target.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
ENV RUSTFLAGS="-C target-feature=+crt-static"
# Map Docker's arch to the Rust musl target. The explicit --target (below) looks
# redundant on alpine (host IS musl) but is load-bearing: with a --target set,
# cargo stops applying crt-static RUSTFLAGS to host units like proc-macros,
# which must stay dylibs. Empty TARGETARCH (plain `docker build`) defaults amd64.
ARG TARGETARCH
RUN case "${TARGETARCH:-amd64}" in \
      amd64) t=x86_64-unknown-linux-musl ;; \
      arm64) t=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    rustup target add "$t"; \
    echo "$t" > /tmp/target
WORKDIR /app

# Cache the dependency build separately from source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release --target "$(cat /tmp/target)" && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release --target "$(cat /tmp/target)" \
    && cp "target/$(cat /tmp/target)/release/nim-proxy" /app/nim-proxy && mkdir /app/data

FROM scratch
# OCI image metadata so the image is self-describing. VERSION comes from the
# release workflow (the git tag); "dev" for local builds.
ARG VERSION=dev
LABEL org.opencontainers.image.title="nim-proxy" \
      org.opencontainers.image.description="Rate-limit-aware OpenAI-compatible proxy for NVIDIA NIM with multi-key load balancing" \
      org.opencontainers.image.source="https://github.com/miztertea/nim-proxy" \
      org.opencontainers.image.url="https://github.com/miztertea/nim-proxy" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}"
COPY --from=build /app/nim-proxy /nim-proxy
# Empty dir owned by the runtime user: a named volume mounted at /data
# inherits this ownership on first use, so history can persist.
COPY --from=build --chown=10001:10001 /app/data /data
ENV DATA_DIR=/data
USER 10001:10001
EXPOSE 8000
# The binary doubles as its own health probe (no shell/curl in scratch).
HEALTHCHECK --interval=30s --timeout=3s --start-period=2s CMD ["/nim-proxy", "--health"]
ENTRYPOINT ["/nim-proxy"]
