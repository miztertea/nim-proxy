---
type: Log
title: Knowledge base chronology
description: Append-only record of ingests, decisions, and maintenance passes.
---

# Log

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
