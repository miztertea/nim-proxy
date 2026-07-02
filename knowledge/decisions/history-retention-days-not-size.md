---
type: Decision
title: History retention in days, not bytes
description: HISTORY_DAYS (default 30, 0 = infinite) governs the metrics snapshot log; a size cap was rejected as dead code.
tags: [history, dashboard, configuration]
timestamp: 2026-07-02T00:00:00Z
---

# History retention in days, not bytes

## Context

Dashboard time-range reports need server-side history. The initial instinct
was raw request logging with a ~1 GB size cap. The design that shipped
instead snapshots the whole Prometheus registry every 5 minutes — the same
text `/metrics` serves — so the dashboard replays history through the exact
parser it uses for live polls.

## Options

1. Size cap (`HISTORY_MAX_MB=1024`), prune oldest on overflow.
2. **Days knob (`HISTORY_DAYS=30`, `0` = keep forever).**
3. Both.

## Choice

Days only. A snapshot is ~4 KB, so 30 days ≈ 35 MB and a 1 GB cap wouldn't
trigger for *decades* — size-pruning would be permanently dead code. Days is
also the natural unit for the reports the history exists to serve ("last
month's usage"). One knob, per the project's configuration philosophy
([configure-env](../ops/configure-env.md)).

## Consequences

- `history.jsonl` in `DATA_DIR` (Docker volume); unwritable dir degrades
  gracefully to in-memory-only with one boot-time warning.
- Compaction rewrites the file only after a day's worth of expired snapshots
  accumulates; otherwise appends.
- 5-minute resolution bounds range-view granularity; the Live view's 3s
  browser-side ring covers recent detail.
