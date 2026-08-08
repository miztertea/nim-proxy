# Captured API payloads

Real responses from the real binary, fulfilled through CDP while
`scripts/render_check.js` boots the current binary and loads its real
dashboard page/assets.

Do not delete these as unused files: nothing imports them, but the render gate
refuses to run without them.

## Why captured rather than hand-written

Every leak scan and screenshot sweep before this one measured a page **at
rest** or against a *healthy* proxy, and both hide most of the interface. KPI
cards, ring gauges, perf blocks and table bodies do not exist in the DOM until
data arrives, and the entire error taxonomy — the `TAX`, `OUTCOMES` and
`REASONS` tables in the Reliability tab — only renders when the selected range
actually contains failures. A fixture without failures silently exempts them.

## What is in them

Captured against `scripts/mock_nim.py` with three NIM keys, three client keys
and three models, driving mixed traffic: streaming and buffered, with and
without tools, JSON mode, capped and uncapped output.

Outcomes deliberately present:

| Outcome | How it was provoked |
|---|---|
| `200` | ordinary completions |
| `504` upstream timeout | client deadline shorter than the paced wait |
| `disconnect` | client abandoned a queued request |
| `429` + rate-limit cooldowns | mock enforcing a lower rpm than the keys advertise |
| worker exhaustion | `--worker-slots` below the offered concurrency |
| affinity `sticky` / `spill` | more concurrent conversations than lanes |
| finish `stop` / `tool_calls` / `length` | plain, tool-offering, and `max_tokens`-capped requests |

`dashboard.json` is captured with the query the page actually issues
(`?from=…&to=…&points=288`), not the bare path — the bare path rolls the whole
retention window into a single point and every chart renders empty, which is
its own way of measuring nothing.

**Known gap:** `content_filter` (the `Filtered` finish reason) is absent. It is
upstream content policy with no honest trigger in a mock, and inventing one
would mean the fixture asserts a behavior the mock made up. That row stays
uncovered, deliberately.

## Regenerating

Start `scripts/mock_nim.py --enforce --rpm 6 --worker-slots 2`, start the
proxy against it with `HISTORY_SAMPLE_SECS` low enough to accumulate samples,
claim it via `POST /setup`, mint client keys, drive the traffic mix above, then
capture `/api/config`, `/api/dashboard?from=…&to=…&points=288` and
`/api/dashboard/now`. Keys and secrets are per-run and none are real.
