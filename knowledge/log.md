---
type: Log
title: Knowledge base chronology
description: Append-only record of ingests, decisions, and maintenance passes.
---

# Log

## [2026-08-01] maintenance — publish the v0.6.6 upgrade boundary

Task 17 reconciles the user-facing release documents with the completed
foundation rather than the early partial integration state. The current
contract is 443 canonical English messages, 16 generated OpenAPI operations,
split same-origin public/operator assets, production `en-US` only, an
intentional reset into canonical `history-v1.jsonl`, the one cooldown metric
rename, and removal of pricing. The legacy `history.jsonl` file remains
untouched for rollback or manual deletion. Client-auth memory now distinguishes
the machine attribution label `local` from the UI mode label **Open (no
authentication)**. See the [client-auth component](architecture/client-auth.md),
[README](../README.md), [changelog](../CHANGELOG.md), and
[Task 17 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-08-01] maintenance — reconcile current visual applicability

Task 17's fresh visual run exposed a stale Task 9 count in the current gate
instructions and concept pages. The current source and report agree on 284
applicable full-document surfaces—188 `en-US` and 96 generated `en-XA`, evenly
distributed as 71 artifacts at each of four supported widths—with empty
coverage and integrity problem sets. Task 9's 224-artifact record remains its
dated evidence rather than current authority. See the
[test strategy](testing/test-strategy.md), [Dashboard](architecture/dashboard.md),
[render-gate decision](decisions/render-gate.md), and the
[Task 17 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-08-01] maintenance — reconcile Task 16 browser-gate authority

Task 17's knowledge lint reconciles the current browser authorities after the
Task 16 observation-quality expansion: 35 Rust-generated stable UI JSON files
feed 69 named served-app interaction rows. Earlier Task 8 and Task 12 counts
remain dated provenance in their plan and decision records, not current
authority. The original stored-XSS decision now marks `history.jsonl` as the
then-current legacy path and points at canonical `history-v1.jsonl`. See the
[test strategy](testing/test-strategy.md), [Dashboard](architecture/dashboard.md),
[presentation layer](architecture/presentation-layer.md), and
[render-gate decision](decisions/render-gate.md).

## [2026-08-01] component — bounded finalized usage-observation quality

Task 16 adds the private, fixed-cardinality
`nimproxy_usage_observations_total{field,result}` counter at the Task 15
finalization boundary. Each successful observed response or finalized stream
exit contributes exactly five closed outcomes; retries, rejected responses,
and failed body reads contribute none. The Overview derives one catalog-backed
quality row from recognized finite nonnegative selected-range/tail rows without
inventing presence from a synthetic zero baseline: absent is Unavailable,
observed zero is No observations, and positive ties favor invalid,
unavailable, estimated, then measured. History completeness remains separate.
See [NIM observations](architecture/nim-observations.md),
[Dashboard](architecture/dashboard.md), [metrics history](architecture/metrics-history.md),
and [test strategy](testing/test-strategy.md).

## [2026-08-01] component — bounded typed NIM response observations

Task 15 replaces the separate buffered shortcuts and `SseScan` with one private
typed observer. It classifies five usage outcomes, finish results, and tool
counts from the already-buffered body or one bounded current SSE event without
altering relayed bytes. Valid completed nonterminal events are the sole
completion-estimate source; malformed, incomplete, invalid, and unavailable
observations do not invent existing metrics. The ordinary in-process and load
mock reasoning values now satisfy the same `reasoning <= completion` rule used
in production, while the dedicated E2E keeps its invalid response to prove
omission. See [NIM observations](architecture/nim-observations.md), the
[streaming pipeline](architecture/streaming-pipeline.md), the
[test strategy](testing/test-strategy.md), and the
[Task 15 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-08-01] research finding — bounded NIM response topology

One authorized four-request run through local nim-proxy produced successful
buffered basic, streamed basic, buffered tool, and streamed tool observations.
Sanitizer-v1 fixtures retain only Task 15's protected structure: buffered usage
and finish reasons; an SSE comment; null progress followed by `stop`; usage-
only final SSE events; `[DONE]`; and buffered/streamed tool-call topology with
a stable redacted id relationship. The evidence manifest leaves every
unobserved error, malformed/truncated, alternate-finish, multi-choice,
multi-line, repeated-fragment, and usage variation explicitly unavailable.
Human and independent review found no credential, URL, request/response prose,
model/provider/account identity, email, or raw opaque id in the committed set.
The exact raw files were descriptor-cleaned after review. See the
[capture runbook](ops/nim-response-capture.md), the
[test strategy](testing/test-strategy.md), and the
[Task 14 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-08-01] component — bounded atomic canonical history retention

Task 13 compacts only `history-v1.jsonl` behind its exclusive writer. It keeps
each retained epoch's boot, latest strictly pre-cutoff full-sample baseline,
and physical-order records at or after the cutoff; checkpoints never stand in
for state. Intersecting recovery evidence or a live epoch without a full
sample defers replacement. Safe candidates are synced and exactly revalidated
in the same directory, generation-authorized through rename, adopted through
the already-open replacement handle, and followed by directory sync. Pre-
rename failures preserve the old path; post-rename directory-sync uncertainty
keeps the complete new path pending. Deterministic real-binary proof spans
more than two horizons, three zero-sample idle checkpoints, and three exact
restart epochs with an independently established 8,227-byte fixture bound.
The load harness can atomically report history growth and native Linux peak
RSS while retaining its zero-client-failure and zero-rate-violation exit
contract. See [metrics history](architecture/metrics-history.md), the
[test strategy](testing/test-strategy.md), and the
[Task 13 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-07-31] component — recoverable canonical history completeness

Task 12 replaces the strict intermediate canonical reader with one file-order
scanner whose `AwaitBoot`, `AwaitSample`, `Usable`, and `InvalidEpoch` states
retain only complete usable epochs. Supported v1 corruption, invalid state,
unknown kinds, sequence mismatch, regression, boot-only epochs, and an
unterminated tail remain append-only evidence and create bounded gaps; unknown
format/version and empty canonical input remain fatal. Range results now carry
query-scoped completeness and diagnostics. The operator UI distinguishes
complete, partial, unavailable, and not-yet-observed history without inventing
zeros. See [metrics history](architecture/metrics-history.md),
[Dashboard](architecture/dashboard.md), the
[test strategy](testing/test-strategy.md), and the
[Task 12 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-07-31] component — canonical history-v1 fail-closed store

Task 11 publishes `history-v1.jsonl` through a synced same-directory temporary
and no-overwrite hard link, then appends/syncs a store-owned boot before the
server listens. Existing canonical bytes are streamed strictly: records must
be newline-terminated, ordered by nondecreasing timestamp, and sequenced under
a matching boot. An invalid file or failed startup boot append/sync refuses;
sync may leave a complete valid boot while a partial tail remains evidence.
Runtime encode/write/flush/sync failure poisons later writes. Legacy
`history.jsonl` is only warned by path and size, never parsed or changed;
stale canonical temporaries are counted once without inspection. Samples carry
live capacity; unchanged normalized state becomes checkpoints. See
[metrics history](architecture/metrics-history.md), the
[reset-aware decision](decisions/reset-aware-dashboard-history.md), and the
[test strategy](testing/test-strategy.md).

## [2026-07-31] component — canonical history-v1 codec foundation

Task 10 defines the exact `nimproxy-history/v1` boot, sample, and checkpoint
JSON field order with codec-only validation. Sanitized golden and negative
fixtures prove canonical encoding, non-state extension tolerance, whole-sample
state rejection, duplicate-series rejection, and distinct future
format/version diagnostics. A metadata-only read-only corpus stream measured
idle-cadenced sampling but traffic/state-driven history-byte growth without
committing row bodies or label values. Runtime storage, startup, and recovery remain later work; see
[metrics history](architecture/metrics-history.md) and the
[test strategy](testing/test-strategy.md).

## [2026-07-30] decision — make delegation a managed contract

The stable agent guide now treats delegation as a management boundary rather
than an accountability transfer. A handoff must define its operating
environment, Outcome, Proof, Constraint, Ponytail Rung, exact scope and
exclusions, authorized actions, and exhaustion behavior. Divergence first
indicts task sizing, context, and ambiguity; managers correct or split the
contract instead of layering reminders or recording worker blame. See the
[OKF memory decision](decisions/okf-query-ingest-lint.md) and the
[agent-instruction plan](../docs/plans/agent-instructions-okf-memory-implementation.md).

## [2026-07-30] component — applicable full-document visual matrix

Task 9 extends the served-browser gate with 224 explicitly applicable visual
surfaces rather than an impossible page/state Cartesian product. It captures
168 `en-US` and 56 generated `en-XA` full-document PNGs at 390, 768, 900, and
1440 CSS-pixel widths and records locale provenance, layout boundaries, CDP
capture geometry, parsed PNG dimensions, internal vertical scrollers, and
clean runtime observations in one JSON report. Mechanical geometry remains
separate from required human inspection. The final controller and unanchored
independent passes inspected every top/middle/bottom contact sheet and found no
actionable visual defect. See [Dashboard](architecture/dashboard.md), the
[test strategy](testing/test-strategy.md), and the
[Task 9 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-07-30] decision — stabilize native UI semantics and machine fidelity

Task 9 replaced incomplete ARIA tab/switch emulation with native navigation
buttons and pressed buttons, added a native one-time-secret dialog with focus
containment and return, and gave public pages native main landmarks. The
Models tool-call composite is non-wrapping, and model components now preserve
the raw API bytes after the vendor split. The served-page semantic self-test
has independent DOM mutations for accessible-name, landmark, action, focus,
and data-fidelity failures. See [Dashboard](architecture/dashboard.md) and the
[test strategy](testing/test-strategy.md).

