---
type: Component
title: Model-pressure governor (src/governor.rs)
description: A per-model concurrency gate beside the RPM dispatcher; classifies worker exhaustion separately from 429s and backs off the model, adaptively, zero-config.
tags: [governor, rate-limiting, reliability]
timestamp: 2026-07-04T00:00:00Z
---

# Model-pressure governor — `src/governor.rs`

NIM's serving stack has a **per-model worker-concurrency cap** that is
orthogonal to the per-key 40 RPM limit
(`ResourceExhausted: Worker local total request limit reached (32/32)`). At
40 RPM with 45–90s generations, steady-state in-flight is 30–60 — you saturate
the worker pool while fully RPM-legal. Crucially the cap is **model-scoped and
shared across all keys** (and all of NIM's tenants), so failing over to another
key can't help; the old behavior of benching the lane just burned healthy key
capacity.

## Classify first, then govern

`is_worker_exhausted(body)` sniffs the signature (`"Worker local total request
limit"`) on the pre-stream error response. In `proxy.rs`'s retry path this is
checked **before** generic 429/5xx handling: worker exhaustion routes to
per-model backoff and **never benches the lane**. Plain 429/5xx behavior is
unchanged ([key-pool](key-pool.md)).

## Admission gate

Beside the RPM dispatcher, a request needs an **RPM slot AND a model permit**.
`admit(model, override)` returns a `ModelPermit` (RAII — dropping it releases
the slot on every exit path) or `None` when the model is at its cap or
draining. Admission is **poll-based** (waiters re-check every 250ms), not FIFO
like the RPM queue — worker slots free stochastically as generations end, and
the RPM dispatcher downstream still serializes the actual sends. Waiting
requests ride the existing SSE heartbeat machinery, so there's no new
client-facing behavior.

## Adaptive, zero-config (AIMD)

Every model starts **ungoverned** (`limit = 0`). On the first worker-exhaustion
error the governor:

- engages at `max(1, inflight_at_error / 2)` (the failing request's permit is
  still held, so the observed count includes it),
- opens a short **2s drain gap** blocking new admissions while workers free up
  (a drain gap, not a lane-style bench),
- then **grows +1 per stable minute** (additive increase),
- and **dissolves back to ungoverned after 30 clean minutes**.

The worker pool is shared infrastructure — other tenants' load moves the real
ceiling — so a static cap would be wrong in both directions; AIMD tracks
reality. An operator can pin a fixed per-model cap in Settings
(`governor.overrides`), which skips all adaptation (exhaustion still opens the
drain gap but never rewrites the pinned cap); the whole governor can be toggled
off (`governor.enabled`). Both live in the
[config store](../decisions/ui-managed-config-store.md).

## Metrics

- `nimproxy_worker_exhausted_total{model}` — exhaustion events.
- `nimproxy_model_inflight{model}` — permits held (gauge).
- `nimproxy_model_limit{model}` — current cap; **0 = ungoverned**.

The dashboard's Reliability tab shows a **Model pressure** card built from these
— rendered only once the governor has engaged, so unaffected deployments see
zero noise. Test scaffolding: `mock_nim.py --worker-slots N` emits the real
error string at a per-model in-flight cap; `loadtest.py` asserts the governor
converges (bounded exhaustion, no thrash, no client-visible failures).
</content>
