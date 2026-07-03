# Dashboard redesign — implementation plan

Status: **planned, not yet implemented**. The design handoff bundle lives in
this directory (`README.md` is the spec; `NIM Proxy Dashboard.dc.html` is the
pixel-accurate prototype; `LineChart.dc.html` / `SortTable.dc.html` are the
interaction references; `assets/` holds the user-provided logo art).

## Goal

Rebuild the presentation layer of `src/dashboard.html` to the handoff spec: a
dark, NVIDIA-green "operator console" with a left sidebar, 5 persona tabs
(`Overview · Models · Clients · Reliability · Capacity`), richer KPI cards,
hover tooltips on every line chart, and click-to-sort growable tables — while
**keeping the entire existing data layer unchanged** (Prometheus parsing,
sample ring, range loading, aggregate math). No backend/metric changes; every
number in the new UI maps to a value the current `render()` already computes.

## Decisions locked in (with the user, 2026-07-03)

| Decision | Choice | Rationale |
|---|---|---|
| Fonts under strict CSP | **Allow Google Fonts in CSP** (`style-src` + `https://fonts.googleapis.com`, new `font-src https://fonts.gstatic.com`) | Matches the LobeHub-logo precedent: CDN with graceful degradation (system-font fallback offline). No binary growth. |
| Light mode | **Dark only** | The design is a committed dark aesthetic; tokens have no light variant. Delete the light palette and `prefers-color-scheme` machinery. |
| PR structure | **One redesign PR**, then a separate small release PR | The change is ~one file; splitting creates valueless intermediate states. Release prep (version/changelog/SECURITY) follows `knowledge/ops/release.md`. |
| Version | **v0.6.0** (from 0.5.0) | New feature, backward compatible; minor bump. |
| Heatmap table toggle | **Drop it** | Not in the final design; heatmap keeps per-cell hover tooltips. |
| Availability SLO reference | **Constant 99.9% in the dashboard JS** | It's a display reference like nothing else configurable needs it yet (YAGNI). Promote to an env var only if someone asks. |
| Delta chips on Overview KPIs | **Second-half vs first-half of the visible window** | Honest trend computable from the existing sample buffer in ~10 lines; no extra history fetches. Hidden when < 4 samples. |

## What stays (do not touch)

- All of `main.rs` routing/auth; the only Rust change is the CSP header string.
- In `dashboard.html`: `parseProm`, `sum`, `groups`, `buckets`, `quantile`,
  `deltaBuckets`, `rate`/`rateSeries`, `windowed`, `wgroups`, `quantSeries`,
  the `samples`/`mode`/`cfg` state, `pollLive`/`loadRange`/`goLive`, poll
  cadence (3 s, KEEP=400), hash-based tab routing, pause behavior, the
  `esc()` XSS discipline (every dynamic string that reaches `innerHTML`), the
  `PUBLISHERS` map + LobeHub CDN logos with monogram fallback, and all the
  aggregate math in `render()`.
- `/dash/config.json`, `/api/history`, `/metrics` contracts.

## What changes

### 1. `src/main.rs` — CSP only

```
style-src 'self' 'unsafe-inline' https://fonts.googleapis.com;
font-src https://fonts.gstatic.com;
```
(keep everything else identical). Existing e2e assertions
(`frame-ancestors 'none'`, `connect-src 'self'`, nosniff, DENY) still pass;
extend `dashboard_sends_security_headers` to pin the new `font-src`.

### 2. `src/dashboard.html` — presentation rewrite

**Chrome** (per spec §Global chrome): 216 px sticky sidebar (logo mark as a
~10 KB base64 data URI at 68×68 — `img-src data:` is already allowed — plus
wordmark, 5 nav items with inline SVG icons, active = green tint + 3 px bar;
footer = live dot / uptime / `vX · N lanes · auth on` — the live pill stays
clickable to pause, replacing the old pause button). Top bar = page title +
range pills + date-range pill that opens the existing `datetime-local` pair.
Below ~860 px the sidebar collapses to an icon-only rail (labels hidden).

