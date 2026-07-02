---
type: Decision
title: FROM scratch image with a self-probing binary
description: Fully static musl binary with compiled-in TLS roots; the binary doubles as its own Docker health probe.
tags: [docker, deployment, security]
timestamp: 2026-07-02T00:00:00Z
---

# FROM scratch image with a self-probing binary

## Context

The proxy should ship as a lightweight, hard-to-attack container. Rust +
rustls makes a fully static binary feasible.

## Options

1. `debian-slim` runtime (~80 MB, familiar debugging).
2. `alpine` runtime (~10 MB, still has a shell/package manager).
3. **`FROM scratch`** — nothing but the binary.

## Choice

Scratch. The musl build is statically linked (verified `static-pie`), and
reqwest's `rustls-tls` compiles the Mozilla root store in via webpki-roots —
so not even CA certificates are needed. Total image ≈ 3.5 MB. Runs as numeric
UID 10001 with `read_only`, `cap_drop: ALL`, `no-new-privileges`; compatible
with rootless Docker/Podman.

Two scratch-specific problems and their solutions:

- **No shell/curl for HEALTHCHECK** → the binary probes itself:
  `nim-proxy --health` opens a TCP connection to its own `/health` and exits
  0/1. `HEALTHCHECK CMD ["/nim-proxy", "--health"]`.
- **Named volume ownership** → an empty `/data` directory is COPY'd into the
  image `--chown=10001:10001`; a fresh named volume inherits that ownership
  so history can persist without an init container.

Verified by running the static binary under `env -i` (empty environment):
serves traffic with zero filesystem/env dependencies beyond its own config.

**Gotcha found by CI (first real Docker build):** on the alpine builder the
*host* is musl, so a global `RUSTFLAGS="-C target-feature=+crt-static"` also
hits proc-macro crates — which must be dylibs — failing with "cannot produce
proc-macro … does not support these crate types". The fix is passing
`--target x86_64-unknown-linux-musl` explicitly even though it equals the
host triple: with a `--target` set, cargo stops applying RUSTFLAGS to host
units. The Dockerfile comments this so nobody "simplifies" it away. (Local
glibc-host cross-builds never hit this, which is why it survived until CI.)

## Consequences

- No exec-ing into the container for debugging — logs and `/metrics` are the
  observability surface (by design).
- Corporate-MITM environments that need custom CA roots can't add them via
  the filesystem; `NIM_BASE_URL` through such proxies is unsupported.
