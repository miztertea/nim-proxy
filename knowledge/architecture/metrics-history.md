---
type: Component
title: Metrics & history (src/history.rs)
description: Prometheus registry, recoverable canonical JSONL, query-scoped completeness, exact range rollups, and deferred canonical retention compaction.
tags: [metrics, history, prometheus]
timestamp: 2026-07-02T00:00:00Z
---

# Metrics & history — `src/history.rs`

**Live metrics** use a `metrics-exporter-prometheus` registry rendered at
authenticated `GET /metrics` (series list in the README). Custom histogram
buckets cover TTFT, tokens/sec, queue wait, upstream latency, and request-shape
distributions. The registry remains the collection source; the dashboard no
longer downloads or parses its raw exposition.

## Persisted format

Task 10 defines the canonical successor format, `nimproxy-history/v1`; Task
11 publishes and appends it at runtime; Task 12 recovers supported v1 damage
without presenting the resulting gaps as complete. `HistoryStore::open`
streams existing canonical rows in physical order before appending one fresh
boot boundary, or publishes the first boot through a unique same-directory
temporary, file sync, no-overwrite hard link, temporary-name removal, and
directory sync. Empty or whitespace-only canonical input and a detectable
unknown format/version anywhere refuse startup without mutation. A failed
fresh-boot append or sync also refuses; a sync failure can leave a complete
valid boot record, while a partial failed tail remains evidence.

Every canonical JSONL row begins, in this exact order, with
`format:"nimproxy-history"`, `v:1`, and `kind`. The complete field order is:

```json
{"format":"nimproxy-history","v":1,"kind":"boot","timestamp":1000,"boot_id":"boot-a","capacity":{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}
{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":1000,"boot_id":"boot-a","capacity":{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]},"state":[{"kind":"counter","metric":"nimproxy_requests_total","labels":{"client":"redacted-client"},"value":1.0}]}
{"format":"nimproxy-history","v":1,"kind":"checkpoint","timestamp":1300,"boot_id":"boot-a","capacity":{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}
```

`boot` and `checkpoint` carry no state. A `sample` has a complete state made
of `counter` or `gauge` entries, each ordered `kind`, `metric`, `labels`, and
`value`. The codec rejects non-finite values and duplicate semantic series
`(kind, metric, labels)`; it never normalizes or chooses a last writer.
Unknown non-state object fields are ignored, while any invalid state entry
rejects the whole sample. Reordering otherwise valid object fields is accepted
on decode and canonicalized by encode. Invalid UTF-8, invalid JSON, corrupt
v1 record/state, and unknown v1 record kind are distinct codec diagnostics.
An unknown `format` or `v` is explicitly `unsupported_format` or
`unsupported_version`, not corrupt-line recovery; Task 11 uses those errors to
refuse startup.

The canonical destination is `history-v1.jsonl`. The production runtime never
reads, renames, truncates, deletes, migrates, or compacts experimental
`history.jsonl`; it emits one startup warning containing only that path and
byte length. Stale same-directory canonical temporaries are likewise opaque
evidence; startup emits at most one warning containing their count, and never
their names or contents. File order is authoritative and equal timestamps are
valid. A runtime encode/write/flush/sync failure poisons the writer, so later
ticks cannot append after partial evidence. `file_bytes` is live metadata from
the canonical writer. Finite-retention compaction applies only to this
canonical destination and follows the protocol below.

## Recovery and completeness

The canonical scanner is one pass with four explicit states:
`AwaitBoot`, `AwaitSample`, `Usable`, and `InvalidEpoch`. A valid boot starts a
candidate epoch; only a valid full sample makes it usable, and checkpoints
carry that sample's state only inside the same boot id. Malformed supported-v1
JSON, an invalid state entry or duplicate semantic series, an unknown v1 kind,
a boot-id mismatch, an orphan checkpoint, or a timestamp regression invalidates
the candidate epoch. Its records are excluded until a monotonic boot followed
by a valid sample re-establishes usable state. A boot-only epoch is excluded
rather than treated as empty history.