## [2026-07-30] fix — align Rust asset proof with catalog ownership

The history-settings asset e2e test now proves that Settings references its
semantic catalog id and the authenticated operator catalog serves the owned
heading. Its former assertion required the retired escaped English literal
inside JavaScript, so full-tests and coverage correctly failed after Task 7
extraction. See the [test strategy](testing/test-strategy.md) and
[Task 7 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-07-30] fix — align Login render proof with selected catalog

The standalone Login render proof now compares repository-owned DOM text with
the selected public catalog. Its former en-US literals became stale when
`--pseudolocale` began selecting generated `en-XA`, causing the harness to
reject correctly localized output before its untranslated-text scan. The
anonymous Login/operator-asset boundary remains unchanged. See the
[test strategy](testing/test-strategy.md) and
[Task 7 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-07-30] decision — complete Settings catalog ownership and guard boundaries

Task 7 extends catalog ownership through Settings Access, Server, Users, and
Account states: repository copy, dialogs, validation/toasts, titles,
placeholders, explicit CLDR count variants, and fallback HTTP-status errors are
catalog-owned. API error bodies, model/client/user values, persisted roles, and
exact numeric values remain raw; Settings applies locale grouping without
dashboard compaction. Native confirm/prompt wrappers are exact canonical
text-only sinks. The source guard now treats the full HTML void-element set as
self-closing so a catalog-tagged input cannot suppress later prose. The
served-browser matrix has 54 rows, including catalog fallback and >10,000
Settings-value probes. See [message catalog and escaping](decisions/message-catalog-and-escaping.md),
[locale guards](decisions/locale-guards.md), [Intl formatting](decisions/intl-formatting.md),
[standard vocabulary](decisions/standard-vocabulary.md), [Dashboard](architecture/dashboard.md),
and [test strategy](testing/test-strategy.md).

## [2026-07-30] component — Rust-owned browser fixtures and interaction matrix

Task 8 makes `tests/fixtures/ui/` a 21-file, typed Rust serialization surface:
`src/api.rs` generates and verifies its bytes, while fixture histogram values
derive from the recorder's production bucket registry. The dependency-free
served-browser gate consumes only those files or `scenarios.json#scenario`; its
52 rows record exact requests, DOM results, and clean-run observations,
including a held-bootstrap loading state. This is named interaction coverage,
not layout or every path. See the [test strategy](testing/test-strategy.md),
[Dashboard](architecture/dashboard.md), [presentation layer](architecture/presentation-layer.md),
[render-gate decision](decisions/render-gate.md), and [Task 8 plan](../docs/plans/v0.6.6-foundation-implementation.md).

## [2026-07-30] research finding — duplicate-safe account JSON boundary

The mixed password-or-locale account endpoint cannot deserialize through
`serde_json::Value`: JSON object construction collapses duplicate known keys
before either typed branch sees them. One typed serde map visitor now rejects
duplicate `current_password`, `new_password`, `action`, and `locale` fields as
`422 invalid_json` without a durable write. The locale action remains an exact
closed object, wrong actions retain `400 invalid_action`, and legacy password
bodies continue to ignore unknown extension fields. The generated operation
documents both session outcomes and all runtime error branches. See the
[typed-response decision](decisions/typed-responses-and-generated-openapi.md),
[HTTP trust-boundary map](architecture/http-trust-boundary-map.md), and
[test strategy](testing/test-strategy.md).

## [2026-07-30] component — dormant locale preference contracts

Config schema v1 now defaults and persists a validated `default_locale` and
stores an optional per-user override. One locale-v1 canonicalizer distinguishes
invalid tags from valid but uninstalled tags; production still installs only
`en-US`. Every authenticated role may set or clear only its own override,
while admin and superuser may change the server default through a separate
atomic settings route. Operator startup resolves public bootstrap →
authenticated config → user override/server default/`en-US` → one gated
catalog before reveal, without browser-language inference. The route inventory
is now 34 contracts and generated OpenAPI is 16 operations. See the
[config-store decision](decisions/ui-managed-config-store.md),
[locale guards](decisions/locale-guards.md), [auth component](architecture/client-auth.md),
[presentation component](architecture/presentation-layer.md), [HTTP trust
boundaries](architecture/http-trust-boundary-map.md), and [test
strategy](testing/test-strategy.md).

## [2026-07-30] component — canonical English and gated catalog startup

The presentation layer now parses one rich `en-US` source and deterministically
projects a complete operator catalog plus the public setup/login subset.
Production no longer commits or serves `en-XA`; render tests generate it in
memory and mutate real catalog-route responses. A public typed locale bootstrap
and two catalog assets bring the route inventory to 33 and generated OpenAPI to
15 operations. Dashboard, Setup, and Login remain hidden until bootstrap and
catalog validation, and response-stage probes pin emergency-only failure and no
later application work. See the [presentation component](architecture/presentation-layer.md),
[locale guards](decisions/locale-guards.md),
[standard vocabulary](decisions/standard-vocabulary.md),
[HTTP trust-boundary map](architecture/http-trust-boundary-map.md), and
[test strategy](testing/test-strategy.md).

## [2026-07-30] fix — close render failure lifecycle gaps

The render gate now uses the same bounded child-stop primitive when proxy
startup times out and during normal shutdown, and observes proxy exit before
removing the run directory. Its cleanup self-test also forces that pre-return
timeout and an intercepted missing-locale failure. The latter must retain its
specific diagnostic instead of being hidden by a generic asset-load summary.
See the [render-gate decision](decisions/render-gate.md) and
[test strategy](testing/test-strategy.md).

## [2026-07-30] fix — make render-gate cleanup part of the result

The served-browser gate no longer kills only Chromium's launcher parent or
downgrades a surviving profile directory to a note. It closes through CDP,
uses bounded process-group escalation for surviving descendants, stops the
proxy, verifies removal of the run directory, and fails if cleanup is
incomplete. A deterministic cleanup self-test forces a descendant profile
writer so this lifecycle cannot be masked by a green page result. See the
[render-gate decision](decisions/render-gate.md) and
[test strategy](testing/test-strategy.md).

## [2026-07-30] component — split and gate embedded presentation assets

Added the [embedded presentation layer](architecture/presentation-layer.md):
three public and four session-gated operator assets, all compile-time embedded
with exact content types and `no-store`. Dashboard, setup, and login now load
same-origin CSS/JavaScript sources; system fonts and fixed local marks replace
Google Fonts and model-logo CDNs. CSP no longer needs external origins or
`unsafe-inline`.

The route inventory now owns 30 contracts including all seven assets. Real
three-state requests prove page/asset gates, CSP, cache policy, and public-byte
isolation. The dependency-free render gate now starts the current binary,
requests production-served page/asset bytes, rejects missing or external
initial resources, invents only captured API payloads through CDP, and drives
all dashboard tabs/chart hovers and setup steps under the production CSP.
Hostile and locale runs derive their HTML from the verified server response
and alter only its inert catalog body. The source scanner parses loading
contexts across direct and protocol-relative URL forms. Runtime-generated
style rules are capped at 512 and compacted to the live DOM; browser proof
forces compaction and pins both the bound and live-node geometry.

## [2026-07-30] decision — plain catalogs with context-owned DOM sinks

Replaced the escaped/plain catalog duality atomically across the guide,
decision, dashboard and setup runtimes, validators, hostile browser probes,
sink inventory, and testing runbook. Recorded in
[message-catalog-and-escaping](decisions/message-catalog-and-escaping.md).

- Page code passes ids to native text/attribute/structured sinks. Fixed-markup
  dashboard builders carry frozen branded descriptors that reveal catalog text
  only when the HTML sink resolves and escapes them.
- Setup emphasis and key/endpoint literals use caller-created fixed DOM nodes;
  repeated value placeholders clone the fixed node and inline marker structure
  must match the source. Remaining fixed-markup builders resolve and escape
  catalog descriptors, and escape plain machine values, at interpolation.
- The negative fixtures name raw and entity-encoded catalog markup, forbidden
  URL/style/event/script/CSS/SVG contexts, direct/concatenated/multiline/
  adjacent raw HTML sinks, direct attribute bypasses, structured-message HTML,
  descriptor misuse, and compatibility helpers separately and require exact
  sink check ids. The normal source gate blanks the lexical resolver declaration
  and exact canonical raw-lookup bodies, then rejects every remaining bare
  resolver identifier, avoiding partial JavaScript owner inference.
- Both populated-page browser probes inject literal entity and markup-shaped
  text into every message, then require literal DOM output, a non-global
  resolver, descriptor coercion refusal, and stable forbidden attribute and
  script/style/SVG destination/replacement/parented-text errors.

## [2026-07-30] ingest — map and enforce every HTTP trust boundary

Added the [HTTP trust-boundary map](architecture/http-trust-boundary-map.md)
and compiled/real-request proofs for all 23 live method/path contracts. Fixed
path constants now feed the existing router without changing its nesting or
handler placement; test-only metadata describes phase/access/OpenAPI rather
than becoming a second dispatch registry.

The real-binary table covers before setup, anonymous configured, ordinary
user, admin, and superuser states; request/success content types, stable
boundary errors, exact coarse upstream-call deltas, session-cookie presence,
and exact config bytes. Separate own/other rows protect NIM-key and client-key
ownership. The map also records zero current asset routes, zero
superuser-exclusive routes, and the deliberate exclusion of upstream-owned
`/v1` schemas from OpenAPI.

## [2026-07-30] lint — bound raw setup-rejection proof and cover open extraction

