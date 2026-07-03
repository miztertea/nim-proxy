# Changelog

All notable changes to nim-proxy are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-03

First public release: the repository is now public, and this tag publishes the
first signed multi-arch container image to GHCR with SBOM and build provenance.

### Fixed

- **Unauthenticated panic in the login handler.** A percent-escape followed by a
  multibyte UTF-8 character (e.g. `password=%€`) in the `POST /login` body sliced a
  `&str` on a non-char boundary and panicked. Percent-decoding is now byte-safe.
- **No timeout on non-streaming upstream reads.** A buffered request whose upstream
  sent headers then stalled the body could hang forever, pinning an in-flight slot.
  Non-streaming requests now honor `REQUEST_TIMEOUT_SECS` (default 300s) and surface a
  `502` on a stalled/failed body read. Streaming still uses `STREAM_IDLE_SECS`.
- **`RPM_PER_KEY=0` wedged the dispatcher** (out-of-bounds index in the pacer). Now
  rejected at startup.
- Login throttle window uses saturating subtraction (robust to clock adjustments).

### Added

- `REQUEST_TIMEOUT_SECS` config (default 300).

### Changed

- Regression tests for all of the above; coverage raised to ~90%.

### Performance

- Build with `opt-level = 3` (was `"z"`): the release profile optimized for size,
  throttling the JSON-parse and SSE-scan hot paths. Binary grows ~3.5→4.6 MB.
- Drop a deep clone of the whole request body on the streaming injection path
  (move it instead); use `Bytes::from_static` for the SSE control frames.
- Routine `cargo update` (`rustc-hash` patch).

### Dependencies

