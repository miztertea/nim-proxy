# nim-proxy

A tiny, rate-limit-aware OpenAI-compatible proxy for the [NVIDIA NIM API](https://build.nvidia.com), built for agent harnesses like [OpenCode](https://opencode.ai).

NIM's free tier has no credits and no token caps — just a ~40 requests-per-minute limit per API key. When an agent harness hits that limit, the upstream returns a 429 and most harnesses simply abort the task. This proxy fixes that, and it has exactly one job: **obey the NIM speed limit so your harness never sees it.**

```
OpenCode ──► nim-proxy ──► integrate.api.nvidia.com
             │
             ├─ paces requests to 40 RPM per key (sliding window)
             ├─ load-balances across all your keys (5 keys = 200 RPM)
             ├─ rides out 429/5xx with retries + Retry-After
             ├─ keeps the harness connection alive with SSE heartbeats
             └─ answers /v1/models from cache (catalog polls cost nothing)
```

## How it works

- **One lane per key.** Each API key gets an exact sliding-window limiter (40 requests per rolling 60 s — matching NIM's limiter, not a burstable token bucket). Every request is sent on the lane with the soonest free slot.
- **Sticky conversations, spread bursts.** Each conversation (hashed from model + system prompt + first user message) prefers the same lane on every turn, so any server-side prefix cache stays warm on one key. When the sticky lane is at capacity the request spills to the least-loaded ready lane — the chat completions API is stateless (full history in every request), so crossing keys is always safe, just potentially a cold cache. Requests with no conversation identity spread across the least-loaded lanes.
- **One queue for all clients.** Point as many harnesses at the proxy as you like — several OpenCode instances, n8n flows, Codex, anything OpenAI-compatible. All connections share the same lane pool through a global FIFO dispatcher, so slots are granted strictly in arrival order: no client can starve another by winning wakeup races, and a client that disconnects while queued returns its slot to the pool.
- **Heartbeats instead of failures.** For streaming requests (`"stream": true`, which is what OpenCode sends), the proxy commits to a `200 text/event-stream` response immediately and emits SSE comment lines (`: heartbeat`) — which every OpenAI client ignores — while it waits for a rate-limit slot or rides out upstream 429/500/502/503/504 responses. The harness never sees the error, so long-running agent tasks keep going.
- **Strict pass-through.** Request and response bodies are untouched. You pick the model in your harness config; any model in the NIM catalog works. All `/v1/*` endpoints (chat completions, completions, embeddings, …) are forwarded.
- **Local answers where possible.** `GET /v1/models` is cached (10 min default), so harness catalog polls don't burn rate budget.

## Quick start

1. Get one or more API keys at [build.nvidia.com](https://build.nvidia.com) (free, `nvapi-...`).

2. Configure and run:

```sh
cp .env.example .env        # paste your keys into NIM_API_KEYS
docker compose up -d --build
```

Or without Docker: `NIM_API_KEYS=nvapi-xxx,nvapi-yyy cargo run --release`

3. Point OpenCode at it — `opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "nim": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "NVIDIA NIM (proxied)",
      "options": {
        "baseURL": "http://localhost:8000/v1"
      },
      "models": {
        "moonshotai/kimi-k2-instruct": { "name": "Kimi K2 Instruct" },
        "deepseek-ai/deepseek-r1": { "name": "DeepSeek R1" }
      }
    }
  }
}
```

Model IDs are passed through verbatim — use any ID from the [NIM catalog](https://build.nvidia.com/models) (or `curl localhost:8000/v1/models`). No API key is needed on the harness side — the proxy injects its own per lane. If your harness insists on one, set any placeholder; inbound `Authorization` headers are ignored.

## Configuration

All via environment variables (or `.env`):

| Variable | Default | Purpose |
|---|---|---|
| `NIM_API_KEYS` | — (required) | Comma-separated `nvapi-...` keys; each is an independent 40 RPM lane |
| `NIM_BASE_URL` | `https://integrate.api.nvidia.com` | Upstream base URL |
| `PORT` | `8000` | Listen port |
| `RPM_PER_KEY` | `40` | Per-key requests per rolling minute |
| `MAX_WAIT_SECS` | `900` | Max time a request waits for a slot / retries before giving up |
| `HEARTBEAT_SECS` | `10` | SSE keepalive interval while waiting |
| `MODELS_TTL_SECS` | `600` | `/v1/models` cache lifetime |
| `RUST_LOG` | `nim_proxy=info` | Log filter |

## Deployment

The image is built `FROM scratch`: no distro, no shell, no libc, no CA bundle — just the ~3.3 MB static musl binary with TLS roots compiled in (rustls + webpki-roots). It runs as a non-root numeric UID, needs no capabilities and no writable filesystem (the compose file sets `read_only`, `cap_drop: ALL`, `no-new-privileges`), and works under rootless Docker or Podman as-is.

**If you expose the proxy beyond localhost** (e.g. pooling keys with friends), know that it has no client authentication — anyone who can reach the port can spend your rate budget. Put it behind a VPN/Tailscale, an authenticating reverse proxy, or ask for a shared-token feature. Also note NVIDIA keys are issued per developer account; pooling keys across people is between you and NVIDIA's terms of service.

## Notes

- **Non-streaming requests** can't be heartbeated (there's no wire format for it), so they just wait silently through pacing/retries up to `MAX_WAIT_SECS`. Agent harnesses stream, so this rarely matters.
- If every lane is saturated and no slot will open within the wait budget, the proxy fails fast with a `504` and an OpenAI-style error body (or an in-stream `error` event if the SSE response already started).
- `Retry-After` headers from NIM are honored when benching a lane.
- One NIM account requires a unique email **and** phone number, which is what bounds how many lanes you can legitimately run.
