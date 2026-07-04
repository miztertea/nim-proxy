---
type: Runbook
title: Deploy with Docker
description: Compose bring-up, volume, healthcheck, hardening, logs.
tags: [docker, deployment]
timestamp: 2026-07-02T00:00:00Z
---

# Deploy with Docker

```sh
cp .env.example .env     # only container vars now (HOST/PORT/DATA_DIR/RUST_LOG/TRUST_PROXY)
docker compose up -d     # pulls ghcr.io/miztertea/nim-proxy:latest (signed, multi-arch)
docker ps                # STATUS shows (healthy) via the built-in probe
docker logs nim-proxy-nim-proxy-1
```

Then open `http://localhost:8000/` — a fresh install shows the **first-run
wizard**: create the superuser account, add ≥1 NIM key (validated against the
upstream), finish. By default the wizard also mints your first client key
(`npk_…`, shown once on the closing connect panel) so harnesses can call `/v1`
immediately. No keys or passwords go in `.env`; everything app-level is
configured in the UI and stored in `DATA_DIR/config.json`
([config store](../decisions/ui-managed-config-store.md)).

To build from source instead (development):
`docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build`
— the dev override tags the local build `nim-proxy:dev` so it can't shadow the
published image.

**Auth** ([posture](../decisions/auth-posture-and-dashboard-password.md)): the
dashboard always requires a logged-in user post-setup; the `/v1` API is `keyed`
(default, needs a client key) or `open` (local/trusted networks only), toggled
in Settings. Pre-setup the data plane is closed (`/v1` → 503) and the **first
visitor becomes the superuser** — a loud boot warning says so; finish setup
immediately.

Deployment patterns:

- **Local**: keep the default loopback publish (`127.0.0.1:8000:8000`) so it
  can't leak; set the API mode to `open` in Settings if you don't want client
  keys. Reach it at `http://localhost:8000`.
- **VPS / bare metal**: behind nginx/Caddy doing TLS; change the publish to
  `8000:8000` (or bind the reverse proxy to the loopback port) and set
  `TRUST_PROXY=true`. Keep the API `keyed`.
- **PaaS (ECS/Railway/Fly)**: platform edge terminates TLS; set
  `TRUST_PROXY=true`. Complete the wizard as soon as the instance is reachable.

What the compose file gives you (see
[distroless-scratch-image](../decisions/distroless-scratch-image.md) for why):

- `FROM scratch` image (~3.5 MB), non-root UID 10001, loopback publish default.
- `read_only: true`, `cap_drop: [ALL]`, `no-new-privileges` — writes go only
  to the named `history` volume mounted at `/data`, which now holds both
  `history.jsonl` **and `config.json` (credentials, 0600)**. A volume backup
  therefore contains the password hashes and NIM keys — treat backups as
  secrets. Total-lockout recovery is a volume edit of `config.json`
  ([configure-env](configure-env.md)).
- `HEALTHCHECK` via `nim-proxy --health` (the binary probes itself; there is
  no shell in the image).
- SIGTERM drains gracefully on `docker stop`.
- Strict CSP + anti-framing/sniffing headers on every response.

Logs are one access line per request plus startup detail; ANSI is disabled
automatically when stdout isn't a TTY. There is no shell to exec into — use
logs, `/metrics`, and the dashboard.

Upgrades: `docker compose pull && docker compose up -d` — history persists in
the volume; rate windows reset (a brief post-restart 429 burst is absorbed
silently).
