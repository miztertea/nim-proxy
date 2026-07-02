---
type: Decision
title: Exact sliding window, not a token bucket
description: The per-key limiter is a rolling-60s window of send timestamps, hand-rolled instead of a GCRA crate.
tags: [rate-limiting, pool]
timestamp: 2026-07-02T00:00:00Z
---

# Exact sliding window, not a token bucket

## Context

NIM enforces ~40 requests per rolling minute per key
([research](../research/nim-free-tier-40rpm-no-credits.md)). The proxy's
whole job is never tripping that limit, so the local limiter's shape must
match the upstream's.

## Options

1. **`governor` crate (GCRA / token bucket)** — prebuilt, battle-tested.
2. **Hand-rolled sliding window** — a `VecDeque<Instant>` of send timestamps
   per lane, pruned to the window.

## Choice

Hand-rolled sliding window (~30 lines in [key-pool](../architecture/key-pool.md)).

The disqualifier for GCRA: with `Quota::per_minute(40)` (burst 40, refill one
per 1.5s), a cold start permits ~79 requests inside the first 60s — the burst
plus the refills. A rolling-window upstream sees that as a 2x violation and
429-storms. The sliding window allows at most 40 in *any* 60s span by
construction.

## Consequences

- Semantics match NIM exactly; the load test's enforcing mock validates this.
- We own ~30 lines of concurrency-sensitive code, covered by unit tests
  (spread, penalty, release, sticky flag).
- A later refinement added a jitter margin to the window length — see
  [window-jitter-margin](window-jitter-margin.md).
