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
-D warnings` + tests (the `check` job), `coverage` (≥90%, `cargo-llvm-cov`),
an `msrv` build against Rust 1.87, `cargo-deny` (advisories + bans + licenses),
`gitleaks` secret scan, `workflow lint` (`actionlint` + `zizmor`),
`dependency review`, and a `docker build` with a container healthcheck smoke.
Three more workflows run outside PR CI: **CodeQL** SAST (`codeql.yml` — PR +
push + weekly), a weekly **cargo-deny advisories** audit (`audit.yml`), a
weekly **fuzz** smoke (`fuzz.yml`, layer 4 below), and the weekly **OpenSSF
Scorecard** scan.

## Proof routing

Use the proof that exercises the changed surface, and preserve the limits of
that proof.

- **Rust logic:** `cargo test` and `cargo clippy --all-targets -- -D warnings`.
- **Canonical history-v1 codec:** `cargo test history::codec::tests --lib`
  exercises sanitized golden boot/sample/checkpoint bytes, canonical encode
  ordering, reordered-object decoding, and every declared negative diagnostic:
  truncated JSON, invalid UTF-8, scalar/type and non-finite failures, duplicate
  semantic series, unknown state/record kinds, and distinct unknown
  format/version refusals. Fixtures contain no local history body, credential,
  prompt, completion, or client-selected value. This unit proof does not open
  a history file or establish stream ordering; storage/startup and recovery
  proofs are separate work.
- **Canonical history store:** `cargo test history::store::tests --lib`
  exercises hard-link first publication, concurrent creators, stale temporary
  preservation, fresh restart boot
  boundaries, poisoned partial runtime writes, changed-sample versus
  idle-checkpoint writes, live capacity, and each test-local filesystem failure
  point. Store tests additionally prove whitespace-only input is fatal.
  `cargo test --test e2e history_startup_is_fail_closed -- --exact` proves the
  real binary never listens with empty or future-version canonical history;
  the named stale-temporary and config-history E2E checks prove opaque
  count-only warning and live canonical `file_bytes` reporting.
- **Canonical history recovery:** `cargo test history::store::tests --lib`
  also exercises the 14-row file-order stream table, the explicit four-state
  scanner, physical timestamp bounds, append-only unterminated-tail handling,
  damaged/boot-only epoch exclusion, query-scoped diagnostics, and later-epoch
  recovery. `cargo test history::tests --lib` pairs production-codec canonical
  files containing repeated samples versus checkpoints and proves identical
  totals, gauges, points, capacity, bounds, retention baseline, and current-tail
  revision at point budgets 2 and 1000. `cargo test --test e2e
  dashboard_history_reports_completeness -- --exact` proves raw ASCII field
  order plus partial and unavailable API states through the real binary.
- **Canonical history retention:** `cargo test history::store::tests --lib`
  proves cutoff selection, the owning boot and full-sample baseline, recovery
  deferral, every pre/post-rename failure point, replacement-inode append, and
  writer serialization. `cargo test --test e2e
  history_retention_survives_restart -- --exact` is the fast real-binary
  compaction/restart equivalence proof. `cargo test --test e2e
  release_restart_and_idle_history -- --ignored --exact` seeds more than two
  30-day horizons and classifies three exact idle checkpoints plus three exact
  no-traffic restart epochs. Its byte allowances are established from the
  first member of each class before later cycles; the fixed fixture measured a
  6,469-byte base, 195-byte checkpoint allowance, 391-byte restart allowance,
  and 8,227-byte final bound/file. Those values validate the proof fixture,
  not arbitrary metric cardinality or workload payload size.
- **Handlers or wire types:** `UPDATE_OPENAPI=1 cargo test --test openapi`,
  then verify that `openapi.json` has only the deliberate diff.
- **HTTP trust boundaries:** `cargo test routes::tests --lib` proves the
  compiled inventory agrees with generated OpenAPI; `cargo test --test e2e
  route_contract_ -- --nocapture` proves real phase/auth/role behavior,
  request and success content types, side effects, ownership, and durable
  bytes. Neither table is a router.
- **Embedded presentation:** `node scripts/render_check.js --asset-selftest`
  proves the four external-origin classes across nine adversarial forms,
  including protocol-relative URLs, reordered/unquoted attributes, `srcset`,
  and quoted CSS imports; `--assets-only` parses real split sources for
  external and inline active/style contexts. `--served-page-selftest` requires
  hostile/locale probes to derive from and record the real page and catalog
  responses.
  `--syntax-selftest` proves the syntax gate can reject its fixtures, and
  `--syntax-only` parses all five real split scripts without Chromium. These
  modes prove source structure, not behavior. For behavior, run
  `node scripts/render_check.js`,
  `node scripts/render_check.js --escape-probe`,
  `node scripts/render_check.js --page setup`, and
  `node scripts/render_check.js --page setup --escape-probe`. Run
  `node scripts/render_check.js --cleanup-selftest` when changing the browser
  lifecycle; it forces a real descendant to continue writing the profile and
  requires bounded tree shutdown plus verified directory removal. The same
  self-test forces a proxy startup timeout and requires its exit before
  removal, then verifies a missing-locale failure prints its originating cause
  and leaves no run directory.
  Behavior mode builds and starts the current binary, loads the real routed
  page/CSS/JS bytes, rejects an initial missing/error/external resource,
  fulfills Rust-owned typed API responses through CDP, and proves the bounded
  dynamic-style rule cache compacts without changing live geometry. Hostile
  catalog modes mutate only the verified catalog-route response.
  `--catalog-startup-selftest` rejects inline-HTML mutation and requires the
  response-stage catalog hook plus a stylesheet-before-bootstrap guard.
  Startup probes cover bootstrap/catalog failure, malformed schema, delayed
  catalog resolution, request-stage stylesheet loss on all three pages, and a
  later operator application-script loss. They require failed CSS to leave the
  page hard-hidden with no bootstrap/catalog/API work, bootstrap before
  catalog, catalog before reveal or application-data work, and later
  dependency failure to reveal only the emergency message with no subsequent
  application request. The authenticated operator `/api/config` read is a
  catalog-selection prerequisite, not application-data work; startup probes
  allow exactly that read between bootstrap and catalog while continuing to
  reject dashboard/history requests before catalog resolution.
  The 35 stable JSON files in `tests/fixtures/ui/` are generated by the
  `src/api.rs` module test from production response types. Update deliberately
  with `UPDATE_UI_FIXTURES=1 cargo test api::tests::ui_fixtures --lib`; verify
  without mutation with `cargo test api::tests::ui_fixtures --lib` and
  `git diff --exit-code -- tests/fixtures/ui`. Matrix recipes refer to a file
  or `scenarios.json#scenario`, never a JavaScript wire body. Two explicitly
  named resilience rows apply an in-memory transform to a generated body: one
  removes an API error message and one raises Settings integers above 10,000.
  `--all-states` runs 69 named rows against the served application: exact ordered requests,
  DOM observations, and clean page-error, console-error, promise-rejection,
  asset, unexpected-request, and fixture-consumption observations. Its loading
  row holds the real bootstrap response until the hidden-state assertion.
  This is interaction coverage, not layout proof or evidence that every page
  path is covered.
  Every behavior mode treats browser/proxy shutdown and run-directory removal
  as part of the result; cleanup failure is a failing check.
  Locale-precedence mode additionally requires the operator sequence
  bootstrap → authenticated `/api/config` → exactly one selected catalog,
  with separate runs proving an explicit user override and a null override
  falling back to the server default. The probe mutates only verified real
  responses at the existing CDP response boundary; production remains
  `en-US`-only.
  The escape probe enforces the id/descriptor context-owned-sink contract:
  lexical resolver isolation, descriptor coercion refusal and one HTML
  resolver, the exact four-attribute
  allowlist, literal hostile text in element content and allowed attributes,
  stable rejection of forbidden attributes and script/style/SVG destinations
  and replacements, including parented text-node contexts, repeated fixed-node
  placeholders, no compatibility lookup, no parsed markup, and no literal
  entity leakage.
