---
type: Component
title: Auth (client keys + multi-user sessions)
description: Fail-closed posture; store-backed users gate the dashboard and observability, an open/keyed mode gates /v1, a first-run wizard claims the install.
tags: [auth, multi-user, security]
timestamp: 2026-07-04T00:00:00Z
---

# Auth

Two independent gates. Since v0.6.0 both are driven by the
[config store](../decisions/ui-managed-config-store.md), not env vars, and the
`ADMIN_PASSWORD` single-operator model is now multi-user (see the
[posture decision](../decisions/auth-posture-and-dashboard-password.md) and its
v0.6.0 amendment).

## Setup phase (`src/settings.rs`, `src/setup.html`)

`setup_required` is an `AtomicBool`, true iff the store has no superuser (a
fresh install, or a lockout-recovery volume edit that emptied `users`). While
true:

- `/health` public (probes unaffected); `/v1/*` → `503 {"code":"setup_required"}`
  (fail-closed — nothing proxies); browsers → 302 `/setup`; `/login` → `/setup`.
- `/setup` serves a 3-step wizard (create superuser [password ≥10 chars] → add
  ≥1 NIM key with per-key rpm, validated live against the upstream via
  `POST /setup/validate-key` → review & finish). **One atomic POST** creates the
  superuser, records the keys, persists, and mints a session — no
  half-configured state, nothing to clean up on abandonment.

Post-setup the setup routes 404 (gated on the AtomicBool). Boot logs a loud
`SETUP REQUIRED — the FIRST VISITOR becomes the superuser` line — the claim
window is [accepted risk](../decisions/ui-managed-config-store.md).

## API gate — `/v1/*` (`src/proxy.rs`)

The store's `client_auth.mode` decides:

- **`keyed`** (default) — inbound `Authorization: Bearer <secret>` is
  SHA-256'd and `ct_eq`'d against the stored digests (`ClientKey.secret_sha256`).
  Fail-closed: keyed with **zero** keys rejects everything. Miss → OpenAI-style
  401, a `nimproxy_unauthorized_total` tick, and a delay to slow brute force.
- **`open`** (labeled "local") — `/v1` is unauthenticated; trusted networks
  only. This is the *only* thing the mode toggle affects — the dashboard is
  never open.

Client secrets are server-generated 128-bit tokens with an `npk_` prefix, shown
**exactly once** at creation; only the SHA-256 digest (+ last-4 for masked
display) is stored, so a leaked store leaks no usable tokens. Each key has an
`owner`; the inbound token is **never forwarded** — the proxy substitutes its
own NIM key per lane.

## Dashboard gate — UI + observability (`src/auth.rs`)

`require_session` gates `/`, `/dash`, `/dash/config.json`, `/api/*`, and
`/metrics` for any logged-in user; server-setting and user-management endpoints
additionally require `role != user`, and ownership checks compare the session
username against a key's `owner` (admins bypass). `/health` stays public.

- **Login** is username + password. `POST /login` → an HMAC-signed, HttpOnly,
  SameSite=Strict cookie whose payload carries
  `expiry || username || first8(sha256(password_hash))`. Signing key = 32 random
  bytes per boot (restart invalidates sessions). Role is looked up from the
  config snapshot **every request**, so role changes and user deletion take
  effect immediately; the password-hash fragment means a password change/reset
  invalidates that user's sessions instantly.
- **Passwords**: PBKDF2-HMAC-SHA256, 600k iterations, per-hash iteration count
  encoded (`pbkdf2-sha256$iters$salt$hash`), pinned by RFC 7914 test vectors;
  verified in `spawn_blocking`.
- **Scrapers**: `Authorization: Bearer <username>:<password>` (or HTTP Basic),
  verified once against the store then memoized via HMAC (no PBKDF2 per scrape).
- Failed logins: `nimproxy_login_failures_total`, a delay, and a per-process
  fixed-window throttle (>10/min → 429). A reverse proxy should add IP-level
  limiting.

## Roles & recovery

superuser (an admin that can never be deleted — a deletion guard, no extra
powers) · admin (server settings + user management) · user (own account, own
client keys, own NIM keys). Dashboards are identical for all roles; only
Settings differs, and `GET /api/config` is filtered **server-side** per role
(hidden sections absent from the payload, not CSS-hidden). Partial lockout:
any admin resets any password. Total lockout: the documented
[volume edit](../ops/configure-env.md).

TLS is not built in — terminate it at a reverse proxy / platform edge and set
`TRUST_PROXY=true` so the session cookie is marked `Secure`.
</content>
