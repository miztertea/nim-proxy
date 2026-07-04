---
type: Component
title: Metrics & history (src/history.rs)
description: Prometheus registry at /metrics; 5-minute full-registry snapshots persisted for dashboard time-range queries.
tags: [metrics, history, prometheus]
timestamp: 2026-07-02T00:00:00Z
---

# Metrics & history — `src/history.rs`

**Live metrics**: a `metrics-exporter-prometheus` registry rendered at
`GET /metrics` (series list in the README). Custom histogram buckets are set
for TTFT, tokens/sec, queue wait, and upstream latency; the dashboard
computes median/p95 from bucket deltas client-side. v0.6.0 adds the
[governor](governor.md) series `nimproxy_worker_exhausted_total{model}` and the
gauges `nimproxy_model_inflight{model}` / `nimproxy_model_limit{model}`
(0 = ungoverned).

**History**: every 5 minutes a sampler appends `(unix_ts, render())` — the
*entire registry as Prometheus text*, ~4 KB — to memory and to
`DATA_DIR/history.jsonl`. The elegance: history entries are byte-identical in
shape to a live `/metrics` poll, so the dashboard replays ranges through the
same parser and the same rendering code paths. No second schema.

- Retention: `history.days` in the [config store](../decisions/ui-managed-config-store.md)
  (default 30, 0 = forever; tunable live from Settings via an `AtomicU64`) —
  [why days, not bytes](../decisions/history-retention-days-not-size.md).
- `GET /api/history?from&to` filters and stride-samples to ≤288 points.
- Unwritable `DATA_DIR` → one boot warning, memory-only operation.
- Compaction rewrites the file after ~a day's worth of expired entries.
- `HISTORY_SAMPLE_SECS` exists as an undocumented test knob; 5 minutes is
  the contract.

Counters are cumulative, so range views compute *windowed* values as
`last − first` — which is how the dashboard's "in range" report semantics
work for every tile, card, and table.