- **Number, date, and duration formatting:**
  `TZ=UTC LC_ALL=en_US.UTF-8 node scripts/formatter_fixture.js --check`.
- **Catalog and UI text:** `python3 scripts/check_i18n.py --selftest`, then
  `python3 scripts/check_i18n.py`; the current selftest has 102 negative or
  control cases, including void-element ownership and Settings source classes.
  When English changes, also run
  `python3 scripts/gen_pseudolocale.py --check`.
- **Locale files:** `python3 scripts/locale_v1.py --selftest` and
  `python3 scripts/locale_v1.py --all`. When the public English projection
  changes, run `python3 scripts/locale_v1.py --update-public` and verify the
  fixture diff. `en-XA` is generated in memory for render tests and must never
  be committed as a production locale.
- **Pacing, pool, dispatch, and affinity:** use the enforcing mock and load
  harness; one upstream violation is failure. Follow the setup prerequisites
  in the [load section](#3-load--scriptsloadtestpy-vs-scriptsmock_nimpy---enforce).
- **NIM response evidence:** `cargo test observation::tests --lib` exercises
  literal sanitized buffered/SSE fixtures plus field, relationship, framing,
  bounded-memory, truncation, finish, tool, and estimator boundaries. `cargo
  test --test e2e observation_preserves_upstream_bytes -- --exact` proves the
  real proxy preserves fixture body/status/content-type behavior while invalid
  reasoning is omitted from existing metrics. `cargo test --test e2e
  dashboard_observation_quality_is_honest -- --exact` additionally locks the
  exact bounded usage-observation exposition, successful and abort finalizers,
  and excluded retry/non-success paths; `node scripts/render_check.js
  --all-states` locks the catalog-backed absent/zero/dominance/tie/live-tail
  quality row independently from history completeness. `python3 scripts/capture_nim.py --selftest`
  exercises environment/URL/profile policy, exact request caps, HTTP/1.0 and
  TLS lifecycles, secret-free diagnostics, owner-only descriptor-contained
  publication, and failure cleanup without a service. `python3
  scripts/sanitize_nim_capture.py --selftest` exercises independent privacy,
  protected JSON/SSE topology, evidence aggregation, exact schemas, symlink/
  mode boundaries, atomic publication, and stale-set rejection. After an
  authorized four-case capture, `python3 scripts/sanitize_nim_capture.py
  --check tests/fixtures/nim-observations` proves the committed fixtures and
  manifest are a complete deterministic set. Self-tests do not establish a
  live NIM shape; live capture does not replace the sentinel and race proofs.
  Follow the [capture runbook](../ops/nim-response-capture.md) for credential,
  human-review, and exact raw-cleanup boundaries.
- **Layout:** use mechanical overflow probes where available plus explicit
  human review under rendered data and supported widths. Behavior passing does
  not prove fit.
- **Task 9 semantics and responsive fidelity:** `node scripts/render_check.js
  --semantic-selftest` drives the served Dashboard with Rust-owned fixtures,
  proves the checker independently catches missing accessible name, invalid
  landmark, non-button action, dialog-focus escape, and data mutation, then
  checks native landmarks/actions and raw model-component text. Run it for
  Setup and Login when changing public markup. `--visual-matrix` resolves 284
  applicable page/state/viewport/locale items, rather than pretending every
  Cartesian combination can exist, and captures one full-document PNG per
  item. The current contract is 188 `en-US` and 96 generated `en-XA` artifacts
  at 390×844, 768×1024, 900×1000, and 1440×1000. Its JSON report records locale
  provenance, layout root/boundary, requested and actual viewport geometry,
  CDP full-document capture geometry, parsed PNG dimensions, allowlisted
  internal vertical scrollers, and clean runtime observations. This is
  mechanical evidence for review, never a claim of human visual approval.
- **Before push:** `cargo fmt --check`.

CI runs the automated checks configured in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml);
it does not perform human layout review or the strict load scenario. `cargo test`
does not execute embedded-page JavaScript. `render_check.js` proves only its
covered fixtures and interactions; it is not evidence that every page path,
locale, or layout is covered. A relevant missing reusable proof is a work
item. A scratch reproduction may demonstrate a problem but does not become the
regression gate.

