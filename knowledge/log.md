---
type: Log
title: Knowledge base chronology
description: Append-only record of ingests, decisions, and maintenance passes.
---

# Log

## [2026-07-04] ingest — v0.6.0 release cut: correctness fixes, wizard client key, outcome charts

The 0.6.0 release closes the loose ends found during the config-store epic
and cuts the version:

- **Streaming inflight accounting fixed**: the `max_inflight` guard now rides
  into the spawned streaming task, so live streams occupy their slot until the
  stream ends (it previously dropped at response-header time, bounding only
  buffered requests). E2e-proven with a hang-stream + shed test.
- **Disconnect noticed during blocked upstream reads** (blind-review finding):
  the streaming relay races each upstream read against `tx.closed()`, so a
  client hang-up frees its `max_inflight` slot immediately instead of at the
  `stream_idle` cutoff — and hung upstreams can't pin slots until restart
  when `stream_idle` is 0. E2e: `disconnected_stream_releases_its_inflight_slot`.
- **Password-change TOCTOU closed**: an own-password change commits only if
  the stored hash is still the one the current password was verified against
  (verify runs outside the store lock); a concurrent admin reset now wins with
  a 409 (`settings::apply_password_change`, unit-tested).
- **Wizard mints the first client key** (default on, explicit warning on
  opt-out — the maintainer's rule: let users run it any way they want, warn
  when it's unsafe): `POST /setup` takes `create_client_key`, returns the
  `npk_` secret once, and the wizard ends on a connect panel (base URL + key +
  copy). [client-auth](architecture/client-auth.md) and the
  [config-store ADR](decisions/ui-managed-config-store.md) updated.
- **Charts for the collected-but-undrawn signals**: a `stackChart` primitive +
  requests-by-outcome-over-time on Reliability; requested output cap
  (`request_max_tokens`) on Clients; tool-call volume per model on Models.
- **Coverage backfill**: governor/pricing/history/limits/account endpoint e2e,
  extended role-denial matrix, unwritable-DATA_DIR boot refusal.
- **README rewritten** as a usage-focused snapshot (logo, live-traffic
  screenshots in `docs/assets/`, boot banner; history/migration framing
  dropped). CHANGELOG promoted to 0.6.0; SECURITY.md supported versions moved
  to 0.6.x.

## [2026-07-04] ingest — UI-managed config store, multi-user, governor (v0.6.0)

App-level configuration moved out of env vars and into a store the app owns,
edited from a new dashboard Settings area and claimed by a first-run wizard.
New ADR [ui-managed-config-store](decisions/ui-managed-config-store.md); the
[auth-posture](decisions/auth-posture-and-dashboard-password.md) ADR gained a
v0.6.0 amendment.

- **Store**: `DATA_DIR/config.json`, version 1, atomic writes (tmp + fsync +
  rename + dir fsync), 0600, snapshot-cached (`RwLock<Arc<Config>>`). JSON not
  SQLite (kilobytes, read-mostly, single-writer; recovery = text edit; zero
  binary weight — revisit triggers recorded in the ADR). Corrupt/unreadable/
  `version>1` = **hard boot error**, never a silent fall-through to setup.
- **First run**: `/v1` → `503 setup_required`, browsers → `/setup`, a 3-step
  wizard (superuser [password ≥10] → ≥1 NIM key validated live → finish) does
  one atomic POST, mints a session, lands on the dashboard. Claim risk accepted
  (matches Grafana/Portainer; loud boot log; no claim token).
- **Multi-user**: roles superuser (undeletable admin — deletion guard only) /
  admin / user; per-key ownership; `GET /api/config` filtered server-side.
  Sessions carry `username || first8(sha256(password_hash))`, so password
  change/reset invalidates sessions and role/deletion apply next request.
  `INSECURE_NO_AUTH` retired → store `open|keyed` mode, `/v1`-only. Client keys
  are `npk_…` 128-bit secrets shown once, stored as SHA-256 digests. Passwords
  PBKDF2-HMAC-SHA256 600k, RFC 7914 vectors.
- **Env retired to 5 container vars** (`HOST`, `PORT`, `DATA_DIR`, `RUST_LOG`,
  `TRUST_PROXY`); legacy vars ignored with one boot warning; no seed-from-env,
  no migration. `configure-env` rewritten; `.env.example` shrunk.
- **Model-pressure governor** (new component page
  [governor](architecture/governor.md)): classifies NIM's per-model
  worker-exhaustion error apart from 429s and backs off the **model** (never
  benches the lane); adaptive AIMD (engage at half in-flight, +1/stable-min,
  dissolve after 30 clean min) with optional pinned caps. New metrics
  `nimproxy_worker_exhausted_total` / `nimproxy_model_inflight` /
  `nimproxy_model_limit`; a Reliability "Model pressure" card appears once
  engaged.
- **Key pool**: per-key rpm (default 40, 1–10000) replacing global
  `RPM_PER_KEY`; live `rebuild` with rate-state carryover; superuser-key pool
  floor invariant. [key-pool](architecture/key-pool.md) updated.
- Docs swept: README (quickstart→wizard, 5-var table, auth/sharing/metrics),
  `deploy-docker` (volume now holds credentials), `sharing-with-friends`
  (create-a-user flow), `client-auth` rewritten, `examples/README`, CHANGELOG.
- **Lint** — flagged in the summary: the Settings admin API (PR 4) and Settings
  UI incl. `npk_` client-key generation and role-filtered `/api/config` (PR 5)
  are not yet in `src/` on this branch; docs describe the intended v0.6.0
  surface per the plan. The store, wizard, auth, and governor **are**
  implemented.

## [2026-07-03] ingest — dashboard operator-console redesign

Presentation-only redesign of `src/dashboard.html` (data layer, metrics, and
history contracts untouched); see
[dashboard-operator-console-redesign](decisions/dashboard-operator-console-redesign.md)
and the rewritten [dashboard](architecture/dashboard.md) architecture page.

- **IA collapsed from six tabs to five**: `Overview · Models · Clients ·
  Reliability · Capacity`. Compare merged into Models as a scorecard section;
  Harnesses/Proxy/Keys renamed to Clients/Reliability/Capacity.
- **Dark-only.** The light palette and `prefers-color-scheme` handling were
  deleted — a committed design choice, not an oversight.
- **New interactions on every chart and table**: line-chart hover crosshair
  with a per-series tooltip snapped to the nearest sample, and click-to-sort
  tables (sticky header, capped height, internal scroll) whose sort order and
  scroll position survive the 3s live re-render.
- **CSP extended** in `src/main.rs`: `style-src` gained
  `https://fonts.googleapis.com`, a new `font-src` allows
  `https://fonts.gstatic.com` — needed for the Space Grotesk / Spline Sans
  Mono webfonts (system-font fallback offline). Everything else in the CSP is
  unchanged; `tests/e2e.rs` now asserts `font-src https://fonts.gstatic.com`
  alongside the existing CSP checks.
- No new `innerHTML` sink bypasses `esc()` — the redesign added interaction
  state (sort index, hover index) but no new dynamic-string interpolation
  path; see the security-invariant note in
  [dashboard](architecture/dashboard.md).

## [2026-07-03] ops — v0.5.0 first public release prep

Repo went public; cutting the first tagged release (which also gives
`release.yml` its first-ever run — GHCR multi-arch image, keyless cosign,
provenance, SBOM, GitHub Release).

- **New runbook** → [ops/release](ops/release.md): tag-driven release
  procedure, post-release verification checklist, roll-forward policy, and the
  one-time repo settings (private vulnerability reporting, auto-delete head
  branches, recommended `main` ruleset).
- Version 0.5.0; CHANGELOG `[Unreleased]` promoted. `release.yml` gained a
  tag↔Cargo.toml version guard so the OCI label and boot banner can't disagree.
- SECURITY.md now points **only** at private GitHub Security Advisories (no
  maintainer email published); CODE_OF_CONDUCT reports go via the maintainer's
  GitHub profile. README gained a release badge and a published-image
  (`ghcr.io`) quick start.

## [2026-07-02] decision + ingest — Benchmarking observability (v0.4.0)

Turned the proxy into a benchmarking / agent-observability tool. The request
body is already deserialized and every SSE event already scanned, so the
agent-behavior + model-quality signal was in hand but unread.

- **New decision** → [request-shape-metrics](decisions/request-shape-metrics.md):
  capture request shape (messages, tools, sampling params, stream/JSON mode) and
  response quality (finish_reason/truncation, tool calls, reasoning tokens, mean
  TPOT) as bounded-cardinality metrics — **counts and sizes, never content**.
  Shape is labeled by *client* (harness behavior), quality by *model*. Enums
  (`finish_reason`, `tool_choice` mode, `stream`) are clamped server-side.
- **Dashboard** rebuilt from three tabs to six persona-aligned views (Overview,
  Models, Compare, Harnesses, Proxy, Keys); see
  [dashboard](architecture/dashboard.md). Added `scorecard()`/`barRows()`
  helpers and a hash-to-hue color fallback past the six categorical slots.
- **Verified** in headless Chromium against a mock driving two named harnesses
  (opencode: tool-heavy/deep; codex: plain): all six tabs populate, the
  Harnesses view distinguishes both with distinct fingerprints, zero JS errors.
  Cardinality bounding is unit- and e2e-tested.

### Pre-merge hardening pass (same PR)

Before merge: security scan (dedicated dashboard-XSS audit + a full
`/security-review` of the branch) found **zero** vulnerabilities — every new
`innerHTML` value is escaped, every new label is a bounded enum / histogram, and
no route left the admin gate. Documentation swept and confirmed current (six
views, metric table, env vars). Test coverage extended to the buffered
`relay()` quality path, an unknown-`finish_reason`→`other` clamp, JSON mode, and
non-`auto` `tool_choice` (now **29 unit + 21 e2e**). The load harness gained
tool/JSON/sampling variety and a corrected boot command (`INSECURE_NO_AUTH`);
re-run at 80×3 = 240 requests → 0 failures, 0 upstream rate violations, balanced
across all keys, with the new metric series confirmed populated.

## [2026-07-02] ingest — Dashboard reporting polish

Client-side only (no server change, security invariants untouched); surfaces
data already collected but previously under-shown. See
[dashboard](architecture/dashboard.md).

- **Generation speed (tok/s) median/p95 trend** on the Models tab — the
  `nimproxy_tokens_per_second` histogram was only ever shown as one average
  tile. Same bucket-delta quantile machinery as TTFT, filtered to
  `source="usage"` so estimates don't drag the trend down.
- **Non-success outcomes table** on the Proxy tab — ranks every recorded
  non-200 status by count with a plain-language reason and share, so the
  status detail already in `nimproxy_requests_total` is legible instead of
  lumped into one "errors/min" line.
- **Threshold-colored gauges** — capacity (blue→amber≥70%→red≥90%) and success
  rate (green→amber<99%→red<90%) so the dials signal, not just count.
- Verified in headless Chromium against the mock: both new elements render with
  live data, gauges take the amber band under induced load/errors, zero JS
  page errors.

## [2026-07-02] ingest — Security hardening (v0.3.0)

A security review of the merged proxy found a stored-XSS chain (client-supplied
`model` → unescaped dashboard `innerHTML`), unbounded metric-label cardinality,
log injection, and an open-by-default posture (unauthenticated dashboard +
optional API auth). Hardening phase (branch `claude/security-hardening-auth`):

- **Fail-closed auth** → [auth-posture-and-dashboard-password](decisions/auth-posture-and-dashboard-password.md):
  refuse to start exposed without auth; `PROXY_API_KEYS` gates the API,
  `ADMIN_PASSWORD` gates the dashboard/`/metrics`/`/api/history` via an
  HMAC-signed session cookie (Bearer/Basic for scrapers).
- **Input hardening** → [input-sanitizing-and-xss](decisions/input-sanitizing-and-xss.md):
  sanitize + cardinality-cap the `model`/`path` labels at ingest, `esc()` every
  dashboard `innerHTML` sink, add a strict CSP + anti-framing/sniffing headers.
- Constant-time secret compares, failed-login throttle, `MAX_INFLIGHT` flood
  cap, `cargo audit` in CI, compose loopback-publish by default.
- Verified: 45 tests (26 unit + 19 e2e incl. boot posture, session flow, label
  sanitizing, security headers), a real-browser XSS check (payload rendered
  inert), secure-mode load test (300/300, 0 rate violations), `cargo audit`
  clean.

## [2026-07-02] ingest — CI caught the musl proc-macro trap

First real Docker build (in CI — this environment has no daemon) failed:
global crt-static RUSTFLAGS broke proc-macro dylibs on the musl-host alpine
builder. Fixed with an explicit `--target`; details appended to
[distroless-scratch-image](decisions/distroless-scratch-image.md).

## [2026-07-02] ingest — Initial bundle

Compiled the founding conversation into the knowledge base: project purpose
(rate-limit-respecting NIM proxy for agent harnesses), all eight design
decisions to date, three validated research findings about NIM's free tier,
six architecture pages, four runbooks, and the test strategy.

Notable facts captured at ingest time:

- Load test (100 clients, strict enforcing mock) caught 7/307 boundary-jitter
  rate violations at an exact 60s window → [window-jitter-margin](decisions/window-jitter-margin.md).
- Dashboard capacity gauge honestly read 133% during a cold-start burst drain
  before smoothing to a trailing-60s average → noted in [dashboard](architecture/dashboard.md).
- The `/v1/models` schema research killed the idea of API-sourced model
  descriptions; cards enrich from the id namespace instead.
