---
type: Component
title: Streaming pipeline (src/proxy.rs)
description: SSE commitment, heartbeats, retry/failover, usage injection, idle cutoff, and token scanning.
tags: [streaming, sse, retries, metrics]
timestamp: 2026-07-02T00:00:00Z
---

# Streaming pipeline — `src/proxy.rs`

For a `stream: true` chat request:

1. **Auth + parse** once: client name, model label, stream flag, affinity
   hash, and (unless `strict_passthrough` is set in Settings)
   [usage injection](../decisions/usage-injection-auto-fallback.md) with the
   untouched body kept for fallback.
2. **Commit** to `200 text/event-stream` immediately; a spawned task owns the
   rest, feeding an mpsc channel the response body streams from.
3. **Wait loop**: queue via the [dispatcher](dispatcher.md); every heartbeat
   interval a `: heartbeat` comment goes out (send failure = client gone →
   the granted slot is never wasted on a dead request).
4. **Send + triage**: 400-after-injection → retry untouched and remember the
   model; 429/500/502/503/504 → bench the lane, `: retrying` comment, loop
   (instant failover to other lanes); other non-2xx → relay as an in-stream
   `error` event; success → pipe.
5. **Pipe with watchdog**: chunks forward verbatim while `SseScan` (a
   line-reassembling observer) counts data events and extracts the `usage`
   object. Each upstream read races two exits in a `select!`: the
   `stream_idle` timeout cuts stalled upstreams with an in-stream error
   (status label `stall`), and `tx.closed()` notices a client hang-up
   immediately — freeing the request's `max_inflight` slot at disconnect
   time rather than at the stall cutoff (with `stream_idle` 0 there is no
   cutoff, so this is what prevents hung upstreams from pinning slots until
   restart).
6. **Account**: TTFT histogram at first chunk; tokens/sec and prompt/
   completion counters at end (`source="usage"` exact, `"estimate"` =
   one-per-event fallback); one access-log line per request.

Non-streaming requests use the same wait/retry loop minus heartbeats, and
harvest `usage` from the buffered JSON. `/v1/models` GETs short-circuit to a
TTL cache with single-flight refresh.
