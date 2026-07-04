---
type: Decision
title: Usage injection with automatic 400 fallback
description: Streaming chat requests get stream_options include_usage injected for exact token accounting; models that reject it are remembered and never injected again.
tags: [metrics, pass-through, streaming]
timestamp: 2026-07-02T00:00:00Z
---

# Usage injection with automatic 400 fallback

## Context

Token accounting ("alice generated 40k tokens on Kimi") is a headline
dashboard feature. Streamed responses only include a `usage` object when the
request asks via `stream_options: {"include_usage": true}` — most harnesses
don't. Without it, the proxy falls back to counting SSE events (~1 token per
event, labeled `source="estimate"`).

This tension sits against the project's "strict pass-through" principle.

## Options

1. Never modify bodies; live with estimates.
2. Always inject (a model that rejects the field would 400 and break the
   harness — unacceptable for a proxy whose product is reliability).
3. Opt-in env flag (data quality off by default = nobody gets it).
4. **Inject by default with automatic fallback**: on a 400 to an injected
   request, retry once with the untouched body and remember the model in a
   process-lifetime deny-set.

## Choice

Option 4, with the `strict_passthrough` Settings toggle as the kill switch for
purists.
Injection only applies to `/v1/chat/completions` with `stream: true` and no
existing `stream_options`.

## Consequences

- Exact token counts (`source="usage"`) become the norm; estimates remain
  only for models that genuinely reject the field.
- One extra upstream request the first time a rejecting model is seen, per
  process lifetime.
- Covered e2e: injection presence, 400-fallback-and-remember, and the kill
  switch each have a test.
