---
type: Runbook
title: Configuration reference
description: The 5 container-level env vars; everything else lives in the Settings UI; lockout recovery.
tags: [configuration]
timestamp: 2026-07-04T00:00:00Z
---

# Configuration

Since v0.6.0, **app-level configuration lives in the dashboard**, not env vars.
A first-run wizard claims a fresh install (create the superuser → add ≥1 NIM
key, validated against the upstream → land on the dashboard, logged in); after
that, Settings edits everything and persists it to `DATA_DIR/config.json`
(atomic, 0600 — see [ui-managed-config-store](../decisions/ui-managed-config-store.md)).
Env now covers **container-level concerns only**.

## The 5 env vars

| Variable | Default | Change it when… |
|---|---|---|
| `HOST` | `0.0.0.0` | Bind loopback-only (`127.0.0.1`) for bare-metal local |
| `PORT` | `8000` | Port conflicts |
| `DATA_DIR` | `data` (image sets `/data`) | Non-Docker layouts. Must be writable — the config store *and* history live here; an unwritable dir is a **hard boot error** |
| `TRUST_PROXY` | `false` | Behind a TLS reverse proxy — trusts `X-Forwarded-Proto` and marks the session cookie `Secure` |
| `RUST_LOG` | `nim_proxy=info` | Debugging (`nim_proxy=debug`) |

(`HISTORY_SAMPLE_SECS` also exists as an undocumented test knob; 5 minutes is
the contract.)

## Everything else → Settings

NIM keys (per-key rpm, enable/disable, ownership), the upstream base URL,
client API keys and the open/keyed API mode, limits (max_wait, heartbeat,
stream_idle, request_timeout, models_ttl, max_inflight, strict_passthrough),
pricing, history retention days, the model-pressure governor, and users/roles
all live in the store and are edited from the dashboard. All apply live — no
restart.

**Legacy env vars are ignored.** `NIM_API_KEYS`, `PROXY_API_KEYS`,
`ADMIN_PASSWORD`, `INSECURE_NO_AUTH`, `NIM_BASE_URL`, `RPM_PER_KEY`,
`MAX_WAIT_SECS`, `HEARTBEAT_SECS`, `MODELS_TTL_SECS`, `STREAM_IDLE_SECS`,
`REQUEST_TIMEOUT_SECS`, `STRICT_PASSTHROUGH`, `REF_PRICE_IN`/`REF_PRICE_OUT`,
`HISTORY_DAYS`, and `MAX_INFLIGHT` no longer do anything; a set-but-ignored one
gets a single boot warning (`ignoring legacy env vars (…) — these settings live
in the dashboard now`). There is no seed-from-env and no migration (there were
no deployments to migrate).

## Lockout recovery

- **Partial** (you forgot one password): any admin resets any password from
  Settings → Users.
- **Total** (no admin can log in): stop the container, empty the `"users"`
  array in `config.json` on the volume, restart → the wizard re-creates the
  superuser while keys/settings survive and the new superuser adopts any
  orphaned keys. The scratch image has no shell, so edit from a throwaway
  container:
  ```sh
  docker run --rm -it -v <volume>:/data alpine vi /data/config.json
  ```

## Gotchas

- **Fail closed**: pre-setup, `/v1` answers `503 {"code":"setup_required"}` and
  browsers land on `/setup`; the first visitor to a fresh install becomes the
  superuser (loud boot warning) — finish setup immediately
  ([posture](../decisions/auth-posture-and-dashboard-password.md)).
- A **corrupt or unreadable store is a hard boot error**, never a silent
  fall-through to setup (that would discard keys). Restore from backup or
  deliberately delete the file.
- Rate state is in-memory: **one instance per key set**, never two replicas
  sharing keys.
- Per-key rpm is per *rolling* minute with a built-in safety margin
  ([why](../decisions/window-jitter-margin.md)) — don't add your own headroom.
