# Explicit Request Deadline Design

## Problem

nim-proxy deliberately keeps requests patient while they wait for RPM capacity,
model-worker capacity, or retryable upstream failures. Its existing timeouts are
phase-specific:

- `max_wait` bounds queueing and retries, but not a successful generation;
- `request_timeout` bounds one buffered upstream attempt;
- `stream_idle` bounds inactivity between streamed chunks, but active traffic
  can continue indefinitely.

Production logs from the Rambler model tournament show the consequence. A
buffered Minimax request outlived its client and continued inside the proxy for
825 seconds before being logged as `200`. Buffered HTTP handlers do not receive
a reliable downstream-close notification while they are still waiting to
produce a response, so client cancellation alone cannot bound orphaned work.
Streaming requests observe downstream closure while relaying body chunks, but
not in every phase before that relay begins.

The proxy needs an opt-in, absolute wall-clock deadline that it owns and can
enforce across the request's entire lifetime.

## Goals

- Let any `/v1` caller request a shorter absolute lifetime with
  `X-Nim-Proxy-Deadline-Ms`.
- Enforce the deadline through model admission, RPM queueing, upstream sends,
  retry/error-body reads, buffered body reads, and active streaming.
- Cancel in-progress upstream work as far as dropping the Rust future and HTTP
  response permits.
- Release queue reservations, model permits, active-request gauges, and the
  global in-flight guard on expiry.
- Classify deadline expiry separately in responses, access logs, Prometheus,
  and history snapshots.
- Preserve current behavior exactly when the header is absent.

## Non-goals

- Detect every buffered client disconnect immediately. Hyper does not expose a
  general downstream-close signal before a buffered handler starts its
  response; the explicit deadline bounds this unavoidable blind spot.
- Add a server-wide generation timeout or change the defaults for existing
  clients.
- Persist per-request deadline state or expose it in the dashboard in this
  change.
- Change NIM lane pacing, retry policy, model-governor adaptation, or existing
  phase-specific timeout settings.

## Public Contract

### Request header

`X-Nim-Proxy-Deadline-Ms` is an unsigned decimal number of milliseconds. It is
accepted for every authenticated or open-mode `/v1` caller because it can only
shorten that caller's own request.

- The deadline begins when `proxy::handle` accepts the request, before auth
  delay, parsing, queueing, or upstream work.
- `0` is valid and means immediate expiry.
- Leading or trailing whitespace, signs, decimal points, non-ASCII digits,
  duplicate header values, values that do not fit `u64`, and durations that
  cannot be added to the request start instant are invalid.
- Invalid values return HTTP 400 with OpenAI-style error code
  `invalid_deadline` before acquiring a model permit or RPM slot.
- Normal authentication still runs before the invalid-header response, so the
  header does not become an authentication oracle.

### Expiry response

For buffered requests, expiry returns HTTP 504:

```json
{
  "error": {
    "message": "proxy request deadline exceeded",
    "type": "proxy_error",
    "code": "deadline_exceeded"
  }
}
```

For streaming requests, HTTP 200 has already been committed. Expiry emits one
SSE `error` event containing the same message and code when the downstream is
still connected, then closes the stream. If the downstream has already gone,
the send may fail, but cleanup and accounting still occur.

Expiry is recorded as `nimproxy_requests_total{status="deadline"}` and in the
access log with status `deadline`. A dedicated counter,
`nimproxy_deadline_exceeded_total{client,model,path}`, makes the outcome easy to
query without changing the bounded label sets already used by request metrics.
The existing history sampler automatically persists both counters.

## Internal Design

### Deadline representation and parsing

Add a small private `RequestDeadline` value in `src/proxy.rs` containing the
absolute `std::time::Instant`. Parse the header once in `handle`, using
`HeaderMap::get_all` to reject duplicates explicitly. Derive the absolute
instant with `checked_add` from the request start captured at the beginning of
the handler.

The value provides:

- its absolute instant for Tokio timeout conversion;
- an effective wait deadline equal to the earlier of the request deadline and
  `Instant::now() + cfg.max_wait`.

No new dependency is required.

### Buffered path

Keep the existing buffered retry loop and phase-specific timeout behavior.
When an explicit deadline exists, run the whole buffered future under
`tokio::time::timeout_at`.

