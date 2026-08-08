---
type: Index
title: nim-proxy knowledge base
description: Catalog of every page in this Open Knowledge Format bundle.
timestamp: 2026-07-02T00:00:00Z
---

# nim-proxy knowledge base

The project's compiled memory: design decisions with their reasoning,
validated research about NVIDIA NIM, per-component architecture, and
operational runbooks. Maintenance rules live in [AGENTS.md](../AGENTS.md);
the chronology in [log.md](log.md).

This bundle records **why** — what is settled and the reasoning behind it. Work
that is *in flight* lives in [`docs/plans/`](../docs/plans/), one file per body
of work: what is decided, what is blocked, what remains, and what the last
session got wrong. Read the plan for your area before these pages; a decision
page describes a conclusion, a plan describes the live state.

## Decisions — why the design is what it is

| Page | One-liner |
|---|---|
| [sliding-window-not-token-bucket](decisions/sliding-window-not-token-bucket.md) | Exact 40-per-rolling-60s window; GCRA-style buckets allow a double burst |
| [window-jitter-margin](decisions/window-jitter-margin.md) | 61s window: load test proved delivery jitter trips a strict upstream at 60s |
| [global-fifo-dispatcher](decisions/global-fifo-dispatcher.md) | One queue for all clients; polling races starve long waiters |
| [sticky-affinity-with-spillover](decisions/sticky-affinity-with-spillover.md) | Conversations pin to one key for prefix cache; throughput beats locality when full |
| [sse-heartbeats-for-rate-waits](decisions/sse-heartbeats-for-rate-waits.md) | Commit to 200 SSE + comment heartbeats so harnesses never see a 429 |
| [history-retention-days-not-size](decisions/history-retention-days-not-size.md) | Time-based retention matches report intent; real operation disproved the fixed snapshot-size estimate |
| [reset-aware-dashboard-history](decisions/reset-aware-dashboard-history.md) | Generic startup index, explicit boot epochs, exact typed rollups, and one analytical window |
| [distroless-scratch-image](decisions/distroless-scratch-image.md) | Static musl binary with baked-in TLS roots; FROM scratch, non-root, --health probe |
| [usage-injection-auto-fallback](decisions/usage-injection-auto-fallback.md) | Inject stream_options for exact tokens; 400 → retry untouched and remember |
| [auth-posture-and-dashboard-password](decisions/auth-posture-and-dashboard-password.md) | Fail closed without auth; API keys + a shared-password dashboard session |
| [input-sanitizing-and-xss](decisions/input-sanitizing-and-xss.md) | Sanitize client `model`/`path` labels; escape + CSP the dashboard (XSS/cardinality/log-injection) |
| [request-shape-metrics](decisions/request-shape-metrics.md) | Capture agent-behavior & quality signal as bounded metrics — counts, never content — for benchmarking |
| [dashboard-operator-console-redesign](decisions/dashboard-operator-console-redesign.md) | 6→5 tabs (Compare merged in), dark-only palette, and window-halves delta chips; local presentation assets supersede the original CDN-font choice |
| [ui-managed-config-store](decisions/ui-managed-config-store.md) | App config moves from env into a JSON store edited from the dashboard; first-run wizard, multi-user + per-key ownership, no encryption at rest |
| [explicit-request-deadline](decisions/explicit-request-deadline.md) | Opt-in wall-clock bound cancels queue/retry/generation work without weakening patient defaults |
| [dependency-update-cooldown](decisions/dependency-update-cooldown.md) | Routine dependency updates wait seven days; security updates remain immediate |
| [lane-cooldown-naming](decisions/lane-cooldown-naming.md) | `bench` → `cooldown`; renames the metric and accepts a bounded history gap over a permanent alias |
| [no-estimated-savings-metric](decisions/no-estimated-savings-metric.md) | "Dollars saved" needed per-model published rates to be honest; deleted rather than faked |
| [message-catalog-and-escaping](decisions/message-catalog-and-escaping.md) | Catalog ids for DOM sinks; inert descriptors resolved only by fixed-markup HTML escaping |
| [intl-formatting](decisions/intl-formatting.md) | Cached `Intl` formatters keyed to the catalog locale; CSS percentages deliberately excluded |
| [locale-guards](decisions/locale-guards.md) | `en-XA` pseudolocale, `locale-v1` validator, untagged-string lint — every check with a negative fixture written first |
| [plural-categories-not-ternaries](decisions/plural-categories-not-ternaries.md) | Counted labels select a CLDR category via `Intl.PluralRules`; all six forms live in the source catalog because `locale-v1` requires id parity |
| [render-gate](decisions/render-gate.md) | A dependency-free headless gate that starts the real binary, loads served assets, and drives pages against Rust-owned typed API fixtures |
| [standard-vocabulary](decisions/standard-vocabulary.md) | Every user-visible term mapped to standard Grafana/HAProxy vocabulary; the frozen-token and retired-term halves are enforced by checks |
| [typed-responses-and-generated-openapi](decisions/typed-responses-and-generated-openapi.md) | Response bodies become structs whose declaration order is the wire order; `openapi.json` generated by utoipa, spec file only — no CDN-backed UI |
| [okf-query-ingest-lint](decisions/okf-query-ingest-lint.md) | Stable agent guide plus Query → Ingest → Lint over the semantic index, concept graph, chronology, and Git history |

