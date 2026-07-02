---
type: Decision
title: Global FIFO dispatcher for slot allocation
description: All client connections queue through one dispatcher task; slots are granted in arrival order.
tags: [dispatcher, fairness, multi-client]
timestamp: 2026-07-02T00:00:00Z
---

# Global FIFO dispatcher for slot allocation

## Context

Multiple harnesses (several OpenCode instances, n8n flows, Codex sessions)
share one proxy. The first implementation had each request poll the pool in a
sleep/retry loop: when all lanes were busy, whichever waiter woke first won
the slot. Under contention that's a wakeup race — a request could wait
minutes while newcomers kept winning.

## Options

1. Keep polling, add randomized backoff (probabilistic fairness).
2. A fair async mutex "turnstile" (blocks heartbeats while queued).
3. **A single dispatcher task with an unbounded FIFO queue of waiters**,
   replying through oneshot channels.

## Choice

Option 3 ([dispatcher](../architecture/dispatcher.md)). Strict arrival-order
fairness; waiters stay unblocked while queued (so streaming requests keep
emitting [heartbeats](sse-heartbeats-for-rate-waits.md)); a client that
disconnects while queued drops its oneshot receiver and the granted slot is
returned to the pool via `Pool::release`.

## Consequences

- Queue depth is naturally bounded by client behavior (closed-loop agents =
  one queued request per client), so unbounded mpsc is safe.
- The dispatcher is the single point where affinity outcomes
  (sticky/spill/none) and [grant pacing](window-jitter-margin.md) are applied.
- Fail-fast: if the soonest possible slot lies beyond the waiter's deadline,
  the waiter is dropped immediately (client gets a 504 / in-stream error)
  rather than waiting pointlessly.
