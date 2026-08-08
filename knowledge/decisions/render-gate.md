---
type: Decision
title: A committed render gate, reversing the plan's "no browser harness" call
description: >-
  The 0.6.6 plan rejected a committed browser harness at ladder rung 1 and
  traded it for a human browser review. The review did not happen and a P0
  shipped in the gap, so the trade is re-made in the other direction.
tags: [testing, ci, dashboard, i18n]
timestamp: 2026-07-29T00:00:00Z
---

# A committed render gate

## Context

Nothing in this repository executes the embedded pages.

- `cargo test` asserts on served HTML **text**. It never parses or runs the
  page JavaScript, so it passes on a page that cannot boot.
- `node --check` proves the syntax parses. Both the `${...}`-inside-single-quotes
  bug and the `const t` shadow parsed fine.
- `scripts/formatter_fixture.js` extracts the real formatter bodies but
  evaluates them in an isolated harness, so it proves the helpers work and says
  nothing about their call sites.
- `scripts/check_i18n.py` and `scripts/locale_v1.py` read source text and
  catalog JSON. Neither renders anything.

The 0.6.6 execution plan considered a Playwright screenshot harness for three
acceptance criteria and killed it at ladder rung 1 for two of them, correctly:
extraction is provable by a text round-trip, and `Intl` output is provable by a
differential table. Both are *stronger* than a screenshot, which cannot see
hidden-tab or offscreen content. For the third — "`en-XA` renders with zero
leakage and zero clipping" — the plan assigned a manual browser review to a
human and explicitly declined to build a harness to avoid a five-minute look.

That reasoning was sound. What broke was the trade: the manual review did not
happen, the PR merged with its acceptance criterion unmet, and a P0 shipped
into the integration branch in the gap.

The P0 is the argument. A module-scope `at()` helper collided with two
pre-existing locals, so every chart threw `TypeError` on hover. Because
`lineChart` re-applies the last hover on each live re-render and the poll
loop's bare `catch` treats any throw as connection loss, a healthy proxy
displayed a red **Disconnected** badge, stopped its uptime clock, and froze
most of the tab. One console error, nothing in any log, and green on every
check in CI.

A second, latent defect had the same shape: `kpiCards` escaped a catalog value
that was already escaped at load. Invisible in English (no KPI label contains
`&` or `'`) and invisible under `en-XA` (which only adds accents and padding),
it would have surfaced as `Jetons d&#39;entrée` on the first real translation.

## Options

1. **Keep the plan's position: no harness, rely on human review.** Free, and
   correct in principle — a person sees clipping and ugliness a script never
   will. But it failed in exactly the way an unenforced convention fails, and
   the class of bug it missed is silent rather than obvious.
2. **Playwright.** The standard answer. It means a `package.json`, a lockfile
   and a dependency tree in a repository that deliberately has no JavaScript
   toolchain, and a browser download in CI.
3. **A committed gate with no dependencies.** Node 22 ships a `WebSocket`
   client, so the Chrome DevTools Protocol is reachable from a plain script,
   and GitHub's `ubuntu-latest` image already has a browser. Ladder rung 4:
   the platform provides it.

## Choice

Option 3. `scripts/render_check.js` starts the current nim-proxy binary,
requests its real page and asset routes, fulfills its fixture API responses
through CDP, walks all five tabs, hovers every chart with real pointer input,
and fails on any uncaught page or initial-resource error. Stdlib only, matching
the convention both existing scripts already declare.

This does **not** replace the human review the plan assigned. Clipping is a
layout property and the gate does not judge it; the `.bval` wrap on Models at
900px is real and was found by eye, not by script. The gate covers the silent
class — a page that throws, a page that never boots, a value escaped twice.

Four properties it needed before it could see anything at all, each of which
first produced a confident green while exercising nothing:

- **It must request production routes and fail external hosts.** The earlier
  gate assembled a private file page and injected an application. The current
  gate starts the binary, authenticates through the real operator boundary,
  observes every required page/CSS/JS request, and fails missing, non-success,
  or cross-origin initial resources. Only API fixture responses are invented.
  Hostile/locale runs capture the real catalog-route response and re-fulfill
  only those response bytes; page HTML and application assets remain the
  binary's unmodified responses. Page and catalog provenance are both required.
- **It must wait for the real load event.** A fixed sleep can measure a parser
  that never reached the application. It asserts `readyState === 'complete'`.
