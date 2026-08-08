---
type: Decision
title: Standard ops-dashboard vocabulary
description: >-
  Every user-visible term maps to standard Grafana/HAProxy/Envoy vocabulary.
  Nothing is invented, so translation needs no glossary — and the mapping now
  lives in the repository where checks can reach it.
tags: [i18n, dashboard, terminology]
timestamp: 2026-07-29T00:00:00Z
---

# Standard ops-dashboard vocabulary

## Context

0.6.6 exists to retire design-phase vocabulary from the presentation layer, on
the premise that localization then falls out as a byproduct: these are terms
every ops team has used for decades, already translated thousands of times in
Grafana, Kibana, Prometheus consoles and every cloud dashboard. A model
produces correct Chinese for `Error rate`, `Queue wait`, `Session affinity` and
`Latency breakdown` without help. **Requiring human linguistic judgment: zero.**

The mapping was decided before implementation began. It was **not committed to
this repository** — it lived in the planning bundle — and that omission is the
root cause of most of what went wrong in this release:

- Nothing downstream could check against it, so five spellings of per-minute,
  four of time-to-first-token and three names for the model governor all
  survived a pass whose entire purpose was standardization.
- Later work re-derived the decisions from the code instead of reading them,
  concluded that `Slot N` under a `Key` column was an invented collision, and
  proposed "writing a glossary" that already existed and said the opposite.
- An audit spent three agents rediscovering findings the extraction spec had
  already inventoried.

The rule this page exists to enforce: **the vocabulary is the authority, and it
has to be somewhere a check can read it.**

## Options

1. **Leave it in the planning bundle.** Zero cost, and demonstrably the state
   that produced the drift above.
2. **Commit it as prose only.** Readable, but nothing prevents the next label
   from ignoring it — exactly the position we were already in.
3. **Commit it and make the enforceable half executable.** Not everything here
   can be checked (whether a *new* label is standard is a judgment call), but
   two rules can be: frozen tokens must survive translation, and retired terms
   must not come back.

## Choice

Option 3.

### Two structural fixes this vocabulary makes

