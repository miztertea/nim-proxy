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

## Per-key rpm and live rebuild (v0.6.0)

Each lane's limit is **per-key** now (`NimKey.rpm`, default 40, range
1–10000 — covers paid tiers / self-hosted NIM), not one global `RPM_PER_KEY`.
Keys, their rpms, and their enabled/disabled state live in the
[config store](../decisions/ui-managed-config-store.md); a Settings save calls
`Pool::rebuild(keys)` under the pool write lock, feeding it **enabled keys
only**. Rebuild **carries over per-lane rate state** (`sent` window,
`cooldown_until`) by key-string match: a kept key keeps its in-window counts
(can't be double-spent across a swap), a lowered rpm is honored immediately
(`try_take` checks `sent.len() < rpm` live), and a disabled key keeps its stored
state so it re-enables **warm**. Grants carry their originating `Arc<Pool>`
(`Slot { pool, lane, key }`) so late bench/release after a swap route to the
pool that granted them — no index-out-of-bounds, late ops on a retired pool are
benign. **Invariant**: the superuser always owns ≥1 enabled key, pinning the
pool floor (removing/disabling the last one is a 400), so the pool can never
empty. Per-model worker-concurrency limits are a separate concern — see the
[governor](governor.md).

Rate state is in-memory only: one proxy instance per key set (documented
limitation), and windows reset on restart (post-restart burst 429s are
absorbed by retry).
