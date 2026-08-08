---
type: Runbook
title: Configuration reference
description: Compose publishing, the 5 container-level env vars, Settings, and lockout recovery.
tags: [configuration]
timestamp: 2026-07-04T00:00:00Z
---

# Configuration

Since v0.6.0, **app-level configuration lives in the dashboard**, not env vars.
A first-run wizard claims a fresh install (create the superuser → add ≥1 NIM
key, validated against the upstream → land on the dashboard, logged in); after
that, Settings edits everything and persists it to `DATA_DIR/config.json`
(atomic, 0600 — see [ui-managed-config-store](../decisions/ui-managed-config-store.md)).
Env now covers **deployment-level concerns only**.

## Compose-only publish setting

`PUBLISH_HOST` controls the host interface where Docker Compose publishes
container port 8000. It defaults to `127.0.0.1`, keeping a bare deployment
loopback-only. Set `PUBLISH_HOST=0.0.0.0` in `.env` only for intentional
LAN/public exposure after the authentication and TLS posture is ready.
Compose consumes this value while interpolating `docker-compose.yml`;
nim-proxy itself does not read it.

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
the default time range, history retention days, the
availability SLO, the model limits, and users/roles all live in the
store and are edited from the dashboard. A Settings save validates the
complete candidate, writes `config.json` atomically, and swaps the live
configuration; no restart is needed.

The file is otherwise **boot-read**, not watched. An out-of-band edit to
`DATA_DIR/config.json`—by an operator, deployment tool, or mounted secret
writer—does not update the running process and requires a restart. Use the
Settings API for live changes.

The default dashboard window and retention are separate settings and both
default to 30 days. The default window must be at least one day. Retention `0`
is unlimited; finite retention must be at least the default window. The SLO
must be a finite percentage greater than 0 and at most 100. A combined save is
all-or-nothing: any invalid field leaves the persisted and live configuration
unchanged. Reducing finite retention trims the visible index immediately and
schedules atomic canonical `history-v1.jsonl` compaction; startup debt remains
visible as `compaction_pending` until safe replacement completes. Recovery
evidence intersecting the retained window defers replacement. Setting
retention to `0` keeps canonical history unlimited and cancels stale finite
work. Experimental `history.jsonl` is never a compaction input or target (see
[metrics history](../architecture/metrics-history.md)).

**Legacy env vars are ignored.** `NIM_API_KEYS`, `PROXY_API_KEYS`,
`ADMIN_PASSWORD`, `INSECURE_NO_AUTH`, `NIM_BASE_URL`, `RPM_PER_KEY`,
`MAX_WAIT_SECS`, `HEARTBEAT_SECS`, `MODELS_TTL_SECS`, `STREAM_IDLE_SECS`,
`REQUEST_TIMEOUT_SECS`, `STRICT_PASSTHROUGH`, `REF_PRICE_IN`/`REF_PRICE_OUT`
(removed in 0.6.6 — see
[no-estimated-savings-metric](../decisions/no-estimated-savings-metric.md)),
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
