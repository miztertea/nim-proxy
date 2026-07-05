---
type: Runbook
title: Test strategy
description: Unit, end-to-end, load, and fuzz layers — what each catches and how to run them.
tags: [testing, ci]
timestamp: 2026-07-02T00:00:00Z
---

# Test strategy

Four layers (unit, end-to-end, load, fuzz). CI (`.github/workflows/ci.yml`)
runs the unit + e2e suites plus a full gate set on every PR: `fmt` + `clippy
-D warnings` + tests (the `check` job), `coverage` (≥80%, `cargo-llvm-cov`),
an `msrv` build against Rust 1.87, `cargo-deny` (advisories + bans + licenses),
`gitleaks` secret scan, `workflow lint` (`actionlint` + `zizmor`),
`dependency review`, and a `docker build` with a container healthcheck smoke.
Three more workflows run outside PR CI: **CodeQL** SAST (`codeql.yml` — PR +
push + weekly), a weekly **cargo-deny advisories** audit (`audit.yml`), a
weekly **fuzz** smoke (`fuzz.yml`, layer 4 below), and the weekly **OpenSSF
Scorecard** scan.

## 1. Unit — `cargo test` (in `src/`)

Pool semantics (window spread, least-loaded, sticky/spill flags, penalize,
release), dispatcher ordering and deadline fail-fast, SSE scanning, history
retention/downsampling. Fast, deterministic, no I/O.

## 2. End-to-end — `tests/e2e.rs` + `tests/support/mod.rs`

Each test launches the **real binary** (`CARGO_BIN_EXE_nim-proxy`) against an
in-process mock NIM whose next responses are scripted per test
(`Behavior::{RateLimited, ServerError, BadRequest, BadRequestIfInjected,
Hang, Ok}`). Boot uses a **pre-written `config.json` in a tempdir `DATA_DIR`**
(`start_proxy_with`, cleaned on drop) or drives the `/setup` wizard
(`start_proxy_fresh` + `complete_setup`); `expect_refuses_to_start` covers a
corrupt store, `version>1`, and an unwritable `DATA_DIR`. Covers: the setup
posture (`/v1`→503, `/`→302 `/setup`) and wizard happy path, open vs keyed
`/v1`, multi-user login / session cookie / scraper Bearer, role and ownership
denials, the config-store round-trip and live pool rebuilds mid-run, per-model
worker-exhaustion governing, 429 ride-out with key failover, Retry-After
timing, verbatim error relay, fail-fast 504, pacing enforcement, conversation
affinity (pin + spread), models cache single-hit, usage injection incl.
rejection fallback and kill switch, stalled-stream cutoff, metrics accuracy
(exact token counts), history persistence across restart, SIGTERM, and
dashboard/config routes.

## 3. Load — `scripts/loadtest.py` vs `scripts/mock_nim.py --enforce`

The enforcing mock plays a *strict* NIM: true per-key sliding window,
counting every violation. `--worker-slots N` adds NIM's orthogonal per-model
worker-concurrency cap (emitting the real exhaustion error) so the
[governor](../architecture/governor.md) is exercised; `loadtest.py` reports
worker exhaustions + peak per-model concurrency. 100 concurrent clients, mixed
streaming/buffered, multiple models and client tokens. **Exit is non-zero on a
single client-visible failure or a single upstream rate violation.**

```sh
python3 scripts/mock_nim.py --enforce --rpm 40 --worker-slots 32 --port 9999 &
cargo run --release &     # boots into first-run setup (no app-level env vars)
# complete the wizard at /setup — base URL http://127.0.0.1:9999, add the mock's
# keys, set the API mode to open (or mint a client key for --proxy-keys)
python3 scripts/loadtest.py --clients 100 --requests 3
```

This layer earned its keep on day one: it caught ~2% boundary-jitter
violations that unit and e2e tests structurally cannot see, leading to
[window-jitter-margin](../decisions/window-jitter-margin.md); it now also gates
the governor's convergence and zero-violation invariant across live pool
rebuilds.

## 4. Fuzz — `cargo +nightly fuzz run <target>` (in `fuzz/`)

libFuzzer/cargo-fuzz harnesses over the three surfaces that parse bytes we
don't control, asserting *never panics* plus each surface's invariant:
`sse_scan` (upstream SSE arrives arbitrarily fragmented — fed whole and
re-fragmented, asserting the 1 MiB pathological-line guard), `sanitize_label`
(the metric-injection defense: output is non-empty, ≤64 chars, safe charset),
and `config_roundtrip` (operator-edited `config.json`: parse never panics,
serialize→parse→serialize is a fixpoint). `fuzz.yml` smoke-fuzzes each target
60s weekly, on demand, and on PRs touching `src/proxy.rs`, `src/config.rs`, or
`fuzz/**`; it is deliberately **not** a required merge check. Seed corpora live
in `fuzz/seeds/` (real SSE shapes, hostile label bytes, a full store); the
evolved working corpus in `fuzz/corpus/` is gitignored. Run one locally:

```sh
cargo +nightly fuzz run sse_scan -- -max_total_time=60
```

Dashboard changes get one more check: real-browser screenshots (headless
Chromium) under live traffic (the UI is dark-only since the operator-console
redesign), inspected by eye — as superuser/admin/user, confirming each role
sees the right Settings sections.
