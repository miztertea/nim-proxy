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
2. Issue one `name:secret` per person in `PROXY_API_KEYS` — the name shows on
   the dashboard leaderboard and in per-client metrics, so contribution and
   consumption stay visible. (Any key works; naming is just for attribution.)
3. Set `ADMIN_PASSWORD` — the shared dashboard/metrics password. With both set,
   the proxy runs in secure mode and every surface requires auth
   ([client-auth](../architecture/client-auth.md)); it refuses to start
   otherwise. `/health` stays public.
4. Terminate TLS in front (reverse proxy / VPN / platform edge) and set
   `TRUST_PROXY=true` — passwords and keys must not travel in cleartext.
5. Everyone points their OpenAI-compatible client at the URL with their secret
   as the API key (recipes in the README); the dashboard is at `/login`.

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