## Research — validated external facts

| Page | One-liner |
|---|---|
| [nim-free-tier-40rpm-no-credits](research/nim-free-tier-40rpm-no-credits.md) | NVIDIA staff: trial usage is not credit-based, ~40 RPM per key governs |
| [nim-kv-cache-reuse](research/nim-kv-cache-reuse.md) | NIM supports prefix caching (~2x TTFT); hosted scope undocumented, likely per-account |
| [nim-models-endpoint-schema](research/nim-models-endpoint-schema.md) | /v1/models returns only id/created/object/owned_by — card visuals need local enrichment |

## Architecture — how each component works

| Page | One-liner |
|---|---|
| [key-pool](architecture/key-pool.md) | Per-key sliding-window lanes; least-loaded selection; cooldown on upstream backoff |
| [dispatcher](architecture/dispatcher.md) | Global FIFO slot queue; abandoned-waiter slot return; affinity accounting |
| [governor](architecture/governor.md) | Per-model concurrency gate; classifies worker exhaustion apart from 429s and backs off the model, adaptively |
| [streaming-pipeline](architecture/streaming-pipeline.md) | Heartbeats, retry/failover, absolute deadlines, idle timeout, and bounded SSE observation |
| [nim-observations](architecture/nim-observations.md) | Private typed response observations validated from sanitized wire evidence without relay interference |
| [metrics-history](architecture/metrics-history.md) | Prometheus registry + recoverable canonical JSONL, query-scoped completeness, exact rollups, and atomic retention |
| [dashboard](architecture/dashboard.md) | Split embedded operator console; one honest complete/partial/unavailable window across 5 tabs plus clearly scoped Now values |
| [presentation-layer](architecture/presentation-layer.md) | Compile-time public/operator pages and assets with strict same-origin CSP, explicit gates, and served-byte browser proof |
| [client-auth](architecture/client-auth.md) | `/v1` client keys (open/keyed) + store-backed multi-user dashboard sessions; fail-closed posture |
| [http-trust-boundary-map](architecture/http-trust-boundary-map.md) | Every live route mapped across phase, authentication, roles, wire types, side effects, callers, OpenAPI, and real-request proof |

## Operations — runbooks

| Page | One-liner |
|---|---|
| [deploy-docker](ops/deploy-docker.md) | Compose, volume, healthcheck, hardening flags |
| [configure-env](ops/configure-env.md) | Compose publishing, the 5 container env vars, Settings, and lockout recovery |
| [sharing-with-friends](ops/sharing-with-friends.md) | Create-a-user multi-user setup, key etiquette, ToS positioning |
| [capacity-math](ops/capacity-math.md) | What N clients on K keys actually does (the 50-clients/3-lanes analysis) |
| [nim-response-capture](ops/nim-response-capture.md) | Bounded four-case NIM capture, deterministic sanitization, privacy review, and exact raw cleanup |

## Testing

| Page | One-liner |
|---|---|
| [test-strategy](testing/test-strategy.md) | Unit / e2e / load layers, what each catches, how to run them |
