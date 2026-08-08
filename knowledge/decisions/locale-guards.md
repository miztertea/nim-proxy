---
type: Decision
title: Guard localization with a pseudolocale, a validator, and an untagged-string lint
description: Three language-independent guards, each with a negative fixture proving it can fail, because translation correctness cannot be reviewed by reading English.
tags: [i18n, testing, ci]
timestamp: 2026-07-29T00:00:00Z
---

# Guard localization with a pseudolocale, a validator, and an untagged-string lint

## Context

With the catalog in place ([message-catalog-and-escaping](message-catalog-and-escaping.md)),
the failure modes stop being things a reviewer can see. A missing key renders the
raw id. A renamed placeholder leaves `{count}` on screen. A translation made from
an older English string stays valid and quietly says the wrong thing. And a
hardcoded label added next month looks exactly like the code around it.

None of that is visible in an English screenshot, and none of it is reachable by
`cargo test`, which asserts on served HTML text and never parses the JavaScript.

## Options

1. **Review translations by eye.** Requires a fluent speaker per locale per
   change. Does not scale past one language and catches none of the structural
   faults, which are the common ones.
2. **Trust the generation pipeline.** The pipeline is exactly what needs
   checking.
3. **Language-independent guards.** Nothing here needs a human to read Chinese.

## Choice

**Option 3**, three guards:

**`en-XA` pseudolocale.** Accents every letter, brackets each message, pads to
135%. Untranslated text stays plain ASCII and stands out; a container that only
fits English fails here rather than after a locale ships; a clipped string shows
as a missing `]`. It is generated in memory for tests, never committed or
served by production, so it is always complete and never stale. Placeholders
pass through untouched — an accented `{çôûñt}` would
not substitute, which would make the pseudolocale test itself instead of the
layout.

**`locale-v1` validator.** Completeness, orphans, placeholder parity, formatter
syntax, separate raw-markup and entity-encoded-markup checks, exact inline
marker structure, source-hash freshness, opt-in length caps. Exact inline
structure keeps the runtime and accepted locale shapes aligned: emphasis
markers may surround translated text, but cannot be removed, duplicated, or
reordered. The hash is what makes a
regenerate-on-drift pipeline possible: it records which English text a
translation was made from, so "still valid, no longer correct" is detectable.
The one rich `src/web/locales/en-US.json` source is also the authority for the
generated public fixture. That projection must contain every `setup.*`, every
`login.*`, and only `common.app_name`; missing, extra, drifted, or non-ASCII
ordered ids fail. The self-test contains 12 file fixtures and seven in-memory
projection/schema cases, 19 named negative cases in total.

**Locale preference tag grammar.** Persisted and API-supplied preferences use
one Rust canonicalizer for the frozen `language[-Script][-REGION]` subset:
language is 2–3 ASCII letters, Script is four ASCII letters, and REGION is two
ASCII letters or three digits. Canonical output lowercases language,
title-cases Script, and uppercases alphabetic REGION. Whitespace, underscores,
non-ASCII, variants, extensions, private use, and extra subtags are invalid.
Syntax is checked before the compiled installed-locale registry, so a valid
but unavailable tag has the distinct `locale_not_installed` contract.
Production installs only `en-US`; `en-XA` remains valid syntax but is test-only
and therefore rejected by the production preference APIs.

**Untagged-string lint.** Fails on a display literal that bypasses `message()`,
covering attributes as well as text. Without it, English creeps back within two
PRs, because a hardcoded label looks exactly like the code beside it.
Its standard-library HTML parser never pushes void elements (`input`, `img`,
`br`, and the complete HTML void set) onto the owning-ancestor stack: HTMLParser
does not emit their end tags, so treating a catalog-tagged input as open would
hide later sibling prose. Negative controls prove both that void-element
ownership ends immediately and that a normal catalog-tagged ancestor still
owns its descendants.

**Contextual-sink lint.** Page code passes ids to native DOM helpers and inert
descriptors to fixed-markup HTML builders. Named self-tests independently
reject URL, style, event, script, CSS, raw-SVG, native-attribute, raw-HTML,
alias, string-spoof, ASI, and structured-message-HTML mutations. Descriptor
tests cover direct text, URL, style, native-attribute, and SVG sinks. Expected
check ids must match exactly. See
[message-catalog-and-escaping](message-catalog-and-escaping.md).
The normal source pass blanks only the lexical resolver declaration and exact
canonical raw-lookup helper bodies; every remaining bare `message` identifier
fails. This bounded convention does not infer JavaScript owners. Text and
structured helpers reject script/style/SVG destinations and replacements, and
HTML builders must resolve descriptors through `escapeHtml()`. This is a
direct-convention regression guard, not a verifier for deliberately obfuscated
trusted JavaScript; runtime resolver isolation, descriptor coercion refusal,
and replacement validation are the accidental-misuse boundary.

**Every check has a negative fixture, and the fixtures came first.** They are
committed in a separate commit before the validator exists, and `--selftest`
asserts each one fails for its own reason. A check nobody has watched fail is
decoration: nothing establishes it can.

## Consequences

- The guards immediately found three real defects that had survived PR 3's
  review and its own linter:
  - The **runtime-churn strings** were never extracted — `Live`, `Absolute`,
    `Disconnected` on the dashboard, and `Validating…`, `Copied`,
    `Select & copy` plus the document `<title>` in the wizard. PR 3's inventory
    predicted this exact gap; the pseudolocale is what made it visible.
  - `locale-v1 --all` was pairing the **wizard's** locale against the
    **dashboard's** source catalog, reporting every id in both as missing or
    orphaned. The validator caught a bug in its own runner.
  - `setup.html` called `tRaw()` while defining only `rawMsg()`. Because
    `applyStatic` aborts on the first throw, one missing helper left the entire
    page untranslated rather than one string — and neither `node --check` nor
    `cargo test` can see it. A dedicated lint now catches that class.
- The untagged-string lint covers Settings after foundation Task 7; `chipHtml`'s
  interior remains excluded permanently.
  Flagging `chipHtml` would invite someone to "fix" it by routing a catalog
  value through the URL and script contexts PR 3 spent its effort removing.
- `en-XA` proves layout mechanically but not **clipping**, which needs eyes.
  That review stays with Thomas, as the plan always had it.
- **A leak scan is only as good as the page it scans.** The first one ran
  against the pages at rest, with no API data — where the KPI cards, ring
  gauges, perf blocks and every table body never render at all. It reported
  zero English and was measuring almost nothing. Replaying payloads captured
  from the real binary currently shows 31 actionable runs and 27 correctly
  untranslated machine/frozen runs; setup shows zero actionable and seven
  correctly untranslated runs. The inventory lives in
  `tests/fixtures/locales/REMAINING.md`. Most are the sentence fragments with
  interpolated counts that PR 3 deferred and PR 4 did not pick up; the rest are
  labels rendered by `ringGauge` and `perfBlock`. Any future leak scan must run
  against a populated page.
