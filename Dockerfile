# Build a small static binary against musl, ship it on bare Alpine.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
WORKDIR /app

# Cache the dependency build separately from source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM alpine:3.21
RUN apk add --no-cache ca-certificates && adduser -D -H proxy
USER proxy
COPY --from=build /app/target/release/nim-proxy /usr/local/bin/nim-proxy
EXPOSE 8000
ENTRYPOINT ["nim-proxy"]
