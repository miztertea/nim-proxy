---
type: Component
title: Dashboard
description: Split, compile-time embedded operator console with one persisted analytical window, typed range/current contracts, and clearly scoped Now values.
tags: [dashboard, dataviz, frontend]
timestamp: 2026-07-03T00:00:00Z
---

# Dashboard

The dashboard is a compile-time embedded page plus same-origin CSS/JavaScript
sources under `src/web/`, with no frontend build, Grafana, runtime files, or
external assets. `src/presentation.rs` owns page assembly and asset lookup;
operator routes share the session gate. See
[the presentation layer](presentation-layer.md). A dark, NVIDIA-green
"operator console":
a 216px sticky sidebar (collapses to an icon-only rail below 860px) with the
nav and follow-state/uptime/version footer, a top bar with range pills + a
custom date-range picker, and five persona-aligned tabs, each ordered
**at-a-glance → trends → detail**:

- **Overview** (landing, balanced) — KPI cards + threshold ring gauges,
  request and token sparklines, a health strip, a p50/p95 performance
  band, top models & clients.
- **Models** (benchmarker) — KPI cards, tokens/min-by-model chart, a
  TTFT/tok-s/TPOT/upstream quantile quad, a "how responses end" breakdown,
  reasoning-vs-output share, a head-to-head scorecard with best-in-column
  highlighting and a tok/s bar race (this section absorbed the former
  Compare tab), leading-model cards, and the full per-model table.
- **Clients** (agent analyst, was **Harnesses**) — per-client tool intensity,
  conversation depth, sampling fingerprint, streaming mix, leaderboard.
  Driven by the per-client request-shape metrics
  ([request-shape-metrics](../decisions/request-shape-metrics.md)).
- **Reliability** (operator, was **Proxy**) — a hero row (availability vs SLO,
  latency composition, live load + error taxonomy), request/outcome/load
  charts, queue-wait quantiles, an hour-of-day heatmap, a non-success-outcome
  breakdown, a reliability & security panel, a request-types panel, per-client
  table.
- **Capacity** (capacity planner, was **Keys**) — a hero row (saturation,
  capacity history, throttling), key utilization meters, 429s/min by key,
  per-key table.

The tabs are **identical for every role** — all authenticated users see the
same observability, the deliberate shared-pool-among-friends model. v0.6.0 adds
a **Settings** area (its own sub-nav: Access & keys · Server · Users · Account;
Server/Users hidden for the `user` role) that reads role-filtered data from
`GET /api/config` (hidden sections are absent from the payload, not CSS-hidden —
see [client-auth](client-auth.md) and
[ui-managed-config-store](../decisions/ui-managed-config-store.md)), and a
**Model pressure** card on Reliability (worker-exhaustion rate + per-model
`inflight vs limit` rows) that appears only once the
[governor](governor.md) has engaged.

Every analytical tab shares one selected time range. **Default · Nd**
(30d by default) follows now using the configured default, relative presets
follow now over their duration, and **All time** follows the server's
current retained boundary. Custom ranges are fixed. Clicking the sidebar
follow control freezes the currently rendered range; clicking again resumes
its preset. Settings hides the range controller because it is
configuration-driven rather than an analytical view.

The former **Compare** tab (head-to-head scorecard + bar race) was folded
into Models as a section — it never carried enough unique content to justify
a sixth tab. See
[dashboard-operator-console-redesign](../decisions/dashboard-operator-console-redesign.md)
for the rationale behind the IA change and the dark-only, typography, and
delta-chip decisions.

## Rendering primitives

All tabs share one set of primitives (`render()` computes cross-tab
aggregates once, then only the active tab's renderer runs, so hidden charts
size to a real `clientWidth` when their tab is switched to):

- **`lineChart`** — full-bleed SVG plot (no left gutter; y-axis labels are
  right-edge overlays), hairline grid, 2px lines, optional gradient area
  fill, end dots. Hover snaps to the nearest real sample (not a uniform
  index) and draws a crosshair + a dot per series + a tooltip card with a
  timestamp header; the last hovered pointer position is re-applied after
  the 3s live re-render so the tooltip doesn't flicker away.
- **`sortTable`** — replaces every ad-hoc `<table>` builder and the old
  `scorecard()`. Sticky `<thead>`, click-to-sort (numeric or string aware,
  asc/desc toggle), active header turns green with a `↑`/`↓` arrow, header
  alignment matches its column's cell alignment, capped height with an
  internal scroll, optional per-column `best:'min'|'max'` highlighting.
  Sort state lives in a global `Map` keyed by table id, and the table's
  scroll position is saved/restored around the `innerHTML` swap — so neither
  resets on the 3s live poll.
- **`ringGauge`** (replaces `arcGauge`) — a 76px threshold-colored circle
  with a centered percentage, label, and mono sub-line.