The raw `Expect: 100-continue` setup body-limit proof now caps its response
read at 4 KiB plus a sentinel byte while retaining its two-second completion
bound. The test also now covers the manual setup extraction path while setup
is open, including malformed/media/body-limit rejections and no config-store
creation. The all-row JSON rejection collector records an absent Content-Type
as a row failure rather than panicking. See [test strategy](testing/test-strategy.md).

## [2026-07-30] lint — complete the typed control-plane rejection gate

Independent review found and the task proof now closes three boundary holes:
the whole nested `/api` router, including its not-found fallback, is inside the
session/setup gate; closed setup POSTs perform their phase check before JSON
extraction or body buffering; and both the normal and racing claim-loser paths
share `409 setup_complete`. The architecture and typed-response decision now
also distinguish bare setup GET 404 from typed setup POST conflict.

The new real-binary E2E proof holds an over-limit setup request at
`Expect: 100-continue`, asserting the 409 response before 64 MiB can be sent;
it also covers malformed and media-type request variants, fallback gating, and
the persisted race outcome. See [test strategy](testing/test-strategy.md) and
[typed responses](decisions/typed-responses-and-generated-openapi.md).

## [2026-07-30] decision — typed JSON control-plane rejections

The JSON control-plane and setup POST boundary now translates Axum extractor,
body-limit, route, and method failures into the stable `ApiError` envelope.
Recorded in [typed-responses-and-generated-openapi](decisions/typed-responses-and-generated-openapi.md).

- `ApiJson` owns syntax/data, media-type, and body-limit normalization;
  `ApiQuery` owns query normalization; the nested `/api/*` router owns only
  its not-found and method fallbacks. Login/form, setup GET, health, metrics,
  and `/v1` remain outside the boundary.
- Post-claim setup POSTs now conflict with `409 setup_complete`; setup GET
  remains a bare 404. The generated OpenAPI responses use `ApiError` for every
  documented non-2xx API response.
- The committed E2E table checks raw response bytes and unchanged
  `config.json` bytes for every rejection row, rather than normalizing the
  response through `serde_json::Value`.

## [2026-07-30] lint — enforce the agent-guide memory contract in CI

PR CI now runs `python3 scripts/check_agent_guide.py --selftest`, which rejects
a validator that fails to observe each named contract check, and `python3
scripts/check_agent_guide.py`, which rejects missing stable guide contracts or
unresolved repository-local guide links.

## [2026-07-30] decision — make agent instructions an OKF memory router

Recorded the repository-memory model in
[okf-query-ingest-lint](decisions/okf-query-ingest-lint.md): `AGENTS.md`
becomes the stable startup contract and router; `knowledge/index.md` remains
the semantic catalog; concept pages own synthesized durable knowledge; and this
log remains append-only chronology. Query → Ingest → Lint uses repository text
search, relative links, and Git history without adding a generator, database,
search service, dependency, or new schema.

The migration boundary is lossless: the existing one-concept-per-file schema,
frontmatter fields and types, relative-link graph, and decision-page ADR shape
were moved into the decision page before their detailed copy is removed from
the guide. Changing proof commands already live in
[test-strategy](testing/test-strategy.md).

Before the guide cutover, `python3 scripts/check_agent_guide.py` exited 1 on
the eight intentionally missing stable contracts: `contract:start`,
`contract:invariants`, `contract:work`, `contract:repository`,
`contract:memory`, `contract:proof-route`, `contract:ponytail`, and
`contract:authority`. It did not report `proof-route`, confirming the durable
proof destination was present before the rewrite.

After the cutover, `python3 scripts/check_agent_guide.py --selftest` exited 0
after all 10 fixtures tripped their exact check ids,
`python3 scripts/check_agent_guide.py` exited 0 with `agent guide OK — stable
contracts present; local links resolve`, and `git diff --check` exited 0. An
independent semantic review compared the pinned old guide, new router,
proof-routing page, decision, index, log, and approved design; it returned spec
PASS and semantic APPROVED with no findings.

## [2026-07-29] lint — 0.6.6 halted by the owner; process failures recorded

The 0.6.6 branch reached draft PR #72 and the owner stopped it. Recorded here
because the next session will read these pages and needs to know how much to
trust them.

**What went wrong was process, not the domain.** A vocabulary change of roughly an
hour became a full day. The instructions in `AGENTS.md` were clear; they were not
followed, and in three specific ways they were *worked around* rather than
forgotten:

- **Proof by throwaway.** `src/setup.html` was verified by a hand-built browser
  harness three separate times, the harness discarded each time, and "setup.html
  has no render coverage" written into the CHANGELOG, a knowledge page and a PR
  body as a disclosed gap. Naming a gap was treated as closing it. The committed
  check (`render_check.js --page setup`) landed only after the owner pushed back.
- **Results reported as verified that were not.** A scratch test grepping for
  `Latency` was reported as proving the retired-term scan handled wrapped terms.
  It had not — `Latency breakdown` is the replacement term, so the prose scan had
  fired. Caught later by `check_i18n.py --selftest`, which asserts *which* check
  trips. Also: a count in a commit message was predicted, not measured.
- **No plan artifact, so no visible scope.** Plan → Act → Verify was performed as
  four lines of prose per edit rather than as a maintained file, so scope growth
  was never in front of anyone and was never brought back to the owner for a
  decision. `docs/plans/` and the rules in `AGENTS.md` → *How to work here* exist
  because of this.

**Trust caveat on this bundle.** The pages written during that session are not
independently verified. One shipped a claim its own file contradicted:
[plural-categories-not-ternaries](decisions/plural-categories-not-ternaries.md)
said two inline plural ternaries existed when six did — found by an adversarial
reviewer reading the file, not by any check. Spot-check page claims against the
code before relying on them, per the standing rule that code is the source of
truth for *what* and the wiki for *why*.

**Also true, and not an excuse for the above:** several defects the branch fixes
were introduced by the branch itself (a module-scope helper collision that threw
on every chart hover, a double-escape, an incomplete status-predicate fix, a lint
guard that exempted every JS assignment). Do not present the fix list as pure
gain; check each against `main`.

Branch state, open decision, verification protocol and remaining work are in
[docs/plans/v0.6.6-presentation-layer-rationalization.md](../docs/plans/v0.6.6-presentation-layer-rationalization.md).

## [2026-07-29] lint — three holes in the untagged-string check, and what shipped through them

Adversarial review of the integration branch before the release PR. The headline
is not any single defect but that **CI was green while 25 English strings
rendered**, one of them a term this release retired.

`check_i18n.py`'s prose scan had three independent blind spots:

- `QUOTED` matched single quotes only, so `setup.html` — double-quoted
  throughout — was effectively unscanned. Eight operator-facing error messages.
- Nothing read text nodes inside template literals. `strip_scripts()` deletes
  the script that holds them and they carry no quotes, so `<span
  class="k">Superuser</span>` was invisible to both scans. Sixteen labels,
  including a `Latency breakdown` literal ten lines from the catalog id for the
  same words, and the retired `rpm total`.
- `NOT_DISPLAY` was applied per line rather than per match, so one `.toFixed(`
  anywhere on a line exempted every string on it — `no eligible traffic`.

`lint_retired_vocabulary` also read only catalog values, which is why `rpm total`
shipped: a label that never entered the catalog still reaches the operator. It
now scans the whole page.

Honest limit recorded rather than papered over: the prose detector still ignores
lowercase single tokens, deliberately, because those are usually enum and metric
label values. `met` and `missed` were found by reading. Counts: 188 → 225
messages (dashboard 159 → 181, setup 29 → 44); en-XA leakage 41 → 30 actionable
runs, measured by the gate both times.

Three further defects the same review found, all fixed here:

- Eight status classifiers compared against the literal `'200'` while the label
  is the upstream status passed through verbatim, so a `204` was counted as
  Success and as an error on two cards inside the same panel. One `IS_2XX` /
  `IS_ERR` pair now, at module scope so the gate can assert on it — the captured
  fixtures contain only 200/429/504/disconnect and can never observe it.
- `setup.html`'s `applyStatic` had no attribute allowlist while `dashboard.html`
  did, and [message-catalog-and-escaping](decisions/message-catalog-and-escaping.md)
  asserted the runtime enforced it. Page corrected, and the correction now names
  which runtimes were checked.
- `--escape-probe` was a double-escape detector blind to the missing-escape
  direction and to every attribute sink. Four injected defects established it:
  two caught, two green. Both halves recorded in
  [render-gate](decisions/render-gate.md), along with the fact that `setup.html`
  has no render coverage in CI at all.

New decision page:
[plural-categories-not-ternaries](decisions/plural-categories-not-ternaries.md) —
two counted labels pluralized with `n === 1 ? '' : 's'`, which no lint here can
see because the English is the absence of a character in one branch.

`tests/fixtures/locales/REMAINING.md` was stale in both directions and is
rewritten from the gate's output rather than by hand.

## [2026-07-29] ingest — the last 29 untagged dashboard strings, extracted

`check_i18n.py` reported 35 problems / 29 unique strings; it now reports
`i18n OK — 188 ids referenced, round-trip clean`. Extraction only: every English
value is byte-identical to the literal it replaced, verified against a copy of
the pre-change source.

- 8 ids were reused rather than minting a duplicate value a translator would
  have to translate twice: `Success rate`,
  `Time to first token`, `Generation speed`, `Inter-token latency`,
  `tokens out`, `Rate limited (429)`, `Unauthorized (401)`, `Other`. The
  taxonomy in `TAX` and `OUTCOMES` is one set of labels rendered twice, so the
  two tables share ids too.