PR CI runs both agent-guide modes: `python3 scripts/check_agent_guide.py
--selftest` proves each contract check can reject its fixture, and `python3
scripts/check_agent_guide.py` rejects a missing stable contract or unresolved
repository-local guide link.

## 1. Unit — `cargo test` (in `src/`)

Pool semantics (window spread, least-loaded, sticky/spill flags, penalize,
release), dispatcher ordering and deadline fail-fast, bounded SSE observation, history
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

### The wire-format guards

Two tests exist purely so the JSON contract cannot move by accident (see
[typed-responses-and-generated-openapi](../decisions/typed-responses-and-generated-openapi.md)):

- `api::field_order_stays_ascii_sorted` (unit) serializes a populated value of
  every response type and asserts the keys come out ASCII-sorted. Declaration
  order *is* the wire order, and the pre-0.6.6 `json!` bodies were sorted by
  `serde_json`'s `BTreeMap` — so a "tidier" field reorder is a wire change,
  and this is what says so.
- `tests/openapi.rs` regenerates `openapi.json` and fails on any difference
  from the committed file. Regenerate with
  `UPDATE_OPENAPI=1 cargo test --test openapi`; CI's `check` job runs that and
  then `git diff --exit-code -- openapi.json`. `spec_is_usable` additionally
  asserts the document is consumable — 16 operations, each tagged with a
  documented 200, the 13 protected `/api/*` operations inheriting the auth
  requirement, and public bootstrap plus `/setup` explicitly waiving it.
