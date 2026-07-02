---
type: Runbook
title: Deploy with Docker
description: Compose bring-up, volume, healthcheck, hardening, logs.
tags: [docker, deployment]
timestamp: 2026-07-02T00:00:00Z
---

# Deploy with Docker

```sh
cp .env.example .env     # NIM_API_KEYS is the only required value
docker compose up -d --build
docker ps                # STATUS shows (healthy) via the built-in probe
docker logs nim-proxy-nim-proxy-1
```

What the compose file gives you (see
[distroless-scratch-image](../decisions/distroless-scratch-image.md) for why):

- `FROM scratch` image (~3.5 MB), non-root UID 10001.
- `read_only: true`, `cap_drop: [ALL]`, `no-new-privileges` — writes go only
  to the named `history` volume mounted at `/data`.
- `HEALTHCHECK` via `nim-proxy --health` (the binary probes itself; there is
  no shell in the image).
- SIGTERM drains gracefully on `docker stop`.

Logs are one access line per request plus startup detail; ANSI is disabled
automatically when stdout isn't a TTY. There is no shell to exec into — use
logs, `/metrics`, and the dashboard.

Upgrades: `docker compose up -d --build` — history persists in the volume;
rate windows reset (a brief post-restart 429 burst is absorbed silently).
