# nim-proxy

A tiny, rate-limit-aware OpenAI-compatible proxy for the [NVIDIA NIM API](https://build.nvidia.com), built for agent harnesses like [OpenCode](https://opencode.ai), Codex, n8n, and anything else that speaks the OpenAI API.

NIM's free tier has no credits and no token caps — just a ~40 requests-per-minute limit per API key. When an agent harness hits that limit, the upstream returns a 429 and most harnesses simply abort the task. This proxy fixes that, and it has exactly one job: **obey the NIM speed limit so your harness never sees it.**

```
OpenCode ─┐
Codex     ├──► nim-proxy ──► integrate.api.nvidia.com
n8n       ┘    │
               ├─ paces requests to 40 RPM per key (sliding window)
               ├─ load-balances across all your keys (5 keys = 200 RPM)
               ├─ pins each conversation to one key (prefix-cache affinity)
               ├─ rides out 429/5xx with retries + Retry-After
               ├─ keeps harness connections alive with SSE heartbeats
               ├─ answers /v1/models from cache (catalog polls cost nothing)
               └─ dashboard + Prometheus metrics for everything above
```

This tool is **not** designed to circumvent NVIDIA's terms of service. It maximizes your own API keys — or a shared pool of keys owned by you and your friends — while *respecting* NVIDIA's speed limits. Every key holds to its 40 RPM; the proxy just makes agents patient enough to live within that budget. Load-tested to prove it: 100 concurrent clients, zero upstream rate violations.

## Quick start

1. Get one or more API keys at [build.nvidia.com](https://build.nvidia.com) (free, `nvapi-...`; each account needs a unique email and phone number).

2. Configure and run:

```sh
cp .env.example .env        # paste keys into NIM_API_KEYS, then pick an auth mode (below)
docker compose up -d --build
```

Or without Docker: `NIM_API_KEYS=nvapi-xxx,nvapi-yyy INSECURE_NO_AUTH=true cargo run --release`

3. Open the dashboard at `http://localhost:8000/` and point your client at `http://localhost:8000/v1`.

> **The proxy refuses to start exposed without auth.** Either set `ADMIN_PASSWORD` + `PROXY_API_KEYS` (secure mode) or `INSECURE_NO_AUTH=true` (open — localhost/firewalled only). See [Security & deployment](#security--deployment).

## Client recipes

Model IDs are passed through verbatim — use any ID from the [NIM catalog](https://build.nvidia.com/models) (or `curl localhost:8000/v1/models`). In open mode no client-side key is needed; in secure mode, use your assigned `PROXY_API_KEYS` secret as the API key.

**OpenCode** — `opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "nim": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "NVIDIA NIM (proxied)",
      "options": { "baseURL": "http://localhost:8000/v1" },
      "models": {
        "moonshotai/kimi-k2-instruct": { "name": "Kimi K2 Instruct" },
        "deepseek-ai/deepseek-r1": { "name": "DeepSeek R1" }
      }
    }
  }
}
```

**Codex CLI** — `~/.codex/config.toml`:

```toml
model_provider = "nim"
model = "moonshotai/kimi-k2-instruct"

[model_providers.nim]
name = "NVIDIA NIM (proxied)"
base_url = "http://localhost:8000/v1"
wire_api = "chat"
```

**n8n** — add an *OpenAI* credential with Base URL `http://localhost:8000/v1` and any placeholder API key (or your proxy secret), then use it in AI nodes with a NIM model id.

**Plain curl**:

```sh
curl http://localhost:8000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-ai/deepseek-r1","stream":true,
       "messages":[{"role":"user","content":"hello"}]}'
```

## How it works

- **One lane per key.** Each API key gets an exact sliding-window limiter (40 requests per rolling 60 s — matching NIM's limiter, not a burstable token bucket — plus a 1 s jitter margin so boundary-timed requests can't land inside the upstream's window).
- **One queue for all clients.** Any number of harnesses share the lane pool through a global FIFO dispatcher: slots are granted strictly in arrival order, no client can starve another, and a client that disconnects while queued returns its slot.
- **Sticky conversations, spread bursts.** Each conversation (hashed from model + system prompt + first user message) prefers the same lane every turn, keeping any server-side [prefix cache](https://docs.nvidia.com/nim/large-language-models/latest/kv-cache-reuse.html) warm on one key. When that lane is full the request spills to the least-loaded ready lane — the API is stateless, so crossing keys is always safe, just potentially a cold cache. The dashboard's "conversation stickiness" tile shows the live hit rate.
- **Heartbeats instead of failures.** For streaming requests the proxy commits to `200 text/event-stream` immediately and emits SSE comment lines (`: heartbeat` — ignored by every OpenAI client) while it waits for a slot or rides out upstream 429/500/502/503/504 with `Retry-After` honored and instant failover between keys. Non-retryable errors surface as in-stream `error` events. Streams that stall mid-generation are cut after `STREAM_IDLE_SECS`.
- **Pass-through with one exception.** Bodies are forwarded untouched, except: streaming chat requests get `stream_options: {"include_usage": true}` injected so token accounting is exact rather than estimated. If a model rejects the field (400), the proxy retries untouched and never injects for that model again. Set `STRICT_PASSTHROUGH=true` to disable entirely.
- **Local answers where possible.** `GET /v1/models` is cached (10 min default, single-flight refresh), so harness catalog polls don't burn rate budget.

## Dashboard

Served at `GET /` — a single embedded HTML file, no Grafana, no config. Three views, light and dark mode:

- **Models** — ranked model cards (publisher logos with offline fallback, requests, tokens, TTFT, tok/s, error rate, dollars saved), TTFT median/p95 over time, tokens-per-minute by model, cumulative savings, full table.
- **Proxy** — capacity-used and success-rate gauges, request/outcome/load charts, queue-wait median/p95, hour-of-day activity heatmap, per-client leaderboard.
- **Keys** — per-lane utilization meters, 429s-per-minute by lane, conversation stickiness, per-lane table.

**Time ranges & history.** The filter row offers Live (pausable, 3 s refresh) plus 1h/6h/24h/7d/30d presets and a custom calendar date-time range. Range views are served from the proxy's own history: a ~4 KB metrics snapshot every 5 minutes, kept `HISTORY_DAYS` days (default 30, `0` = forever) in `DATA_DIR` (a Docker volume in the compose file; ~35 MB per 30 days). In a range view every tile, card, and table reports totals *for that window* — instant usage reports.

## Security & deployment

The proxy **fails closed**: it will not start on a network-reachable port without authentication. There are exactly two ways to run it.

### Secure mode (any exposed deployment)

Set both credentials:

```sh
PROXY_API_KEYS=alice:8f3k...,bob:2mq9...   # gates the API (/v1/*); any key works
ADMIN_PASSWORD=a-long-random-string        # gates the dashboard, /metrics, /api/history
```

- **API** (`/v1/*`): clients send `Authorization: Bearer <key>`. The optional `name:secret` form labels that client in metrics (per-friend leaderboard); a bare secret is auto-labeled. Unknown keys get an OpenAI-style 401. Key comparison is constant-time.
- **Dashboard**: browsers hit `/login`, enter the password, and get a signed, HttpOnly, SameSite=Strict session cookie (12 h). `/`, `/metrics`, and `/api/history` require it.
- **Scrapers**: Prometheus scrapes `/metrics` with `Authorization: Bearer <ADMIN_PASSWORD>` (or HTTP Basic). Example scrape config:
  ```yaml
  scrape_configs:
    - job_name: nim-proxy
      authorization: { credentials: "<ADMIN_PASSWORD>" }
      static_configs: [{ targets: ["nim-proxy:8000"] }]
  ```
- `/health` stays public (load-balancer / Docker probe; exposes nothing).

### Open mode (localhost / firewalled only)

```sh
INSECURE_NO_AUTH=true
```

Everything is unauthenticated. Only use this bound to loopback or on a fully private network. The compose file publishes `127.0.0.1:8000:8000` by default for exactly this case.

### Deployment patterns

| Pattern | How |
|---|---|
| **Local self-host** | `INSECURE_NO_AUTH=true`, keep the default loopback port publish. Or set `HOST=127.0.0.1` when running the binary directly. |
| **VPS / bare metal** | Secure mode + a TLS-terminating reverse proxy (nginx/Caddy) in front. Set `TRUST_PROXY=true` so the session cookie is marked `Secure` (needs `X-Forwarded-Proto: https`). |
| **PaaS (ECS / Railway / Fly)** | Secure mode; the platform edge terminates TLS. Set `TRUST_PROXY=true`. Inject `ADMIN_PASSWORD` / `PROXY_API_KEYS` as platform secrets, not in the image. |

**TLS is not built in** — passwords and keys must travel over HTTPS, so terminate TLS at a reverse proxy or platform edge for any exposed deployment. Additional hardening in place: a strict `Content-Security-Policy` and anti-framing/sniffing headers on all responses, a failed-login throttle, and an in-flight cap (`MAX_INFLIGHT`) that sheds floods with a 503.

## Configuration

All via environment variables (or `.env`). Only `NIM_API_KEYS` is required.

| Variable | Default | Purpose |
|---|---|---|
| `NIM_API_KEYS` | — (required) | Comma-separated `nvapi-...` keys; each is an independent 40 RPM lane |
| `NIM_BASE_URL` | `https://integrate.api.nvidia.com` | Upstream base URL |
| `HOST` / `PORT` | `0.0.0.0` / `8000` | Bind address and port |
| `PROXY_API_KEYS` | unset | API keys (`secret` or `name:secret`, comma-separated). Required in secure mode |
| `ADMIN_PASSWORD` | unset | Dashboard/observability password. Required in secure mode |
| `INSECURE_NO_AUTH` | `false` | `true` runs fully open (localhost/firewalled only) |
| `TRUST_PROXY` | `false` | Trust `X-Forwarded-Proto` and mark the session cookie `Secure` |
| `MAX_INFLIGHT` | `512` | Concurrent-request cap before shedding with 503 |
| `RPM_PER_KEY` | `40` | Per-key requests per rolling minute |
| `MAX_WAIT_SECS` | `900` | Max time a request waits for a slot / retries before giving up |
| `HEARTBEAT_SECS` | `10` | SSE keepalive interval while waiting |
| `MODELS_TTL_SECS` | `600` | `/v1/models` cache lifetime |
| `STREAM_IDLE_SECS` | `300` | Abort a stream after this much upstream silence (0 = off) |
| `STRICT_PASSTHROUGH` | `false` | Never modify request bodies (disables usage injection) |
| `REF_PRICE_IN` / `REF_PRICE_OUT` | `0.5` / `2.0` | $/1M-token reference prices for "dollars saved" |
| `HISTORY_DAYS` | `30` | Metrics-history retention in days (0 = forever) |
| `DATA_DIR` | `data` (`/data` in Docker) | Where `history.jsonl` lives; empty = in-memory only |
| `RUST_LOG` | `nim_proxy=info` | Log filter |

## Operations

- **Image**: built `FROM scratch` — a ~3.5 MB static musl binary with TLS roots compiled in. No shell, no libc, no CA bundle. Runs as a non-root UID with `read_only`, `cap_drop: ALL`, `no-new-privileges`; rootless Docker/Podman compatible.
- **Healthcheck**: the binary doubles as its own probe (`nim-proxy --health`); `docker ps` shows `healthy`.
- **Logs**: an ASCII banner + structured startup detail, then one access line per request (`200 alice model /v1/chat/completions (3210 ms)`). ANSI color is TTY-detected, so `docker logs` stays clean.
- **Metrics**: Prometheus exposition at `GET /metrics` (scrapeable by any OTel collector's Prometheus receiver). Full series list below.
- **Shutdown**: SIGTERM and SIGINT both drain gracefully.

| Metric | Labels | Meaning |
|---|---|---|
| `nimproxy_requests_total` | client, model, path, status | Every request (`status` includes `disconnect`, `stall`, `stream_error`) |
| `nimproxy_prompt_tokens_total` | client, model | Prompt tokens, from upstream `usage` |
| `nimproxy_completion_tokens_total` | client, model, source | Completion tokens; `usage` = exact, `estimate` = per-SSE-event fallback |
| `nimproxy_ttft_seconds` | model | Upstream send → first streamed byte |
| `nimproxy_tokens_per_second` | model, source | Generation speed |
| `nimproxy_upstream_seconds` | model | Non-streaming upstream latency |
| `nimproxy_queue_wait_seconds` | — | Time waiting for a rate-limit slot |
| `nimproxy_queue_depth` / `nimproxy_active_requests` | — | Live load gauges |
| `nimproxy_lane_requests_total` | lane | Requests per key lane |
| `nimproxy_lane_benched_total` | lane, status | Upstream 429/5xx/connect per lane |
| `nimproxy_affinity_total` | result | Conversation routing: `sticky` / `spill` / `none` |
| `nimproxy_unauthorized_total` | — | Rejected API requests |
| `nimproxy_login_failures_total` | — | Failed dashboard logins |
| `nimproxy_shed_total` | — | Requests shed at the in-flight cap |

The `model` and `path` labels are sanitized (safe charset, length-capped) and `model` cardinality is bounded, so untrusted clients can't inject into the exposition format or explode the registry.

## Testing

Three layers, all runnable locally:

```sh
cargo test          # 26 unit + 19 end-to-end tests (real binary vs scripted mock NIM)
```

The e2e suite covers auth (API keys, admin password / session cookie / Bearer, fail-closed boot posture), 429 ride-out with key failover, Retry-After timing, pacing enforcement, fail-fast 504s, conversation affinity, models caching, usage injection (incl. rejection fallback), stalled-stream recovery, label-injection sanitizing, security headers, metrics accuracy, history persistence across restart, and SIGTERM.

Load test (100 concurrent clients against a mock that *strictly enforces* NIM's per-key window and counts violations):

```sh
python3 scripts/mock_nim.py --enforce --rpm 40 --port 9999 &
NIM_API_KEYS=k1,k2,k3 NIM_BASE_URL=http://127.0.0.1:9999 cargo run --release &
python3 scripts/loadtest.py --clients 100 --requests 3
```

Exits non-zero on any client-visible failure or a single upstream rate violation. This harness is what caught the boundary-jitter bug that motivated the 1 s window margin.

## FAQ & limitations

- **Is this against NVIDIA's ToS? It's designed not to be.** The proxy never exceeds any key's rate limit — that's its entire purpose. Keys are issued per developer account; whether you pool keys with friends is between you and [NVIDIA's terms](https://www.nvidia.com/en-us/agreements/) — the proxy just guarantees each key behaves.
- **Non-streaming requests can't be heartbeated** (no wire format for it) — they wait silently through pacing/retries up to `MAX_WAIT_SECS`. Agent harnesses stream, so this rarely matters.
- **One instance per key set.** Rate state is in-memory; two replicas sharing keys would each assume the full 40 RPM. Run one instance (it comfortably saturates far more keys than you can register).
- **Rate windows reset on restart.** A restart right after heavy traffic can draw a burst of 429s — the retry machinery absorbs them invisibly.
- **Chart history in a Live view lives in the browser** (~20 min); range views and totals come from server-side history and survive refresh.
- **"OTel metrics?"** Prometheus exposition format, which every OpenTelemetry collector ingests natively (`prometheus` receiver). In secure mode the scraper authenticates with `Authorization: Bearer <ADMIN_PASSWORD>`.
- **No built-in TLS.** Terminate TLS at a reverse proxy or platform edge for any exposed deployment; set `TRUST_PROXY=true` so session cookies are marked `Secure`.
- **Sessions reset on restart.** The cookie signing key is random per boot, so a restart logs everyone out of the dashboard (API keys are unaffected).

## Project knowledge base

The `knowledge/` directory is an [Open Knowledge Format](https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing) bundle — design decisions with their reasoning, validated research about NIM, per-component architecture notes, and runbooks, all cross-linked markdown. Start at [`knowledge/index.md`](knowledge/index.md). `AGENTS.md` tells AI agents how to maintain it.

## License

MIT — see [LICENSE](LICENSE).
