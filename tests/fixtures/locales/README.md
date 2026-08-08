# locale-v1 negative fixtures

One deliberately broken locale file per check the validator makes, plus
`valid.json` as a positive control. `_source-en-US.json` is a four-message
synthetic source — small enough to read, so a fixture's defect is obvious
without diffing against the canonical catalog.

These were written **before** the validator, and
`scripts/locale_v1.py --selftest` asserts that each one fails for its own
reason and that `valid.json` passes. A validator whose negative fixtures were
written afterwards tends to test what the implementation happens to do; these
describe what it is *supposed* to do.

Task 5 adds seven in-memory contract mutations for the public projection,
production-locale registry, duplicate ids, key order, and wire schema. Together
with the twelve files below, the self-test executes 19 cases. The generated
`public-en-US.json` fixture is never edited independently; refresh it with
`python3 scripts/locale_v1.py --update-public`.

| Fixture | Defect | Check it must trip |
|---|---|---|
| `valid.json` | none | must pass — guards against a validator that rejects everything |
| `missing-key.json` | `app.count` absent | completeness |
| `orphan-key.json` | extra `app.nonexistent` | no orphan keys |
| `placeholder-mismatch.json` | `{total}` renamed `{grand_total}` | placeholder parity |
| `bad-formatter-syntax.json` | unclosed `{total` | formatter syntax |
| `raw-html.json` | value contains `<script>` | no raw markup in values |
| `entity-html.json` | value contains entity-encoded markup | no entity-encoded markup in values |
| `stale-hash.json` | hash does not match the source it was translated from | source-hash freshness |
| `too-long.json` | 19 chars against a `maxLen` of 12 | per-key length cap |
| `inline-dropped.json` | a required `{b}`/`{/b}` pair is removed | inline-marker structure |
| `unbalanced-inline.json` | `{b}` with no `{/b}` | inline-markup balance |
| `frozen-token-dropped.json` | a never-translate token is changed | frozen-token preservation |

Adding a check means adding a fixture here first. A check without one is
decoration: it has never been observed to fail, so nothing establishes that it
can.
