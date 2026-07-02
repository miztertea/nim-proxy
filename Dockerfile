# Build a fully static musl binary, ship it on scratch: no distro, no shell,
# no libc, no package manager. TLS roots are compiled into the binary
# (rustls + webpki-roots), so not even CA certificates are needed.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
ENV RUSTFLAGS="-C target-feature=+crt-static"
WORKDIR /app

# Cache the dependency build separately from source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release && mkdir /app/data

FROM scratch
COPY --from=build /app/target/release/nim-proxy /nim-proxy
# Empty dir owned by the runtime user: a named volume mounted at /data
# inherits this ownership on first use, so history can persist.
COPY --from=build --chown=10001:10001 /app/data /data
ENV DATA_DIR=/data
USER 10001:10001
EXPOSE 8000
# The binary doubles as its own health probe (no shell/curl in scratch).
HEALTHCHECK --interval=30s --timeout=3s --start-period=2s CMD ["/nim-proxy", "--health"]
ENTRYPOINT ["/nim-proxy"]