- Bump `metrics-exporter-prometheus` 0.17 → 0.18 and refresh CI/release action
  versions, including the Node 24 runtime wave (gitleaks-action v3, the docker/*
  build actions, download-artifact v8).
- Hold the auth crypto/RNG stack (`hmac` 0.12, `sha2` 0.10, `getrandom` 0.2) on
  the proven-stable line — the proposed 0.13/0.11/0.3 majors are breaking with no
  security fix; Dependabot is configured to only take patches for these.

## [0.4.0] - 2026-07-02

The proxy becomes a **benchmarking and agent-observability tool**: because it
sits in the request path for every harness and model, it can now report *how*
each agent behaves and *how well* each model responds — all from counts and
sizes, never message content.

### Added

- **Request-shape & response-quality metrics**, captured from the request path
  that was already deserialized and scanned: conversation depth, tools offered,
  sampling temperature, `max_tokens`, stream-vs-buffered and JSON-mode mix
  (labeled by client/harness), plus finish-reason/truncation, tool calls,
  reasoning ("thinking") tokens, and mean TPOT (labeled by model). Everything is
  bounded-cardinality with server-clamped enums — counts and sizes only, never
  content. See `knowledge/decisions/request-shape-metrics.md`.
- **Six persona-aligned dashboard views** (Overview, Models, Compare, Harnesses,
  Proxy, Keys), rebuilt from the previous three tabs, each ordered
  at-a-glance → trends → detail, in light and dark mode. Adds a head-to-head
  model scorecard, per-harness fingerprints, and a hash-to-hue color fallback
  past the six categorical slots.
- Generation-speed (tok/s) median/p95 trend, a ranked non-success-outcome
  breakdown, and threshold-colored capacity/success-rate gauges.
- Example [`examples/opencode.json`](examples/opencode.json) config tuned for
  GLM-5.2 (context, compaction, sampling), with rationale in
  `examples/README.md`.

### Changed

- Test coverage extended to the buffered `relay()` quality path, an
  unknown-`finish_reason` → `other` clamp, JSON mode, and non-`auto`
  `tool_choice` — now **29 unit + 21 e2e** tests.
- Load harness gained tool/JSON/sampling variety and a corrected boot command
  (`INSECURE_NO_AUTH`); re-run clean at 240 requests, 0 failures, 0 upstream
  rate violations, balanced across all keys.

### Security

- Pre-merge hardening pass: a dedicated dashboard-XSS audit plus a full security
  review of the branch found **zero** vulnerabilities — every new `innerHTML`
  value is escaped, every new label is a bounded enum/histogram, and no route
  left the admin gate.

## [0.3.0] - 2026-07-02

Security-hardening release. A review of the merged proxy found a stored-XSS
chain, unbounded metric-label cardinality, log injection, and an open-by-default
posture. All fixed.

### Added

- **Fail-closed auth.** The proxy refuses to start on a network-reachable port
  without auth. Secure mode requires `PROXY_API_KEYS` (gates `/v1/*`, any key
  works, constant-time compare) and `ADMIN_PASSWORD` (gates the dashboard,
  `/metrics`, and `/api/history` via an HMAC-signed, HttpOnly, SameSite=Strict
  session cookie; Bearer/Basic for scrapers). Open mode is an explicit
  `INSECURE_NO_AUTH=true` opt-in. See
  `knowledge/decisions/auth-posture-and-dashboard-password.md`.
- Failed-login throttle, a rejected-API-key delay, and a `MAX_INFLIGHT`
  flood-shedding cap.
- `cargo audit` in CI.

### Security

- **Input sanitizing.** Client-controlled `model`/`path` labels are sanitized to
  a conservative charset, length-capped, and cardinality-bounded at ingest —
  killing the exposition/log-injection and cardinality-blowup vectors. The
  dashboard `esc()`-escapes every dynamic `innerHTML` sink, and all responses
  carry a strict `Content-Security-Policy` plus anti-framing/anti-sniffing
  headers. See `knowledge/decisions/input-sanitizing-and-xss.md`.
- Compose now publishes `127.0.0.1:8000:8000` (loopback) by default, so a bare
  `docker compose up` can't accidentally expose an open instance.
- Verified with a real-browser XSS check (payload rendered inert), a secure-mode
  load test (300/300, 0 rate violations), and a clean `cargo audit`.

## [0.2.0] - 2026-07-02

Observability and hardening enrichments on top of the core proxy.

### Added

- **Prometheus `/metrics`** exposition and optional client access keys
  (`PROXY_API_KEYS`) for per-client attribution.
- **Built-in dashboard** — a single embedded HTML file (no Grafana, no config) —
  plus an ASCII boot banner, structured startup detail, one-line-per-request
  access logs (TTY-detected ANSI color), and a self-probe healthcheck
  (`nim-proxy --health`) that works `FROM scratch`.
- **Metrics history**: a ~4 KB snapshot every 5 minutes, retained `HISTORY_DAYS`
  days, powering time-range reports (1h/6h/24h/7d/30d + custom) that survive
  restart.
- Model cards with id-namespace enrichment (the `/v1/models` schema research
  killed the idea of API-sourced descriptions).

### Changed

- Proxy hardened and given a full test suite (unit + e2e against a scripted mock
  NIM) and a load harness (`scripts/mock_nim.py --enforce` + `scripts/loadtest.py`).
- The `knowledge/` Open Knowledge Format bundle was compiled: design decisions,
  validated NIM research, architecture notes, and runbooks.

### Fixed

- Docker build on the musl-host Alpine builder: pass an explicit `--target` so
  global `crt-static` RUSTFLAGS skip proc-macro dylibs.

## [0.1.0] - 2026-07-01

Initial rate-limit-aware proxy.

### Added

- OpenAI-compatible pass-through to NVIDIA NIM with **per-key sliding-window
  rate limiting** (40 requests per rolling 60 s, matching NIM's limiter) and
  multi-key load balancing.
- **Global FIFO dispatcher** — one queue for all clients, slots granted strictly
  in arrival order, abandoned-waiter slots returned — for fair multi-client
  allocation.
- **Conversation affinity with least-loaded spillover**: each conversation pins
  to one key to keep the server-side prefix cache warm, spilling to the
  least-loaded ready lane when its lane is full.
- **Distroless image**: a static musl binary shipped `FROM scratch` (~3.5 MB,
  TLS roots compiled in), running non-root with hardened compose defaults.

[Unreleased]: https://github.com/miztertea/nim-proxy/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/miztertea/nim-proxy/releases/tag/v0.5.0
[0.4.0]: https://github.com/miztertea/nim-proxy/releases/tag/v0.4.0
[0.3.0]: https://github.com/miztertea/nim-proxy/releases/tag/v0.3.0
[0.2.0]: https://github.com/miztertea/nim-proxy/releases/tag/v0.2.0
[0.1.0]: https://github.com/miztertea/nim-proxy/releases/tag/v0.1.0
