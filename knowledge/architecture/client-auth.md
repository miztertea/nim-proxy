---
type: Component
title: Auth (API keys + admin password)
description: Fail-closed posture; API Bearer keys gate /v1/*, a shared-password session gates the dashboard and observability.
tags: [auth, multi-user, security]
timestamp: 2026-07-02T00:00:00Z
---

# Auth

Two independent gates, both required to run exposed (see the
[posture decision](../decisions/auth-posture-and-dashboard-password.md)). The
process refuses to start unless either both credentials are set (secure mode)
or `INSECURE_NO_AUTH=true` (open mode).

## API gate — `/v1/*` (`src/proxy.rs`)

- `PROXY_API_KEYS` = comma-separated secrets; `name:secret` labels that client
  in metrics/leaderboard, a bare secret auto-labels `client0, …`. **Any** key
  works — the requirement is "≥1 key exists," not a specific one.
- Inbound `Authorization: Bearer <secret>` is matched **constant-time**
  (`auth::ct_eq`) against every configured key; no early-exit timing leak.
  Miss → OpenAI-style 401, a `nimproxy_unauthorized_total` tick, and a 250 ms
  delay to slow brute force.
- The inbound token is **never forwarded** — the proxy substitutes its own NIM
  key per lane.

## Admin gate — dashboard + observability (`src/auth.rs`)

- `ADMIN_PASSWORD` protects `/`, `/dash`, `/dash/config.json`, `/api/history`,
  `/metrics` via `require_admin` middleware. `/health` stays public (probes).
- Browsers `POST /login` → an HMAC-signed, HttpOnly, SameSite=Strict session
  cookie (12 h). Signing key = 32 random bytes per boot (no persisted secret;
  restart invalidates sessions). Scrapers use `Authorization: Bearer
  <password>` or HTTP Basic. All secret compares are constant-time.
- Failed logins: `nimproxy_login_failures_total`, a 500 ms delay, and a
  per-process fixed-window throttle (>10/min → 429). A reverse proxy should
  add IP-level limiting.

## Open mode

`INSECURE_NO_AUTH=true` disables both gates (login endpoints inert, dashboard
served directly). Intended for loopback / firewalled hosts only; the compose
file publishes `127.0.0.1:8000:8000` for this case.

TLS is not built in — terminate it at a reverse proxy / platform edge and set
`TRUST_PROXY=true` so the session cookie is marked `Secure`.
