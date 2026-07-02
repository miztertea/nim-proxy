---
type: Runbook
title: Capacity math
description: What N clients on K keys actually does — worked through for the 50-clients/3-lanes stress question.
tags: [capacity, planning]
timestamp: 2026-07-02T00:00:00Z
---

# Capacity math

Throughput ceiling = `keys × 40` RPM. The proxy itself is never the
bottleneck (slot grants are microseconds; parked SSE connections are ~KB
each; the 3.5 MB binary shuffles bytes).

Worked example — **50 clients, 3 keys (120 RPM)**:

- **Cold start**: empty windows hold 120 instant slots ≥ 50 first requests —
  the stampede is absorbed immediately (grants spaced 25ms apart).
- **Steady state**: agents are closed-loop (each waits for its response
  before the next request), so demand ≈ `clients × 60/T` RPM for T-second
  turns. 50 clients at 30 s/turn = 100 RPM < 120: nobody queues. At
  15 s/turn = 200 RPM: the queue engages.
- **Saturated behavior**: queue depth is bounded by client count (≤ ~1
  request per client in flight); a slot frees every 0.5 s, so the 50th
  waiter waits ~25 s. FIFO spreads the slowdown evenly. Heartbeats keep
  every stream alive throughout.
- **Load shedding** begins when queue wait exceeds `MAX_WAIT_SECS` (900 s
  default) — roughly `0.67 × keys × MAX_WAIT` simultaneous waiters
  (~2,000 per 5 keys). Below that: slower, never broken.

Measured (100 clients × 3 requests, 3 enforced lanes, `scripts/loadtest.py`):
300/300 success, ~146 req/min sustained through the proxy, p95 end-to-end
~61 s (dominated by queue wait at 2.5× oversubscription), **zero upstream
rate violations**, keys balanced 96/103/101.

Non-streaming clients (some n8n nodes) can't be heartbeated — their
client-side timeout must exceed expected queue wait.
