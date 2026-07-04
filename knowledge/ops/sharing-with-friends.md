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

Since v0.6.0 this is a **multi-user** flow — you don't collect anyone's keys or
edit a `.env`. You create each friend a login; they add **their own** NIM key,
which nobody else (not even you) can read back ([why](../decisions/ui-managed-config-store.md)).

## Setup

1. Bring the proxy up and finish the [first-run wizard](deploy-docker.md) — the
   account you create is the **superuser** and its NIM key(s) anchor the pool.
2. Put it behind TLS (reverse proxy / VPN / platform edge) and set
   `TRUST_PROXY=true` — passwords and keys must not travel in cleartext.
3. For each friend: Settings → Users → add a `user` (or `admin`) with an
   initial password. Send them the URL and credentials.
4. Each friend logs in, changes their password, and adds their own NIM key in
   Settings (per-key rpm defaults to 40; validated against the upstream live).
   Their key joins the shared pool as **their** key — masked and owner-labeled
   to you, invisible to other users. They can also mint their own `/v1` client
   API key (an `npk_…` secret shown once) if the API is in `keyed` mode.
5. Everyone points their OpenAI-compatible client at the URL with their client
   key as the API key (recipes in the README); the dashboard login is at
   `/login`.

Ownership means fairness is legible: contribution (whose keys, at what rpm) and
consumption (per-client metrics) both stay visible on the dashboard, which is
identical for every role. Deleting a user pulls their NIM keys from the pool and
revokes their client keys — their harnesses stop, which is the point.

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
