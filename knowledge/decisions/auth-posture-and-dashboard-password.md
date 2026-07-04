---
type: Decision
title: Fail-closed auth posture with a dashboard password
description: Refuse to start exposed without auth; gate the API with any key and the dashboard/observability with a shared-password session.
tags: [security, auth, deployment]
timestamp: 2026-07-02T00:00:00Z
---

# Fail-closed auth posture with a dashboard password

## Context

A security review found the proxy shipped **open by default**: the dashboard,
`/metrics`, and `/api/history` were unauthenticated, and `/v1/*` auth was
optional (unset `PROXY_API_KEYS` = anyone can spend the rate budget). The
listener binds `0.0.0.0` and the project is meant for VPS / ECS / Railway, so
an accidental exposure leaked usage data and the operator's browser was a
[stored-XSS](input-sanitizing-and-xss.md) target. Deployment patterns to
support: local self-host (loopback/firewalled), VPS behind a TLS reverse
proxy, and PaaS behind a platform edge.

## Options

1. Keep optional auth, add a warning log. (Rejected — the dangerous default
   that caused the review persists.)
2. Infer safety from the bind address (loopback = open, else require auth).
   (Rejected — Docker always binds `0.0.0.0`; host port-publish, invisible to
   the process, controls real exposure. The heuristic would be wrong in a
   container every time.)
3. **Fail closed on explicit flags**: refuse to start without auth unless the
   operator opts into open mode.

## Choice

Option 3, with two clean states and no silent open default:

- **Secure mode** — `PROXY_API_KEYS` (≥1 key) *and* `ADMIN_PASSWORD` both set.
  API requires a Bearer key (constant-time compare, any key works — names are
  optional metrics labels); dashboard/`/metrics`/`/api/history` require the
  password.
- **Open mode** — `INSECURE_NO_AUTH=true`, everything unauthenticated, loud
  startup warning. Loopback / firewalled only.
- Anything else → `eprintln!` guidance + `exit(1)`.

Two *separate* credentials (not one master key): the API key is a per-client
secret (bonus attribution), the admin password is the operator's — so they
rotate independently and a friend's leaked API key never touches the
dashboard.

**Dashboard auth mechanism**: a shared password → `POST /login` sets an
HMAC-signed, HttpOnly, SameSite=Strict session cookie (signing key = 32 random
bytes per boot, so no secret to persist and restarts invalidate sessions).
Scrapers use `Authorization: Bearer <password>` (or Basic) on `/metrics`.
`/health` stays public for probes. A per-process failed-login throttle + a
250 ms delay on rejected API keys slow brute force; a reverse proxy should add
IP-level limiting.

## Consequences

- Safe to expose by default; the three deployment patterns are documented in
  [deploy-docker](../ops/deploy-docker.md) and [sharing-with-friends](../ops/sharing-with-friends.md).
- **No built-in TLS** — credentials must ride HTTPS, so TLS terminates at a
  reverse proxy / platform edge; `TRUST_PROXY=true` marks the cookie `Secure`.
- Compose now publishes `127.0.0.1:8000:8000` by default (loopback), so a bare
  `docker compose up` can't accidentally expose an open instance.
- Session-cookie state is per-boot; a restart logs dashboard users out (API
  keys unaffected).

## Amendment (v0.6.0 — store-backed users)

The fail-closed *stance* below is unchanged; its inputs moved from env vars
into the [UI-managed config store](ui-managed-config-store.md), which changes
three specifics:

- **`INSECURE_NO_AUTH` retires.** Its replacement is the store's `open|keyed`
  API-access mode, and it now governs **only `/v1`**. `keyed` (default) is
  fail-closed — keyed with zero client keys rejects everything; `open` (labeled
  "local") is trusted-network only. Every dashboard/`/metrics`/`/api/history`
  surface **always** requires a logged-in session post-setup; there is no
  "everything open" mode for the UI anymore. Pre-setup, only `/health`,
  `/setup`, and `/login` (which redirects to `/setup`) are reachable.
- **The single `ADMIN_PASSWORD` becomes multi-user.** Users live in the store
  (`{username, password_hash, role}`); login is username + password. Passwords
  are **PBKDF2-HMAC-SHA256, 600k iterations**, the iteration count encoded in
  the stored string (`pbkdf2-sha256$iters$salt$hash`) so it's tunable without a
  migration, pinned by RFC 7914 test vectors. Client API keys are now
  server-generated 128-bit secrets (`npk_` prefix), shown once, stored as
  SHA-256 digests + last-4 — see [ui-managed-config-store](ui-managed-config-store.md).
- **Sessions carry identity, not just expiry.** The signed cookie payload
  extends to `expiry || username || first8(sha256(password_hash))`. Role is
  looked up from the config snapshot on every request (never baked into the
  token), so role changes and user deletion take effect immediately; the
  password-hash fragment means **a password change or reset invalidates that
  user's existing sessions instantly**. `Admin` no longer holds a password —
  credentials live in the store, hashed. Prometheus scrapers authenticate with
  `Authorization: Bearer <username>:<password>` (or HTTP Basic), verified once
  against the store then memoized via HMAC (no 600k-iteration PBKDF2 per
  scrape). The per-boot signing key and per-process login throttle are
  unchanged.
