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
# Build and run locally (loopback only — see Security below):
cargo run --release

# Or via Docker, building your working tree (a plain `docker compose up`
# pulls the published GHCR image, not your local changes):
cp .env.example .env      # only container vars now; no keys/passwords
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build
```

The dashboard is served at `http://localhost:8000/` and the OpenAI-compatible
API at `http://localhost:8000/v1`. A fresh run opens the first-run setup wizard
— create the superuser and add a NIM key; everything app-level is configured in
the dashboard, not env vars ([config store](knowledge/decisions/ui-managed-config-store.md)).

## Testing, formatting, linting

These three must be clean before you open a PR — run them locally:

```sh
cargo test                                   # 69 unit + 53 end-to-end tests
cargo fmt                                     # (CI runs `cargo fmt --check`)
cargo clippy --all-targets -- -D warnings    # zero warnings — warnings are errors
```

CI enforces more on every PR, most of it reproducible locally: line coverage
≥90% (`cargo llvm-cov`), a build against the declared MSRV
(`cargo +1.87 check --locked --all-targets`), `cargo deny check` (advisories,
bans, licenses), a `gitleaks` secret scan, workflow linting (`actionlint` +
`zizmor`, only relevant if you touch `.github/workflows/**`), dependency
review, and a Docker build with a healthcheck smoke. Static analysis (CodeQL)
and a weekly fuzz smoke run as separate workflows. If you edit a workflow,
keep every Action SHA-pinned with a `# vX.Y.Z` comment.

`cargo test` launches the **real binary** against a scriptable mock NIM (see
`tests/support/mod.rs`) — booted with a pre-written `config.json` or by driving
the `/setup` wizard, in a tempdir `DATA_DIR`. The e2e suite covers the setup
posture and wizard, open/keyed `/v1`, multi-user login and role/ownership
enforcement, the config-store round-trip, per-model worker-exhaustion
governing, 429 ride-out with key failover, Retry-After timing, pacing
enforcement, conversation affinity, usage injection, stalled-stream recovery,
label sanitizing, security headers, metrics accuracy, history persistence
across restart, and graceful shutdown.

### The fail-closed test posture

nim-proxy **fails closed on purpose** — before setup the data plane is closed
(`/v1`→503, browsers→`/setup`), and after setup every dashboard/observability
surface requires a logged-in session (only `/v1` can be `open`). A
corrupt/unreadable/future-version store refuses to boot. Tests assert this
directly (setup posture, wizard flow, session/scraper auth, role denials,
boot-refusal cases, label sanitizing, security headers). If your change touches
`src/auth.rs`, `src/config.rs`, `src/settings.rs`, the API-key gate or label
sanitizing in `src/proxy.rs`, or the dashboard's `innerHTML`, you **must** keep
those invariants and the tests that guard them. Read the
[auth-posture](knowledge/decisions/auth-posture-and-dashboard-password.md) and
[input-sanitizing](knowledge/decisions/input-sanitizing-and-xss.md) decision
pages before editing.

### The load harness (required for anything touching rate-limiting)

The proxy's core promise is *zero upstream rate violations*. If your change
touches pacing, the key pool, the dispatcher, or affinity, prove it against a
mock that **strictly enforces** NIM's per-key window and counts violations:

```sh
python3 scripts/mock_nim.py --enforce --rpm 40 --worker-slots 32 --port 9999 &
cargo run --release &     # boots into first-run setup (no app-level env vars)
# complete the wizard at /setup — base URL http://127.0.0.1:9999, add the mock's
# keys, set the API mode to open (or mint a client key for --proxy-keys)
python3 scripts/loadtest.py --clients 100 --requests 3
```

It exits non-zero on any client-visible failure **or a single upstream rate
violation**. Zero violations is a hard requirement, not a target — this harness
is what caught the boundary-jitter bug that motivated the 1 s window margin.

### Fuzzing

The untrusted-byte parsers (SSE scanner, label sanitizer, config round-trip)
have cargo-fuzz harnesses under `fuzz/` — CI smoke-fuzzes them weekly and on
PRs that touch the fuzzed code. To run one locally (needs nightly):

```sh
cargo +nightly fuzz run sse_scan -- -max_total_time=60
```

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
