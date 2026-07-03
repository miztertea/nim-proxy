---
type: Component
title: Dashboard (src/dashboard.html)
description: Single embedded HTML file; dark operator-console UI; live/range modes; color-follows-entity; hover charts and sortable tables.
tags: [dashboard, dataviz, frontend]
timestamp: 2026-07-03T00:00:00Z
---

# Dashboard — `src/dashboard.html`

One self-contained HTML file compiled into the binary (`include_str!`), no
build step, no config, no external assets except optional CDN logos with an
offline monogram fallback. A dark, NVIDIA-green "operator console": a 216px
sticky sidebar (collapses to an icon-only rail below 860px) with the nav and
live/uptime/version footer, a top bar with range pills + a custom date-range
picker, and five persona-aligned tabs, each ordered **at-a-glance → trends →
detail**:

- **Overview** (landing, balanced) — KPI cards + threshold ring gauges,
  request/token/savings sparklines, a health strip, a p50/p95 performance
  band, top models & harnesses.
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
  provisioning, rate-limit pressure), lane utilization meters, 429s/min by
  lane, per-lane table.

The former **Compare** tab (head-to-head scorecard + bar race) was folded
into Models as a section — it never carried enough unique content to justify
a sixth tab. See
[dashboard-operator-console-redesign](../decisions/dashboard-operator-console-redesign.md)
for the rationale behind the IA change and the dark-only, fonts-via-CDN, and
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
known harnesses (`claude-code`, `aider`, `opencode`, `cline`, `continue`,
`cursor`, `roo-code`, `zed`, `codex`, `n8n`) take a fixed client-color map;
anything else — and lane colors, which use six fixed slot colors — falls
back to a stable hash-to-hue (`hueFor`). The old first-six-slots categorical
allocator (`modelSlots`/`slotFor`) is gone; there's no "ran out of colors"
case left to handle.

**Dark-only.** The light palette and `prefers-color-scheme` handling were
removed; the `:root` tokens are a single dark set (page `#0B0D09` with a
faint green radial glow, cards `rgba(255,255,255,0.03)`, accent
`#76B900`/`#A7D65A`, amber `#D9A521`, red `#E36868`, blue `#4D6BFE`). This
was a committed design decision, not an oversight — see
[dashboard-operator-console-redesign](../decisions/dashboard-operator-console-redesign.md).

**Fonts**: Space Grotesk (UI/headings) and Spline Sans Mono (all numeric
values, axis labels, table cells) load from Google Fonts via
`<link>`/`@import`, allowed by an extended CSP (`style-src` gained
`https://fonts.googleapis.com`, a new `font-src` allows
`https://fonts.gstatic.com`). Offline or CDN-blocked, the CSS falls back to
`system-ui`/`monospace` — same graceful-degradation pattern as the LobeHub
logo CDN. The NIM Proxy logo mark itself is inlined as a base64 data URI
(68×68 PNG, ~10KB) in the sidebar, so it never depends on the network.

## Data flow (unchanged)

Live mode polls `/metrics` every 3s into a browser-side ring (~20 min); range
mode fetches `/api/history` once and rebuilds the same `samples: [{t, rows}]`
structure — every chart, tile, card, and table renders identically from
either source. Range views report windowed (last − first) values; live views
report lifetime totals. Pause suspends the live poll. Two small additions to
the aggregate-math layer support the new UI: `wbuckets()` computes windowed
histogram quantiles (lifetime in live mode, delta-over-view in range mode),
and `avgSeries()` builds a pairwise average trend line from a histogram's
`_sum`/`_count` pair (used for the Overview/Models KPI sparklines).

**Notable derivations, worth recording so they aren't rediscovered:**

- **Delta chips** (the `+8.2%`-style pill on every KPI card) compare the
  second half of the visible window's average against the first half — an
  honest trend computable from the sample buffer already in memory, with no
  extra history fetch. Hidden below 4 samples.
- **"Where time goes"** (Reliability hero) splits average end-to-end time
  into queue wait, first token, and generation, where **generation = avg
  `upstream_seconds` − avg `nimproxy_ttft_seconds`** — verified against
  `proxy.rs`: `upstream_seconds` spans send→stream-end, `ttft` spans
  send→first-byte, so the difference is genuinely token-generation time, not
  double-counted latency.
- **Availability** (Reliability hero) is judged against a **hardcoded 99.9%
  SLO constant** in the dashboard JS, not a config value — it's a display
  reference with nothing else that needs it to be configurable yet.

Chart history in live mode does not survive refresh; range views do (server
history). Model cards derive identity from the id namespace
([schema research](../research/nim-models-endpoint-schema.md)): LobeHub CDN
logo with brand-colored monogram fallback, ranked by completion tokens.

## Security invariant (unchanged)

Every dynamic string that reaches `innerHTML` — model/client names, tooltip
and legend labels, table cells — passes through the `esc()` HTML-escaper.
This redesign touched only markup, styling, and the two new interactions
(hover, sort); it did not add a new `innerHTML` sink that skips `esc()`. See
[input-sanitizing-and-xss](../decisions/input-sanitizing-and-xss.md).
