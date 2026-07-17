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

## Decisions — why the design is what it is

| Page | One-liner |
|---|---|
| [sliding-window-not-token-bucket](decisions/sliding-window-not-token-bucket.md) | Exact 40-per-rolling-60s window; GCRA-style buckets allow a double burst |
| [window-jitter-margin](decisions/window-jitter-margin.md) | 61s window: load test proved delivery jitter trips a strict upstream at 60s |
| [global-fifo-dispatcher](decisions/global-fifo-dispatcher.md) | One queue for all clients; polling races starve long waiters |
| [sticky-affinity-with-spillover](decisions/sticky-affinity-with-spillover.md) | Conversations pin to one key for prefix cache; throughput beats locality when full |
| [sse-heartbeats-for-rate-waits](decisions/sse-heartbeats-for-rate-waits.md) | Commit to 200 SSE + comment heartbeats so harnesses never see a 429 |
| [history-retention-days-not-size](decisions/history-retention-days-not-size.md) | 5-min ~4KB snapshots make days the right knob; a size cap would never trigger |
| [distroless-scratch-image](decisions/distroless-scratch-image.md) | Static musl binary with baked-in TLS roots; FROM scratch, non-root, --health probe |
| [usage-injection-auto-fallback](decisions/usage-injection-auto-fallback.md) | Inject stream_options for exact tokens; 400 → retry untouched and remember |
| [auth-posture-and-dashboard-password](decisions/auth-posture-and-dashboard-password.md) | Fail closed without auth; API keys + a shared-password dashboard session |
| [input-sanitizing-and-xss](decisions/input-sanitizing-and-xss.md) | Sanitize client `model`/`path` labels; escape + CSP the dashboard (XSS/cardinality/log-injection) |
| [request-shape-metrics](decisions/request-shape-metrics.md) | Capture agent-behavior & quality signal as bounded metrics — counts, never content — for benchmarking |
| [dashboard-operator-console-redesign](decisions/dashboard-operator-console-redesign.md) | 6→5 tabs (Compare merged in), dark-only palette, webfonts via Google Fonts CDN under CSP, window-halves delta chips |
| [ui-managed-config-store](decisions/ui-managed-config-store.md) | App config moves from env into a JSON store edited from the dashboard; first-run wizard, multi-user + per-key ownership, no encryption at rest |
| [explicit-request-deadline](decisions/explicit-request-deadline.md) | Opt-in wall-clock bound cancels queue/retry/generation work without weakening patient defaults |

## Research — validated external facts

| Page | One-liner |
|---|---|
| [nim-free-tier-40rpm-no-credits](research/nim-free-tier-40rpm-no-credits.md) | NVIDIA staff: trial usage is not credit-based, ~40 RPM per key governs |
| [nim-kv-cache-reuse](research/nim-kv-cache-reuse.md) | NIM supports prefix caching (~2x TTFT); hosted scope undocumented, likely per-account |
| [nim-models-endpoint-schema](research/nim-models-endpoint-schema.md) | /v1/models returns only id/created/object/owned_by — card visuals need local enrichment |

## Architecture — how each component works

| Page | One-liner |
|---|---|
| [key-pool](architecture/key-pool.md) | Per-key sliding-window lanes; least-loaded selection; cooldown benching |
| [dispatcher](architecture/dispatcher.md) | Global FIFO slot queue; abandoned-waiter slot return; affinity accounting |
| [governor](architecture/governor.md) | Per-model concurrency gate; classifies worker exhaustion apart from 429s and backs off the model, adaptively |
| [streaming-pipeline](architecture/streaming-pipeline.md) | Heartbeats, retry/failover, absolute deadlines, idle timeout, SSE usage scanning |
| [metrics-history](architecture/metrics-history.md) | Prometheus registry + 5-min snapshot history replayed by the dashboard |
| [dashboard](architecture/dashboard.md) | Single embedded HTML; dark operator console; 5 tabs; hover charts & sortable tables |
| [client-auth](architecture/client-auth.md) | `/v1` client keys (open/keyed) + store-backed multi-user dashboard sessions; fail-closed posture |

## Operations — runbooks

| Page | One-liner |
|---|---|
| [deploy-docker](ops/deploy-docker.md) | Compose, volume, healthcheck, hardening flags |
| [configure-env](ops/configure-env.md) | The 5 container env vars; everything else lives in the Settings UI; lockout recovery |
| [sharing-with-friends](ops/sharing-with-friends.md) | Create-a-user multi-user setup, key etiquette, ToS positioning |
| [capacity-math](ops/capacity-math.md) | What N clients on K keys actually does (the 50-clients/3-lanes analysis) |

## Testing

| Page | One-liner |
|---|---|
| [test-strategy](testing/test-strategy.md) | Unit / e2e / load layers, what each catches, how to run them |
