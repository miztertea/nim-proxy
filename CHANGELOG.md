# Changelog

All notable changes to nim-proxy are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

> **Breaking (v0.6.0):** app-level configuration moved from env vars into a
> UI-managed store. `NIM_API_KEYS`, `PROXY_API_KEYS`, `ADMIN_PASSWORD`,
> `INSECURE_NO_AUTH`, `NIM_BASE_URL`, `RPM_PER_KEY`, `MAX_WAIT_SECS`,
> `HEARTBEAT_SECS`, `MODELS_TTL_SECS`, `STREAM_IDLE_SECS`,
> `REQUEST_TIMEOUT_SECS`, `STRICT_PASSTHROUGH`, `REF_PRICE_IN`/`REF_PRICE_OUT`,
> `HISTORY_DAYS`, and `MAX_INFLIGHT` are **ignored** (a one-line boot warning
> lists any still set). Configure everything in the dashboard on first run. The
> dashboard is now multi-user (username + password), and `INSECURE_NO_AUTH` is
> replaced by an `open|keyed` API-access mode that affects only `/v1`. There is
> no migration (there were no deployments to migrate).

### Added

- **UI-managed config store + first-run setup wizard**: app-level config lives
  in `DATA_DIR/config.json` (version 1, atomic writes, 0600), edited from a new
  dashboard **Settings** area (sub-nav: Access & keys · Server · Users ·
  Account) and claimed by a 3-step wizard (create superuser → add ≥1 NIM key,
  validated live against the upstream → finish, logged in). A corrupt/unreadable
  or future-version store is a hard boot error, never a silent fall-through to
  setup. JSON not SQLite — see
  `knowledge/decisions/ui-managed-config-store.md`.
- **Multi-user with roles & per-key ownership**: `superuser` (an admin that can
  never be deleted), `admin` (server settings + user management), `user` (own
  account, own client keys, own NIM keys). Dashboards are identical for every
  role; `GET /api/config` is filtered server-side so hidden sections are absent
  from the payload, not CSS-hidden. Sessions carry the username plus a fragment
  of the password hash, so a password change/reset invalidates that user's
  sessions instantly and role changes/deletion apply on the next request.
  Passwords are PBKDF2-HMAC-SHA256 (600k iterations, RFC 7914 vectors). See the
  v0.6.0 amendment in
  `knowledge/decisions/auth-posture-and-dashboard-password.md`.
- **Per-key rpm and live key management**: each NIM key has its own rpm
  (default 40, range 1–10000), an owner, and an enable/disable toggle; the pool
  rebuilds live on any change with rate-state carryover (kept keys keep their
  in-window counts; disabled keys re-enable warm). The superuser always owns ≥1
  enabled key (the pool floor). Client API keys are server-generated 128-bit
  secrets with an `npk_` prefix, shown exactly once and stored only as SHA-256
  digests (+ last-4 for display).
- **Model-pressure governor**: classifies NIM's per-model worker-concurrency
  exhaustion (`Worker local total request limit reached`) apart from plain 429s
  and backs off the **model** (never benches the lane, since key failover can't
  help). Adaptive and zero-config (engages at half observed in-flight, +1 per
  stable minute, dissolves after 30 clean minutes) with optional per-model
  pinned caps in Settings. New metrics `nimproxy_worker_exhausted_total{model}`,
  `nimproxy_model_inflight{model}`, `nimproxy_model_limit{model}` (0 =
  ungoverned), and a Reliability **Model pressure** card that appears only once
  the governor has engaged. See `knowledge/architecture/governor.md`;
  `mock_nim.py` gained `--worker-slots N` and `loadtest.py` reports worker
  exhaustions + peak per-model concurrency.
- **Redesigned dashboard**: a dark, NVIDIA-green "operator console" — left
  sidebar nav (collapses to an icon rail below 860px), top bar with range
  pills, Space Grotesk + Spline Sans Mono webfonts. Five persona-aligned tabs
  (`Overview · Models · Clients · Reliability · Capacity`), richer KPI cards
  with trend delta chips and sparklines, ring gauges, and a Reliability hero
  (availability vs a 99.9% SLO, a "where time goes" latency breakdown) and a
  Capacity hero (saturation bar, keys-for-peak provisioning chip). Every line
  chart now has a hover crosshair with a per-series tooltip, and every table
  is click-to-sort with a sticky header and internal scroll — sort order and
  scroll position both survive the 3s live refresh. See
  `knowledge/decisions/dashboard-operator-console-redesign.md`.

### Changed

- **Env shrinks to 5 container-level vars** (`HOST`, `PORT`, `DATA_DIR`,
  `RUST_LOG`, `TRUST_PROXY`); `DATA_DIR` must be writable (it now holds the
  credential store as well as history) and an unwritable dir is a hard boot
  error. `.env.example`, README, and the runbooks are rewritten to match; the
  quickstart is now `docker compose up` → open the dashboard → complete the
  wizard.
- **Dashboard auth is now user-based.** Login takes a username and password;
  the single `ADMIN_PASSWORD` gate is gone. Prometheus scrapers authenticate as
  `Authorization: Bearer <username>:<password>` (or HTTP Basic). Volume backups
  now contain credentials (`config.json`, 0600) — treat them as secrets.
- `docker compose up` now runs the published `ghcr.io/miztertea/nim-proxy:latest`
  image instead of building from source; source builds move to an explicit dev
  override (`docker-compose.dev.yml`, tagged `nim-proxy:dev`). README,
  CONTRIBUTING, and the deploy runbook updated to match.
- **CSP** now allows the dashboard's webfonts: `style-src` gained
  `https://fonts.googleapis.com`, and a new `font-src` allows
  `https://fonts.gstatic.com`. Falls back to system fonts if the CDN is
  unreachable.

### Removed

- **All app-level env vars** (see the breaking note above) — they're ignored,
  with a one-line boot warning listing any still set. No seed-from-env, no
  migration.
- **`INSECURE_NO_AUTH`.** Replaced by the store's `open|keyed` API-access mode,
  which governs only `/v1`; every dashboard/observability surface always
  requires a logged-in session post-setup.
- **Light mode.** The dashboard is dark-only now; the light palette and
  `prefers-color-scheme` handling were deleted as a committed design choice.
- **The Compare tab** — its head-to-head scorecard and generation-speed bar
  race are now a section of the Models tab.
- **The heatmap's table-view toggle** — not part of the redesign; the heatmap
  keeps its per-cell hover tooltips.

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
