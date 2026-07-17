---
type: Decision
title: Opt-in absolute request deadlines
description: A caller-supplied deadline bounds the whole proxy lifecycle independently of heartbeats and phase-specific timeouts.
tags: [timeouts, cancellation, streaming, benchmarking]
timestamp: 2026-07-16T00:00:00Z
---

# Opt-in absolute request deadlines

## Context

The proxy is deliberately patient: it waits through RPM saturation, model
worker pressure, and retryable upstream failures. Its existing limits are
phase-specific. `max_wait` bounds admission and retries, `request_timeout`
bounds one buffered upstream attempt, and `stream_idle` bounds gaps between
stream chunks. None bounds an actively producing request end to end.

Rambler's NIM model tournament exposed the operational consequence: a buffered
request continued upstream for 825 seconds after its client had timed out.
Buffered handlers cannot reliably observe a downstream disconnect before they
have a response to write. Streaming handlers can observe many disconnects, but
heartbeats and regular chunks also keep ordinary socket read timeouts alive.

## Options

1. Rely on client timeouts. Rejected: activity resets read timeouts, and a
   disconnected buffered client does not cancel the proxy's upstream future.
2. Add one server-wide maximum. Rejected: it would weaken the patient default
   for agent harnesses and make one duration serve unrelated workloads.
3. Accept an opt-in absolute request deadline. It lets bounded workloads state
   their policy without changing other clients.

## Choice

Option 3. Any `/v1` caller may send `X-Nim-Proxy-Deadline-Ms` with one unsigned
decimal millisecond value. The clock starts when the handler accepts the
request and never resets. The buffered request future or spawned streaming
workflow races that absolute instant; when the timer wins, dropping the work
future cancels reqwest work and releases dispatcher, governor, active-request,
and in-flight ownership through existing RAII guards.

Buffered expiry returns HTTP 504 with `deadline_exceeded`. A stream has already
committed HTTP 200, so it gets a terminal SSE error with that code when channel
capacity permits; delivery is best-effort so a slow downstream cannot delay
cleanup. Invalid headers fail with `invalid_deadline` after normal client auth
and before upstream admission.

## Consequences

- Benchmarks get a true wall-clock bound through queueing, retries, and active
  generation.
- Buffered work that cannot notice client disconnect is bounded by a deadline
  the proxy can enforce itself.
- Requests without the header behave exactly as before.
- Deadline expiry has request status `deadline` and a dedicated
  `nimproxy_deadline_exceeded_total{client,model,path}` counter.
- A caller can only shorten its own work, so the header is safe in open mode.
- The feature does not provide immediate buffered-disconnect detection; that
  remains a transport limitation.

See the [streaming pipeline](../architecture/streaming-pipeline.md) for task
ownership and cancellation flow.
