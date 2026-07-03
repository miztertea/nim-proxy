# Contributing to nim-proxy

Thanks for your interest! nim-proxy is a small, focused Rust proxy with exactly
one job: **keep an agent harness within NVIDIA NIM's per-key rate limit so it
never sees a 429.** Contributions that make it better at that job — or safer,
faster, or easier to run — are very welcome.

By participating you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- **Read the design first.** The `knowledge/` directory is the project's
  compiled memory — every non-obvious decision has a page explaining *why*.
  Start at [`knowledge/index.md`](knowledge/index.md). The reasoning that
  constrains your change is very likely already recorded there (rate-limit
  window math, dispatcher fairness, auth posture, sanitizing, etc.).
- **Read [`AGENTS.md`](AGENTS.md).** It is the source of truth for build/test/lint
  commands and for the knowledge-base maintenance rules, and it maps the source
  layout (`src/main.rs`, `proxy.rs`, `pool.rs`, `dispatch.rs`, `history.rs`,
  `auth.rs`, `dashboard.html`).
- For anything beyond a small fix, **open an issue first** so we can agree on the
  approach before you write code.

## Building and running

You need a stable Rust toolchain (`rustup`, edition 2021) and Python 3 for the
load harness.

```sh
# Build and run locally, open mode (loopback only — see Security below):
NIM_API_KEYS=nvapi-xxx,nvapi-yyy INSECURE_NO_AUTH=true cargo run --release

# Or via Docker, building your working tree (a plain `docker compose up`
# pulls the published GHCR image, not your local changes):
cp .env.example .env      # paste keys, pick an auth mode
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build
```

The dashboard is served at `http://localhost:8000/` and the OpenAI-compatible
API at `http://localhost:8000/v1`.

## Testing, formatting, linting

All three must be clean before you open a PR — CI enforces every one of them:

```sh
cargo test                                   # 29 unit + 21 end-to-end tests
cargo fmt                                     # (CI runs `cargo fmt --check`)
cargo clippy --all-targets -- -D warnings    # zero warnings — warnings are errors
```

`cargo test` launches the **real binary** against a scriptable mock NIM (see
`tests/support/mod.rs`); the e2e suite covers auth, 429 ride-out with key
failover, Retry-After timing, pacing enforcement, conversation affinity, usage
injection, stalled-stream recovery, label sanitizing, security headers, metrics
accuracy, history persistence across restart, and graceful shutdown.

### The fail-closed test posture

nim-proxy **fails closed on purpose** — it refuses to start on a network port
without auth. Tests assert this posture directly (boot-refusal cases, session
flow, Bearer/Basic scraper auth, label sanitizing, security headers). If your
change touches `src/auth.rs`, the API-key gate or label sanitizing in
`src/proxy.rs`, or the dashboard's `innerHTML`, you **must** keep those
invariants and the tests that guard them. Read the
[auth-posture](knowledge/decisions/auth-posture-and-dashboard-password.md) and
[input-sanitizing](knowledge/decisions/input-sanitizing-and-xss.md) decision
pages before editing.

### The load harness (required for anything touching rate-limiting)

The proxy's core promise is *zero upstream rate violations*. If your change
touches pacing, the key pool, the dispatcher, or affinity, prove it against a
mock that **strictly enforces** NIM's per-key window and counts violations:

```sh
python3 scripts/mock_nim.py --enforce --rpm 40 --port 9999 &
NIM_API_KEYS=k1,k2,k3 NIM_BASE_URL=http://127.0.0.1:9999 cargo run --release &
python3 scripts/loadtest.py --clients 100 --requests 3
```

It exits non-zero on any client-visible failure **or a single upstream rate
violation**. Zero violations is a hard requirement, not a target — this harness
is what caught the boundary-jitter bug that motivated the 1 s window margin.

### The dashboard

The dashboard is **one embedded file**, `src/dashboard.html` — no build step and
no external assets (optional CDN logos have an offline fallback). Edit the HTML
directly. CI checks the embedded `<script>` block with `node --check`; every
dynamic value written into `innerHTML` must go through the `esc()` escaper.

## Keep the knowledge base in lockstep

This is a hard rule, not a nicety. When a change alters behavior described in
`knowledge/`, **update the affected pages in the same PR** — code and knowledge
must not diverge (`AGENTS.md` §"The knowledge base"):

1. **Ingest** — update any page whose described behavior your change alters.
2. **New decisions** get a new page under `knowledge/decisions/` *and* a row in
   [`knowledge/index.md`](knowledge/index.md), following the ADR shape
   (Context → Options → Choice → Consequences).
3. **Log** — append a dated entry to
   [`knowledge/log.md`](knowledge/log.md)
   (`## [YYYY-MM-DD] ingest|decision|lint — summary`).

The code is the source of truth for *what*; the knowledge base for *why*. If you
spot a contradiction between the two, fix the page and note it in your PR.

## Pull request expectations

Use the [PR template](.github/PULL_REQUEST_TEMPLATE.md). A PR is ready when:

- **Tests and docs move in lockstep** — new behavior ships with tests, and the
  README / `knowledge/` are updated in the same PR.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` are all green.
- Rate-limit-adjacent changes show **zero upstream rate violations** from the
  load harness.
- Security-sensitive changes (auth, sanitizing, dashboard `innerHTML`) preserve
  the fail-closed and escaping invariants — see [SECURITY.md](SECURITY.md).
- Commits are focused and messages explain the *why*.

Keep the scope tight — nim-proxy is deliberately small. A change that makes it do
one thing well is worth more than one that makes it do many things.

Thanks for contributing!