- `routes::tests::inventory_agrees_with_generated_openapi` owns the 34-row
  compiled method/path inventory, including explicit OpenAPI omissions, the
  `/v1/{*path}` template versus concrete probe, all nine presentation assets,
  and zero superuser-exclusive routes. `route_contract_behavior_matrix` sends the
  five-state matrix through the real binary and asserts request/success
  content types, stable boundary errors, side effects, and `config.json`
  bytes. `route_contract_ownership_matrix` adds own/other NIM-key and
  client-key mutations; `route_contract_client_auth_matrix` separately proves
  keyed missing/wrong/valid bearer behavior and pre-setup closure. See the
  [HTTP trust-boundary map](../architecture/http-trust-boundary-map.md).
- `control_plane_rejections_are_typed` sends raw requests through the real
  binary for malformed JSON, JSON media-type failures, body-size rejection,
  invalid dashboard query, unknown/method-mismatched `/api/*`, and post-claim
  setup POSTs. It asserts status, `application/json`, exact `ApiError` bytes,
  and unchanged `config.json` bytes. Run it with `cargo test --test e2e
  control_plane_rejections_are_typed -- --exact`.
- `locale_preferences_are_fail_closed` exercises both locale writers through
  the real binary. Its boundary table distinguishes invalid syntax from
  canonical valid-but-uninstalled tags, proves admin/superuser server-default
  authority and every role's caller-only override/clear, proves authorization
  wins before malformed or wrong-media server-locale bodies, and compares raw
  `config.json` bytes across all 58 rejected mutations. Separate raw account
  rows prove duplicate known fields return `422 invalid_json` without changing
  durable bytes, while password bodies retain unknown-field compatibility.
  Six invalid/noncanonical/uninstalled durable-locale rows run through both
  direct load and real binary startup; each refusal compares the original
  `config.json` bytes before cleanup.
- `unknown_control_plane_paths_are_gated_before_fallback` proves that the
  control-plane fallback remains inside the setup/auth gate: a fresh install
  returns `503 setup_required`, an anonymous configured install returns 401,
  and an authenticated caller receives typed `404 not_found`.
- `closed_setup_posts_win_before_body_rejections` proves both setup POSTs
  answer `409 setup_complete` before malformed, missing/wrong-media, or
  oversized bodies are parsed. Its oversized request sends only an over-limit
  `Content-Length` with `Expect: 100-continue`, proving the route answers
  before buffering 64 MiB. `setup_double_claim_is_rejected_with_409` also
  checks the race loser emits that exact envelope.
- `open_setup_posts_keep_typed_extractor_rejections` covers both manual setup
  extractors while setup is still open: malformed JSON, missing/wrong media
  types, and the bounded body limit retain the exact `ApiError` bytes and do
  not create a config store.

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

For a release run, provide all three measurement options or none:

```sh
python3 scripts/loadtest.py --clients 100 --requests 3 \
  --history-path /absolute/data/history-v1.jsonl \
  --proxy-pid "$PROXY_PID" \
  --report-json /absolute/run/load-report.json
```

