---
type: Component
title: NIM response observations
description: Private bounded typed observations of buffered and SSE NIM responses without proxy relay interference.
tags: [nim, observations, sse, metrics, privacy]
timestamp: 2026-08-01T00:00:00Z
---

# NIM response observations

`src/observation.rs` classifies only response metadata needed by the existing
metrics: five usage fields, per-choice finish results, and tool-call counts.
It is a private crate component. It neither changes response bytes nor exposes
an API, metric, or dashboard availability field; Task 16 owns observation
quality telemetry.

## Field and completion rules

The observer accepts only JSON integers in `0..=u64::MAX`. Missing or null
`usage` is unavailable; present malformed values are invalid. Repeated equal
values collapse, while conflicts and valid/invalid mixtures invalidate just
that field. Total is invalid on checked prompt-plus-completion overflow or when
smaller than their measured sum. Cached/reasoning require measured bounded
prompt/completion parents. Unrelated measured siblings survive an invalid
relationship.

Only completed streams without measured completion can estimate completion.
The estimate is the count of parsed events with a nonempty valid indexed
`choices` array and no non-null terminal reason. Zero count is unavailable.
Malformed JSON/UTF-8, `[DONE]`, comments, errors, usage-only/empty-choice,
terminal, invalid-terminal, disconnected, and truncated input do not estimate.

Buffered choices sum `message.tool_calls` arrays. Streamed choices deduplicate
`(choice_index, tool_call_index)` fragments. Known finish strings map to the
bounded enum; unknown strings are `Other`. Malformed choice/tool shapes are
invalid, and valid but terminal-less choices are unavailable.

## Transport and privacy boundary

Buffered observation reads the body after it is already available for relay.
SSE observation receives the same chunks only after they are read for relay;
it stores at most 1 MiB across its current unfinished line/event, joins
standard multi-line `data:` values with LF for parsing only, and drops event
data immediately after classification. Over-bound, malformed, and invalid
UTF-8 events are unobservable. No prompt, completion text, model identity, or
raw event body is retained after classification.

The proxy records only finalized measured prompt/reasoning/tool/finish values
and measured or estimated completion using its existing metric names and
labels. Invalid/unavailable values are omitted there. Total and cached have no
existing token metric.

Every finalized response also emits exactly five
`nimproxy_usage_observations_total` counter outcomes, one for each fixed field
(`prompt_tokens`, `completion_tokens`, `total_tokens`, `cached_tokens`, and
`reasoning_tokens`) with one closed result label (`measured`, `estimated`,
`unavailable`, or `invalid`). The canonical observation-owned mapping is the
only classification-to-counter boundary, so it neither reparses response bytes
nor repeats an SSE event. Its maximum cardinality is 20 series and it carries
no request, model, client, provider, or upstream-content label. Successful
buffered and completed streams use their final typed values; disconnect,
truncation, idle cutoff, and unterminated completed SSE all finalize five
unavailable outcomes before the proxy preserves their existing request/error
behavior. Rejected/retried responses and failed buffered body reads have no
final observed body and emit none. See the [streaming pipeline](streaming-pipeline.md),
[metrics history](metrics-history.md), [usage injection decision](../decisions/usage-injection-auto-fallback.md),
[capture runbook](../ops/nim-response-capture.md), and
[test strategy](../testing/test-strategy.md).
