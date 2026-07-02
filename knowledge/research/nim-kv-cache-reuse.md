---
type: Research Finding
title: NIM supports KV cache reuse (prefix caching)
description: ~2x TTFT improvement on repeated prompt prefixes; hosted-tier enablement undocumented, isolation implies per-account scope.
tags: [nim, prefix-cache, performance]
timestamp: 2026-07-02T00:00:00Z
resource: https://docs.nvidia.com/nim/large-language-models/latest/kv-cache-reuse.html
---

# NIM supports KV cache reuse (prefix caching)

- NIM (the serving software) supports **KV cache reuse / prefix caching**
  (`NIM_ENABLE_KV_CACHE_REUSE=1` when self-hosting). NVIDIA cites **~2x
  faster time-to-first-token** when ≥90% of the prompt prefix repeats across
  requests.
- The agent-harness pattern — identical system prompt plus a growing
  conversation history — is the ideal case NVIDIA describes.
- Whether the **hosted** `integrate.api.nvidia.com` endpoints enable it is
  not documented. NVIDIA's guidance on
  [securing the KV cache](https://developer.nvidia.com/blog/structuring-applications-to-secure-the-kv-cache/)
  says shared caches must be scoped per user/account to prevent cross-tenant
  leakage — so if enabled, a key switch almost certainly means a cold cache.

Implications:

- Motivates [sticky-affinity-with-spillover](../decisions/sticky-affinity-with-spillover.md).
- Never a correctness concern: the chat completions API is stateless, so
  crossing keys can only cost latency, never break a conversation.
- The dashboard's sticky-vs-spill TTFT data can eventually answer the
  hosted-enablement question empirically.
