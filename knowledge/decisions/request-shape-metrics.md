---
type: Decision
title: Capture request-shape & response-quality metrics (counts, never content)
description: The proxy already deserializes every request and streams every response, so agent-behavior and model-quality signal is in hand but unread. Record it as bounded-cardinality metrics to turn the proxy into a benchmarking / agent-observability tool.
tags: [metrics, benchmarking, observability, cardinality, privacy]
timestamp: 2026-07-02T00:00:00Z
---

# Capture request-shape & response-quality metrics (counts, never content)

## Context

nim-proxy sits in the request path for every harness (OpenCode, Codex, n8n)
and every model, so it can *see* what agents actually do: how many tools they
offer, how deep their conversations run, how they set sampling params, where
models truncate, how much a reasoning model "thinks". The
[streaming pipeline](../architecture/streaming-pipeline.md) already
deserializes the request body once into a `serde_json::Value` (`proxy.rs`,
`handle`) and scans every SSE event / buffered `usage` object. That signal was
being discarded — the dashboard only ever showed tokens and latency.

The tension: capturing more is nearly free, but the proxy is security-hardened
(see [input-sanitizing-and-xss](input-sanitizing-and-xss.md)) and metric-label
cardinality is a real exposure — an attacker who controls a label value can
explode the registry.

## Options

1. **Do nothing extra** — keep tokens/latency only. Leaves the most interesting
   data (agent behavior, truncation, reasoning cost) invisible.
2. **Capture everything as labels** — e.g. `temperature="0.7"`, `messages="12"`.
   Trivial to read, but every distinct value is a new time series: unbounded
   cardinality, a registry-explosion vector.
3. **Capture as bounded metrics** — counts/sizes/params go to **histograms**
   (fixed buckets, no per-value series); categorical signal goes to **labels
   only when the value set is a fixed enum** (`finish_reason`, `tool_choice`
   mode, `stream` bool), clamped server-side so an odd upstream value collapses
   to `other`. Record **counts and sizes only — never message content.**

## Choice

Option 3. New metrics, split by the label that makes them useful:

- **Per client (harness behavior)** — `nimproxy_request_messages`,
  `nimproxy_request_tools`, `nimproxy_request_max_tokens`,
  `nimproxy_request_temperature` (histograms); `nimproxy_stream_requests_total`
  `{stream}` and `nimproxy_json_mode_total`. Powers the Harnesses view.
- **Per model (quality)** — `nimproxy_finish_reason_total` `{reason}` (→
  truncation rate), `nimproxy_tool_calls_total`, `nimproxy_reasoning_tokens_total`,
  `nimproxy_tpot_seconds` (mean inter-token latency). Powers Models & Compare.
- **Global enum** — `nimproxy_tool_choice_total` `{mode}`.

Request shape is read from the already-parsed body at `Ctx` construction, so no
second deserialize. `SseScan` was broadened to parse only the events that carry
`usage`, a concrete `finish_reason`, or `tool_calls` (plain content deltas are
still skipped). `finish_reason` and `tool_choice` are clamped to known enums.

## Consequences

- The dashboard becomes a benchmarking / agent-observability tool: six
  persona-aligned views (see [dashboard](../architecture/dashboard.md)),
  including a head-to-head model Compare scorecard and a Harnesses view that
  fingerprints each agent's tool intensity, conversation depth, and sampling.
- Cardinality stays bounded by construction: no client-controlled free-text
  reaches a label; params live in histogram buckets; enums are clamped. Verified
  by unit tests (`finish_label`, `tool_choice_mode`, `SseScan`) and an e2e that
  asserts the `stream` label is only ever `true`/`false`.
- Privacy posture is explicit and documented: **counts and sizes, never
  message content.** Nothing that could carry prompt text is recorded.
- Shape is labeled by *client*, not *model* — the harness determines tool use
  and sampling, so per-harness is the meaningful cut and avoids a
  client×model×buckets cardinality blow-up.