The atomic JSON report records start/end history bytes, sampled peak RSS from
Linux `/proc/<pid>/status`, completed/client/upstream counts, worker
exhaustions, and rate violations. It is published before client- or
rate-failure exit evaluation, but invalid history/RSS measurement produces no
report. `python3 scripts/loadtest.py --selftest` is service-free and fails
closed across incomplete CLI options, relative/missing/non-file/symlink/FIFO
history paths, dead/malformed/changing RSS, sampler lifecycle/thread failure,
and report write/fsync/replace/directory-sync crash points.

This layer earned its keep on day one: it caught ~2% boundary-jitter
violations that unit and e2e tests structurally cannot see, leading to
[window-jitter-margin](../decisions/window-jitter-margin.md); it now also gates
the governor's convergence and zero-violation invariant across live pool
rebuilds.

## 4. Fuzz — `cargo +nightly fuzz run <target>` (in `fuzz/`)

libFuzzer/cargo-fuzz harnesses over the three surfaces that parse bytes we
don't control, asserting *never panics* plus each surface's invariant:
`sse_scan` (upstream SSE arrives arbitrarily fragmented — fed whole and
re-fragmented through the bounded private observer), `sanitize_label`
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

Dashboard changes get two more checks.

**Automated — `node scripts/render_check.js`.** Starts the real binary, loads
its served page/assets, and fulfills the 35 Rust-generated typed JSON
fixtures in `tests/fixtures/ui/` at the API boundary. `--all-states` walks the
69 named request/DOM/clean-run interactions (including loading via a held
bootstrap response), every dashboard tab, and chart hovers; it is not a layout
test or a claim that every application path is covered. Regenerate deliberately
with `UPDATE_UI_FIXTURES=1 cargo test api::tests::ui_fixtures --lib`, then
verify with `cargo test api::tests::ui_fixtures --lib` and
`git diff --exit-code -- tests/fixtures/ui`.
`--escape-probe` additionally mutates every catalog-route value with hostile literal
text and fails if a page parses it as markup, renders entity text, permits a
forbidden catalog attribute, or retains an escaped/plain compatibility helper.
The mutation starts from the real catalog response; page HTML and application
assets remain unmodified production responses.
`check_i18n.py --selftest` carries the static forbidden-context matrix, while
`locale_v1.py --selftest` distinguishes raw and entity-encoded catalog markup
and rejects inline-marker structure the runtime cannot render.
The source gate blanks only the lexical resolver declaration and exact
canonical raw-lookup helper bodies, then fails on every remaining bare
`message` identifier. Negative controls cover aliases, `call`, `bind`, global
property access, string-spoofed owners, ASI-separated calls, fake canonical
attribute writes, script/style/SVG element aliases and helper targets, plus
descriptors sent directly to text, URL, style, native-attribute, or raw-SVG
sinks.
This direct-convention scan is a regression guard, not a general verifier for
trusted source deliberately obfuscated with computed properties,
`Object.assign`, or native-method `.call`. Runtime proof supplies the security
boundary for accidental misuse: the resolver is not global, descriptors throw
on coercion, and structured helpers validate destinations and replacements.
This is the only gate that proves the page *runs*: `cargo test` asserts on
served HTML text and `node --check` proves only that it parses. See
[render-gate](../decisions/render-gate.md) and
[message-catalog-and-escaping](../decisions/message-catalog-and-escaping.md).
Run the complete Settings matrix under `--all-states --pseudolocale` after
catalog work: it covers accessibility text, dialogs, validation/toast state,
raw API errors, the catalog fallback for an unusable error body, and exact
grouped Settings values above 10,000.
Page-specific render assertions must compare rendered owned text with the
selected catalog, not en-US literals; otherwise a real `en-XA` selection makes
the harness reject correctly localized output before the untranslated-text
scan runs.
Rust asset e2e tests follow the same ownership boundary: assert the semantic
catalog id in the application asset and the expected source text in the served
catalog, never the retired English literal inside JavaScript.

**Human — screenshots, still.** Real-browser screenshots under live traffic
(the UI is dark-only since the operator-console redesign), inspected by eye —
as superuser/admin/user, confirming each role sees the right Settings sections.
The visual matrix mechanically rejects named overflow, clipping, capture,
runtime, and coverage failures, but it cannot judge hierarchy, density, or
whether a valid wrap remains readable. Inspect the retained full-document
artifacts across all four widths and both locale classes; record who inspected
them and any originals opened from a contact-sheet pass.
