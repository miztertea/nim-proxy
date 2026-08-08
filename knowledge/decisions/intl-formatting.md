---
type: Decision
title: Format numbers, durations, and dates with Intl, keyed to the catalog locale
description: Hand-rolled K/M/B suffixes and concatenated units are replaced by cached Intl formatters reading the catalog's locale. CSS percentages are deliberately excluded.
tags: [i18n, dashboard, formatting]
timestamp: 2026-07-29T00:00:00Z
---

# Format numbers, durations, and dates with Intl, keyed to the catalog locale

## Context

With text coming from a catalog ([message-catalog-and-escaping](message-catalog-and-escaping.md)),
the numbers beside it were still formatted by hand: a ternary chain appending
`K`/`M`/`B`, durations built by concatenating `' ms'`/`' s'`/`' min'`, and seven
date strings assembled from `toLocaleDateString`/`toLocaleTimeString` with the
locale argument left as `[]` — the browser's, not the interface's.

That last part is the real defect. A dashboard rendered in German would have
grouped thousands the American way because the operator's OS happened to be
en-US.

The hand-rolled `fmt` also had two arithmetic bugs that nobody had hit:
`999999` rendered as **`1000.0K`** rather than rolling over to `1M`, because the
`>= 1e4` branch divided by `1e3` for everything below `1e6`; and `1e12` rendered
as **`1000.0B`**, because there was no tier above `B`.

## Options

1. **Leave it.** Cheapest, but locks the dashboard to one language's number
   habits and keeps two rollover bugs.
2. **Extend the ternary chain** — add a `T` tier, fix the divisor, add
   separators by hand. Fixes the bugs, still wrong for every non-English locale,
   and grows the thing that was wrong in the first place.
3. **`Intl`, keyed to the catalog locale.**

## Choice

**Option 3.** Formatters are constructed once at module scope — `Intl`
constructors are expensive and these run inside render loops — and read
`LOCALE` from the shipped catalog rather than the browser.
`PLURALS` is cached in that same shared formatter layer, so dashboard and
Settings counted messages select CLDR categories without either constructing a
second `Intl.PluralRules` or depending on script load order.

Thresholds are deliberately **unchanged**. `Intl`'s own compact notation begins
at 1,000 (`1K`); this dashboard shows exact counts up to 10,000 because request
counts in the hundreds are meaningful to an operator and `1.2K` is not.

**CSS percentages are excluded, and this is the important part.** The
`toFixed()` calls that remain are inside `style=` attributes or SVG path
geometry — both machine formats, never read by a human.
`style="width:12.3%"` must stay locale-independent: a comma-decimal locale would
emit `width:12,3%`, which is invalid CSS and silently collapses the element to
zero width. Display percentages go through `Intl` with `style: 'percent'`;
layout percentages stay raw. Confusing the two is a layout bug that appears only
in some locales, which is the worst kind.

Date stamps use explicit components rather than `dateStyle: 'short'`, which
abbreviates the year to two digits — ambiguous on a dashboard that retains 30+
days of history.

## Consequences

- Formatter outputs change in en-US; the golden fixture records them. Two are
  bug fixes (`999999` → `1M`, `1e12` → `1T`), four drop a redundant trailing
  `.0`, four change the seconds unit from `s` to the locale-correct `sec`, and
  one gains thousands grouping. Rounding also moves from `Math.round`
  (ties toward +∞) to Intl's half-expand, so `-100.5` now renders `-101` rather
  than `-100`; the compact tiers still top out at `T`, so values above `1e15`
  read `1000T`.
- `secs()` now reads `1.0 sec` rather than `1.0 s`. Slightly longer, but it is
  what `Intl` considers correct for en-US and it matches the `ms`/`min` forms,
  which already used a space and an abbreviation.
- Settings numbers are a different contract from dashboard compact display:
  catalog parameters for key slots, rate-window values, pool capacity,
  validation model counts, and user key counts use `NUM_GROUPED` so every API
  integer remains exact while adopting the catalog locale's grouping. A value
  above 10,000 must never become dashboard-style `12K` in Settings.
- The fixture harness reads formatter bodies straight out of
  `src/web/shared.js` and `src/web/dashboard.js`, plus the locale from the one
  canonical rich English catalog, so it cannot drift from the code and source
  locale it pins. It refuses to run unless `TZ=UTC`. An unpinned fixture would
  encode whichever machine last wrote it.
- Verified in a second locale: `de-DE` yields `1,2 Mio.`, `1,5 Sek.`, `50 %`
  with the non-breaking space German uses. Note the compact tiers are CLDR's,
  not ours: German has no thousands abbreviation (`12345` → `12.345`) and
  Japanese uses 万. The "sized for 12.3K" intent behind the ≥1e4 threshold holds
  for en and fr only.
- The catalog's `locale` is validated before any formatter is constructed. A
  malformed tag — the POSIX `en_US` spelling, say — would otherwise throw at
  module scope, before a single function was defined, leaving the page
  completely dead rather than mis-formatted. Startup now rejects the catalog
  and reveals the dependency-free emergency message instead of silently
  formatting under a different locale.
- `Intl.DateTimeFormat` throws `RangeError` on a non-finite time where
  `toLocaleString()` returned `"Invalid Date"`. Every date call site goes
  through a guard, because a throw escapes the template it sits in and blanks a
  whole panel rather than one label.
