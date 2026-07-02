---
type: Runbook
title: Configuration reference
description: Every env var, its default, and when you'd actually change it.
tags: [configuration]
timestamp: 2026-07-02T00:00:00Z
---

# Configuration

Philosophy: flat env vars only (`.env` supported via dotenvy), read once at
boot, exactly one required value, sane defaults for everything else. No
config files, no reload.

| Variable | Default | Change it when… |
|---|---|---|
| `NIM_API_KEYS` | required | Always — comma-separated `nvapi-…` keys, one lane each |
| `PROXY_API_KEYS` | — | Secure mode (required with `ADMIN_PASSWORD`). `secret` or `name:secret` |
| `ADMIN_PASSWORD` | — | Secure mode — gates dashboard/`/metrics`/`/api/history` |
| `INSECURE_NO_AUTH` | `false` | `true` runs fully open (localhost/firewalled only) |
| `TRUST_PROXY` | `false` | Behind a TLS reverse proxy — marks the session cookie `Secure` |
| `HOST` | `0.0.0.0` | Bind loopback-only (`127.0.0.1`) for bare-metal local |
| `MAX_INFLIGHT` | `512` | Concurrent-request flood cap (503 beyond it) |
| `NIM_BASE_URL` | `https://integrate.api.nvidia.com` | Pointing at self-hosted NIM or a mock |
| `PORT` | `8000` | Port conflicts |
| `RPM_PER_KEY` | `40` | NVIDIA grants you a higher limit |
| `MAX_WAIT_SECS` | `900` | Your longest acceptable stall differs from 15 min |
| `HEARTBEAT_SECS` | `10` | A client's idle timeout is unusually aggressive |
| `MODELS_TTL_SECS` | `600` | Rarely |
| `STREAM_IDLE_SECS` | `300` | Models that legitimately pause >5 min mid-stream (0 disables) |
| `STRICT_PASSTHROUGH` | `false` | You want zero body modification (loses exact token counts) |
| `REF_PRICE_IN` / `REF_PRICE_OUT` | `0.5` / `2.0` | Different reference $/1M-token prices for "saved" |
| `HISTORY_DAYS` | `30` | Longer/shorter report horizon (0 = forever; ~35 MB / 30 days) |
| `DATA_DIR` | `data` (image sets `/data`) | Non-Docker layouts; empty disables persistence |
| `RUST_LOG` | `nim_proxy=info` | Debugging (`nim_proxy=debug`) |

Gotchas:

- **Fail closed**: the proxy refuses to start unless secure mode
  (`PROXY_API_KEYS` + `ADMIN_PASSWORD`) or `INSECURE_NO_AUTH=true` is set
  ([posture](../decisions/auth-posture-and-dashboard-password.md)).
- Rate state is in-memory: **one instance per key set**, never two replicas
  sharing keys.
- `RPM_PER_KEY` is per *rolling* minute with a built-in safety margin
  ([why](../decisions/window-jitter-margin.md)) — don't add your own headroom.
