---
type: Runbook
title: Sharing the proxy with friends
description: Multi-user setup, exposure checklist, and the ToS position.
tags: [multi-user, security, tos]
timestamp: 2026-07-02T00:00:00Z
---

# Sharing the proxy with friends

The intended shape: several people each register their own NIM account
(unique email + phone per NVIDIA's signup), contribute their key(s) to the
pool, and share the aggregate throughput. Five keys = 200 RPM for everyone.

## Setup

1. Collect keys into `NIM_API_KEYS`.
2. Issue one `name:secret` per person in `PROXY_API_KEYS` — the name is what
   shows on the dashboard leaderboard and in per-client metrics, so
   contribution and consumption stay visible.
3. Expose **only the proxy port**, ideally via Tailscale/VPN or an
   authenticating reverse proxy. `/`, `/metrics`, `/health`, and
   `/api/history` are unauthenticated by design
   ([client-auth](../architecture/client-auth.md)) — keep them private.
4. Everyone points their OpenAI-compatible client at the URL with their
   secret as the API key (recipes in the README).

## Fairness & capacity

The [FIFO dispatcher](../decisions/global-fifo-dispatcher.md) guarantees no
friend can starve another; under saturation everyone slows down equally.
Size expectations with [capacity-math](capacity-math.md).

## Terms of service

The proxy is designed to *respect* NVIDIA's limits, not evade them: every
key is held to its own 40 RPM window (with margin), load-tested to zero
violations. Whether pooling keys across people complies with NVIDIA's terms
is between the key owners and NVIDIA — the proxy's guarantee is only that no
key ever misbehaves. All traffic for all users originates from one IP; that
is visible to NVIDIA and intentionally not disguised.
