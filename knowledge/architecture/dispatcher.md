---
type: Component
title: Dispatcher (src/dispatch.rs)
description: Single-task FIFO queue granting pool slots in arrival order, with grant pacing and affinity accounting.
tags: [dispatcher, fairness]
timestamp: 2026-07-02T00:00:00Z
---

# Dispatcher — `src/dispatch.rs`

A single tokio task owns slot allocation
([why FIFO](../decisions/global-fifo-dispatcher.md)):

1. Requests call `acquire(deadline, prefer)` → a `Waiter` (oneshot reply +
   deadline + preferred lane) joins an unbounded mpsc queue. A queue-depth
   gauge tracks occupancy via a drop-guard so every exit path decrements.
2. The dispatcher serves waiters strictly in order: it polls
   [`Pool::reserve`](key-pool.md), sleeping in ≤500ms slices so it notices
   abandoned waiters, and drops a waiter whose soonest slot lies beyond its
   deadline (caller turns that into a 504 / in-stream error).
3. On grant it records `nimproxy_affinity_total{result}` (sticky/spill/none)
   and sleeps `GRANT_GAP` (25ms) before the next waiter — burst-concurrency
   pacing from [window-jitter-margin](../decisions/window-jitter-margin.md).
4. If the reply channel is dead (client hung up between queueing and grant),
   the slot goes back via `Pool::release`.

Callers wait on the oneshot with a `select!` against a heartbeat tick, so
streaming requests emit keepalives the whole time they're queued
([sse-heartbeats](../decisions/sse-heartbeats-for-rate-waits.md)).