- 22 ids are new — 20 in the dashboard catalog (139 → 159), 2 in the wizard's
  (27 → 29). The whole non-success status taxonomy landed under
  `dashboard.common.status.*`, beside the two entries that were already there.
- The escaping map in
  [message-catalog-and-escaping](decisions/message-catalog-and-escaping.md) was
  contradicted by the code — `kpiCards` stopped escaping `k.label` in `77421e2`
  and the page still said it did — and it never covered `ringGauge`, `legend`,
  the chart hover tooltips, or the two call sites that `esc()` the value
  themselves. All five take `tRaw()`. Page corrected.
- Measured: `--escape-probe` clean, and observed to fail (2 double-escaped runs)
  when two `tRaw()` calls were flipped to `t()`. `--locale en-XA` actionable
  untranslated runs 57 → 41; the remainder are prose in template literals,
  lowercase single-token labels and double-quoted strings, none of which the
  untagged-string lint can see. Those are not extracted and are still English.

## [2026-07-29] decision — the standard vocabulary, committed and enforced

Recorded in [standard-vocabulary](decisions/standard-vocabulary.md).

The vocabulary that this whole release applies was decided before any code was
written and was never committed to the repository — it lived in the planning
bundle. That single omission is the root cause of most of the drift: nothing
downstream could check against it, so five spellings of per-minute and three
names for the model governor survived a pass whose purpose was standardization,
and later work re-derived the decisions from the code and got them backwards.

- The mapping is now a decision page, and the two enforceable halves are
  checks: `locale_v1.py`'s `frozen` (a never-translate token the source uses
  must survive verbatim in every translation) and `check_i18n.py`'s
  `lint_retired_vocabulary` (no catalog value may reintroduce a retired term).
- Written test-first: `frozen-token-dropped.json` and its selftest entry landed
  one commit before the check, observed failing with
  `expected check 'frozen', got nothing`.
- One definition of `NEVER_TRANSLATE`, imported by all three scripts. A copied
  list drifts, and it is about to be load-bearing for eight locales.
- The retired list is deliberately multi-word: `window` is still correct for
  the rate-limit window and `lane` for metric labels, so banning the bare words
  would repeat the label sweep that renamed a rate-limit counter.
- Lint: the `frozen` check failed on the shipped `en-XA` the first time it ran.
  `gen_pseudolocale.py` was accenting frozen tokens, rendering `NÎM` and a
  mangled `/v1` across nine messages — a string no real locale would produce,
  in the locale that exists to prove layout. Generator fixed, en-XA regenerated.

## [2026-07-29] decision — a committed render gate, and the two page defects it found

Recorded in [render-gate](decisions/render-gate.md).

An audit of the merged 0.6.6 work found a P0 that every existing check passes:
`at` was bound three times in `src/dashboard.html`, so every chart threw
`TypeError` on hover, and because the poll loop's bare `catch` treats a throw
as connection loss, a healthy proxy rendered a red "Disconnected" badge and
froze most of the tab. A second, latent defect had `kpiCards` escaping a
catalog value already escaped at load — invisible in English and in `en-XA`,
and due to surface as `&#39;` on the first real translation.

- `scripts/render_check.js` runs the page against captured API payloads and
  fails on any uncaught page error. Committed **before** the fix and observed
  failing at `src/dashboard.html:945`, per the write-the-check-first rule in
  [AGENTS.md](../AGENTS.md).
- This **reverses** the execution plan's decision to ship no browser harness.
  That call traded the harness for a manual browser review; the review did not
  happen, the PR merged with its acceptance criterion unmet, and the P0 landed
  in the gap. The decision page records both sides.
- Fixtures are captured with failures in them — 504s, disconnects, 429
  cooldowns, worker exhaustion, `length` finishes. Every previous leak scan
  measured a healthy proxy, which silently exempts the whole error taxonomy.
- `scripts/mock_nim.py` honors `max_tokens` so the Truncated column is
  reachable. `content_filter` is left uncovered rather than faked.
- Lint: [architecture/dashboard.md](architecture/dashboard.md) described a
  chart tooltip that never worked in this release. The code is now fixed to
  match the page, so the description is accurate again rather than aspirational.

## [2026-07-29] decision — typed API responses and a generated openapi.json

Part of the 0.6.6 rationalization. Replaced the ~20 hand-built
`serde_json::json!` response bodies in `src/settings.rs` (plus the two in
`src/lib.rs`) with `derive(Serialize, ToSchema)` structs in a new `src/api.rs`,
then generated `openapi.json` from them with `utoipa` (pinned `=5.5.0`).
Recorded in [typed-responses-and-generated-openapi](decisions/typed-responses-and-generated-openapi.md).

- The finding that shaped the whole change: the *old* responses were
  **ASCII-key-ordered**, not insertion-ordered. `serde_json::Map` is a
  `BTreeMap` without the `preserve_order` feature, so every `json!` body
  emitted sorted keys, while `derive(Serialize)` emits declaration order. A
  struct written in reading order would have silently reshaped every response.
  Every wire struct is therefore declared ASCII-sorted, four existing types
  (`MetricValue`, `RollupPoint`, `HistoryDiagnostics`, `config::Limits`) were
  reordered to match what they already serialized as, and
  `api::field_order_stays_ascii_sorted` guards the rule.
- `config::GovernorCfg::overrides` moved `HashMap` → `BTreeMap` for the same
  reason. Serialized directly it would have been hash-ordered; the side effect
  is that `config.json` is now byte-deterministic across saves.
- Byte-identity was proved, not assumed. A throwaway harness captured raw
  response bytes for 31 request/response pairs — both `/api/config` role
  views, both dashboard endpoints with and without traffic, every settings
  write, every error branch — before and after the refactor, and compared them
  key-for-key at every nesting level. The 47 API-touching tests in `tests/e2e.rs`
  are **unmodified**.
- `/api/config`'s role filter is now a type: `server`/`users` are
  `Option<..>` that are `None` for a `user` role, so the body is built
  admin-only rather than built-then-augmented.
- Spec covers 14 operations — the 12 `/api/*` routes plus `POST /setup` and
  `POST /setup/validate-key`, both flagged unauthenticated (they run before
  any user exists and 404 once one does). `/v1` is excluded on purpose: that
  contract belongs to the upstream.
- No served UI. `utoipa-scalar`/`utoipa-redoc` fetch JS from a CDN the
  dashboard CSP forbids, and bundling would add ~1 MB to a `FROM scratch`
  image. Ship the file.
- Drift is a build failure: CI's `check` job regenerates the spec and runs
  `git diff --exit-code -- openapi.json`. Because `info.version` tracks
  `CARGO_PKG_VERSION`, a release bump makes the spec stale — added to step 1
  of [Cutting a release](ops/release.md).

## [2026-07-29] decision — pseudolocale, validator, and untagged-string lint

Fifth change of the 0.6.6 rationalization. Recorded in
[locale-guards](decisions/locale-guards.md).

- Written test-first: the nine negative fixtures are a separate commit that
  lands *before* the validator, so they describe what the checks must catch
  rather than what an implementation happens to do.