- **`kpiCard`** — icon + label, an optional trend delta chip, a big value,
  a mono sub-line, and a bottom-pinned gradient sparkline.
- **`barList`** / **`leaderList`** — one shared row primitive for every
  labeled progress bar and leaderboard row (name, track, chip-colored fill,
  mono value); replaces the old `barRows`/`miniList` near-duplicates.
- **`heatmap`** — same weekday×hour matrix math as before, now a sequential
  green ramp (`#141A0E→#233312→#33501A→#4E7A0F→#76B900→#A7D65A`) instead of
  blue, with per-cell hover tooltips; the table-view toggle was dropped (not
  in the final design).

Colors follow the entity, not the chart: models take their publisher's brand
color from the `PUBLISHERS` map (extended with StepFun and a Moonshot teal);
known clients (`claude-code`, `aider`, `opencode`, `cline`, `continue`,
`cursor`, `roo-code`, `zed`, `codex`, `n8n`) take a fixed client-color map;
anything else — and key colors, which use six fixed slot colors — falls
back to a stable hash-to-hue (`hueFor`). The old first-six-slots categorical
allocator (`modelSlots`/`slotFor`) is gone; there's no "ran out of colors"
case left to handle.

**Dark-only.** The light palette and `prefers-color-scheme` handling were
removed; the `:root` tokens are a single dark set (page `#0B0D09` with a
faint green radial glow, cards `rgba(255,255,255,0.03)`, accent
`#76B900`/`#A7D65A`, amber `#D9A521`, red `#E36868`, blue `#4D6BFE`). This
was a committed design decision, not an oversight — see
[dashboard-operator-console-redesign](../decisions/dashboard-operator-console-redesign.md).

**Fonts and marks** use system UI/monospace stacks and fixed local SVG/text
primitives. The earlier Google Fonts and model-logo CDN choice was superseded
when the presentation layer removed every external origin. CSP now permits
only same-origin styles/scripts/connections plus self/data images.

## Data flow

Two authenticated typed endpoints replace browser parsing of raw exposition:

- `GET /api/dashboard?from&to&points` returns exact totals, chart buckets,
  latest gauges, effective/available bounds, sample-time capacity, diagnostics,
  query-scoped `window.complete`, and both history/config revisions. Omitting
  `to` uses now and marks the request as following; omitting `from` also
  selects `now - default_window_days`.
- `GET /api/dashboard/now` is lightweight current state: live metrics,
  current capacity/config/SLO, retained bounds, revisions, and the
  post-persistence counter tail.

The retired `/api/history` and `/dash/config.json` routes are absent. Raw
`/metrics` remains available to authenticated Prometheus scrapers, not as a
dashboard transport.

Both bodies — and every `/api/settings/*` body — are `derive(Serialize)`
structs in `src/api.rs`, not hand-built JSON, and `openapi.json` at the repo
root is generated from them. Field *declaration* order is the wire order, so
those structs are declared ASCII-sorted; see
[typed-responses-and-generated-openapi](../decisions/typed-responses-and-generated-openapi.md)
before adding or moving a field.

The browser fixture authority is the 35 stable JSON files in
`tests/fixtures/ui/`, generated by the `src/api.rs` module test from those
production response types. Dashboard metric fixtures derive histogram bounds
from the same `HISTOGRAM_BUCKETS` registry the recorder uses, with concrete
samples producing their bucket counts and sums; the browser does not maintain
another metric model. The 69-row served-app matrix consumes those files or
`scenarios.json#scenario` at the HTTP boundary and records exact requests,
DOM results, and clean-run observations. Its rows include Settings dialogs,
validation/toast transitions, raw API errors, the catalog-owned status fallback,
and exact grouped Settings values above 10,000. It proves named interactions, not
layout or all dashboard paths.

`rangeSamples()` adapts the normalized range contract back into the
`samples: [{t, rows}]` cumulative structure the rendering primitives consume.
Every selection begins with a synthetic zero-counter baseline, then applies
server deltas; gauges are replaced rather than accumulated. The exact range
totals overwrite the final historical point. A live tail is accepted only
when its `base_history_revision` matches the selected range, and is replaced
after persistence advances that revision.

The browser polls `/api/dashboard/now` every three seconds. Only a
following, unpaused window refetches history when its revision changes.
Custom and paused totals remain fixed, while **Now** widgets—active requests,
queue depth, current RPM/capacity, enabled lane slots, uptime, and header
metadata—continue refreshing. A config-revision change updates capacity,
auth state, default window, retention, and SLO without reloading
the page; if the active preset is the default, its following bounds are
recomputed.

The range status treats completeness independently from request volume. A
complete range keeps the established following/fixed or no-traffic wording.
An incomplete range with effective points says **Partial history** and shows
only its effective bounds. An incomplete requested interval inside globally
available history but with no usable points says **History unavailable for
selected time range**. No global history remains **No data yet**. These states
are catalog-owned; absent observations never become zeros.