- **It must capture unhandled rejections.** The page boots from an `async`
  IIFE, so a throw there is a rejection, not an exception, and
  `Runtime.exceptionThrown` does not reliably report it.
- **It must keep production source line numbers.** Error capture is installed
  through CDP before navigation rather than injected into page source. Hostile
  catalog runs replace only the catalog-route response; page HTML, scripts,
  and styles remain production-served assets.

The payloads are **captured, not hand-written**, and deliberately contain
failures: 504s, client disconnects, 429 rate-limit cooldowns, worker
exhaustion, affinity spill, and `stop`/`tool_calls`/`length` finish reasons.
Every leak scan before this one measured a page at rest or a *healthy* proxy,
and the entire error taxonomy — the `TAX`, `OUTCOMES` and `REASONS` tables —
only renders when the range contains errors. A fixture without failures
silently exempts them. `content_filter` remains uncovered because there is no
honest way to provoke it from a mock, and that gap is recorded rather than
faked.

## Consequences

### Supersession — Task 8 fixture authority (2026-07-30)

The captured/manual-fixture statements below describe the gate as it existed
when this decision was made; they are not the current fixture authority. Task
8 replaces them with 21 stable JSON files in `tests/fixtures/ui/`, generated
by the `src/api.rs` module test from production response types. Deliberate
regeneration is `UPDATE_UI_FIXTURES=1 cargo test api::tests::ui_fixtures --lib`;
normal mode compares bytes and CI rejects a fixture diff. The 52-row
`--all-states` matrix consumes only a file or `scenarios.json#scenario`, then
asserts exact request order, DOM results, and clean browser observations. Its
loading proof holds a real bootstrap response; it is not a layout test or
complete application-path inventory. This supersession keeps the original
decision's provenance while making typed Rust serialization the current wire
authority.

### Supersession — Task 12 history completeness (2026-07-31)

Task 9 expanded the interaction matrix to 55 rows. Task 12 adds three
Rust-generated fixtures and three browser rows, bringing the current authority
to 24 stable JSON files and 58 named rows. The generated partial fixture proves
incomplete history with usable effective bounds;
`dashboard-unavailable.json` proves an incomplete requested interval inside
global availability with no usable points or invented zeros; and the two
outside-bound fixtures prove ordinary empty requests wholly before or after
global availability remain no-data ranges. All states assert their exact
catalog-owned range status through the same served-byte CDP boundary. The Task
8 numbers above remain the dated scope of that supersession rather than current
totals.

### Supersession — Task 16 observation quality (2026-08-01)

Tasks 15 and 16 extend the typed fixture authority to 35 stable JSON files,
the served-app interaction authority to 69 named rows, and the visual
applicability authority to 284 surfaces: 188 `en-US` and 96 generated `en-XA`.
The added fixtures and rows cover bounded NIM observation topology and the
catalog-backed absent/zero/dominance/tie/live-tail quality states without
changing the gate's Rust-serialization or served-byte boundaries. The Task 8
and Task 12 totals above remain dated provenance rather than current counts.

- Two CI steps in the existing `check` job, so the required-status-check list
  in [release.md](../ops/release.md) does not change.
- `scripts/mock_nim.py` honors `max_tokens` so a capped request finishes with
  `length`. The dashboard's Truncated column was otherwise unreachable and
  would have been exempt from every future check.
- The current fixture set is generated from production Rust response types;
  changing an API shape requires deliberate regeneration and a reviewed
  committed diff, not recapture or JavaScript rewrites.
- `--asset-selftest` independently names external script, stylesheet/font,
  image, and CSS URL defects across direct and protocol-relative URLs,
  reordered/unquoted attributes, `srcset`, quoted `@import`, font URLs, and
  ordinary CSS URLs. `--assets-only` parses tag/attribute and CSS contexts in
  `src/web/` for those origins plus inline executable script,
  stylesheet/style attributes, and event-handler attributes. The normal CDP
  modes prove the source scan agrees with what the real routes load.
- `--served-page-selftest` makes the response provenance non-optional: it
  rejects the former private source-file assembly and requires real page and
  catalog response-body reads plus explicit provenance records before hostile
  mutation.
- `--catalog-startup-selftest` rejects inline-HTML catalog mutation and
  requires response-stage mutation plus the stylesheet-before-bootstrap guard.
  The startup matrix independently rejects bootstrap/catalog/schema failure,
  delayed catalog resolution, request-stage stylesheet loss across Dashboard,
  Setup, and Login, and later operator application-script loss. It pins hard
  hidden/no-request behavior after CSS failure, bootstrap before catalog,
  catalog before reveal/polling, emergency-only later failure text, and no
  subsequent application request.