- The guards found three defects that had survived PR 3's adversarial review
  and its own linter: the runtime-churn strings were never extracted (`Live`,
  `Absolute`, `Disconnected`, `Validating…`, `Copied`, `Select & copy`, and the
  wizard's `<title>`); `locale-v1 --all` paired the wizard's locale against the
  dashboard's source catalog; and `setup.html` called `tRaw()` while defining
  only `rawMsg()`.
- Lint: that last one is worth remembering. `applyStatic` aborts on the first
  throw, so a single undefined helper left the whole page in English rather
  than one string — and neither `node --check` (syntax only) nor `cargo test`
  (never parses the JS) can see it. A dedicated check now covers that class.

## [2026-07-29] decision — Intl formatting keyed to the catalog locale

Fourth change of the 0.6.6 rationalization. Recorded in
[intl-formatting](decisions/intl-formatting.md).

- Written test-first: `scripts/formatter_fixture.js` and 105 golden cases were
  committed *before* any formatter was touched, so the migration's diff is the
  review evidence rather than a claim. Inputs sit on every branch boundary.
- The fixture immediately earned it — it surfaced two arithmetic bugs in the
  hand-rolled `fmt` that had been shipping (`999999` → `1000.0K`, `1e12` →
  `1000.0B`), and caught an inconsistency I introduced myself, where seconds
  used a different unit style from milliseconds and minutes.
- Lint: six `toFixed()` calls remain and all six are inside `style=`
  attributes. Those must NOT be localized — `width:12,3%` is invalid CSS in a
  comma-decimal locale and collapses the element. Display percentages and
  layout percentages are now visibly different code paths.

## [2026-07-29] decision — message catalog and the escape-once contract

Third change of the 0.6.6 rationalization. Extracted the dashboard and setup
wizard to an embedded `en-US` catalog (159 messages) behind a `t()` runtime.
Recorded in [message-catalog-and-escaping](decisions/message-catalog-and-escaping.md).

- Removed the file's only JavaScript-context interpolation before extracting
  anything. `chipHtml` built an `onerror="..."` attribute containing a JS
  statement containing single-quoted JS literals, safe only because
  `initialsOf()` strips to `[A-Za-z0-9 ]` — a monogram helper, not a
  sanitizer. One catalog candidate reached it.
- Catalog values are plain text, escaped once at load. This is not a style
  choice: `metricRow`, `perfBlock`, `tile`, and `prow` interpolate their label
  into `innerHTML` with no `esc()`, and `kpiCards` escapes `k.label` but not
  `k.value`/`k.sub`. Escaping at load covers all of them at one point.
- Lint: two chart hover handlers declared `const t` for a cursor timestamp,
  which shadowed the new global `t()`. Renamed the locals to `at`. Nothing was
  broken yet — no `t()` call sat inside those scopes — but the next one added
  there would have failed silently, and `cargo test` cannot see it.
- Adversarial review found a user-visible bug the tests could not: `applyStatic`
  ran after the tab-restore loop, so deep-linking to any non-Overview tab showed
  that section under a topbar reading "Overview". It also found `sortTable`
  double-escaping every catalog column header, four blind spots in
  `check_i18n.py`, `locales/*.json` validated by nothing, an unenforced
  attribute allowlist, six more `t` shadows, and a dozen half-extracted
  surfaces. All fixed in-PR; each new check proven to fail on a broken input.
- Verification is `scripts/check_i18n.py` plus `node --check` on the extracted
  script bodies plus a headless-Chromium render that mutates three catalog
  values and confirms the DOM follows. The last one matters: `cargo test`
  passes on JavaScript that does not parse, and it did — an earlier pass wrote
  `${...}` into single-quoted strings and only `node --check` caught it.

## [2026-07-29] ingest — standard vocabulary across the interface

Second change of the 0.6.6 rationalization. Applied the agreed standard
ops-dashboard vocabulary to `src/dashboard.html` and `src/setup.html`:
`Harness` → `Client`, dashboard `window` → `time range`, `lane` → `key`,
`Conversation stickiness` → `Session affinity`, `Model-pressure governor` →
`Model limits`, `Where time goes` → `Latency breakdown`, `Rate-limit
pressure` → `Throttling`, `Keyed` → `API key required`, and the composite
`Shed · 401 · failed logins` row split into three.

Display strings only. Every identifier held: DOM ids, CSS classes, `data-*`
attributes, metric names, and the `sortTable` state keys — where `harness`
and `clients` both exist, so renaming that argument would have silently made
the two tables share sort state.

- Lint: the interface now says **key** while the Prometheus exposition still
  says `lane` (`nimproxy_lane_requests_total`, and the `lane` label on
  `nimproxy_lane_cooldown_total`). That divergence is deliberate — renaming a
  second series would be another breaking change, and 0.6.6 already carries
  one. `README.md`'s architecture section still says "lane" for the same
  reason: it describes the code, which has not been renamed.
- `knowledge/architecture/dashboard.md`, `ops/configure-env.md`, and the
  README dashboard-tour section were updated to name the new labels; the
  `agent harness` prose describing what OpenCode/Codex *are* was deliberately
  left alone.
- Adversarial review caught the one dangerous rename: the Access &amp; keys chip
  reads `${k.in_window} / ${k.rpm}`, which is the **rate-limit** rolling
  window, and a blanket `window` → `time range` pass had turned it into
  "in range". That is the single place the two meanings had to stay apart, and
  it is also factually wrong — reverted. Review also found five dashboard
  `window` strings the pass missed (so two empty-state vocabularies were
  visible at once), a `Per lane` heading left sitting above a `Key` table, a
  `lane N` chip beside `Slot N`, `Peak shortfall` rendering without its rpm
  unit, and a dead `sul` binding orphaned by the composite-row split.

## [2026-07-29] decision — lane cooldown naming, savings metric removed

First change of the 0.6.6 presentation-layer rationalization.

- Retired the `bench` idiom for the post-backoff lane state in favor of
  **cooldown**, which the `cooldown_until` field already used. Renamed across
  `src/`, `tests/`, `scripts/`, `knowledge/`, and the `README.md` metrics table;
  `nimproxy_lane_benched_total` → `nimproxy_lane_cooldown_total`;
  `proxy::bench` → `proxy::enter_cooldown`. Recorded in
  [lane-cooldown-naming](decisions/lane-cooldown-naming.md), including why a
  read-time history alias was rejected in favor of a bounded, documented gap.
- Removed the estimated-savings metric and everything feeding it: the
  `Dollars saved` KPI, three `Saved` table columns, `money()`, the `Pricing`
  settings card and config block, `/api/settings/pricing`, and the pricing
  validation branch. Recorded in
  [no-estimated-savings-metric](decisions/no-estimated-savings-metric.md).
  `REF_PRICE_IN`/`REF_PRICE_OUT` deliberately stay in the legacy-env warning
  list. New regression test proves a 0.6.5 store carrying a `pricing` block
  still loads.
- Lint: `knowledge/architecture/key-pool.md` and `governor.md` described the
  state as "benching" while `pool.rs` already named the field `cooldown_until`
  — the pages and the code now agree. Adversarial review of the change also
  caught `README.md` and `architecture/dashboard.md` still advertising the
  deleted savings KPI and sparklines, and both breaking-change notes naming
  `/api/dashboard` where the removed fields were actually emitted by
  `/api/dashboard/now`; all corrected in-PR. Removing the savings card
  orphaned the `valColor` and `green` options on `kpiCards`/`sparkSvg` — it
  was their only caller — so the unreachable branches went with it.

## [2026-07-28] ingest — prepare v0.6.5 maintenance release

- Promoted the accumulated maintenance, dashboard-history corrections, and
  security notes from Unreleased into v0.6.5; synchronized the crate/lockfile
  version and changelog comparison links.

## [2026-07-28] lint — remove duplicate planning and prototype documents

- Removed `docs/superpowers` and the root `design/` prototype handoffs;
  durable design decisions and operational facts remain in the project
  knowledge graph instead of parallel planning archives.

## [2026-07-28] decision — reset-aware dashboard history

- Replaced browser-local lifetime and cross-boot subtraction with a
  server-side reset-aware history index, one selected analytical window,
  separately configured retention/default window, and lightweight current
  polling.

## [2026-07-28] ingest — bound capacity to observed history

- Clarified that a partial default window keeps the first retained sample's
  exact totals without inventing duration or capacity in the unavailable
  prefix; saturated retention arithmetic also keeps extreme valid values safe.

## [2026-07-28] ingest — synchronize dashboard history documentation

- Updated the dashboard, metrics-history, configuration, retention, and auth
  pages to match the typed range/current contracts, revision-bound tail,
  sample-time capacity, live Settings behavior, boot-read file policy, and
  atomic compaction boundary.

## [2026-07-28] lint — correct history sizing premise

- Corrected the disproven fixed snapshot-size estimate using the observed
  235,598,655-byte production history as workload evidence, not a replacement
  universal sizing formula.

## [2026-07-28] ingest — publish GitHub Releases with the runner CLI

Replaced `softprops/action-gh-release` with the GitHub-hosted runner's
preinstalled `gh release create`. The release job still uses the prepared tag,
prepends the container and Sigstore verification instructions to generated
notes, and uploads the same signed tarballs and SBOM assets, while removing
one third-party action from the release trust surface.

## [2026-07-28] ingest — harden workflow inputs and parameterize Compose publishing

Moved release metadata and digest values out of shell-script template
expansions and into step-scoped environment variables, preserving the release
pipeline while removing seven template-injection findings. Added a
`PUBLISH_HOST` Compose interpolation with a loopback default so intentional
LAN exposure lives in ignored `.env` deployment state rather than a tracked
`docker-compose.yml` edit.

## [2026-07-28] decision — delay routine dependency updates for seven days

Applied one seven-day Dependabot cooldown to Cargo, GitHub Actions, and Docker
version updates. The explicit observation window follows zizmor's
supply-chain recommendation without delaying security updates, which
Dependabot exempts from cooldowns.

## [2026-07-28] ingest — repair Cosign v3 release asset signing

The first v0.6.4 release attempt built and signed the multi-arch image but
failed before publishing the GitHub Release. `cosign-installer` 4.1.2 had
changed its default CLI from Cosign v2 to v3 while the release job retained
the legacy `.sig` + `.pem` `sign-blob` flags. Migrated release assets and
verification instructions to `.sigstore.json` bundles, explicitly pinned the
Cosign CLI separately from the installer, and added a real offline
sign/verify contract smoke test to the workflow-lint gate.

## [2026-07-17] ingest — prepare v0.6.4 release metadata

Promoted the accumulated deadline, security, cleanup, and dependency entries
from Unreleased into v0.6.4; bumped the crate and lockfile package version; and
repaired the stale changelog comparison links so v0.6.3 and v0.6.4 each have a
complete release range.

## [2026-07-16] lint — low-risk cleanup batch (dead async, redundant clones)

Staged a tight, YAGNI-scoped cleanup batch on a sub-branch off the
`claude/dependabot-pull-requests-2z8240` integration branch (PR into it, not
into `main`). Scope was chosen for highest signal / least risk: (1) removed a
redundant `async` on `proxy::streaming` — every `.await` lives inside its
`tokio::spawn`ed task, so the function body only spawns and returns a
`Response`; dropping `async` avoids a pointless future wrapper and the single
caller drops its `.await`; (2) removed two redundant `String` clones on the
nim-key / client-key add paths (the value was moved into the struct, not
reused); (3) `clone_from` buffer reuse when re-owning orphan keys during
superuser claim. Deliberately excluded as churn/net-negative: the 13
`redundant_closure_for_method_calls` rewrites (`|x| x.as_u64()` →
`serde_json::Value::as_u64`) which are longer and less readable, the
`clone_into` inversions on cold config-write paths, and adding a new
`[lints.clippy]` gate (not requested; nursery lints risk future CI
false-positives). Verified: fmt clean, `clippy --all-targets -D warnings`
clean, lib 84 + e2e 72 tests green.

Follow-up (same PR): CI's `fmt, clippy, tests` job went red — not on the
cleanup, but on pre-existing code. Rust stable rolled 1.94 → 1.97 on
2026-07-14, and 1.97's improved `clippy::question_mark` now flags the
`else if let Some(basic) = … else { return None }` shape in `auth::identify`
under `-D warnings`. The Dependabot PRs (#47–49) had passed this job because
they ran on 2026-07-09, before the toolchain bump; this PR was the first to
run CI afterward, so it surfaced here. Applied clippy's own `?`-operator
rewrite (behavior identical, auth tests cover it). Reproduced locally by
`rustup update stable` to 1.97.1 before and after the fix. Because #52's head
is the integration branch, merging this PR into it also clears the same
failure for #52.

## [2026-07-16] decision — opt-in absolute request deadlines

Rambler's model tournament showed a buffered request continuing inside the
proxy for 825 seconds after its client timed out. Root cause: `max_wait`,
`request_timeout`, and `stream_idle` bound individual phases, while buffered
handlers cannot reliably observe a downstream disconnect before producing a
response. Added `X-Nim-Proxy-Deadline-Ms` as an opt-in absolute clock across
admission, retries, and generation. Expiry drops the request workflow and its
RAII-owned resources; buffered callers receive `504 deadline_exceeded`, while
streams receive a best-effort terminal SSE error. Status `deadline` and
`nimproxy_deadline_exceeded_total` make the outcome independently visible.

## [2026-07-16] lint — crossbeam-epoch advisory fix (RUSTSEC-2026-0204)

`cargo-deny`'s advisories check went red on `main` — and therefore on every
open Dependabot PR (#47 bytes, #48 actions group, #49 cosign-installer) — after
RUSTSEC-2026-0204 was published against `crossbeam-epoch` < 0.9.20 (invalid
pointer dereference in the `fmt::Pointer` impl for `Atomic`/`Shared`). It is a
transitive dep via `metrics-util` → `metrics-exporter-prometheus`, not a direct
one. Bumped the `Cargo.lock` entry to 0.9.20 (the advisory's recommended fix) —
a single-package lockfile change, no `Cargo.toml` edits. Staged on the
`claude/dependabot-pull-requests-2z8240` integration branch pending a decision
on batching further fixes vs. cutting a release.

## [2026-07-05] v0.6.3 — release-asset signing + CodeQL fixture-noise triage

Maintenance release closing two loose ends from the rigor pass.

- **Release assets are signed** (`cosign sign-blob`, keyless): the `release`
  job now signs each downloadable tarball and the SBOM, attaching a `.sig` +
  `.pem` per asset (needed `id-token: write` on the job). The container
  manifest was already cosign-signed; this extends verifiability to a binary
  pulled straight from the Releases page. Feeds Scorecard's Signed-Releases
  lever. Release notes carry the `cosign verify-blob` command.
- **CodeQL hard-coded-secret triage**: the 5 Critical `rust/hardcoded-
  cryptographic-value` alerts were all false positives — test fixtures (fake
  passwords, RFC-7914 vector salts) plus one scratch buffer (`let mut salt =
  [0u8; 16]` in `hash_password`, immediately overwritten by `getrandom`, but
  the extractor doesn't model the `&mut` overwrite). Added
  `.github/codeql/codeql-config.yml` with `paths-ignore: [tests/**, fuzz/**]`
  (verified: honored for Rust under `build-mode: none`) to kill the separate
  test-crate alert and prevent future fixture noise. The 4 alerts inside
  `#[cfg(test)]` modules in scanned `src/` can't be path-excluded without
  dropping the whole file — dismissed in the code-scanning UI as "used in
  tests" / false-positive. Deliberately did NOT `query-filter` the rule
  globally: it must keep firing on a real hard-coded key in shipped code.
- Fixed two off-by-a-minute cron comments (audit.yml 06:42→06:43,
  scorecard.yml 07:27→07:28) — comments now match the actual cron minute.
- **Coverage expansion 91.4%→96.1% lines** (gate raised 80→90). Applied the
  YAGNI gates to eliminate a planned clock/injection seam: the throttle
  window-rollover branch is reachable by setting `Throttle.window_start`
  directly from an in-module test, and the password-change HashRotated/UserGone
  logic was *already* unit-tested — so no production code changed. Wave 1: pure
  in-module unit tests (auth primitives → `auth.rs` 100%; `config::validate`
  branches; `parse_role`; SSE 1 MiB guard; history load + compaction). Wave 2:
  e2e legs via the existing harness (setup double-claim/orphan-adoption/throttle,
  key-probe non-success + unreachable, client/nim-key/user validation +
  ownership, auth Basic/logout/login redirects). A second blind auditor then
  showed several "excluded" filesystem/boot paths were in fact cheaply and
  deterministically reachable with tricks already used in the harness, so those
  WERE added (round 2): `config` serde-defaults + unreadable-store (invalid
  UTF-8), `history` dir-create/file-open/write failures (dir-as-file trick),
  `lib` empty-`DATA_DIR` and the `--health` probe (subprocess exit codes), and
  the `setup` commit `invalid_config` leg. Genuinely left uncovered (documented
  residual): every handler's `role_of==None` stale-session arm and account's
  own None/commit arms — a REAL TOCTOU race (the auth middleware validates the
  session under one store-lock and releases it; each handler re-locks
  separately, and a concurrent user-deletion must land in that window), not
  deterministically triggerable through the black-box harness without a
  test-only sync hook (the pure logic — `apply_password_change`'s
  UserGone/HashRotated — is already unit-tested); `lib.rs` banner/`tracing`
  logging, `warn_legacy_env`, and the unused `GovernorSettings::default`;
  `tracing!` argument lines the test subscriber never evaluates; and the proxy
  request-flow branches (streaming/models/relay/buffered — Wave 3, out of
  scope this release).
- **PR template** rewritten into a standard, agent-legible form (typed
  sections + a checklist whose conditional groups name their trigger, so an
  agent pulling the template sees which gates apply). Requirements sourced from
  CONTRIBUTING.md + AGENTS.md.
- **Doc-consistency lint** (agent sweep) fixed post-rigor-pass drift: SECURITY
  said `cargo audit` runs in CI (it's `cargo-deny`, self-contradicting the same
  file); release.md's required-checks list and rulesets were stale (missing
  msrv/workflow-lint/dependency-review/codeql, and marked "not yet applied"
  when both `main` and `v*` rulesets are live); the test-strategy page had no
  fuzz layer and an incomplete CI description; CONTRIBUTING framed the gate set
  as "three"; README lacked a supply-chain section and called testing "three
  layers"; bug_report.yml still placeheld `0.4.0`. Test counts (69+53) and all
  internal doc links verified clean — no change needed.

## [2026-07-04] ingest — repo-rigor pass 3: fuzzing the untrusted-byte parsers

- **cargo-fuzz harnesses** for the three surfaces that parse bytes we don't
  control: `SseScan::feed` (upstream SSE arrives arbitrarily fragmented —
  fed whole AND re-fragmented at an input-derived chunk size, asserting the
  1 MiB pathological-line guard), `sanitize_label` (asserting the output
  invariants that ARE the metric-injection defense: non-empty, ≤64 chars,
  safe charset), and the `StoredConfig` JSON round-trip (operator-edited
  file: parse never panics, serialize→parse→serialize is a fixpoint).
- **Crate restructure**: src/main.rs became a 3-line shim over src/lib.rs
  (`nim_proxy::run()`) so the fuzz crate can link the internals. Modules
  stay private; the fuzz surface is `#[doc(hidden)]` wrapper fns re-exported
  as `fuzz_proxy`/`fuzz_config`. All `crate::` paths survived the move
  unchanged; 69 unit + 53 e2e tests unaffected.
- **fuzz.yml**: weekly + dispatch + PR-path-filtered smoke pass (60 s per
  target, nightly via explicit `cargo +nightly` which outranks
  rust-toolchain.toml); crash reproducers upload as artifacts on failure.
  Deliberately not a required merge check. ClusterFuzzLite deferred —
  OSS-Fuzz scaffolding is disproportionate; escalate if Scorecard doesn't
  credit in-repo cargo-fuzz or a target finds a real bug.
- Seed corpora committed under `fuzz/seeds/` (real SSE shapes incl.
  truncated mid-JSON, hostile label bytes, a full store.json), marked
  `binary` in .gitattributes so eol-normalization can't corrupt them.
  `fuzz/corpus/` is the gitignored working corpus — a local run generates
  thousands of evolved entries that must never be committed.

## [2026-07-04] gotcha — pin the PEELED commit SHA for annotated tags

Bumping CodeQL Action v3→v4 broke the Scorecard `publish_results` step:
`400 … imposter commit: 8533807f…`. Cause: the bulk `git ls-remote 'refs/tags/v4*'
| grep -v '^{}'` dropped the peeled entries, so for github/codeql-action's
**annotated** tags it returned the tag-OBJECT SHA, not the commit. GitHub
Actions dereferences a tag-object SHA silently (init/analyze ran green), but
Scorecard's imposter-commit check rejects any pin that isn't a real commit.
Fix: pin the `refs/tags/vX^{}` peeled commit SHA
(`54f647b7…` for v4.36.3). Rule going forward: always resolve pins with
`git ls-remote <repo> refs/tags/TAG 'refs/tags/TAG^{}'` and take the `^{}`
value when the two differ.

## [2026-07-04] ingest — repo-rigor pass 2: hygiene, metadata, MSRV, release-notes taxonomy

- **MSRV**: measured honestly with `cargo msrv find` → **1.87.0**, re-verified
  with `--all-targets` (dev-deps included). Declared in Cargo.toml
  `rust-version` and enforced by a CI `msrv` job that must `rm
  rust-toolchain.toml` first — the toolchain file (channel=stable) outranks
  `rustup default`, so without the rm the job would silently test stable.
- **Language stats**: GitHub listed the repo as an HTML project (design/
  prototypes ≈220 KB HTML vs ≈198 KB Rust). `.gitattributes` marks `design/**`
  linguist-documentation; `src/*.html` deliberately stays counted (shipped
  source).
- **Release notes**: `.github/release.yml` groups generated notes by PR label
  (Dependabot's default `dependencies` label buckets its bumps for free);
  `skip-changelog` opts a PR out. Labels to create in repo settings:
  `security`, `breaking-change`, `skip-changelog`.
- **Docker base digest-pinned** (`rust:1-alpine@sha256:a41f…`); Dependabot's
  docker ecosystem advances the pin. `FROM scratch` has no digest to pin.
- Also: `.editorconfig`, `rust-toolchain.toml` (stable channel — a pinned
  1.XX rejected as weekly bump chores), SUPPORT.md, Best Practices badge
  (bestpractices.dev project 13484, registered by the maintainer same day),
  README contributing/security/support section.
- **CodeQL Rust caveat (investigated, upstream)**: the maintainer spotted
  "11/11 files extracted with errors" in a green CodeQL run. Every
  diagnostic is a failed macro expansion — including std macros like
  `format!` — which is an open limitation of the Rust extractor
  (github/codeql#19966, #19982, #20659), not a local config problem: adding
  a rust-src toolchain + `cargo fetch` produced byte-identical diagnostics
  (195 suppressed, 0/11 clean) and was reverted rather than cargo-culted.
  Queries still run on all non-macro code. Watch the "extracted with
  errors" metric drop as CodeQL bundles update.

## [2026-07-04] ingest — repo-rigor pass 1: SAST, workflow lint, dep review, scheduled audit

Scorecard run #1 scored five checks at 0; the fixable ones drove this PR
(Code-Review and Maintained are structural for a 2-day-old single-maintainer
repo — accepted, time fixes them):

- **CodeQL for Rust** (`codeql.yml`): GA since CodeQL 2.23.3 with
  `build-mode: none`, so the scan needs no cargo build (~4–8 min). Fixes
  SAST=0. clippy-SARIF-to-code-scanning was rejected — Scorecard's SAST check
  doesn't recognize it.
- **Workflow lint** (`lint-workflows` in ci.yml): `actionlint` always gates
  (correctness); `zizmor` uploads all severities as SARIF and gates only on
  high so new low-noise rules can't block unrelated PRs. actionlint isn't in
  install-action's registry → pinned release binary, checksum-verified.
  zizmor immediately paid for itself: it flagged a real template-injection
  (`${{ github.event.repository.default_branch }}` inline in the release
  prepare script) — now passed via `env`. The prepare checkout's kept
  credentials carry an inline `zizmor: ignore[artipacked]` with reason.
- **Dependency review** on PRs (vulnerabilities only; `license-check: false`
  because deny.toml is the single license policy — ClearlyDefined's crate
  data is spottier and would double-gate with drift).
- **Weekly advisories run** (`audit.yml`): same cargo-deny + deny.toml as CI
  (rustsec/audit-check rejected — second ignore-list format). Failure = red
  scheduled run + GitHub's failure email.
- **Release concurrency**: global `release` group, `cancel-in-progress:
  false` — queue, never cancel a half-done signed release; also serializes a
  dispatch racing a tag push.

## [2026-07-04] ingest — actions hardening + native-runner releases (v0.6.2+)

Two follow-ups to the release automation, in the order they shipped:

- **v0.6.2 — native-runner split**: the release image build moved from one
  QEMU-emulated multi-arch buildx invocation to two parallel native jobs
  (amd64 on `ubuntu-latest`, arm64 on `ubuntu-24.04-arm`), pushed by digest
  and stitched by a `merge` job; cosign/provenance/SBOM anchor to the manifest
  digest. Measured: v0.6.1 (QEMU) 34m12s → v0.6.2 (native) **5m18s**, same
  artifact set. Buildx GHA caching + CI concurrency groups landed alongside.
- **Workflow hardening (OpenSSF baseline)**: all actions pinned to full commit
  SHAs (Dependabot's `github-actions` ecosystem keeps pins fresh);
  `step-security/harden-runner` (egress audit) opens every job;
  `persist-credentials: false` on non-pushing checkouts (the release `prepare`
  job keeps credentials — it pushes the minted tag); a weekly OpenSSF
  Scorecard workflow publishes to code scanning + a README badge. The SLSA L3
  isolated builder was considered and deferred (documented in SECURITY.md
  posture; revisit if consumers demand L3). A `v*` tag ruleset (no
  update/delete/force-push; admin bypass) was applied in repo settings —
  "Restrict creations" is deliberately unchecked because the built-in
  github-actions app cannot be added to bypass lists on personal repos and
  the dispatch path mints tags with `GITHUB_TOKEN`.

## [2026-07-04] ingest — release automation: workflow_dispatch cuts releases (v0.6.1)

Tagging by hand (`git tag` + `git push`) was the one release step that
required a local terminal with tag-push rights — and restricted sessions
(e.g. Claude Code remote, whose git proxy only allows the designated branch)
cannot do it at all, which bit the v0.6.0 cut. The Release workflow gained a
`workflow_dispatch` entry point: a new `prepare` job resolves the version
from Cargo.toml on the default branch, refuses if the tag already exists,
mints and pushes the tag itself, and the same run releases end-to-end.
Design constraint that shaped it: tags pushed with `GITHUB_TOKEN` trigger no
follow-on workflow runs (GitHub's recursion guard), so the dispatch path must
never rely on the tag-push event — hence one workflow with two triggers, and
image/release tags now derive from the resolved version, not the git ref.
Full automation (release-plz/release-please) was considered and rejected:
version choice is a scope decision and the CHANGELOG is deliberately
hand-written. Runbook: [release](ops/release.md). v0.6.1 is the maintenance
release that shipped and validated this path.

## [2026-07-04] ingest — v0.6.0 release cut: correctness fixes, wizard client key, outcome charts

The 0.6.0 release closes the loose ends found during the config-store epic
and cuts the version:

- **Streaming inflight accounting fixed**: the `max_inflight` guard now rides
  into the spawned streaming task, so live streams occupy their slot until the
  stream ends (it previously dropped at response-header time, bounding only
  buffered requests). E2e-proven with a hang-stream + shed test.
- **Disconnect noticed during blocked upstream reads** (blind-review finding):
  the streaming relay races each upstream read against `tx.closed()`, so a
  client hang-up frees its `max_inflight` slot immediately instead of at the
  `stream_idle` cutoff — and hung upstreams can't pin slots until restart
  when `stream_idle` is 0. E2e: `disconnected_stream_releases_its_inflight_slot`.
- **Password-change TOCTOU closed**: an own-password change commits only if
  the stored hash is still the one the current password was verified against
  (verify runs outside the store lock); a concurrent admin reset now wins with
  a 409 (`settings::apply_password_change`, unit-tested).
- **Wizard mints the first client key** (default on, explicit warning on
  opt-out — the maintainer's rule: let users run it any way they want, warn
  when it's unsafe): `POST /setup` takes `create_client_key`, returns the
  `npk_` secret once, and the wizard ends on a connect panel (base URL + key +
  copy). [client-auth](architecture/client-auth.md) and the
  [config-store ADR](decisions/ui-managed-config-store.md) updated.
- **Charts for the collected-but-undrawn signals**: a `stackChart` primitive +
  requests-by-outcome-over-time on Reliability; requested output cap
  (`request_max_tokens`) on Clients; tool-call volume per model on Models.
- **Coverage backfill**: governor/pricing/history/limits/account endpoint e2e,
  extended role-denial matrix, unwritable-DATA_DIR boot refusal.
- **README rewritten** as a usage-focused snapshot (logo, live-traffic
  screenshots in `docs/assets/`, boot banner; history/migration framing
  dropped). CHANGELOG promoted to 0.6.0; SECURITY.md supported versions moved
  to 0.6.x.

## [2026-07-04] ingest — UI-managed config store, multi-user, governor (v0.6.0)

App-level configuration moved out of env vars and into a store the app owns,
edited from a new dashboard Settings area and claimed by a first-run wizard.
New ADR [ui-managed-config-store](decisions/ui-managed-config-store.md); the
[auth-posture](decisions/auth-posture-and-dashboard-password.md) ADR gained a
v0.6.0 amendment.

- **Store**: `DATA_DIR/config.json`, version 1, atomic writes (tmp + fsync +
  rename + dir fsync), 0600, snapshot-cached (`RwLock<Arc<Config>>`). JSON not
  SQLite (kilobytes, read-mostly, single-writer; recovery = text edit; zero
  binary weight — revisit triggers recorded in the ADR). Corrupt/unreadable/
  `version>1` = **hard boot error**, never a silent fall-through to setup.
- **First run**: `/v1` → `503 setup_required`, browsers → `/setup`, a 3-step
  wizard (superuser [password ≥10] → ≥1 NIM key validated live → finish) does
  one atomic POST, mints a session, lands on the dashboard. Claim risk accepted
  (matches Grafana/Portainer; loud boot log; no claim token).
- **Multi-user**: roles superuser (undeletable admin — deletion guard only) /
  admin / user; per-key ownership; `GET /api/config` filtered server-side.
  Sessions carry `username || first8(sha256(password_hash))`, so password
  change/reset invalidates sessions and role/deletion apply next request.
  `INSECURE_NO_AUTH` retired → store `open|keyed` mode, `/v1`-only. Client keys
  are `npk_…` 128-bit secrets shown once, stored as SHA-256 digests. Passwords
  PBKDF2-HMAC-SHA256 600k, RFC 7914 vectors.
- **Env retired to 5 container vars** (`HOST`, `PORT`, `DATA_DIR`, `RUST_LOG`,
  `TRUST_PROXY`); legacy vars ignored with one boot warning; no seed-from-env,
  no migration. `configure-env` rewritten; `.env.example` shrunk.
- **Model-pressure governor** (new component page
  [governor](architecture/governor.md)): classifies NIM's per-model
  worker-exhaustion error apart from 429s and backs off the **model** (never
  cools down the lane); adaptive AIMD (engage at half in-flight, +1/stable-min,
  dissolve after 30 clean min) with optional pinned caps. New metrics
  `nimproxy_worker_exhausted_total` / `nimproxy_model_inflight` /
  `nimproxy_model_limit`; a Reliability "Model pressure" card appears once
  engaged.
- **Key pool**: per-key rpm (default 40, 1–10000) replacing global
  `RPM_PER_KEY`; live `rebuild` with rate-state carryover; superuser-key pool
  floor invariant. [key-pool](architecture/key-pool.md) updated.
- Docs swept: README (quickstart→wizard, 5-var table, auth/sharing/metrics),
  `deploy-docker` (volume now holds credentials), `sharing-with-friends`
  (create-a-user flow), `client-auth` rewritten, `examples/README`, CHANGELOG.
- **Lint** — flagged in the summary: the Settings admin API (PR 4) and Settings
  UI incl. `npk_` client-key generation and role-filtered `/api/config` (PR 5)
  are not yet in `src/` on this branch; docs describe the intended v0.6.0
  surface per the plan. The store, wizard, auth, and governor **are**
  implemented.

## [2026-07-03] ingest — dashboard operator-console redesign

Presentation-only redesign of `src/dashboard.html` (data layer, metrics, and
history contracts untouched); see
[dashboard-operator-console-redesign](decisions/dashboard-operator-console-redesign.md)
and the rewritten [dashboard](architecture/dashboard.md) architecture page.

- **IA collapsed from six tabs to five**: `Overview · Models · Clients ·
  Reliability · Capacity`. Compare merged into Models as a scorecard section;
  Harnesses/Proxy/Keys renamed to Clients/Reliability/Capacity.
- **Dark-only.** The light palette and `prefers-color-scheme` handling were
  deleted — a committed design choice, not an oversight.
- **New interactions on every chart and table**: line-chart hover crosshair
  with a per-series tooltip snapped to the nearest sample, and click-to-sort
  tables (sticky header, capped height, internal scroll) whose sort order and
  scroll position survive the 3s live re-render.
- **CSP extended** in `src/main.rs`: `style-src` gained
  `https://fonts.googleapis.com`, a new `font-src` allows
  `https://fonts.gstatic.com` — needed for the Space Grotesk / Spline Sans
  Mono webfonts (system-font fallback offline). Everything else in the CSP is
  unchanged; `tests/e2e.rs` now asserts `font-src https://fonts.gstatic.com`
  alongside the existing CSP checks.
- No new `innerHTML` sink bypasses `esc()` — the redesign added interaction
  state (sort index, hover index) but no new dynamic-string interpolation
  path; see the security-invariant note in
  [dashboard](architecture/dashboard.md).

## [2026-07-03] ops — v0.5.0 first public release prep

Repo went public; cutting the first tagged release (which also gives
`release.yml` its first-ever run — GHCR multi-arch image, keyless cosign,
provenance, SBOM, GitHub Release).

- **New runbook** → [ops/release](ops/release.md): tag-driven release
  procedure, post-release verification checklist, roll-forward policy, and the
  one-time repo settings (private vulnerability reporting, auto-delete head
  branches, recommended `main` ruleset).
- Version 0.5.0; CHANGELOG `[Unreleased]` promoted. `release.yml` gained a
  tag↔Cargo.toml version guard so the OCI label and boot banner can't disagree.
- SECURITY.md now points **only** at private GitHub Security Advisories (no
  maintainer email published); CODE_OF_CONDUCT reports go via the maintainer's
  GitHub profile. README gained a release badge and a published-image
  (`ghcr.io`) quick start.

## [2026-07-02] decision + ingest — Benchmarking observability (v0.4.0)

Turned the proxy into a benchmarking / agent-observability tool. The request
body is already deserialized and every SSE event already scanned, so the
agent-behavior + model-quality signal was in hand but unread.

- **New decision** → [request-shape-metrics](decisions/request-shape-metrics.md):
  capture request shape (messages, tools, sampling params, stream/JSON mode) and
  response quality (finish_reason/truncation, tool calls, reasoning tokens, mean
  TPOT) as bounded-cardinality metrics — **counts and sizes, never content**.
  Shape is labeled by *client* (harness behavior), quality by *model*. Enums
  (`finish_reason`, `tool_choice` mode, `stream`) are clamped server-side.
- **Dashboard** rebuilt from three tabs to six persona-aligned views (Overview,
  Models, Compare, Harnesses, Proxy, Keys); see
  [dashboard](architecture/dashboard.md). Added `scorecard()`/`barRows()`
  helpers and a hash-to-hue color fallback past the six categorical slots.
- **Verified** in headless Chromium against a mock driving two named harnesses
  (opencode: tool-heavy/deep; codex: plain): all six tabs populate, the
  Harnesses view distinguishes both with distinct fingerprints, zero JS errors.
  Cardinality bounding is unit- and e2e-tested.

### Pre-merge hardening pass (same PR)

Before merge: security scan (dedicated dashboard-XSS audit + a full
`/security-review` of the branch) found **zero** vulnerabilities — every new
`innerHTML` value is escaped, every new label is a bounded enum / histogram, and
no route left the admin gate. Documentation swept and confirmed current (six
views, metric table, env vars). Test coverage extended to the buffered
`relay()` quality path, an unknown-`finish_reason`→`other` clamp, JSON mode, and
non-`auto` `tool_choice` (now **29 unit + 21 e2e**). The load harness gained
tool/JSON/sampling variety and a corrected boot command (`INSECURE_NO_AUTH`);
re-run at 80×3 = 240 requests → 0 failures, 0 upstream rate violations, balanced
across all keys, with the new metric series confirmed populated.

## [2026-07-02] ingest — Dashboard reporting polish

Client-side only (no server change, security invariants untouched); surfaces
data already collected but previously under-shown. See
[dashboard](architecture/dashboard.md).

- **Generation speed (tok/s) median/p95 trend** on the Models tab — the
  `nimproxy_tokens_per_second` histogram was only ever shown as one average
  tile. Same bucket-delta quantile machinery as TTFT, filtered to
  `source="usage"` so estimates don't drag the trend down.
- **Non-success outcomes table** on the Proxy tab — ranks every recorded
  non-200 status by count with a plain-language reason and share, so the
  status detail already in `nimproxy_requests_total` is legible instead of
  lumped into one "errors/min" line.
- **Threshold-colored gauges** — capacity (blue→amber≥70%→red≥90%) and success
  rate (green→amber<99%→red<90%) so the dials signal, not just count.
- Verified in headless Chromium against the mock: both new elements render with
  live data, gauges take the amber band under induced load/errors, zero JS
  page errors.

## [2026-07-02] ingest — Security hardening (v0.3.0)

A security review of the merged proxy found a stored-XSS chain (client-supplied
`model` → unescaped dashboard `innerHTML`), unbounded metric-label cardinality,
log injection, and an open-by-default posture (unauthenticated dashboard +
optional API auth). Hardening phase (branch `claude/security-hardening-auth`):

- **Fail-closed auth** → [auth-posture-and-dashboard-password](decisions/auth-posture-and-dashboard-password.md):
  refuse to start exposed without auth; `PROXY_API_KEYS` gates the API,
  `ADMIN_PASSWORD` gates the dashboard/`/metrics`/`/api/history` via an
  HMAC-signed session cookie (Bearer/Basic for scrapers).
- **Input hardening** → [input-sanitizing-and-xss](decisions/input-sanitizing-and-xss.md):
  sanitize + cardinality-cap the `model`/`path` labels at ingest, `esc()` every
  dashboard `innerHTML` sink, add a strict CSP + anti-framing/sniffing headers.
- Constant-time secret compares, failed-login throttle, `MAX_INFLIGHT` flood
  cap, `cargo audit` in CI, compose loopback-publish by default.
- Verified: 45 tests (26 unit + 19 e2e incl. boot posture, session flow, label
  sanitizing, security headers), a real-browser XSS check (payload rendered
  inert), secure-mode load test (300/300, 0 rate violations), `cargo audit`
  clean.

## [2026-07-02] ingest — CI caught the musl proc-macro trap

First real Docker build (in CI — this environment has no daemon) failed:
global crt-static RUSTFLAGS broke proc-macro dylibs on the musl-host alpine
builder. Fixed with an explicit `--target`; details appended to
[distroless-scratch-image](decisions/distroless-scratch-image.md).

## [2026-07-02] ingest — Initial bundle

Compiled the founding conversation into the knowledge base: project purpose
(rate-limit-respecting NIM proxy for agent harnesses), all eight design
decisions to date, three validated research findings about NIM's free tier,
six architecture pages, four runbooks, and the test strategy.

Notable facts captured at ingest time:

- Load test (100 clients, strict enforcing mock) caught 7/307 boundary-jitter
  rate violations at an exact 60s window → [window-jitter-margin](decisions/window-jitter-margin.md).
- Dashboard capacity gauge honestly read 133% during a cold-start burst drain
  before smoothing to a trailing-60s average → noted in [dashboard](architecture/dashboard.md).
- The `/v1/models` schema research killed the idea of API-sourced model
  descriptions; cards enrich from the id namespace instead.
