---
type: Component
title: Key pool (src/pool.rs)
description: One lane per NIM key; exact sliding-window limiter, least-loaded selection, cooldown benching, releasable reservations.
tags: [pool, rate-limiting]
timestamp: 2026-07-02T00:00:00Z
---

# Key pool — `src/pool.rs`

Each API key is a **lane** holding a `VecDeque<Instant>` of send timestamps
(the sliding window) and a `cooldown_until` instant (benching).

- **Reserve** (`reserve(prefer)`): the preferred lane wins if it has capacity
  ([affinity](../decisions/sticky-affinity-with-spillover.md)); otherwise the
  ready lane with the fewest in-window sends (spreads bursts ~evenly).
  Reservation pushes the timestamp immediately, so concurrent callers can't
  oversubscribe. Returns `Ready { lane, key, stamp, sticky }` or
  `Wait(duration)` until the soonest slot.
- **Window** is 61s for a 60s upstream limit — see
  [window-jitter-margin](../decisions/window-jitter-margin.md).
- **Penalize**: an upstream 429/5xx benches the lane (`Retry-After` honored,
  defaults 10s for 429, 5s for connect errors). Benched lanes are skipped;
  other lanes absorb traffic.
- **Release**: a reservation granted to a client that vanished while queued
  is removed from the window by its stamp, returning the slot.

Rate state is in-memory only: one proxy instance per key set (documented
limitation), and windows reset on restart (post-restart burst 429s are
absorbed by retry).