- Generated `data-style` declarations use a 512-entry rule/cache bound.
  Browser proof creates more than twice that many distinct metric-like values,
  requires a real compaction, verifies cache/rule agreement, and checks a live
  node's geometry before and after the rewrite.
- A green page result also requires a clean browser teardown. The gate closes
  Chromium through CDP, waits a bounded interval, escalates to its isolated
  process group when descendants survive, stops the proxy, and verifies the
  run directory was removed. `--cleanup-selftest` forces a descendant to keep
  writing the profile so parent-only termination or note-only cleanup fails.
  It also forces proxy startup to time out and requires the proxy exit event
  before run-directory removal. An intercepted missing-locale failure must
  print that originating cause before the generic asset-load summary.
- `--escape-probe` gives the contextual-sink rule in
  [message-catalog-and-escaping](message-catalog-and-escaping.md) an
  enforcement mechanism instead of a paragraph. The probe requires inert
  dashboard descriptors to resolve only through the HTML escape boundary, the
  exact four-attribute allowlist, literal DOM text/attribute values, stable
  script/style/SVG target refusal, repeated fixed-node placeholders, and
  catches both entity leakage and markup parsing.

  It enforces **both directions**, and the second one was added only after an
  adversarial review proved the first was not enough. The probe appends
  `Ampersand & Quote' <b>Tag</b>` to every catalog value:

  - *Wrong text/attribute context* — the `&` and `'` come back as literal
    `&amp;` / `&#39;` in the rendered output.
  - *Raw HTML context* — the `<b>` parses into a real element, so a catalog
    value that reached an HTML parser without its sink escape is a DOM node
    rather than text.

  The original probe carried no tag and scanned text nodes only, which made it
  an entity-leak detector that was structurally blind to the markup-parsing
  direction — the XSS direction — and blind to every attribute sink
  (`deltaChip`'s and the taxonomy segbar's `title=`, and all of
  `applyStatic`'s `setAttribute` path). Four deliberate
  defects were injected to establish that: two the probe caught, two it passed
  green. It now scans `title`, `aria-label`, `placeholder` and `alt` as well as
  text, and both injected defects fail.

- The gate also asserts the **status predicates agree**. `IS_2XX` / `IS_ERR`
  are module-scope in `dashboard.html` specifically so the gate can evaluate
  them: the captured payloads contain only `200`, `429`, `504` and
  `disconnect`, so replaying fixtures can never observe a `204` being counted
  as Success on one card and as an error on the card beside it. Fixtures prove
  the page runs; the predicate assertion proves the rule it runs on.

- **Both pages are covered.** `--page setup` drives the wizard: it trips the
  validation errors, adds a key, reaches the review panel, toggles the
  client-key option both ways, and finishes to the one-time-secret screen.
  Each step returns true only once the panel it should have revealed is
  visible, so a step that silently does nothing fails loudly instead of letting
  the scan measure step 1 five times — the same failure mode as the dashboard's
  hash-versus-click bug.

  This was owed for a while. The wizard was proved by hand **three separate
  times** — once for the attribute-allowlist fix, once when its strings were
  extracted, once by a reviewer — and each time the harness was thrown away and
  "setup.html has no render coverage" was written up as a known gap. Three
  throwaway proofs cost more than one committed check and leave nothing behind.
  Naming a gap is not the same as closing it.

  The wizard needs no fixtures: it fetches nothing at load. Its two endpoints
  are stubbed in the shapes `openapi.json` declares
  (`ValidateKeyResponse`, `SetupResponse`/`MintedClientKey`) rather than shapes
  chosen to make the page work, because a stub answering in a shape the server
  never sends proves the page works against fiction.

- **Both runtimes are asserted to refuse non-allowlisted attributes.** The gate
  calls each page's own `setMessageAttr` for all four allowed attributes and
  for `href`, `src`, `style`, and `onclick`. Allowed values must equal the
  hostile plain message byte-for-byte; forbidden attributes must throw the
  stable refusal and remain unset. Static self-tests separately cover URL
  properties, script text, CSS, raw SVG, and HTML-string sinks.
- It is not a screenshot test and takes no screenshots. Layout review stays
  human, per the plan's original and still-correct reasoning.
