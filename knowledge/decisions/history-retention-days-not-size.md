---
type: Decision
title: History retention in days, not bytes
description: Time-based retention matches report intent; the original fixed snapshot-size estimate was disproven by real operation.
tags: [history, dashboard, configuration]
timestamp: 2026-07-02T00:00:00Z
---

# History retention in days, not bytes

## Context

Dashboard time-range reports need bounded server-side history. The initial
instinct was raw request logging with a size cap. The first snapshot design
instead persisted the complete Prometheus registry every five minutes and
selected a days-based retention knob using a guessed per-snapshot size.

That sizing premise was wrong. A real deployment produced a 235,598,655-byte
history file containing 7,316 samples. Metric-label cardinality and histogram
series can make one full-registry snapshot far larger than a small fixed
estimate. This observation is evidence that size is workload-dependent, not a
new universal sizing formula.

## Options

1. Size cap, pruning the oldest samples on overflow.
2. **Days-based retention (`history.days`, default 30, `0` = unlimited).**
3. Both.

## Choice

Days only, but for operator intent rather than predicted bytes. The history
exists to answer time questions ("last hour", "last month", "all retained"),
so age is the stable and understandable boundary. A byte cap makes the
available time horizon change with traffic/cardinality and complicates exact
range expectations.

The default dashboard window is a separate setting: keeping 90 days while
opening on 30 is valid, while a finite retention window shorter than the
default view is rejected. Operators can see the actual history-file size in
Settings and choose retention for their workload without the project claiming
a fixed bytes-per-day ratio.

## Consequences

- Canonical `history-v1.jsonl` lives in `DATA_DIR` (the Docker volume).
  Experimental `history.jsonl` remains opaque legacy evidence and is never a
  retention input or target. If canonical persistence fails after the config
  store is usable, the index can continue in memory with a warning; an
  unusable config/data directory remains a hard boot error.
- The knob lives in the [config store](ui-managed-config-store.md), not the
  retired `HISTORY_DAYS` env var, and is tunable live in Settings.
- Lowering finite retention trims the visible index immediately and schedules
  atomic background compaction. Safe replacement preserves the pre-cutoff full
  sample and owning boot required for exact first-window deltas. Recovery
  evidence intersecting the retained window, or a live epoch without a full
  sample, defers replacement and keeps it pending; `0` cancels finite work and
  retains canonical history without a cutoff.
- Five-minute samples bound event-time precision. The dashboard combines
  persisted rollups with a revision-bound current tail rather than a
  browser-only recent-history ring
  ([architecture](../architecture/metrics-history.md)).
- This decision does not add a size cap. Operators must monitor observed file
  size; a future size safeguard would require a new decision that states how
  it interacts with the promised retained time boundary.
