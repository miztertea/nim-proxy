# Handoff: NIM Proxy — Metrics Dashboard Redesign

## Overview
A visual + information-architecture redesign of the NIM Proxy metrics dashboard. The **data, charts, and backend all already work** in the current `src/dashboard.html` (it polls `/metrics` Prometheus text + `/api/history` and renders live). This project **does not change any of that** — it re-imagines *how the data is presented*: a polished, dark, NVIDIA-green "operator console" aesthetic with a persona-based tab structure, richer metric cards, interactive charts, and sortable tables that stay tidy as data grows.

## About the Design Files
The files in this bundle are **design references built in HTML** (a small streaming-component runtime), not production code to ship as-is. They are a pixel-accurate prototype of the intended look and behavior, running on **mock data**.

**Your task:** recreate this redesign inside the real `src/dashboard.html` in the `nim-proxy` repo. That file is a single self-contained vanilla-JS document (no build step, no framework) that:
- parses Prometheus text from `/metrics` into rows,
- keeps a rolling sample buffer (live) or fetches `/api/history` (ranges),
- computes per-model / per-client / per-lane aggregates and renders SVG charts + tables.

**Keep the entire data layer** (`parseProm`, `sum`, `groups`, `wgroups`, `buckets`, `quantile`, `rateSeries`, `windowed`, polling, range loading, the `render()` → per-tab functions). Replace only the **markup, styling, layout, and tab structure**, and add the two new **interactions** (chart hover tooltip, click-to-sort tables). Every number shown in the prototype maps to a metric the current dashboard already computes — see "Data mapping" below.

Do **not** literally import the `.dc.html` runtime into the repo. Read the prototype for exact styles/structure and hand-write equivalent vanilla HTML/CSS/JS in `dashboard.html`, matching its existing style (plain DOM, template strings, inline SVG).

## Fidelity
**High-fidelity.** Colors, typography, spacing, radii, and interactions are final. Recreate pixel-for-pixel. Exact inline style values live in `NIM Proxy Dashboard.dc.html` — when in doubt, read the element's `style="..."` there.

---

## Information Architecture (changed)
Original tabs: `Overview · Models · Compare · Harnesses · Proxy · Keys` (6).
**New tabs: `Overview · Models · Clients · Reliability · Capacity` (5).**
- `Harnesses` → renamed **Clients**
- `Proxy` → renamed **Reliability**
- `Keys` → renamed **Capacity**
- **`Compare` is merged into Models** — the scorecard (best-in-column) + head-to-head bars should become a sub-view/section inside Models. ⚠️ *Not yet built in this prototype* — fold the existing Compare visualizations in, restyled to match.

Left **vertical sidebar nav** (replaces the old top tab bar). Range pills (`Live 1h 6h 24h 7d 30d`) + a date-range picker move to a **top bar** on the right. Live status + uptime + version move to the **sidebar footer**.

---

## Design Tokens

### Color
| Token | Hex | Use |
|---|---|---|
| Page background | `#0B0D09` | app bg (with a faint green radial glow top-right: `radial-gradient(1100px 400px at 82% -14%, rgba(118,185,0,0.07), transparent 66%)`) |
| Sidebar bg | `#0E110B` | left nav; border `#1F2318` |
| Card bg | `rgba(255,255,255,0.03)` | every panel/card |
| Card border | `rgba(255,255,255,0.07)` | 1px |
| Hairline / row border | `rgba(255,255,255,0.05)` | grid lines, table row separators |
| Text primary | `#F0F2EC` | values, headings |
| Text secondary | `#C6CDBB` / `#9BA391` | labels |
| Text muted | `#6F7767` | captions, axis labels |
| Text faint | `#5A6150` | zero/disabled |
| **Accent green** | `#76B900` | primary accent (NVIDIA green), bars, active line |
| Accent green light | `#A7D65A` | large numbers, hover, medians |
| Accent green dark | `#4E7A0F` | gradient stops |
| Amber (warn) | `#D9A521` / `#D9C25A` | queued, 429s, budget |
| Red (error) | `#E36868` | errors |
| Blue | `#4D6BFE` | upstream/disconnect series + DeepSeek chip |
| Table header bg | `#12150E` | sticky `<thead>` |
| Tooltip bg | `#12150E` | chart hover card |
| Delta chip bg | `rgba(118,185,0,0.14)` | "+8.2%" pills, green text |

**Publisher chip colors** (model monogram squares): NVIDIA `#76B900`, DeepSeek `#4D6BFE`, Meta `#0668E1`, Qwen `#615CED`, Mistral `#FA520F`, Moonshot `#16B8C0`, Google `#2E96FF`, StepFun `#EE6002`, MiniMax `#F23F5D`, Microsoft `#00A4EF`, OpenAI `#10A37F`, Upstage `#805CFB`. (The current dashboard already has a `PUBLISHERS` map — reuse/extend it.)