Overview's **Observation quality** health row reads the existing generic
`nimproxy_usage_observations_total` rows from the selected range's final
cumulative sample, including only a revision-matching live tail already
accepted by `rangeSamples()`. It recognizes only the five fixed field labels
and four fixed result labels, ignores non-finite or negative values, and keeps
recognized-row presence separate from totals so the synthetic zero baseline
cannot invent telemetry. No recognized row is **Unavailable**; recognized rows
whose aggregate is zero are **No observations**. Otherwise the greatest result
total wins, with conservative ties `invalid`, `unavailable`, `estimated`, then
`measured`. Invalid is critical, unavailable/estimated warn, measured is
normal, and no-observations uses the zero tone. This response-quality signal is
independent from `window.complete`: **Partial history** and **Measured** may
appear together. Its counter semantics live in
[NIM observations](nim-observations.md).

**Notable derivations, worth recording so they aren't rediscovered:**

- **Delta chips** (the `+8.2%`-style pill on every KPI card) compare the
  second half of the visible range's average against the first half — an
  honest trend computable from the selected sample buffer, with no extra
  history fetch. Hidden below 4 samples.
- **"Latency breakdown"** (Reliability hero) splits average end-to-end time
  into queue wait, first token, and generation, where **generation = avg
  `upstream_seconds` − avg `nimproxy_ttft_seconds`** — verified against
  `proxy.rs`: `upstream_seconds` spans send→stream-end, `ttft` spans
  send→first-byte, so the difference is genuinely token-generation time, not
  double-counted latency.
- **Availability** (Reliability hero) uses
  `dashboard.slo_target_percent` from the current server config (99.9% by
  default). HTTP 4xx and disconnect outcomes stay visible in outcome/error
  views but do not consume the service-availability error budget. Capacity
  history uses the capacity-at-the-time value stored with each v2 sample;
  intervals without it explicitly show no capacity data. Active load, key
  count, current RPM, and utilization are labeled **Now**; selected-range
  key request and cooldown counts remain historical.

Following history survives refresh and process restart because it is rebuilt
from the server index; only the adjacent-poll rate shown in **Now** widgets
needs two current polls. Model cards derive identity from the id namespace
([schema research](../research/nim-models-endpoint-schema.md)) and render a
fixed local monogram with the existing brand-color map, ranked by completion
tokens.

## Security invariant

Ordinary catalog writes pass ids to the canonical `textContent` helper;
`title`, `placeholder`, `aria-label`, and `alt` pass ids to the allowlisted
attribute helper. Setup emphasis and literals use fixed DOM nodes. Dashboard
HTML builders carry frozen catalog descriptors and resolve and escape them only
through `escapeHtml()` at interpolation. Raw lookup is confined to those
canonical sink bodies. SVG geometry is fixed or machine-generated; catalog
descriptors do not enter SVG.

Model/client names, tooltip and legend data, table cells, and other API data
remain plain machine data and follow the same sink-specific escaping rule; they
are never localized. Catalog ids and descriptors never enter URL, style,
event, executable script, CSS, or raw-SVG contexts. See
[message-catalog-and-escaping](../decisions/message-catalog-and-escaping.md)
and [input-sanitizing-and-xss](../decisions/input-sanitizing-and-xss.md).

Settings renders dynamic Access, Server, Users, and Account markup through the
same context-owned sinks: `applyStatic` owns declarative text/attribute ids,
fixed-markup builders escape inert descriptors once, and the five-second Access
refresh uses `setMessageText`. Confirm/prompt dialogs are exact catalog text
sinks. Repository fallback errors are catalog messages; usable API error bodies
remain raw text. Settings keeps API integers exact with locale grouping rather
than dashboard compact-number formatting.

## Semantic and responsive boundary

The operator page has one native `main` and page heading, native navigation
buttons (not incomplete ARIA tabs), native tables, and a native `dialog` for
the one-time client secret. Opening that dialog focuses Copy; Done closes it
and returns focus to the client-key action. Escape follows the same close and
focus-return path. Settings' compact toggles remain native buttons with
`aria-pressed` and catalog-owned accessible names.

The Models tool-call value is an atomic formatted value: it may claim the
space it needs but never wraps its count/separator/rate composite. Model
presentation keeps the vendor/model split, while the rendered model component
is byte-for-byte the raw component supplied by the API—no title casing,
separator rewriting, or localization.

Responsive layout is content-driven rather than locale-specific. Range
controls and status text may wrap, fixed-width form controls may shrink, and
Settings stacks before translated labels or machine values clip. The visual
gate resolves 284 applicable page/state/viewport/locale surfaces, captures each
full document at 390, 768, 900, and 1440 CSS-pixel widths, and records layout
and PNG geometry for mechanical verification. Human review of those artifacts
remains required because clean geometry does not prove visual hierarchy.