If the timeout wins:

1. dropping the buffered future drops any pending dispatcher receiver,
   `ModelPermit`, reqwest send/body future, and active-request guard;
2. the handler's in-flight guard drops when the 504 response returns;
3. the proxy records `deadline` and increments the dedicated counter;
4. the caller receives the 504 error contract above.

The buffered loop also receives the effective wait deadline so queue/retry
logic cannot outlive a shorter explicit deadline.

### Streaming path

The streaming handler continues to return an SSE response immediately. Inside
its spawned request-lifetime task, construct the existing streaming workflow as
one future and race it against `tokio::time::sleep_until` for the explicit
deadline. Without a header, use a pending future so the existing workflow is
unchanged.

If the deadline wins, dropping the workflow future drops any dispatcher
receiver, model permit, pending reqwest send/error-body read, or active upstream
body stream. The outer task then records `deadline`, increments the counter,
attempts the terminal SSE error, and exits. Existing in-flight and active
guards remain owned by the outer task and drop on exit.

This outer race closes the gaps where the current streaming path does not watch
`tx.closed()`, while retaining the faster existing disconnect detection during
queue heartbeats and body relay.

### Interaction with existing limits

- The explicit deadline is absolute and never resets on heartbeats, response
  bytes, retries, or worker-governor activity.
- `max_wait` continues to classify ordinary admission/retry exhaustion as
  status `504`; if the explicit deadline is earlier, expiry is classified as
  `deadline`.
- `request_timeout` and `stream_idle` may fire first and retain their existing
  `502`/`stall` classifications.
- A retry cannot start after the effective wait deadline.
- The header cannot extend any existing limit.

## Security and Input Handling

The header is untrusted input. Parsing accepts only one canonical decimal
value and uses checked time arithmetic. It is never copied into logs or metric
labels. Existing client/model/path label sanitization remains the only source
of deadline metric labels.

Allowing the header in open mode does not grant additional capacity or access:
it only causes earlier cancellation of the request carrying it. Authentication
continues to precede detailed validation errors.

## Tests

Use the real-binary end-to-end harness and its scriptable upstream. Extend the
mock only with deterministic behaviors needed to hold specific phases open.

Required cases:

1. malformed and duplicate headers return `400 invalid_deadline` with zero
   upstream hits;
2. `0` expires immediately as `504 deadline_exceeded`;
3. a buffered request expires while awaiting response headers, and the next
   request proves the in-flight slot was released;
4. a buffered response body expires despite periodic upstream body activity;
5. a streaming request expires while queued or retrying and emits the terminal
   SSE error;
6. an actively producing stream expires despite chunks arriving more often
   than `stream_idle`;
7. model and in-flight permits are available immediately after expiry;
8. metrics contain request status `deadline` and the dedicated counter;
9. a request without the header retains the current patient behavior;
10. an explicit deadline cannot extend a shorter existing phase timeout.

Each behavior is implemented test-first: add one failing test, verify the
expected failure, add only enough production code to pass it, and run the
focused test again before proceeding.

## Documentation and Knowledge Ingest

Implementation updates:

- `README.md` with the opt-in header, buffered 504, streaming SSE outcome, and
  the new metric;
- `knowledge/architecture/streaming-pipeline.md` with the absolute deadline and
  cancellation ownership;
- a new decision page
  `knowledge/decisions/explicit-request-deadline.md` covering client-only,
  server-wide, and opt-in alternatives;
- `knowledge/index.md` and `knowledge/log.md` per repository maintenance rules;
- `CHANGELOG.md` under Unreleased.

## Investigation Discipline

During implementation and verification, every selected log or test slice gets
the same secondary scan used in diagnosis:

1. Why is this evidence relevant to the deadline hypothesis?
2. Does anything adjacent look anomalous even if unrelated?
3. Has the same client, model, lane, status, or duration pattern appeared
   elsewhere?
4. Which resource owner should release at this boundary, and is there evidence
   it did?
5. Is missing observability preventing a firm conclusion?

Secondary findings remain separately tracked unless evidence connects them to
the deadline defect. In particular, the July 5 three-lane 429 retry storm is
not part of this change.

