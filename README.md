# nim-proxy

[![CI](https://github.com/miztertea/nim-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/miztertea/nim-proxy/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/miztertea/nim-proxy)](https://github.com/miztertea/nim-proxy/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Container: GHCR](https://img.shields.io/badge/container-ghcr.io-2496ED?logo=docker&logoColor=white)](https://github.com/miztertea/nim-proxy/pkgs/container/nim-proxy)

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

1. Get one or more API keys at [build.nvidia.com](https://build.nvidia.com) (free, `nvapi-...`; each account needs a unique email and phone number). You'll paste these into the setup wizard, not a file.

2. Run — compose pulls the published image (multi-arch, signed, ~5 MB) with hardened defaults and persistent history. No secrets in `.env` anymore; it holds only container-level vars:

```sh
cp .env.example .env        # optional — only HOST/PORT/DATA_DIR/RUST_LOG/TRUST_PROXY live here
docker compose up -d
```

Or without a checkout:

```sh
docker run -d --name nim-proxy -p 127.0.0.1:8000:8000 -v nim-proxy-data:/data \
  ghcr.io/miztertea/nim-proxy:latest
```

Or build from source: `docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build`, or without Docker: `cargo run --release`

3. Open the dashboard at `http://localhost:8000/`. A fresh install runs a **first-run wizard**: create the superuser account, add at least one NIM key (validated live against the upstream), finish → you're on the dashboard, logged in. Point your client at `http://localhost:8000/v1`.

> **The first visitor to a fresh install becomes the superuser** — the setup window is claimable, so finish the wizard immediately (the boot log says so). Until it's done, `/v1` is closed (503) and browsers are sent to `/setup`. Everything app-level — NIM keys, client keys, the open/keyed API mode, users, limits — is configured in the dashboard, not env vars. See [Security & deployment](#security--deployment).

## Client recipes

Model IDs are passed through verbatim — use any ID from the [NIM catalog](https://build.nvidia.com/models) (or `curl localhost:8000/v1/models`). In `keyed` mode (default) use a client API key (`npk_…`) you mint in the dashboard Settings as the API key; in `open` mode no client-side key is needed.

**OpenCode** — `opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "nim": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "NVIDIA NIM (proxied)",
      "options": { "baseURL": "http://localhost:8000/v1", "timeout": false },
      "models": {
        "moonshotai/kimi-k2-instruct": { "name": "Kimi K2 Instruct" },
        "deepseek-ai/deepseek-r1": { "name": "DeepSeek R1" }
      }
    }
  }
}
```

Set `options.timeout: false` so OpenCode waits through the proxy's rate-limit heartbeats instead of aborting. For a complete config tuned for **GLM-5.2** (context, compaction, sampling), copy [`examples/opencode.json`](examples/opencode.json) — see [`examples/README.md`](examples/README.md) for the rationale behind each setting.

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
- **Heartbeats instead of failures.** For streaming requests the proxy commits to `200 text/event-stream` immediately and emits SSE comment lines (`: heartbeat` — ignored by every OpenAI client) while it waits for a slot or rides out upstream 429/500/502/503/504 with `Retry-After` honored and instant failover between keys. Non-retryable errors surface as in-stream `error` events. Streams that stall mid-generation are cut after the `stream_idle` limit.
- **Model-pressure aware.** NIM caps per-model worker concurrency independently of the 40 RPM key limit; the proxy detects that specific exhaustion, backs off the affected *model* adaptively (never wasting healthy key capacity on failover), and surfaces it on the dashboard — see [How it works: governor](knowledge/architecture/governor.md).
- **Pass-through with one exception.** Bodies are forwarded untouched, except: streaming chat requests get `stream_options: {"include_usage": true}` injected so token accounting is exact rather than estimated. If a model rejects the field (400), the proxy retries untouched and never injects for that model again. Turn on `strict_passthrough` in Settings to disable entirely.
- **Local answers where possible.** `GET /v1/models` is cached (10 min default, single-flight refresh), so harness catalog polls don't burn rate budget.

## Dashboard

Served at `GET /` — a single embedded HTML file, no Grafana, no config. Because the proxy sits in the request path for every harness and model, it doubles as a **benchmarking and agent-observability tool**: it can see how tool-heavy each harness is, how deep its conversations run, how it tunes sampling, where models truncate, and how much "thinking" a reasoning model burns — all from counts and sizes, never message content. A dark, NVIDIA-green "operator console": a left sidebar nav (collapses to an icon rail below 860px) and a top bar with range pills, in Space Grotesk + Spline Sans Mono loaded from Google Fonts (falls back to system fonts if the CDN is unreachable). Five persona-aligned tabs, each ordered at-a-glance → trends → detail:

- **Overview** — the one-screen landing: dollars saved, capacity and success-rate ring gauges (threshold-colored), request/token/savings sparklines, a health strip (active/queued/shed/401/failed-logins), and top models & harnesses.
- **Models** — ranked model cards, TTFT / generation-speed (tok/s) / inter-token-latency (TPOT) / upstream-latency median-p95 charts, tokens-per-minute by model, a "how responses end" (truncation) breakdown, reasoning-vs-output share, a head-to-head scorecard (best value per column highlighted) with a generation-speed bar race, and a full per-model table.
- **Clients** — what each agent is *doing*: tool intensity (avg tools/request), conversation depth (avg messages/request), sampling fingerprint (avg temperature), streaming-vs-buffered mix, and a per-harness leaderboard.
- **Reliability** — an availability hero (window success rate against a 99.9% SLO with an error-budget bar) and a "where time goes" latency breakdown (queue wait / first token / generation), live load with an error-taxonomy breakdown, request/outcome/load charts, an hour-of-day heatmap, a ranked non-success-outcome breakdown, a reliability & security panel, a request-types panel, and a per-client leaderboard.
- **Capacity** — a saturation bar (current load vs aggregate capacity, with a peak marker) and a provisioning readout that flags when you're a key short, rate-limit pressure (429s, conversation stickiness, lanes), per-lane utilization meters, 429s-per-minute by lane, and a per-lane table.

Every line chart has a hover crosshair snapped to the nearest sample with a per-series tooltip; every table is click-to-sort (sticky header, capped height with an internal scroll) — sort order and scroll position both survive the 3s live refresh.

**Time ranges & history.** The filter row offers Live (pausable, 3 s refresh) plus 1h/6h/24h/7d/30d presets and a custom calendar date-time range. Range views are served from the proxy's own history: a ~4 KB metrics snapshot every 5 minutes, kept for the retention days set in Settings (default 30, `0` = forever) in `DATA_DIR` (a Docker volume in the compose file; ~35 MB per 30 days). In a range view every tile, card, and table reports totals *for that window* — instant usage reports.

**Settings.** A Settings area (sub-nav: Access & keys · Server · Users · Account; Server/Users hidden for the `user` role) manages NIM keys, client keys, the API mode, limits, pricing, history, the governor, and users — all live. The Reliability tab also grows a **Model pressure** card (per-model worker-exhaustion rate and `inflight vs limit`) that appears only once the governor has engaged, so unaffected deployments see no noise.

## Security & deployment

The proxy **fails closed**. Before setup, the data plane is closed (`/v1` → `503 setup_required`) and browsers are sent to the `/setup` wizard. After setup, the dashboard and all observability **always** require a logged-in user; the `/v1` API is either `keyed` or `open` (a Settings toggle). Credentials live in the config store on the `/data` volume (`config.json`, 0600), not env vars.

### Users & roles

The wizard creates the **superuser** — an admin that can never be deleted (so the last admin can't vanish). From Settings → Users, admins add more users:

- **superuser** — an admin; the one account that can't be deleted, and it always owns ≥1 enabled NIM key (the pool floor).
- **admin** — server settings + user management.
- **user** — own account, own client API keys, own NIM keys. Sees every dashboard tab (identical for all roles) but only their own key rows.

Login is username + password → a signed, HttpOnly, SameSite=Strict session cookie. The cookie carries a fragment of the password hash, so changing or resetting a password logs that user's sessions out instantly; deleting a user kills their sessions on the next request. Passwords are PBKDF2-HMAC-SHA256 (600k iterations). Forgot a password? Any admin resets it. Locked out entirely? Stop the container, empty the `"users"` array in `config.json` on the volume, restart — the wizard re-creates the superuser and keys/settings survive.

### The `/v1` API: keyed or open

- **`keyed`** (default) — clients send `Authorization: Bearer <npk_…>`. Each user mints their own client keys in Settings; a key's 128-bit secret is shown **exactly once** (only its SHA-256 digest + last-4 are stored). Keyed with zero keys rejects everything (fail closed). Unknown keys get an OpenAI-style 401; comparison is constant-time.
- **`open`** (labeled "local") — `/v1` is unauthenticated. Only for loopback or a fully private network. This toggle affects **only `/v1`** — the dashboard is never open.

The compose file publishes `127.0.0.1:8000:8000` by default so a bare bring-up can't leak.

### Scrapers

Prometheus scrapes `/metrics` with `Authorization: Bearer <username>:<password>` (or HTTP Basic) for any dashboard user. Example scrape config:
```yaml
scrape_configs:
  - job_name: nim-proxy
    authorization: { credentials: "<username>:<password>" }
    static_configs: [{ targets: ["nim-proxy:8000"] }]
```
`/health` stays public (load-balancer / Docker probe; exposes nothing).

### Deployment patterns

| Pattern | How |
|---|---|
| **Local self-host** | Keep the default loopback port publish; set the API mode to `open` in Settings if you don't want client keys. Or set `HOST=127.0.0.1` when running the binary directly. |
| **VPS / bare metal** | A TLS-terminating reverse proxy (nginx/Caddy) in front; keep the API `keyed`. Set `TRUST_PROXY=true` so the session cookie is marked `Secure` (needs `X-Forwarded-Proto: https`). |
| **PaaS (ECS / Railway / Fly)** | The platform edge terminates TLS. Set `TRUST_PROXY=true`. Complete the wizard as soon as the instance is reachable. |

**TLS is not built in** — passwords and keys must travel over HTTPS, so terminate TLS at a reverse proxy or platform edge for any exposed deployment. Additional hardening in place: a strict `Content-Security-Policy` and anti-framing/sniffing headers on all responses, a failed-login throttle, and an in-flight cap (`max_inflight`) that sheds floods with a 503.

## Configuration

Since v0.6.0, **app-level configuration lives in the dashboard** (Settings), persisted to `DATA_DIR/config.json`. Env vars now cover container-level concerns only:

| Variable | Default | Purpose |
|---|---|---|
| `HOST` / `PORT` | `0.0.0.0` / `8000` | Bind address and port |
| `DATA_DIR` | `data` (`/data` in Docker) | Where the config store **and** `history.jsonl` live; must be writable (an unwritable dir is a hard boot error) |
| `TRUST_PROXY` | `false` | Trust `X-Forwarded-Proto` and mark the session cookie `Secure` |
| `RUST_LOG` | `nim_proxy=info` | Log filter |

Everything else — NIM keys (per-key rpm, enable/disable, ownership), the upstream base URL, client API keys and the open/keyed API mode, limits (`max_wait`, `heartbeat`, `stream_idle`, `request_timeout`, `models_ttl`, `max_inflight`, `strict_passthrough`), reference pricing, history retention days, the model-pressure governor, and users/roles — is edited in Settings and applies live, no restart. Legacy app-level env vars (`NIM_API_KEYS`, `PROXY_API_KEYS`, `ADMIN_PASSWORD`, `INSECURE_NO_AUTH`, `NIM_BASE_URL`, `RPM_PER_KEY`, `HISTORY_DAYS`, …) are ignored, with a one-line boot warning if any are still set. There's no migration and no seed-from-env.

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
| `nimproxy_tpot_seconds` | model | Mean inter-token latency (time per output token) |
| `nimproxy_upstream_seconds` | model | Upstream latency (streaming + non-streaming) |
| `nimproxy_finish_reason_total` | model, reason | How generations end; `length` = truncation |
| `nimproxy_tool_calls_total` | model | Tool calls emitted |
| `nimproxy_reasoning_tokens_total` | model | Reasoning ("thinking") tokens, from `usage` details |
| `nimproxy_stream_requests_total` | client, stream | Requests per harness, streaming vs buffered |
| `nimproxy_request_messages` | client | Conversation depth per request (histogram) |
| `nimproxy_request_tools` | client | Tools offered per request (histogram) |
| `nimproxy_request_max_tokens` | client | Requested output cap (histogram) |
| `nimproxy_request_temperature` | client | Sampling temperature (histogram) |
| `nimproxy_tool_choice_total` | mode | Tool-selection mode: `auto`/`none`/`required`/`named` |
| `nimproxy_json_mode_total` | client | Structured-output (JSON-mode) requests |
| `nimproxy_queue_wait_seconds` | — | Time waiting for a rate-limit slot |
| `nimproxy_queue_depth` / `nimproxy_active_requests` | — | Live load gauges |
| `nimproxy_lane_requests_total` | lane | Requests per key lane |
| `nimproxy_lane_benched_total` | lane, status | Upstream 429/5xx/connect per lane |
| `nimproxy_affinity_total` | result | Conversation routing: `sticky` / `spill` / `none` |
| `nimproxy_unauthorized_total` | — | Rejected API requests |
| `nimproxy_login_failures_total` | — | Failed dashboard logins |
| `nimproxy_shed_total` | — | Requests shed at the in-flight cap |
| `nimproxy_worker_exhausted_total` | model | NIM per-model worker-concurrency exhaustion events (governed apart from 429s) |
| `nimproxy_model_inflight` | model | Requests in flight per model (governor gauge) |
| `nimproxy_model_limit` | model | Current per-model concurrency cap; `0` = ungoverned |

Request shape (messages, tools, sampling params) is captured as **counts and
sizes only — never message content**. The `model` and `path` labels are
sanitized (safe charset, length-capped) and `model` cardinality is bounded;
`reason`, `mode`, and `stream` are fixed enums — so untrusted clients can't
inject into the exposition format or explode the registry.

## Testing

Three layers, all runnable locally:

```sh
cargo test          # 29 unit + 21 end-to-end tests (real binary vs scripted mock NIM)
```

The e2e suite covers auth (client keys, multi-user login / session cookie / scraper Bearer, role and ownership enforcement, the fail-closed setup posture and the wizard), the config store (round-trip across restart, atomic-save, refusal on corrupt/future-version stores), 429 ride-out with key failover, per-model worker-exhaustion governing, Retry-After timing, pacing enforcement (incl. live pool rebuilds mid-run), fail-fast 504s, conversation affinity, models caching, usage injection (incl. rejection fallback), stalled-stream recovery, label-injection sanitizing, security headers, metrics accuracy (incl. request-shape & response-quality signal on both the streaming and buffered paths, and the finish-reason cardinality clamp), history persistence across restart, and SIGTERM. Tests boot the real binary against a pre-written `config.json` (or drive the `/setup` wizard) in a tempdir `DATA_DIR`.

Load test (100 concurrent clients — a mix of plain, tool-offering, and JSON-mode calls — against a mock that *strictly enforces* NIM's per-key window and counts violations; `--worker-slots` also emits NIM's real per-model worker-exhaustion error so the governor is exercised):

```sh
python3 scripts/mock_nim.py --enforce --rpm 40 --worker-slots 32 --port 9999 &
cargo run --release &     # boots into first-run setup (no app-level env vars)
# complete the wizard at http://localhost:8000/setup: create an account, set the
# upstream base URL to http://127.0.0.1:9999, add the mock's keys, then either set
# the API mode to open or mint a client key to pass as --proxy-keys below
python3 scripts/loadtest.py --clients 100 --requests 3
```

It exits non-zero on any client-visible failure or a single upstream rate violation, and reports worker exhaustions + peak per-model concurrency (which the governor should bound). This harness is what caught the boundary-jitter bug that motivated the 1 s window margin.

## FAQ & limitations

- **Is this against NVIDIA's ToS? It's designed not to be.** The proxy never exceeds any key's rate limit — that's its entire purpose. Keys are issued per developer account; whether you pool keys with friends is between you and [NVIDIA's terms](https://www.nvidia.com/en-us/agreements/) — the proxy just guarantees each key behaves.
- **Non-streaming requests can't be heartbeated** (no wire format for it) — they wait silently through pacing/retries up to the `max_wait` limit. Agent harnesses stream, so this rarely matters.
- **One instance per key set.** Rate state is in-memory; two replicas sharing keys would each assume the full 40 RPM. Run one instance (it comfortably saturates far more keys than you can register).
- **Rate windows reset on restart.** A restart right after heavy traffic can draw a burst of 429s — the retry machinery absorbs them invisibly.
- **Chart history in a Live view lives in the browser** (~20 min); range views and totals come from server-side history and survive refresh.
- **"OTel metrics?"** Prometheus exposition format, which every OpenTelemetry collector ingests natively (`prometheus` receiver). The scraper authenticates with `Authorization: Bearer <username>:<password>` for any dashboard user (or HTTP Basic).
- **No built-in TLS.** Terminate TLS at a reverse proxy or platform edge for any exposed deployment; set `TRUST_PROXY=true` so session cookies are marked `Secure`.
- **Sessions reset on restart.** The cookie signing key is random per boot, so a restart logs everyone out of the dashboard (API keys are unaffected).

## Project knowledge base

The `knowledge/` directory is an [Open Knowledge Format](https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing) bundle — design decisions with their reasoning, validated research about NIM, per-component architecture notes, and runbooks, all cross-linked markdown. Start at [`knowledge/index.md`](knowledge/index.md). `AGENTS.md` tells AI agents how to maintain it.

## License

MIT — see [LICENSE](LICENSE).
