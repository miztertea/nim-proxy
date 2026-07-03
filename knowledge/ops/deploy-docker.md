---
type: Runbook
title: Deploy with Docker
description: Compose bring-up, volume, healthcheck, hardening, logs.
tags: [docker, deployment]
timestamp: 2026-07-02T00:00:00Z
---

# Deploy with Docker

```sh
cp .env.example .env     # set NIM_API_KEYS + an auth mode (below)
docker compose up -d     # pulls ghcr.io/miztertea/nim-proxy:latest (signed, multi-arch)
docker ps                # STATUS shows (healthy) via the built-in probe
docker logs nim-proxy-nim-proxy-1
```

To build from source instead (development):
`docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build`
— the dev override tags the local build `nim-proxy:dev` so it can't shadow the
published image.

**Auth is mandatory** ([posture](../decisions/auth-posture-and-dashboard-password.md)):
set `ADMIN_PASSWORD` + `PROXY_API_KEYS` (secure), or `INSECURE_NO_AUTH=true`
(open). The proxy exits with guidance if neither is configured.

Deployment patterns:

- **Local**: `INSECURE_NO_AUTH=true`; compose publishes `127.0.0.1:8000:8000`
  (loopback) so it can't leak. Reach it at `http://localhost:8000`.
- **VPS / bare metal**: secure mode behind nginx/Caddy doing TLS; change the
  publish to `8000:8000` (or bind the reverse proxy to the loopback port) and
  set `TRUST_PROXY=true`.
- **PaaS (ECS/Railway/Fly)**: secure mode; platform edge terminates TLS; inject
  `ADMIN_PASSWORD`/`PROXY_API_KEYS` as platform secrets; `TRUST_PROXY=true`.

What the compose file gives you (see
[distroless-scratch-image](../decisions/distroless-scratch-image.md) for why):

- `FROM scratch` image (~3.5 MB), non-root UID 10001, loopback publish default.
- `read_only: true`, `cap_drop: [ALL]`, `no-new-privileges` — writes go only
  to the named `history` volume mounted at `/data`.
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
