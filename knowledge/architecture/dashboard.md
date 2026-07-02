---
type: Component
title: Dashboard (src/dashboard.html)
description: Single embedded HTML file; live/range modes; model cards; validated palette; chart discipline.
tags: [dashboard, dataviz, frontend]
timestamp: 2026-07-02T00:00:00Z
---

# Dashboard — `src/dashboard.html`

One self-contained HTML file compiled into the binary (`include_str!`), no
build step, no config, no external assets except optional CDN logos with an
offline monogram fallback. Six persona-aligned tabs, light + dark, each ordered
**at-a-glance → trends → detail**:

- **Overview** (landing, balanced) — hero KPIs + threshold gauges, request/
  token/savings sparklines, a health strip, top models & harnesses.
- **Models** (benchmarker) — cards + TTFT/tok/s/TPOT/upstream quantile charts,
  truncation breakdown, reasoning-vs-output share, per-model table.
- **Compare** (benchmarker) — head-to-head scorecard, best-in-column
  highlighted, via the `scorecard()` helper; a tok/s bar race via `barRows()`.
- **Harnesses** (agent analyst) — per-client tool intensity, conversation
  depth, sampling fingerprint, streaming mix, leaderboard. Driven by the
  per-client request-shape metrics
  ([request-shape-metrics](../decisions/request-shape-metrics.md)).
- **Proxy** (operator) — gauges, outcome/load charts, non-success breakdown,
  reliability & security panel, request-types panel, heatmap, per-client table.
- **Keys** (capacity) — lane meters, 429/min, stickiness, live headroom,
  keys-for-peak sizing.

Colors follow the entity: the first six take the validated categorical slots,
beyond that a stable hash-to-hue (`colorFor`) keeps every model/harness a
consistent color instead of dropping to none.

**Data flow**: Live mode polls `/metrics` every 3s into a browser-side ring
(~20 min); range mode fetches `/api/history` once and rebuilds the same
`samples: [{t, rows}]` structure — every chart, tile, card, and table renders
identically from either source. Range views report windowed (last − first)
values; live views report lifetime totals. Pause suspends the live poll.

**Charts** (per the dataviz discipline used throughout):

- Hairline grids, 2px lines, 4px end-dots with surface rings, crosshair +
  tooltip hover layer, legend for ≥2 series, table twin for everything.
- Categorical palette is the validated 6-slot set (CVD-checked in both
  modes); slots assign to models first-seen and never recycle.
- Quantile lines (TTFT, queue wait, generation speed) interpolate from
  histogram bucket deltas; the two quantiles use an ordinal same-hue pair, not
  two categorical slots. Generation speed filters to `source="usage"` buckets
  so the trend reflects real reported throughput, not the ~1-token-per-event
  estimate used when upstream omits usage.
- The capacity gauge uses a **trailing-60s average**: the raw 3s pairwise
  rate honestly read 133% during a cold-start burst drain, which is
  math-correct but reads as broken.
- Gauges are **colored by threshold**, not just numbered: capacity goes blue →
  amber (≥70%) → red (≥90%) as lanes saturate; success rate goes green → amber
  (<99%) → red (<90%). The dial itself signals health at a glance.
- The Proxy tab pairs the outcomes-per-minute line chart with a ranked
  **non-success-outcome table** (every recorded status that isn't 200 —
  `429`/`400`/`504`/`disconnect`/`stall`/`stream_error`/… — mapped to a
  plain-language reason with count and share), so *why* requests failed is
  legible, not just how many.
- Heatmap (weekday × hour) uses the sequential blue ramp with a table toggle.
- Model cards derive identity from the id namespace
  ([schema research](../research/nim-models-endpoint-schema.md)): LobeHub CDN
  logo with brand-colored monogram fallback, ranked by completion tokens.

Chart history in live mode does not survive refresh; range views do (server
history).
