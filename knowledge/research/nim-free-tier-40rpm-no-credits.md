---
type: Research Finding
title: "NIM free tier: no credits, ~40 RPM per key"
description: NVIDIA staff confirmed build.nvidia.com trial usage is governed by a rate limit, not inference credits.
tags: [nim, rate-limits, upstream]
timestamp: 2026-07-02T00:00:00Z
resource: https://forums.developer.nvidia.com/t/clarity-on-nim-api-free-tier-rate-limit-increases/369624
---

# NIM free tier: no credits, ~40 RPM per key

Validated 2026-07-02 (the founding claim of the whole project):

- NVIDIA staff state on the developer forums that **build.nvidia.com trial
  usage is not credit-based** — it is governed by a rate limit that depends
  on model, use case, and current overall traffic, with a practical
  community baseline of **~40 requests per minute per key**. (Older
  documentation describing 1,000 signup credits reflects the previous
  scheme.)
- There is no self-service rate increase; forum threads requesting 40 → 200
  RPM (numerous through mid-2026) are told requests in the forum do not
  grant increases.
- Keys are issued per developer account; registration requires a unique
  email **and** phone number, which bounds how many lanes one person can
  legitimately hold.
- The API is OpenAI-compatible at `https://integrate.api.nvidia.com/v1`
  (Bearer `nvapi-...` auth).

Implication: with no credit meter, **the rate limit is the only scarcity**,
so a proxy that perfectly obeys it turns the free tier into an effectively
unlimited (if speed-capped) backend for long-running agents. See
[sliding-window-not-token-bucket](../decisions/sliding-window-not-token-bucket.md).

Additional sources:
- https://forums.developer.nvidia.com/t/api-rate-limit-increase-for-nvidia-nim/366043
- https://forums.developer.nvidia.com/t/request-nvidia-nim-free-tier-rate-limit-increase-40-rpm-severely-limits-agentic-ai-workflows/369762
