---
type: Component
title: Client auth (PROXY_API_KEYS)
description: Local mode (no auth) vs named Bearer tokens; per-client attribution everywhere.
tags: [auth, multi-user]
timestamp: 2026-07-02T00:00:00Z
---

# Client auth

- **Local mode** (default, `PROXY_API_KEYS` unset): no authentication; all
  traffic is attributed to the client label `local`.
- **Shared mode**: `PROXY_API_KEYS=alice:secret1,bob:secret2` — inbound
  `Authorization: Bearer <secret>` must match; the *name* becomes the client
  label on every metric, dashboard tile, and leaderboard row. Bare secrets
  (no name) get `client0, client1, …`. Unknown/missing tokens get an
  OpenAI-style 401 plus a `nimproxy_unauthorized_total` tick.
- Inbound Authorization is **never forwarded** — the proxy substitutes its
  own NIM key per lane. Clients that insist on an API key can use any
  placeholder in local mode.
- `/` (dashboard), `/metrics`, `/health`, and `/api/history` are deliberately
  unauthenticated (scrape/probe surfaces) — firewall them when exposing the
  proxy beyond localhost ([runbook](../ops/sharing-with-friends.md)).

There is no rate limit on failed auth attempts; a fat brute-force is visible
in `nimproxy_unauthorized_total`. Accepted for the friends-scale threat
model.