The scanner never sorts. A trustworthy timestamp from a rejected JSON object
still advances the physical ordering bound, so a later lower-timestamp boot
cannot manufacture validity. A supported unterminated tail is left untouched;
startup syncs one delimiter before appending its fresh boot so later restarts
can scan the append-only evidence. A complete unterminated record that declares
an unsupported format/version remains fatal. All recovered bytes remain in the
file; startup neither truncates nor repairs them.

Recovery records bounded gaps and query-timestamped diagnostic events rather
than raw rows. `excluded_epochs` counts invalidated or boot-only candidate
epochs; `excluded_records` counts their physical records and damaging records.
`valid_samples` and `valid_checkpoints` count accepted record kinds through the
query end, while `normalized_series` counts the state entries normalized by
those accepted samples/checkpoints. `skipped_metric_lines` remains separate
live Prometheus-text parsing evidence. Diagnostics are cumulative only through
the requested `to`; they do not claim that every count occurred inside the
requested `from..to` slice.

`History::rollup` returns `HistoryWindow<Rollup>` with `complete`, `data`, and
`diagnostics`. A window is complete only when it contains usable observations
and no recovery gap overlaps the requested interval. A gap-free query with no
usable observation is unavailable rather than an invented zero. A later valid
epoch can therefore be complete for its own interval even when earlier queries
remain partial or unavailable.

### Sanitized corpus evidence

On 2026-07-31, a read-only ephemeral container streamed the local
`nim-proxy_history` volume through a metadata-only analyzer: 235,966,850 bytes,
8,014 rows, one boot row and 8,013 legacy-sample rows, zero malformed rows,
and timestamps 1,783,077,758 through 1,785,479,582. The dominant cadence was
300 seconds (7,985 intervals), with 15 at 301 seconds and isolated restart or
timing gaps. The analyzer self-check first extracted a synthetic metric and
two synthetic label keys. The corpus extraction then found 45 repository metric
names and their label-key sets without printing values; its three structural
hashes were `ed3745ecb9bc2cc4` (7,503), `087debe8b773af5d` (510), and
`bebe937ef48eda06` (1). Of 8,013 payloads, 7,238 were nonempty and all 7,238
had distinct payload hashes; those nonempty payloads contributed 221,875,277
bytes. Sampling rows are idle-cadenced, but byte growth is traffic/state-driven
rather than empty-idle snapshots. Fixtures preserve only approved metric
identifiers, label keys, scalar shapes, and ordering with synthetic redacted
values.

## Startup index and range rollups

History indexing is a synchronous startup task and completes before the
server listens. The canonical store streams and validates the physical JSONL
in file order, then moves its validated records once into the typed index; it
does not sort, repair, or reread raw rows. It normalizes every valid sample so
a pre-retention sample can supply the boundary baseline, then indexes only the
retained points (or all points when retention is unlimited). Canonical startup
does not promise the older detailed byte/sample diagnostic summary. This is the
only precomputation: page-specific dashboard models are deliberately not cached
([reset-aware decision](../decisions/reset-aware-dashboard-history.md)).

`History::rollup(from, to, points)` returns:

- exact counter totals for the requested/effective window;
- the latest observed value per gauge series within the window;
- chart buckets capped by the requested point budget;
- contemporaneous capacity per chart bucket;
- available/effective bounds, query-scoped completeness/diagnostics, and a
  monotonic history revision.

Totals are computed from the normalized index, not from the chart buckets, so
`points=2` and `points=288` return identical totals. Sample timestamps are the
precision boundary; partial buckets do not pretend to know intra-sample event
times. A delta belongs to a range when `from < sample_time <= to`. The HTTP
contract defaults to 288 presentation points and clamps requests to 2–1000.
When a requested range begins before the first retained sample, that sample's
exact delta remains in the total, but its chart point has zero duration and no
capacity average: the unavailable prefix is not treated as observed time.