**`window` was overloaded.** It meant both the rate-limit rolling window and
the dashboard time selection — two unrelated concepts sharing a word, the worst
case for a translator. Rate limiting **keeps** `window`; the dashboard adopts
**time range** (Grafana's word).

**A lane is a key.** The highway metaphor was design-phase; in the product a
lane is one NIM credential with its own rate window, 1:1 with keys. The
interface says **key**. Metric labels keep `lane`
(`nimproxy_lane_requests_total`) — renaming the series would be a second
breaking change and was not taken.

### The mapping

`KEEP` already standard · `RENAME` → standard equivalent · `DROP` removed ·
`RAW` machine value, never translated.

| Retired | Standard |
|---|---|
| Harness / Harnesses | **Client / Clients** |
| Default dashboard window | **Default time range** |
| selected window | **selected time range** |
| in window | **in range** *(dashboard sense only)* |
| No traffic in selected window | **No data in selected time range** |
| All retained | **All time** |
| fixed range | **Absolute** (Grafana) |
| following now | **Live** |
| Earliest retained snapshot | **Oldest data point** |
| History file | **Data file** |
| Capacity used · Now | **Capacity used** |
| Now rpm / rpm total / rpm free | **Current rate** / **Total** / **Available** |
| Historical provisioning | **Capacity history** |
| rpm short at peak | **Peak shortfall** |
| % vs contemporaneous capacity | **% of capacity at the time** |
| exhaustions/min | **Capacity errors/min** |
| Lane | **Key** |
| Lane slot | **Slot** |
| Lane slots · Now | **Slots in use** |
| Rate-limit pressure | **Throttling** |
| Rate-limit benches / Other benches | **Rate-limit cooldowns** / **Upstream error cooldowns** |
| 429s this window | **Rate limited (429)** |
| Shed · 401 · failed logins | **Dropped** · **Unauthorized** · **Failed logins** |
| Where time goes | **Latency breakdown** |
| avg request, end to end | **avg end-to-end** |
| Avg reply | **Avg response** |
| Tool-offering / Tool-using requests | **Requests with tools** / **Requests using tools** |
| No reasoning-token usage seen | **No reasoning tokens** |
| Conversation stickiness | **Session affinity** (HAProxy/nginx) |
| Model-pressure governor | **Model limits** |
| Keyed / Open | **API key required** / **Open (no authentication)** |
| live down / live idle | **Disconnected** / **Idle** |
| Dollars saved, `money()`, vs reference pricing | **DROP** — see [no-estimated-savings-metric](no-estimated-savings-metric.md) |

Kept as already standard: Overview, Models, Clients, Reliability, Capacity,
Requests, Active now, Queued, Queue wait, Count, Saturation, Capacity, Error
rate, error budget, Availability, Availability SLO, Success rate, Time to first
token, Inter-token latency, Generation speed, Prompt/Completion tokens, Tokens
in/out, Streaming, Buffered, Filtered, Truncated, Completed, Reason.

### Never translated

`HTTP` · `POST` · `Content-Type` · `application/json` · `JSON mode` · `429` ·
`401` · `5xx` · `504` · `rpm` · `req/min` · `requests/min` · `tok/s` · `TTFT` ·
`TPOT` · `SLO` · `p50` · `p95` · `%` · `NIM` · `nim-proxy` · `nvapi-…` ·
`npk_…` · every `nimproxy_*` series · all model ids · all persisted enums ·
`/v1` · base URLs · `Msgs/req` · `Tools/req`

Note `TTFT` (abbreviation, raw) and `Time to first token` (expanded,
translated) both exist and both belong in the catalog. That is not an
inconsistency.

### Canonical English inventory

`src/web/locales/en-US.json` is the sole rich authoring source. Each id records
its canonical English value, UI intent in `desc`, placeholders and plural
shape, and source hash; public membership is structural rather than separately
authored: every `setup.*`, every `login.*`, and only `common.app_name`.
Task 5 reconciled the setup wording to ordinary operational language:
“Complete setup,” “NIM API keys,” the durable-key warning, “API key required,”
and “immediate access.” Login adds only the expected title, prompt, field
labels/placeholders, submit label, and invalid-credentials message. These are
repository-owned labels; model ids, client/publisher names, persisted values,
and metric identifiers remain raw.
Task 7 applies the same distinction to Settings: use **NIM API key**, **client
API key**, **API key required**, **Open (no authentication)**, **Model limits**,
**Model cache TTL**, **Stream idle timeout**, and **concurrent** for the
governor cap. Settings status/validation/dialog copy is repository-owned;
model ids, usernames, client names, API error messages, persisted role values,
and numeric API values remain raw (with locale grouping only for numeric
display). The pool note keeps the standard **Total** label rather than the
retired `rpm total` run.

## Consequences

- **Two checks, no new script.** `locale_v1.py` gains `frozen`: for every
  never-translate token the source uses, the translation must contain it
  verbatim. `check_i18n.py` gains `lint_retired_vocabulary`: no catalog value
  may reintroduce a retired term. Both reuse the single `NEVER_TRANSLATE`
  definition rather than copying it.
- **The retired list is multi-word and distinctive on purpose.** Single
  ambiguous words are excluded: `window` is still correct for the rate-limit
  window, `lane` for metric labels, and `Open`/`bench` have unrelated
  legitimate senses. Banning a word that is both a retired label and a live
  domain term is precisely the mistake that renamed a rate-limit counter during
  the label sweep — see the trap list in [AGENTS.md](../../AGENTS.md).
- **The `frozen` check found a defect immediately.**
  `gen_pseudolocale.py` was accenting the frozen tokens, so `en-XA` rendered
  `NÎM` and `ŦŦƑŦ` — a string no real locale would produce, in the one locale
  generated to prove layout. The generator now protects them and emits it only
  to test consumers; production ships only `en-US`.
- **What is still not checkable:** whether a *new* label is standard. That
  needs a person, and this page is what they check against.
- **The context bundle mostly disappears.** A per-message translator glossary
  existed to explain idiosyncratic terms. Once every term is standard there is
  nothing to explain; this page is the context.
