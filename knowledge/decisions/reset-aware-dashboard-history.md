---
type: Decision
title: Reset-aware dashboard history and one analytical window
description: Normalize persisted counters into a generic startup index, expose typed range/current contracts, and keep one time scope across analytical tabs.
tags: [history, dashboard, metrics, configuration]
timestamp: 2026-07-28T00:00:00Z
---

# Reset-aware dashboard history and one analytical window

## Context

The first dashboard mixed two incompatible concepts. "Live" meant a short
browser-owned sample ring and displayed process-lifetime counters, while
historical presets subtracted the first cumulative snapshot from the last.
A browser refresh lost recent chart context, a process restart reset counters,
and a default recent slice could appear empty even though the retained file
contained substantial traffic. Capacity history also substituted current key
configuration for past intervals, and availability used a hardcoded display
target.

Operators need one answer to "what time window am I looking at?" across every
analytical tab, exact totals independent of chart resolution, explicit current
exceptions, and useful first-login behavior after restart. The existing JSONL
history and embedded dashboard should remain lightweight.

## Options

1. **Repair resets only in the frontend.** Keep raw endpoints and teach the
   browser to infer epochs while rebuilding every page.
2. **Adopt an external TSDB or application database.** Delegate resets,
   retention, and range queries to a new service/schema.
3. **Build a persisted sidecar index or accept asynchronous partial
   readiness.** Serve quickly, then fill dashboard history after startup.
4. **Precompute page-specific dashboard views at startup.** Materialize
   Overview/Models/Clients/Reliability/Capacity responses independently.
5. **Build one generic reset-aware startup index and typed range/current
   contracts.**

## Choice

Choose option 5.

- Canonical JSONL records carry a store-owned random boot id, boot marker, and
  sample-time capacity. Existing valid v1 records stay readable; canonical
  file order is strict, with nondecreasing timestamps and a matching boot
  before every sample or checkpoint. Counter decreases and a
  no-counters-to-counters transition receive best-effort reset inference with
  diagnostics inside the canonical index.
- Startup synchronously scans the complete physical JSONL before listening,
  normalizes all valid samples so the retention boundary has a baseline, and
  indexes only retained gauges, deltas, and capacity. Startup latency is
  explicit and logged. No page-specific dashboard result is precomputed or
  cached.
- `GET /api/dashboard?from&to&points` returns exact totals, downsampled chart
  points, latest gauges, contemporaneous capacity, effective retained bounds,
  diagnostics, and revisions. `GET /api/dashboard/now` returns lightweight
  current metrics/config plus a counter tail bound to the persisted history
  revision.
- Exact totals are computed before chart bucketing. Point budgets affect
  presentation-series resolution only, never exact totals.
- The browser adapts range deltas into the cumulative sample shape used by all
  renderers. Overview, Models, Clients, Reliability, and Capacity share one
  following/fixed selection. Settings is outside that analytical scope.
- **Default** follows now over `dashboard.default_window_days`; **All
  retained** follows the current retained boundary; Custom and pause freeze
  the analytical range. Operational gauges and adjacent-poll rates are
  labeled **Now** and keep refreshing.
- Dashboard-window, retention, and SLO settings live in the UI-managed config
  store. Finite retention cannot be shorter than the default window.
- Range boundaries are precise to persisted sample timestamps. The system does
  not claim exact intra-sample event timing it did not record.

## Consequences

- Restarted counters contribute correctly across process epochs, while old v1
  history remains usable with inference diagnostics exposed in the range
  contract.
- First login can show the retained 30-day default immediately; a new server's
  effective window grows from its first sample.
- Exact totals are independently authoritative and callers must not derive
  report totals from presentation buckets; gauges separately use latest-value
  semantics.
- Synchronous indexing adds startup work proportional to the complete physical
  JSONL, including expired rows that still await compaction.
  In a validated real-data case, 235,598,655 bytes and 7,316 samples indexed
  in about 8.63 seconds before listen. This is an observed acceptance result,
  not a general sizing promise.
- Page rendering stays frontend-owned, avoiding a matrix of page caches and
  invalidation rules. History and config revisions provide the only
  invalidation boundaries the browser needs.
- Lowering finite retention changes visible bounds immediately and schedules
  atomic background compaction while preserving the hidden full-sample
  boundary baseline and relevant boot marker. Replacement remains pending when
  intersecting recovery evidence would otherwise be erased; unlimited
  retention cancels stale finite work
  ([retention decision](history-retention-days-not-size.md)).
- The old `/api/history` and `/dash/config.json` dashboard transports are
  removed. Prometheus `/metrics` remains for authenticated scrapers.

The v0.6.6 canonical writer establishes its new boot before listening. The
first sample/checkpoint uses that store-owned id; unchanged normalized state
writes a checkpoint, while changed state writes a full sample with the live
capacity supplied at that write. Experimental `history.jsonl` is intentionally
not an "old v1" reader input and therefore cannot contribute inferred epochs.
The store streams validation before moving records into the index, and a
runtime durable-write failure poisons later writes rather than extending a
partial tail.

See [metrics-history](../architecture/metrics-history.md),
[dashboard](../architecture/dashboard.md), and
[configuration](../ops/configure-env.md).