`History::current()` renders the live registry under the same history
generation lock. It returns current typed metrics plus a tail whose totals are
the counter delta after the persisted baseline. The tail carries
`base_history_revision`; the browser accepts it only alongside the matching
range revision, so a newly persisted sample cannot be double-counted. The
bounded `nimproxy_usage_observations_total` counter is ordinary generic
counter state: history preserves its exact field/result rows without a special
API field, while the dashboard's response-quality derivation ignores
non-finite/negative rows and only reads a revision-matching live tail. See
[NIM observations](nim-observations.md) and [Dashboard](dashboard.md).

## Retention and durability

`history.days` in the
[config store](../decisions/ui-managed-config-store.md) defaults to 30;
`0` means unlimited. It is distinct from
`dashboard.default_window_days`, and finite retention must be at least as
long as the default dashboard window.

Changing retention in Settings validates and persists the complete config,
then immediately trims the visible in-memory index. A finite canonical cutoff
is `now - history.days * 86,400`, using saturating arithmetic. Existing
retention debt is exposed as `compaction_pending` at startup; the first full
sampler append schedules it. A live finite-retention change schedules work
immediately. Setting `history.days` to `0` invalidates the finite generation,
clears pending work, and prevents a stale worker from renaming its candidate.
Compaction runs on a blocking worker behind the canonical store mutex, never
on the request dispatcher or rate-limiter path.

For each eligible epoch, canonical compaction retains the epoch boot, the
latest full sample strictly before the cutoff as its boundary baseline, and
every record at or after the cutoff, all in original physical order. An epoch
is eligible when it has a retained observation or is the usable live epoch.
A checkpoint carries no state of its own and therefore cannot replace the
pre-cutoff full-sample baseline. This is the minimum context that preserves
counter deltas, gauges, capacity, completeness, and effective bounds after a
restart.

The store revalidates the current path while holding its exclusive writer. If
a recovery gap ends after the cutoff, or the live epoch has no full sample,
replacement is deferred and `compaction_pending` remains true. A gap ending
exactly at the cutoff is outside the retained window and may be discarded.
This rule prevents validated-row rewriting from erasing evidence that still
makes a retained query incomplete.

For a safe stream, the writer creates a unique same-directory temporary,
writes the selected canonical records, flushes and syncs it, and requires the
temporary to decode as exactly that selected stream with no recovery evidence.
Generation authorization is then held through the atomic rename. The already
open append-capable temporary handle becomes the live writer after rename; no
fallible reopen separates replacement from subsequent append. The parent
directory is synced last. Failures before rename leave the old path complete
(and may leave an opaque stale temporary); rename produces the complete new
path. A directory-sync failure after rename is reported as committed but
durability-uncertain, installs the replacement replay, and leaves cleanup
pending. Superseding retention generations are rescheduled without overwriting
newer append diagnostics or events.

The accelerated release proof seeds slightly more than two 30-day horizons,
then observes three idle checkpoints followed by three no-traffic restart
epochs. Idle intervals append exactly one checkpoint and zero full samples;
each 3,600-second restart appends an exact `boot`/`sample` pair while preserving
the prior raw-byte prefix and range results. In the deterministic fixture the
settled base was 6,469 bytes, the first checkpoint allowance was 195 bytes, the
first restart allowance was 391 bytes, and the independent linear bound
`6,469 + 3*195 + 3*391 = 8,227` bytes equaled the final file. These byte values
describe the fixed synthetic fixture, not a universal workload-size promise;
the durable guarantee is bounded retained context and small idle checkpoints
instead of repeated full idle snapshots.

A canonical history-file write failure warns and leaves the in-memory index
operating, but poisons further durable writes for that process; an unusable
`DATA_DIR` or config store is still a hard boot error
([configuration](../ops/configure-env.md)).
