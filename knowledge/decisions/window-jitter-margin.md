---
type: Decision
title: 61-second window plus paced grants
description: Load testing proved an exact 60s window trips a strict upstream via delivery jitter; pad the window 1s and space grants 25ms apart.
tags: [rate-limiting, load-testing, pool, dispatcher]
timestamp: 2026-07-02T00:00:00Z
---

# 61-second window plus paced grants

## Context

The first 100-client load test run (strict enforcing mock, exact 60s local
window) produced **7 upstream rate violations out of 307 requests (~2%)**.
The proxy stamps its window at slot-grant time, but the upstream clocks
*arrivals*. Two effects compress upstream-observed gaps below 60s:

1. **Delivery jitter** — a boundary-timed request whose predecessor was
   delayed more than it lands "early" in the upstream's window.
2. **Cold-start stampede** — an empty pool can grant `lanes x rpm` slots
   instantly (120 simultaneous connects in the test), and accept-queue
   delay skews the early requests' arrival timestamps by over a second.

## Options

1. Ignore it — the 429s are absorbed by retry anyway.
2. Pad the window (60s → 61s).
3. Pace grants to kill the stampede.
4. Reduce RPM headroom (40 → 39).

## Choice

**2 + 3 together.** Padding alone cut violations 7 → 2 (jitter handled,
stampede not). Adding a 25ms minimum gap between consecutive slot grants in
the [dispatcher](../architecture/dispatcher.md) spreads a cold-start burst
over ~3s and got to **zero violations**. 25ms = 2,400 grants/min, far above
any realistic key pool's aggregate RPM, so throughput is unaffected;
the window pad costs ~1.6% of peak sustained rate.

Option 1 was rejected because "obey the speed limit" is the product; option 4
costs 2.5% throughput without addressing arrival-time skew at all.

## Consequences

- `scripts/loadtest.py` treats a single violation as failure, so regressions
  here fail CI-able checks.
- Real NIM presumably tolerates boundary-adjacent arrivals better than the
  strict mock; the margin means we never have to find out.
