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
offline monogram fallback. Three tabs (Models / Proxy / Keys), light + dark.

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
- Quantile lines (TTFT, queue wait) interpolate from histogram bucket deltas;
  the two quantiles use an ordinal same-hue pair, not two categorical slots.
- The capacity gauge uses a **trailing-60s average**: the raw 3s pairwise
  rate honestly read 133% during a cold-start burst drain, which is
  math-correct but reads as broken.
- Heatmap (weekday × hour) uses the sequential blue ramp with a table toggle.
- Model cards derive identity from the id namespace
  ([schema research](../research/nim-models-endpoint-schema.md)): LobeHub CDN
  logo with brand-colored monogram fallback, ranked by completion tokens.

Chart history in live mode does not survive refresh; range views do (server
history).