**Design tokens**: replace the `:root` palette with the spec's dark tokens
(page `#0B0D09` + radial glow, card `rgba(255,255,255,0.03)`, accent
`#76B900`/`#A7D65A`/`#4E7A0F`, amber `#D9A521`, red `#E36868`, blue
`#4D6BFE`, text tiers, `#12150E` header/tooltip). Fonts: Space Grotesk (UI) +
Spline Sans Mono (all numerics), with `system-ui`/`monospace` fallbacks.
Keep the CSS-variable approach — one source of truth.

**Rendering primitives** (rewrite/replace; all shared across tabs):

- `lineChart(el, series, fmt, opts)` — full-bleed plot (no left gutter;
  y-labels are right-edge overlays), hairline grid, 2 px lines, optional
  gradient area fill (`gGreen`/`gMuted` defs), end dots. **Hover**: nearest
  *sample* (real timestamps, not the prototype's uniform index), crosshair +
  per-series dots + tooltip card with a time header, horizontal edge-flip —
  port the logic from `LineChart.dc.html`, adapted to `pts:[{t,v}]`.
- `sortTable(el, id, {columns, rows, defaultIdx, defaultAsc, maxH, minW, best})`
  — replaces **all** ad-hoc `<table>` string builders (~8 sites) *and* the old
  `scorecard()`: sticky `#12150E` thead, click-to-sort (numeric/string aware),
  active header green + `↑/↓`, header alignment matches cell alignment,
  capped height + internal scroll. Optional per-column `best:'min'|'max'`
  keeps the Compare best-in-column highlight. Sort state lives in a global
  `Map` keyed by table id and **scrollTop is saved/restored around the
  innerHTML swap** — the 3 s live re-render must not reset either.
- `ringGauge` (replaces `arcGauge`): 76 px circle, `stroke-dasharray` fill,
  centered % text, label + mono sub-line.
- `barRow` — one helper for all labeled progress bars (merges the current
  `barRows` + `miniList` near-duplicates): name, rounded track, chip-colored
  fill, mono value.
- `kpiCard` — icon + label, optional delta chip, big value, mono sub-line,
  bottom-pinned gradient sparkline.
- `heatmap` — same matrix math, green ramp
  `#141A0E→#233312→#33501A→#4E7A0F→#76B900→#A7D65A`, keep hover tooltip,
  drop the table toggle.
- Small one-offs built inline: p50/p95 dot-track (Overview performance band),
  stacked latency-composition bar, saturation bar with peak marker,
  provisioning `ADD N KEY` chip, error-taxonomy micro-bar.
- Hover state per chart id survives the 3 s re-render (re-apply last index).

**Color-follows-entity, simplified**: models use their publisher chip color
(extend `PUBLISHERS` with the spec's additions/overrides — Moonshot
`#16B8C0`, StepFun `#EE6002`, MiniMax `#F23F5D`, Zhipu, etc.); harnesses use
a small known-client map (`claude-code #C15F3C`, `aider #2E7D5B`, …) with the
existing `hueFor` hash fallback; lanes use the fixed slot colors. The old
first-six-slots allocator (`modelSlots`/`slotFor`) goes away.

**Tabs** (per spec §Screens; all data references are existing `render()`
values — see spec §Data mapping):

1. **Overview** — 4 KPI cards (Requests, Tokens out, Success rate, Dollars
   saved-in-green) with delta chips + sparklines; Traffic panel (area
   req/min chart, `N now` in header) beside Capacity & reliability (2 ring
   gauges + health list); Performance band (TTFT / gen-speed / TPOT p50-p95
   dot-tracks from windowed bucket deltas); Top models / Top harnesses
   leaderboards.
2. **Models** — 5 KPI cards; full-width tokens/min-by-model multi-line;
   quantile quad (TTFT, gen speed, TPOT, upstream; median `#A7D65A` /
   p95 `#6F7767`); How-responses-end (sortable) + Reasoning-vs-output bars;
   **Compare folded in**: model scorecard as a `sortTable` with
   best-in-column highlight + tok/s head-to-head bars; Leading-models top-4
   cards (CDN logo chip w/ monogram fallback, 6 stats); Per-model sortable
   table (11 cols, default sort Tokens-out desc, `min-width` + h-scroll).
3. **Clients** (was Harnesses) — 5 KPI tiles; Tool intensity / Conversation
   depth / Sampling fingerprint / Streaming share bar panels; Per-harness
   sortable table.
4. **Reliability** (was Proxy) — hero: Availability tile (window success %,
   `SLO 99.9%` met/missed, amber error-budget bar = (1−ok)/(1−SLO)),
   Where-time-goes tile (stacked bar: avg queue wait `#D9A521` → avg TTFT
   `#4D6BFE` → generation = avg `upstream_seconds` − avg TTFT `#76B900`;
   verified against proxy.rs: `upstream_seconds` spans send→stream-end,
   `ttft` spans send→first-byte), Live-load tile (Active/Queued split bar,
   error rate + taxonomy micro-bar from non-200 status groups). Then:
   req/min, outcomes/min, in-flight & queued, queue-wait charts; heatmap;
   Non-success outcomes (sortable) + Reliability & security list + Request
   types list; Per-client sortable table.
5. **Capacity** (was Keys) — hero: Saturation tile (gradient bar, white peak
   marker, `cur / cap rpm · peak · free`), Provisioning tile (keys active,
   keys-for-peak, amber `ADD N KEY` chip when under-provisioned), Rate-limit
   pressure tile (429s, stickiness, lanes); lane utilization meters +
   429s/min chart; Per-lane sortable table.

**Deletions** (simplifications the redesign earns): Compare tab + its render
fn (merged), light palette + `dark` detection + light `RAMP`, `heatAsTable`,
`arcGauge`, `scorecard`, `miniList`, the slot allocator, the old top-nav/bar
CSS. Net: one render path per tab, fewer primitives than today serving more
UI.

**Security invariant** (PR checklist item): every dynamic string still passes
through `esc()`; sort/hover state never interpolates unescaped input. The
existing e2e label-injection test plus a manual XSS pass with a hostile model
id (`<img src=x onerror=…>` via mock) before merge.

### 3. Docs & knowledge (same PR, per AGENTS.md)

- `README.md` — Dashboard section: 5 tabs w/ new names, dark operator
  console, fonts note (CSP now allows Google Fonts; offline falls back to
  system fonts), sortable tables + chart tooltips.
- `knowledge/architecture/dashboard.md` — rewrite for the new IA, primitives,
  and interactions.
- New `knowledge/decisions/dashboard-operator-console-redesign.md` (ADR:
  IA merge/renames, dark-only, fonts-via-CDN-under-CSP, delta-chip
  semantics) + `index.md` row + dated `knowledge/log.md` entry.
- `CHANGELOG.md` — under `[Unreleased]`.

## Verification (before the PR merges — protect the 30–45 min release build)

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test` (49 tests; extend the CSP assertion; everything else should
   pass untouched — the data contract didn't move).
2. **Live walkthrough in the container**: `scripts/mock_nim.py` + proxy +
   Playwright (pre-installed Chromium): screenshot all 5 tabs in live mode
   with traffic flowing, plus a range view (`/api/history`-backed), the
   empty-data state, hover tooltips, table sorting (click twice → asc/desc),
   sticky-scroll behavior, and the <860 px rail. Compare side-by-side with
   prototype screenshots.
3. XSS probe via mock with hostile model/client labels (see above).
4. No Dockerfile / release-workflow / dependency changes in this PR ⇒ the
   tag build risk is the same as v0.5.0's known-good pipeline. Nothing new
   to cross-compile: the dashboard is `include_str!`.

## Release (separate PR, then tag — `knowledge/ops/release.md`)

1. Release PR: `Cargo.toml` 0.5.0→0.6.0 + `cargo update --package nim-proxy`,
   promote `[Unreleased]`→`[0.6.0]` w/ compare links, SECURITY.md supported
   versions → 0.6.x. Wait for full CI, merge.
2. Tag `v0.6.0` on the merge commit, watch the Release workflow, then the
   §3 verify steps (pull, imagetools inspect, smoke run, cosign verify).

## Out of scope (explicitly)

- No metric/backend additions, no history-format changes.
- No SLO env var, no per-model drill-down pages, no CSV export.
- No mobile-first layout beyond the responsive reflow + icon rail.
- `design/` stays in-repo as the spec of record (excluded from the Docker
  image by context; not compiled into the binary).