### Typography
- **Space Grotesk** (Google) — UI, headings, big numbers. Weights 400/500/600/700.
- **Spline Sans Mono** (Google) — all numeric values, axis labels, captions, table cells, chips.
- Wordmark: Space Grotesk **700**, 17px, `-0.2px` letter-spacing; "NIM" in `#76B900`, "Proxy" in `#F0F2EC`.
- Section header: 14px / 500. KPI value: 23–34px / 600, `letter-spacing:-0.5px…-1px`. Hero number (Dollars saved): 32px. Labels: 11.5–12px. Mono captions: 10–11px.

### Shape & spacing
- Card radius **16px** (small cards/chips 14px, monogram squares 8–9px, pills 99px).
- Card padding **16px 20px** (dense KPI 12–14px). Grid gap **12–14px**. Main content padding `22px 26px 40px`.
- Sidebar width **216px**. No shadows except the chart tooltip (`0 6px 18px rgba(0,0,0,0.45)`).

---

## Screens / Views

### Global chrome
- **Sidebar (216px, sticky, full height):** logo mark (`assets/nim-logo-mark.png`, 34px, radius 9) + wordmark; nav items (icon + label, active = `rgba(118,185,0,0.12)` bg + `#76B900` 3px left bar + green icon); footer = live dot + `up 3d 14h` + `v0.4.2 · 4 lanes · auth on` in muted mono.
- **Top bar:** page title (19px/600) left; right = range pills (active pill = `rgba(118,185,0,0.14)` bg, `#A7D65A` text) + date-range picker pill (calendar glyph + `Jun 3 – Jul 3` + chevron).

### 1. Overview — "operator glance"
- **4 KPI metric cards** (auto-fit, min 160px): Requests, Tokens out, Success rate, Dollars saved. Each: icon + label (top-left), delta chip (top-right, e.g. `+8.2%`), big value, mono sub-line, and a **full-bleed gradient area sparkline** pinned to the card bottom (green gradient `url(#gGreen)` for Dollars saved, muted `url(#gMuted)` otherwise). Dollars saved value is green `#A7D65A`.
- **Traffic + Capacity row** (flex-wrap): Traffic panel (`flex:3 1 380px`) with a large **requests/min area LineChart**; Capacity & reliability panel (`flex:1 1 300px`, min-width 300 so it matches the Dollars-saved card width at desktop and wraps below when narrow) with two **ring gauges** (Capacity used, Success rate) + a compact health list (Active/Queued/benches/shed).
- **Performance band:** three p50/p95 range readouts (Time to first token, Generation speed, Inter-token latency) — a track with a filled `#A7D65A` p50 dot and a hollow p95 dot, with p50/p95 values below.
- **Leaderboards:** Top models / Top harnesses, each a row = monogram chip + name + value + thin progress bar.

### 2. Models — "how are my models performing?"
Order (performance-aggregate first, per-model last):
1. **5 KPI cards** (no Dollars saved here — it lives on Overview): Requests, Completion tokens, Prompt tokens, Avg TTFT, Avg speed.
2. **Completion tokens per minute, by model** — full-width multi-series LineChart + legend.
3. **Quantile quad** (auto-fit): Time to first token, Generation speed, Inter-token latency, Upstream latency — each a small 2-line (median `#A7D65A` / p95 `#6F7767`) LineChart + p50/p95 labels.
4. **How responses end** (sortable table) + **Reasoning vs output** (bars) row.
5. **Leading models** — top-4 cards (rank, monogram, publisher, 6 stat cells). Capped at 4; the rest live in the table below.
6. **Per model** — sortable growable table (11 columns), capped height with sticky header + internal scroll.

### 3. Clients — "what are my agents/harnesses doing?"
5 KPI tiles (Harnesses, Streaming share, Avg tools/req, Avg messages/req, Tool-using %) → 4 bar panels (Tool intensity, Conversation depth, Sampling fingerprint = avg temp, Streaming share) → **Per harness** sortable table.

### 4. Reliability — "is the proxy healthy & fast, and where does time go?"  *(hero redesigned — no Overview duplication)*
- **Hero (flex-wrap):**
  - **Availability** tile: big `99.94%`, `SLO 99.9% · met`, and an **error-budget bar** (amber, % used).
  - **Where time goes** tile (wide): end-to-end median total + a **stacked latency-composition bar** (Queue wait `#D9A521` / Upstream `#4D6BFE` / Generation `#76B900`) with a legend showing each segment's ms.
  - **Live load** tile: Active (green) / Queued (amber) big numbers + split bar; divider; Error rate `0.8%` with a mini **error-taxonomy** stacked bar (rate-limit / disconnect / timeout / 5xx).
- Charts: Requests/min, Outcomes/min, In-flight & queued, Queue wait (all LineCharts w/ hover) → Activity-by-hour **heatmap** (7×24, green sequential ramp) → Non-success outcomes (sortable) + Reliability & security list + Request types list → Per client sortable table.

### 5. Capacity — "am I near my limits & do I have enough keys?"  *(hero redesigned)*
- **Hero (flex-wrap):**
  - **Saturation** tile (wide): big `42%`, a capacity **bar** (green fill = current 67/160 rpm, a white **peak marker** at 118), `67 / 160 rpm` · `peak 118` · `93 rpm free`.
  - **Provisioning** tile: `4 keys active`, `peak demand needs 5 keys`, an **"ADD 1 KEY"** amber status chip when under-provisioned.
  - **Rate-limit pressure** tile: 429s this window, Conversation stickiness, Key lanes.
- Lane utilization meters + Rate-limit hits/min (LineChart) → Per lane sortable table.

---

## Interactions & Behavior
- **Tab nav:** click sidebar item → show that section, hide others; keep the existing `location.hash` sync if desired.
- **Chart hover (NEW — add to every line/area chart):** on `mousemove` over the plot, snap to the nearest sample index and draw a vertical **crosshair** (`rgba(255,255,255,0.28)`, 1px) + a **dot on each series** + a floating **tooltip card** (`#12150E`, 1px border, radius 8, shadow) listing each series' name (color swatch) and value, with a header like `22 min ago`. Tooltip flips its horizontal anchor near edges (translateX 0 / -50% / -100%). See `LineChart.dc.html` for the reference implementation (nearest-index mapping via `getBoundingClientRect`, value formatting via a passed `fmt` fn).
- **Sortable tables (NEW — all tables):** every column header is click-to-sort, single-column, toggling asc/desc; active header shows `↑`/`↓` and turns green `#A7D65A`. **Header text-alignment must match its column's cell alignment** (left for the name column, right for numeric) — this was a specific fix. Default sorts: Per-model = Tokens out desc; others sort by their primary numeric column desc. See `SortTable.dc.html`.
- **Charts span the full width** of their container (plot reaches the right edge; y-axis value labels are compact **right-edge overlays**, not a reserved left/right gutter).
- **Growable tables:** cap the card/table height (~300–440px), sticky `<thead>` (`background:#12150E`), vertical scroll for rows, horizontal scroll for wide column sets — so a table with 8 or 80 rows never leaves dead space or pushes the page.
- **Responsive:** all multi-column rows use `repeat(auto-fit, minmax(…,1fr))` or `flex-wrap` so they reflow (never a rigid grid that overflows). The main content column has `min-width:0; overflow-x:hidden`.

## State
- `tab` (active screen). Per-table sort state `{columnIndex, ascending}`. Chart hover `{hoveredIndex}` (transient, cleared on mouseleave). Everything else (samples, mode live/range, cfg) is the **existing** dashboard state — unchanged.

## Data mapping (prototype → existing metrics)
The prototype uses mock series; wire each to what `render()` already computes:
- KPI Requests/Tokens/Success/Saved, gauges, top models/harnesses → existing `wreq`, `allC`, `okRatio`, `saved`, `capRatio`, `wgroups(...)`.
- Models table columns → existing per-model maps (`ctok`, `reqModels`, `ttftMS/MC`, `tpsMS/MC`, `tpotAvg`, `truncPct`, `reasonPct`, `errPct`, saved).
- Quantile charts → existing `quantSeries('nimproxy_ttft_seconds')`, `_tokens_per_second`, `_tpot_seconds`, `_upstream_seconds`.
- Reliability latency composition → `queue_wait` avg + `upstream_seconds` avg + `ttft`/generation; error taxonomy → `nimproxy_requests_total` by non-200 status + `lane_benched_total`.
- Capacity saturation/provisioning → `cfg.lanes`, `cfg.rpm`, `curRpm`/`avgRpm`, `peakRpm`, `headroom`, `k-needed`.
- Clients bars/table → existing `wgroups('...request_tools...','client')`, messages, temperature, stream mix.

## Assets
- `assets/nim-logo-mark.png` — the NIM Proxy mascot logo (green "N" cop with shades), used at 34px in the sidebar. **User-provided.**
- `assets/nim-logo-lockup.png` — horizontal logo + wordmark (spare, for headers/README).
- Fonts: Google Fonts — Space Grotesk, Spline Sans Mono. Publisher/model icons in the current dashboard come from the LobeHub AI-icons CDN (`@lobehub/icons-static-svg`) with a monogram fallback — keep that.
- Gauge SVG defs: `gGreen` (green→transparent) and `gMuted` (grey→transparent) linear gradients for the KPI sparkline fills.

## Files in this bundle
- `NIM Proxy Dashboard.dc.html` — the full redesigned dashboard prototype (all 5 screens, exact inline styles, mock data + geometry helpers). **Primary reference.**
- `LineChart.dc.html` — reusable line/area chart with hover crosshair + tooltip. Reference for the hover interaction.
- `SortTable.dc.html` — reusable sortable/growable table (sticky header, scroll, alignment rule). Reference for table sorting.
- `support.js` — the prototype runtime (only needed to open the `.dc.html` files locally; **do not port this**).
- `assets/` — logo images.

To view the prototype: open `NIM Proxy Dashboard.dc.html` in a browser (it self-loads `support.js`). It renders with mock data; use it purely as the visual/interaction spec.
