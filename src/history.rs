//! Metrics history uses a canonical, newline-delimited v1 store. It publishes
//! a boot record before listeners start, then writes normalized state samples
//! or idle checkpoints as the sampler runs. Dashboard range queries consume a
//! typed in-memory index reconstructed from the store's already-validated
//! records. The prior `history.jsonl` format remains opaque reset evidence.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Task 10 establishes this public-to-history boundary before Task 11 connects
// it to storage; until then no runtime caller constructs canonical records.
#[allow(dead_code)]
pub mod codec;
pub mod store;

pub type MetricState = codec::StateEntry;

pub const SAMPLE_SECS: u64 = 300;
const MAX_EXPOSITION_LINE_BYTES: usize = 1024 * 1024;
const MAX_EXPOSITION_SERIES: usize = 100_000;
const COMPACT_AFTER_EXPIRED_SAMPLES: usize = 288;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct MetricKey {
    metric: String,
    labels: BTreeMap<String, String>,
}

/// One metric series at one instant. Fields are declared in ASCII order:
/// this type is served inside `/api/dashboard`, where declaration order is
/// the wire order — see the module docs in `src/api.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct MetricValue {
    pub labels: BTreeMap<String, String>,
    pub metric: String,
    pub value: f64,
}

#[derive(Default)]
struct ParsedSnapshot {
    counters: BTreeMap<MetricKey, f64>,
    gauges: BTreeMap<MetricKey, f64>,
    skipped_lines: usize,
}

#[derive(Clone, Copy)]
enum PrometheusType {
    Counter,
    Gauge,
    Histogram,
}

fn parse_exposition(exposition: &str) -> ParsedSnapshot {
    let mut parsed = ParsedSnapshot::default();
    let mut types = BTreeMap::new();
    let mut series = 0;

    for line in exposition.lines() {
        if line.len() > MAX_EXPOSITION_LINE_BYTES {
            parsed.skipped_lines += 1;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if let Some(metadata) = line.strip_prefix("# TYPE ") {
            let mut fields = metadata.split_whitespace();
            let name = fields.next();
            let kind = fields.next();
            if fields.next().is_none() {
                let kind = match kind {
                    Some("counter") => Some(PrometheusType::Counter),
                    Some("gauge") => Some(PrometheusType::Gauge),
                    Some("histogram") => Some(PrometheusType::Histogram),
                    _ => None,
                };
                if let (Some(name), Some(kind)) = (name, kind) {
                    if valid_metric_name(name) {
                        if types.len() < MAX_EXPOSITION_SERIES || types.contains_key(name) {
                            types.insert(name.to_owned(), kind);
                        } else {
                            parsed.skipped_lines += 1;
                        }
                    }
                }
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if series >= MAX_EXPOSITION_SERIES {
            parsed.skipped_lines += 1;
            continue;
        }

        let split_at = line.rfind('}').map_or(0, |index| index + 1);
        let sample_and_value = if split_at == 0 {
            line.split_once(char::is_whitespace)
        } else {
            line[split_at..]
                .split_once(char::is_whitespace)
                .map(|(_, value)| (&line[..split_at], value))
        };
        let Some((series_text, value_text)) = sample_and_value else {
            parsed.skipped_lines += 1;
            continue;
        };
        let value_text = value_text.trim_start();
        if value_text.is_empty() || value_text.chars().any(char::is_whitespace) {
            parsed.skipped_lines += 1;
            continue;
        }
        let Ok(value) = value_text.parse::<f64>() else {
            parsed.skipped_lines += 1;
            continue;
        };
        if !value.is_finite() {
            parsed.skipped_lines += 1;
            continue;
        }
        let Some(key) = parse_metric_key(series_text) else {
            parsed.skipped_lines += 1;
            continue;
        };

        series += 1;
        if is_counter_like(&key.metric, &types) {
            parsed.counters.insert(key, value);
        } else {
            parsed.gauges.insert(key, value);
        }
    }

    parsed
}

fn valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

fn valid_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_metric_key(series: &str) -> Option<MetricKey> {
    let Some(open) = series.find('{') else {
        return valid_metric_name(series).then(|| MetricKey {
            metric: series.to_owned(),
            labels: BTreeMap::new(),
        });
    };
    if !series.ends_with('}') {
        return None;
    }
    let metric = &series[..open];
    if !valid_metric_name(metric) {
        return None;
    }
    let labels = parse_labels(&series[open + 1..series.len() - 1])?;
    Some(MetricKey {
        metric: metric.to_owned(),
        labels,
    })
}

fn parse_labels(input: &str) -> Option<BTreeMap<String, String>> {
    let mut chars = input.chars().peekable();
    let mut labels = BTreeMap::new();

    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Some(labels);
        }

        let mut name = String::new();
        while let Some(c) = chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '_') {
            name.push(c);
        }
        if !valid_label_name(&name) {
            return None;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next() != Some('=') {
            return None;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next() != Some('"') {
            return None;
        }

        let mut value = String::new();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    '"' => value.push('"'),
                    '\\' => value.push('\\'),
                    'n' => value.push('\n'),
                    _ => return None,
                },
                c => value.push(c),
            }
        }
        if labels.insert(name, value).is_some() {
            return None;
        }

        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        match chars.next() {
            Some(',') => {}
            None => return Some(labels),
            _ => return None,
        }
    }
}

fn is_counter_like(metric: &str, types: &BTreeMap<String, PrometheusType>) -> bool {
    if let Some(kind) = types.get(metric) {
        return matches!(kind, PrometheusType::Counter | PrometheusType::Histogram);
    }
    for suffix in ["_bucket", "_count", "_sum"] {
        if let Some(base) = metric.strip_suffix(suffix) {
            if matches!(types.get(base), Some(PrometheusType::Histogram)) {
                return true;
            }
        }
    }
    metric.ends_with("_total")
        || metric.ends_with("_bucket")
        || metric.ends_with("_count")
        || metric.ends_with("_sum")
}

#[derive(Default)]
struct NormalizedMetrics {
    deltas: BTreeMap<MetricKey, f64>,
    gauges: BTreeMap<MetricKey, f64>,
    inferred_reset: bool,
}

fn normalize(
    previous: Option<&ParsedSnapshot>,
    current: &ParsedSnapshot,
    reset: bool,
) -> NormalizedMetrics {
    let inferred_reset = !reset
        && previous.is_some_and(|previous| {
            current.counters.iter().any(|(key, current_value)| {
                previous
                    .counters
                    .get(key)
                    .is_some_and(|previous_value| current_value - previous_value < -f64::EPSILON)
            })
        });
    let reset_epoch = reset || inferred_reset;
    let mut deltas = BTreeMap::new();

    for (key, current_value) in &current.counters {
        let delta = if reset_epoch {
            *current_value
        } else {
            previous
                .and_then(|snapshot| snapshot.counters.get(key))
                .map_or(*current_value, |previous_value| {
                    let delta = current_value - previous_value;
                    if (-f64::EPSILON..0.0).contains(&delta) {
                        0.0
                    } else {
                        delta
                    }
                })
        };
        deltas.insert(key.clone(), delta);
    }

    NormalizedMetrics {
        deltas,
        gauges: current.gauges.clone(),
        inferred_reset,
    }
}

fn canonical_capacity(capacity: &CapacitySnapshot) -> codec::Capacity {
    codec::Capacity {
        capacity_rpm: capacity.capacity_rpm,
        enabled_keys: capacity.enabled_lanes,
        key_rpms: capacity.rpms.clone(),
    }
}

fn canonical_state(snapshot: &ParsedSnapshot) -> Vec<MetricState> {
    let mut state = Vec::with_capacity(snapshot.counters.len() + snapshot.gauges.len());
    for (key, value) in &snapshot.counters {
        state.push(MetricState {
            kind: codec::StateKind::Counter,
            metric: key.metric.clone(),
            labels: key.labels.clone(),
            value: *value,
        });
    }
    for (key, value) in &snapshot.gauges {
        state.push(MetricState {
            kind: codec::StateKind::Gauge,
            metric: key.metric.clone(),
            labels: key.labels.clone(),
            value: *value,
        });
    }
    state
}

fn parsed_canonical_state(state: &[MetricState]) -> ParsedSnapshot {
    let mut parsed = ParsedSnapshot::default();
    for entry in state {
        let key = MetricKey {
            metric: entry.metric.clone(),
            labels: entry.labels.clone(),
        };
        match entry.kind {
            codec::StateKind::Counter => {
                parsed.counters.insert(key, entry.value);
            }
            codec::StateKind::Gauge => {
                parsed.gauges.insert(key, entry.value);
            }
        }
    }
    parsed
}

fn ingest_canonical_sample(
    inner: &mut HistoryInner,
    timestamp: u64,
    boot_id: String,
    capacity: codec::Capacity,
    current: ParsedSnapshot,
    cutoff: Option<u64>,
) {
    let reset =
        inner.last_parsed.is_some() && inner.last_sample_boot.as_deref() != Some(boot_id.as_str());
    let normalized = normalize(inner.last_parsed.as_ref(), &current, reset);
    inner.diagnostics.valid_samples += 1;
    inner.diagnostics.normalized_series += normalized.deltas.len() + normalized.gauges.len();
    if cutoff.is_none_or(|cutoff| timestamp >= cutoff) {
        inner.points.push(IndexedPoint {
            t: timestamp,
            deltas: normalized.deltas,
            gauges: normalized.gauges,
            capacity: Some(CapacitySnapshot {
                enabled_lanes: capacity.enabled_keys,
                rpms: capacity.key_rpms,
                capacity_rpm: capacity.capacity_rpm,
            }),
        });
    }
    inner.revision = inner.revision.wrapping_add(1);
    inner.last_parsed = Some(current);
    inner.last_sample_boot = Some(boot_id);
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapacitySnapshot {
    pub enabled_lanes: usize,
    pub rpms: Vec<usize>,
    pub capacity_rpm: usize,
}

struct IndexedPoint {
    t: u64,
    deltas: BTreeMap<MetricKey, f64>,
    gauges: BTreeMap<MetricKey, f64>,
    capacity: Option<CapacitySnapshot>,
}

#[derive(Default)]
struct HistoryInner {
    points: Vec<IndexedPoint>,
    revision: u64,
    available_from: Option<u64>,
    available_to: Option<u64>,
    diagnostics: HistoryDiagnostics,
    last_parsed: Option<ParsedSnapshot>,
    last_sample_boot: Option<String>,
    recovery_gaps: Vec<store::RecoveryGap>,
    recovery_events: Vec<store::DiagnosticEvent>,
    recovery_events_complete: bool,
}

/// ASCII-ordered: served inside `/api/dashboard` (see `src/api.rs`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, ToSchema)]
pub struct HistoryDiagnostics {
    pub excluded_epochs: usize,
    pub excluded_records: usize,
    pub normalized_series: usize,
    pub skipped_metric_lines: usize,
    pub valid_checkpoints: usize,
    pub valid_samples: usize,
}

pub struct HistoryWindow<T> {
    pub complete: bool,
    pub data: T,
    pub diagnostics: HistoryDiagnostics,
}

fn diagnostics_at(inner: &HistoryInner, to: u64) -> HistoryDiagnostics {
    if !inner.recovery_events_complete {
        return inner.diagnostics.clone();
    }
    inner
        .recovery_events
        .iter()
        .filter(|event| event.at <= to)
        .fold(HistoryDiagnostics::default(), |mut total, event| {
            total.excluded_epochs += event.diagnostics.excluded_epochs;
            total.excluded_records += event.diagnostics.excluded_records;
            total.normalized_series += event.diagnostics.normalized_series;
            total.skipped_metric_lines += event.diagnostics.skipped_metric_lines;
            total.valid_checkpoints += event.diagnostics.valid_checkpoints;
            total.valid_samples += event.diagnostics.valid_samples;
            total
        })
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryStatus {
    pub available_from: Option<u64>,
    pub available_to: Option<u64>,
    pub compaction_pending: bool,
    pub file_bytes: u64,
    pub persistence: String,
}

/// ASCII-ordered: served inside `/api/dashboard` (see `src/api.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct CapacityRollup {
    pub average_rpm: f64,
    pub latest_rpms: Vec<usize>,
}

/// ASCII-ordered: served inside `/api/dashboard` (see `src/api.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct RollupPoint {
    /// Absent for a bucket sampled before capacity was recorded.
    pub capacity: Option<CapacityRollup>,
    pub duration_seconds: u64,
    pub from: u64,
    pub to: u64,
    pub values: Vec<MetricValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rollup {
    pub available_from: Option<u64>,
    pub available_to: Option<u64>,
    pub effective_from: Option<u64>,
    pub effective_to: Option<u64>,
    pub totals: Vec<MetricValue>,
    pub latest: Vec<MetricValue>,
    pub points: Vec<RollupPoint>,
    pub diagnostics: HistoryDiagnostics,
    pub history_revision: u64,
}

/// ASCII-ordered: served inside `/api/dashboard/now` (see `src/api.rs`).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Tail {
    pub base_history_revision: u64,
    pub from: Option<u64>,
    pub to: u64,
    pub totals: Vec<MetricValue>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CurrentMetrics {
    pub available_from: Option<u64>,
    pub available_to: Option<u64>,
    pub metrics: Vec<MetricValue>,
    pub tail: Tail,
}

#[derive(Clone)]
struct RawPoint {
    t: u64,
    m: String,
    boot: Option<String>,
    capacity: Option<CapacitySnapshot>,
}

#[derive(Deserialize)]
struct StoredRecord {
    v: Option<u8>,
    t: Option<u64>,
    boot: Option<String>,
    kind: Option<String>,
    capacity: Option<CapacitySnapshot>,
    m: Option<String>,
}

enum LoadedRecord {
    Boot { t: u64 },
    Sample(RawPoint),
}

impl LoadedRecord {
    fn timestamp(&self) -> u64 {
        match self {
            Self::Boot { t, .. } => *t,
            Self::Sample(point) => point.t,
        }
    }
}

#[derive(Serialize)]
struct BootRecord<'a> {
    v: u8,
    t: u64,
    boot: &'a str,
    kind: &'static str,
    capacity: &'a CapacitySnapshot,
}

#[derive(Serialize)]
struct SampleRecord<'a> {
    v: u8,
    t: u64,
    boot: &'a str,
    capacity: &'a CapacitySnapshot,
    m: &'a str,
}

enum CompactionOutcome {
    Durable,
    CommittedSyncPending(std::io::Error),
}

struct CompactionControl {
    generation: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompactionTestPoint {
    CanonicalReplayCaptured,
    BeforeCanonicalPendingClear,
}

#[cfg(test)]
type CompactionTestHook = Arc<dyn Fn(CompactionTestPoint) + Send + Sync>;

pub struct History {
    inner: Mutex<HistoryInner>,
    file: Option<PathBuf>,
    /// Retention in days (0 = keep forever). Atomic so the settings layer can
    /// retune it live; the sampler reads it on every append.
    days: AtomicU64,
    dropped_since_compact: Mutex<usize>,
    filesystem: Mutex<()>,
    compaction_pending: AtomicBool,
    compaction_running: AtomicBool,
    compaction_cutoff: AtomicU64,
    compaction_generation: AtomicU64,
    compaction_control: Mutex<CompactionControl>,
    boot_id: String,
    boot_t: u64,
    initial_capacity: CapacitySnapshot,
    canonical: Option<Mutex<store::HistoryStore>>,
    #[cfg(test)]
    compaction_test_hook: Mutex<Option<CompactionTestHook>>,
}

impl History {
    /// Open the canonical store before any listener exists. The legacy reader
    /// is intentionally not consulted here: `history.jsonl` remains opaque
    /// reset evidence, not input to the new format.
    pub fn open(
        dir: PathBuf,
        days: u64,
        initial_capacity: CapacitySnapshot,
    ) -> Result<Self, store::OpenError> {
        let timestamp = unix_now();
        let capacity = canonical_capacity(&initial_capacity);
        let cutoff = (days > 0).then(|| timestamp.saturating_sub(days.saturating_mul(86_400)));
        if let Ok(count) = store::stale_temporary_count(&dir) {
            if count > 0 {
                tracing::warn!("stale canonical history temporaries: count={count}");
            }
        }
        let mut canonical = store::HistoryStore::open(&dir, timestamp, capacity)?;
        let compaction_pending = cutoff.is_some_and(|cutoff| canonical.compaction_needed(cutoff));
        let legacy = dir.join("history.jsonl");
        if let Ok(metadata) = fs::metadata(&legacy) {
            tracing::warn!(
                "legacy history ignored: path={} bytes={}",
                legacy.display(),
                metadata.len()
            );
        }
        let mut history = Self::load(None, days, initial_capacity);
        let replay = canonical.take_replay();
        history.load_canonical(&replay.records, cutoff);
        {
            let mut inner = history.inner.lock().unwrap();
            inner.diagnostics = replay.diagnostics;
            inner.recovery_gaps = replay.gaps;
            inner.recovery_events = replay.diagnostic_events;
            inner.recovery_events_complete = true;
        }
        history.boot_id = canonical.boot_id().to_owned();
        history.boot_t = timestamp;
        history.canonical = Some(Mutex::new(canonical));
        history
            .compaction_pending
            .store(compaction_pending, Ordering::SeqCst);
        history
            .compaction_cutoff
            .store(cutoff.unwrap_or(0), Ordering::SeqCst);
        history
            .compaction_generation
            .store(u64::from(compaction_pending), Ordering::SeqCst);
        history.compaction_control.lock().unwrap().generation = u64::from(compaction_pending);
        Ok(history)
    }

    #[cfg(test)]
    fn invoke_compaction_test_hook(&self, point: CompactionTestPoint) {
        let hook = self.compaction_test_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }

    fn load_canonical(&mut self, records: &[codec::Record], cutoff: Option<u64>) {
        let mut current_boot = None;
        let mut current_state = None;
        let mut inner = self.inner.lock().unwrap();

        for record in records {
            match record {
                codec::Record::Boot(boot) => {
                    current_boot = Some(boot.boot_id.clone());
                    current_state = None;
                }
                codec::Record::Sample(sample) => {
                    let parsed = parsed_canonical_state(&sample.state);
                    ingest_canonical_sample(
                        &mut inner,
                        sample.timestamp,
                        sample.boot_id.clone(),
                        sample.capacity.clone(),
                        parsed,
                        cutoff,
                    );
                    current_boot = Some(sample.boot_id.clone());
                    current_state = Some(sample.state.clone());
                }
                codec::Record::Checkpoint(checkpoint)
                    if current_boot.as_deref() == Some(checkpoint.boot_id.as_str()) =>
                {
                    if let Some(state) = &current_state {
                        let parsed = parsed_canonical_state(state);
                        ingest_canonical_sample(
                            &mut inner,
                            checkpoint.timestamp,
                            checkpoint.boot_id.clone(),
                            checkpoint.capacity.clone(),
                            parsed,
                            cutoff,
                        );
                    }
                }
                codec::Record::Checkpoint(_) => {}
            }
        }
        inner.available_from = inner.points.first().map(|point| point.t);
        inner.available_to = inner.points.last().map(|point| point.t);
    }

    pub fn load(dir: Option<PathBuf>, days: u64, initial_capacity: CapacitySnapshot) -> Self {
        let started = std::time::Instant::now();
        let boot_t = unix_now();
        let cutoff = (days > 0).then(|| boot_t.saturating_sub(days.saturating_mul(86_400)));
        let mut source_bytes = 0;
        let mut diagnostics = HistoryDiagnostics::default();
        let mut records = Vec::new();

        let file = dir.and_then(|d| {
            if let Err(e) = fs::create_dir_all(&d) {
                tracing::warn!("history disabled: cannot create {}: {e}", d.display());
                return None;
            }
            let path = d.join("history.jsonl");
            if let Ok(f) = fs::File::open(&path) {
                source_bytes = f.metadata().map_or(0, |metadata| metadata.len());
                for line in std::io::BufReader::new(f).lines() {
                    let Ok(line) = line else {
                        diagnostics.excluded_records += 1;
                        break;
                    };
                    let Ok(record) = serde_json::from_str::<StoredRecord>(&line) else {
                        diagnostics.excluded_records += 1;
                        continue;
                    };
                    match decode_record(record) {
                        Some(record) => records.push(record),
                        None => diagnostics.excluded_records += 1,
                    }
                }
            }
            // Verify writability up front so we warn once at boot, not on
            // every sample.
            match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(_) => Some(path),
                Err(e) => {
                    tracing::warn!(
                        "history persistence disabled ({}: {e}); keeping in-memory only",
                        path.display()
                    );
                    None
                }
            }
        });

        records.sort_by_key(LoadedRecord::timestamp);
        let mut indexed_points = Vec::new();
        let mut last_parsed: Option<ParsedSnapshot> = None;
        let mut last_sample_boot: Option<String> = None;
        let mut filtered_by_retention = 0;
        for record in records {
            let LoadedRecord::Sample(point) = record else {
                continue;
            };
            let current = parse_exposition(&point.m);
            let explicit_reset = last_parsed.is_some() && point.boot != last_sample_boot;
            let legacy_empty_reset = !explicit_reset
                && point.boot.is_none()
                && last_sample_boot.is_none()
                && last_parsed
                    .as_ref()
                    .is_some_and(|previous| previous.counters.is_empty())
                && !current.counters.is_empty();
            let mut normalized = normalize(
                last_parsed.as_ref(),
                &current,
                explicit_reset || legacy_empty_reset,
            );
            if legacy_empty_reset {
                normalized.inferred_reset = true;
            }
            diagnostics.valid_samples += 1;
            diagnostics.skipped_metric_lines += current.skipped_lines;
            diagnostics.normalized_series += normalized.deltas.len() + normalized.gauges.len();
            let retained = cutoff.is_none_or(|cutoff| point.t >= cutoff);
            if retained {
                indexed_points.push(IndexedPoint {
                    t: point.t,
                    deltas: normalized.deltas,
                    gauges: normalized.gauges,
                    capacity: point.capacity.clone(),
                });
            } else {
                filtered_by_retention += 1;
            }
            last_parsed = Some(current);
            last_sample_boot = point.boot.clone();
        }

        let available_from = indexed_points.first().map(|point| point.t);
        let available_to = indexed_points.last().map(|point| point.t);
        let revision = diagnostics.valid_samples as u64;
        let boot_id = new_boot_id();
        let compaction_pending = file.is_some() && filtered_by_retention > 0;
        let history = Self {
            inner: Mutex::new(HistoryInner {
                points: indexed_points,
                revision,
                available_from,
                available_to,
                diagnostics,
                last_parsed,
                last_sample_boot,
                recovery_gaps: Vec::new(),
                recovery_events: Vec::new(),
                recovery_events_complete: false,
            }),
            file,
            days: AtomicU64::new(days),
            dropped_since_compact: Mutex::new(filtered_by_retention),
            filesystem: Mutex::new(()),
            compaction_pending: AtomicBool::new(compaction_pending),
            compaction_running: AtomicBool::new(false),
            compaction_cutoff: AtomicU64::new(cutoff.unwrap_or(0)),
            compaction_generation: AtomicU64::new(u64::from(compaction_pending)),
            compaction_control: Mutex::new(CompactionControl {
                generation: u64::from(compaction_pending),
            }),
            boot_id,
            boot_t,
            initial_capacity,
            canonical: None,
            #[cfg(test)]
            compaction_test_hook: Mutex::new(None),
        };
        history.persist_boot_marker();
        let inner = history.inner.lock().unwrap();
        tracing::info!(
            "history           {} bytes, {} samples, {} skipped records, {} skipped metric lines, \
             {} normalized series, {} inferred resets, indexed in {:?}; retention {}",
            source_bytes,
            inner.diagnostics.valid_samples,
            inner.diagnostics.excluded_records,
            inner.diagnostics.skipped_metric_lines,
            inner.diagnostics.normalized_series,
            inner.diagnostics.excluded_epochs,
            started.elapsed(),
            if days == 0 {
                "infinite".to_owned()
            } else {
                format!("{days} days")
            }
        );
        drop(inner);
        history
    }

    /// Retune retention live. Visible queries prune synchronously; durable
    /// compaction is serialized with appends and runs off the async executor.
    pub fn reconfigure_retention(self: &Arc<Self>, days: u64, now: u64) {
        if days == 0 {
            let mut control = self.compaction_control.lock().unwrap();
            self.days.store(0, Ordering::Relaxed);
            let generation = self
                .compaction_generation
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1);
            control.generation = generation;
            self.compaction_pending.store(false, Ordering::SeqCst);
            return;
        }
        self.days.store(days, Ordering::Relaxed);

        let cutoff = now.saturating_sub(days.saturating_mul(86_400));
        let mut inner = self.inner.lock().unwrap();
        let before = inner.points.len();
        inner.points.retain(|point| point.t >= cutoff);
        let removed = before - inner.points.len();
        if removed > 0 {
            *self.dropped_since_compact.lock().unwrap() += removed;
            inner.revision = inner.revision.wrapping_add(1);
        }
        inner.available_from = inner.points.first().map(|point| point.t);
        inner.available_to = inner.points.last().map(|point| point.t);
        drop(inner);

        if self.canonical.is_some() || self.file.is_some() {
            self.request_compaction(cutoff);
        }
    }

    pub fn revision(&self) -> u64 {
        self.inner.lock().unwrap().revision
    }

    pub fn status(&self) -> HistoryStatus {
        let (available_from, available_to) = {
            let inner = self.inner.lock().unwrap();
            (inner.available_from, inner.available_to)
        };
        let compaction_pending = self.compaction_pending.load(Ordering::SeqCst);
        let (file_bytes, persistence) = self.canonical.as_ref().map_or_else(
            || {
                (
                    self.file
                        .as_ref()
                        .and_then(|path| fs::metadata(path).ok())
                        .map_or(0, |metadata| metadata.len()),
                    "degraded".to_owned(),
                )
            },
            |store| {
                let store = store.lock().unwrap();
                (
                    store.file_bytes().unwrap_or(0),
                    if store.poisoned() {
                        "degraded".to_owned()
                    } else {
                        "ok".to_owned()
                    },
                )
            },
        );
        HistoryStatus {
            available_from,
            available_to,
            compaction_pending,
            file_bytes,
            persistence,
        }
    }

    pub fn append(self: &Arc<Self>, t: u64, snapshot: &str, capacity: CapacitySnapshot) {
        let canonical_last = self
            .canonical
            .as_ref()
            .and_then(|store| store.lock().unwrap().last_timestamp());
        let current = parse_exposition(snapshot);
        let canonical_state = canonical_state(&current);
        let mut inner = self.inner.lock().unwrap();
        // Wall clocks can move backward, and a retained file can contain a
        // future-dated point. Keep ingestion order without fabricating time:
        // equal timestamps preserve binary-search ordering and exact deltas.
        let t = [Some(t), inner.available_to, canonical_last]
            .into_iter()
            .flatten()
            .max()
            .expect("the input timestamp is always present");
        let explicit_reset = inner.last_parsed.is_some()
            && inner.last_sample_boot.as_deref() != Some(self.boot_id.as_str());
        let normalized = normalize(inner.last_parsed.as_ref(), &current, explicit_reset);
        let diagnostics = HistoryDiagnostics {
            normalized_series: normalized.deltas.len() + normalized.gauges.len(),
            skipped_metric_lines: current.skipped_lines,
            valid_samples: 1,
            ..HistoryDiagnostics::default()
        };
        inner.diagnostics.valid_samples += diagnostics.valid_samples;
        inner.diagnostics.skipped_metric_lines += diagnostics.skipped_metric_lines;
        inner.diagnostics.normalized_series += diagnostics.normalized_series;
        if inner.recovery_events_complete {
            inner
                .recovery_events
                .push(store::DiagnosticEvent { at: t, diagnostics });
        }

        let days = self.days.load(Ordering::Relaxed);
        if days > 0 {
            let cutoff = t.saturating_sub(days.saturating_mul(86_400));
            let before = inner.points.len();
            inner.points.retain(|point| point.t >= cutoff);
            *self.dropped_since_compact.lock().unwrap() += before - inner.points.len();
        }
        inner.points.push(IndexedPoint {
            t,
            deltas: normalized.deltas,
            gauges: normalized.gauges,
            capacity: Some(capacity.clone()),
        });
        inner.revision = inner.revision.wrapping_add(1);
        inner.available_from = inner.points.first().map(|point| point.t);
        inner.available_to = inner.points.last().map(|point| point.t);
        inner.last_parsed = Some(current);
        inner.last_sample_boot = Some(self.boot_id.clone());
        drop(inner);

        if self.file.is_some() {
            let line = serde_json::to_string(&SampleRecord {
                v: 2,
                t,
                boot: &self.boot_id,
                capacity: &capacity,
                m: snapshot,
            })
            .expect("history sample record serializes");
            self.persist_sample(&line);
        }

        if let Some(store) = &self.canonical {
            let mut store = store.lock().unwrap();
            let was_poisoned = store.poisoned();
            if let Err(error) =
                store.append_sample(t, canonical_capacity(&capacity), canonical_state)
            {
                if !was_poisoned && store.poisoned() {
                    metrics::gauge!("nimproxy_history_persistence_degraded").set(1.0);
                    tracing::error!("canonical history persistence degraded: {error}");
                } else if !matches!(error, store::WriteError::Poisoned) {
                    tracing::warn!("canonical history append failed: {error}");
                }
            }
        }

        if days > 0 && self.canonical.is_some() {
            let cutoff = t.saturating_sub(days.saturating_mul(86_400));
            if self.compaction_pending.load(Ordering::SeqCst)
                || *self.dropped_since_compact.lock().unwrap() > COMPACT_AFTER_EXPIRED_SAMPLES
            {
                self.request_compaction(cutoff);
            }
        }

        if self.file.is_some() {
            if self.compaction_pending.load(Ordering::SeqCst) {
                self.start_pending_compaction();
            } else if days > 0
                && *self.dropped_since_compact.lock().unwrap() > COMPACT_AFTER_EXPIRED_SAMPLES
            {
                self.request_compaction(t.saturating_sub(days.saturating_mul(86_400)));
            }
        }
    }

    pub fn rollup(&self, from: u64, to: u64, points: usize) -> HistoryWindow<Rollup> {
        let inner = self.inner.lock().unwrap();
        let diagnostics = diagnostics_at(&inner, to);
        let empty = || Rollup {
            available_from: inner.available_from,
            available_to: inner.available_to,
            effective_from: None,
            effective_to: None,
            totals: Vec::new(),
            latest: Vec::new(),
            points: Vec::new(),
            diagnostics: diagnostics.clone(),
            history_revision: inner.revision,
        };
        let incomplete = |inner: &HistoryInner| {
            from >= to
                || inner.points.is_empty()
                || inner
                    .recovery_gaps
                    .iter()
                    .any(|gap| gap.from < to && from < gap.to)
        };
        if from >= to {
            let data = empty();
            return HistoryWindow {
                complete: false,
                data,
                diagnostics,
            };
        }

        let first = inner.points.partition_point(|point| point.t <= from);
        let end = inner.points.partition_point(|point| point.t <= to);
        if first == end {
            let data = empty();
            return HistoryWindow {
                complete: false,
                data,
                diagnostics,
            };
        }

        let point_budget = points.clamp(2, 1000);
        let mut totals = BTreeMap::new();
        let mut latest = BTreeMap::new();
        let mut buckets: Vec<BucketAccumulator> = (0..point_budget)
            .map(|_| BucketAccumulator::default())
            .collect();

        for index in first..end {
            let point = &inner.points[index];
            add_metrics(&mut totals, &point.deltas);
            latest.extend(point.gauges.clone());

            let offset = point.t.saturating_sub(from) as u128;
            let width = to.saturating_sub(from) as u128;
            let bucket_index = ((offset * point_budget as u128) / width) as usize;
            let bucket_index = bucket_index.min(point_budget - 1);
            let bucket = &mut buckets[bucket_index];
            add_metrics(&mut bucket.deltas, &point.deltas);
            bucket.gauges.extend(point.gauges.clone());

            let interval_start = if index == 0 {
                // The first retained sample provides no evidence about the
                // preceding interval. Keep its exact delta, but do not invent
                // duration or capacity before history begins.
                point.t.max(from)
            } else {
                inner.points[index - 1].t.max(from)
            };
            let interval_end = point.t.min(to);
            let duration = interval_end.saturating_sub(interval_start);
            bucket.from = Some(
                bucket
                    .from
                    .map_or(interval_start, |current| current.min(interval_start)),
            );
            bucket.to = Some(
                bucket
                    .to
                    .map_or(interval_end, |current| current.max(interval_end)),
            );
            bucket.duration_seconds += duration;
            bucket.capacity_duration += duration;
            if let Some(capacity) = &point.capacity {
                bucket.capacity_weight += capacity.capacity_rpm as f64 * duration as f64;
                bucket.known_capacity_duration += duration;
                bucket.latest_rpms = capacity.rpms.clone();
            }
        }

        let effective_from = if first == 0 {
            Some(inner.points[first].t)
        } else {
            Some(from.max(inner.points[first - 1].t))
        };
        let effective_to = Some(inner.points[end - 1].t);
        let points = buckets
            .into_iter()
            .filter_map(BucketAccumulator::finish)
            .collect();

        let data = Rollup {
            available_from: inner.available_from,
            available_to: inner.available_to,
            effective_from,
            effective_to,
            totals: metric_values(totals),
            latest: metric_values(latest),
            points,
            diagnostics: diagnostics.clone(),
            history_revision: inner.revision,
        };
        HistoryWindow {
            complete: !incomplete(&inner),
            data,
            diagnostics,
        }
    }

    pub fn current(&self, t: u64, render: impl FnOnce() -> String) -> CurrentMetrics {
        let inner = self.inner.lock().unwrap();
        let current_exposition = render();
        let current = parse_exposition(&current_exposition);
        let reset = inner.last_parsed.is_some()
            && inner.last_sample_boot.as_deref() != Some(self.boot_id.as_str());
        let normalized = normalize(inner.last_parsed.as_ref(), &current, reset);
        let mut metrics = current.counters.clone();
        metrics.extend(current.gauges);

        CurrentMetrics {
            available_from: inner.available_from,
            available_to: inner.available_to,
            metrics: metric_values(metrics),
            tail: Tail {
                base_history_revision: inner.revision,
                from: inner.available_to,
                to: t,
                totals: metric_values(normalized.deltas),
            },
        }
    }

    fn persist_boot_marker(&self) {
        let Some(path) = &self.file else {
            return;
        };
        let line = serde_json::to_string(&BootRecord {
            v: 2,
            t: self.boot_t,
            boot: &self.boot_id,
            kind: "boot",
            capacity: &self.initial_capacity,
        })
        .expect("history boot record serializes");
        let _filesystem = self.filesystem.lock().unwrap();
        if let Err(e) = append_line(path, &line) {
            tracing::warn!("history boot marker write failed: {e}");
        }
    }

    fn persist_sample(&self, line: &str) {
        let Some(path) = &self.file else {
            return;
        };
        let _filesystem = self.filesystem.lock().unwrap();
        if let Err(e) = append_line(path, line) {
            tracing::warn!("history write failed: {e}");
        }
    }

    fn request_compaction(self: &Arc<Self>, cutoff: u64) {
        {
            let mut control = self.compaction_control.lock().unwrap();
            self.compaction_cutoff.store(cutoff, Ordering::SeqCst);
            let generation = self
                .compaction_generation
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1);
            control.generation = generation;
            self.compaction_pending.store(true, Ordering::SeqCst);
        }
        self.start_pending_compaction();
    }

    fn start_pending_compaction(self: &Arc<Self>) {
        if !self.compaction_pending.load(Ordering::SeqCst)
            || self
                .compaction_running
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }

        let history = self.clone();
        let generation = history.compaction_generation.load(Ordering::SeqCst);
        let cutoff = history.compaction_cutoff.load(Ordering::SeqCst);
        let claimed_drops = *history.dropped_since_compact.lock().unwrap();
        tokio::task::spawn_blocking(move || {
            if let Some(canonical) = &history.canonical {
                let inner_revision = history.inner.lock().unwrap().revision;
                let (result, replay) = {
                    let mut store = canonical.lock().unwrap();
                    let result = store.compact_if_authorized(cutoff, || {
                        let control = history.compaction_control.lock().unwrap();
                        (control.generation == generation
                            && history.compaction_generation.load(Ordering::SeqCst) == generation
                            && history.compaction_pending.load(Ordering::SeqCst)
                            && history.days.load(Ordering::Relaxed) > 0)
                            .then_some(control)
                    });
                    let replay = matches!(
                        &result,
                        Ok(store::CompactionOutcome::Durable)
                            | Ok(store::CompactionOutcome::CommittedSyncPending(_))
                    )
                    .then(|| store.take_replay());
                    (result, replay)
                };
                let mut durable = false;
                let mut superseded = false;
                let mut sync_pending = false;
                match result {
                    Ok(store::CompactionOutcome::Durable) => {
                        #[cfg(test)]
                        history.invoke_compaction_test_hook(
                            CompactionTestPoint::CanonicalReplayCaptured,
                        );
                        history.install_canonical_replay_if_unchanged(
                            replay.expect("committed canonical replacement has replay"),
                            inner_revision,
                        );
                        durable = true;
                        let mut dropped = history.dropped_since_compact.lock().unwrap();
                        *dropped = dropped.saturating_sub(claimed_drops);
                        drop(dropped);
                        if history.compaction_generation.load(Ordering::SeqCst) == generation {
                            #[cfg(test)]
                            history.invoke_compaction_test_hook(
                                CompactionTestPoint::BeforeCanonicalPendingClear,
                            );
                            history.compaction_pending.store(false, Ordering::SeqCst);
                            if history.compaction_generation.load(Ordering::SeqCst) != generation
                                && history.days.load(Ordering::Relaxed) > 0
                            {
                                history.compaction_pending.store(true, Ordering::SeqCst);
                            }
                        }
                    }
                    Ok(store::CompactionOutcome::Deferred) => {}
                    Ok(store::CompactionOutcome::Superseded) => {
                        superseded = true;
                    }
                    Ok(store::CompactionOutcome::CommittedSyncPending(error)) => {
                        sync_pending = true;
                        #[cfg(test)]
                        history.invoke_compaction_test_hook(
                            CompactionTestPoint::CanonicalReplayCaptured,
                        );
                        history.install_canonical_replay_if_unchanged(
                            replay.expect("committed canonical replacement has replay"),
                            inner_revision,
                        );
                        tracing::warn!(
                            "canonical history compaction was atomically renamed, but directory sync failed; \
                             durability is uncertain and cleanup remains pending: {error}"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            "canonical history compaction failed before replacement; original path is unchanged: \
                             {error}"
                        );
                    }
                }

                history.compaction_running.store(false, Ordering::SeqCst);
                let newer_sync_pending = sync_pending
                    && history.compaction_generation.load(Ordering::SeqCst) != generation
                    && history.days.load(Ordering::Relaxed) > 0
                    && history.compaction_pending.load(Ordering::SeqCst);
                if (durable || superseded || newer_sync_pending)
                    && history.compaction_pending.load(Ordering::SeqCst)
                {
                    history.start_pending_compaction();
                }
                return;
            }

            let result = history.compact_file(cutoff);
            let durable = matches!(&result, Ok(CompactionOutcome::Durable));
            match result {
                Ok(CompactionOutcome::Durable) => {
                    let mut inner = history.inner.lock().unwrap();
                    inner.revision = inner.revision.wrapping_add(1);
                    drop(inner);
                    let mut dropped = history.dropped_since_compact.lock().unwrap();
                    *dropped = dropped.saturating_sub(claimed_drops);
                    drop(dropped);

                    if history.compaction_generation.load(Ordering::SeqCst) == generation {
                        history.compaction_pending.store(false, Ordering::SeqCst);
                        if history.compaction_generation.load(Ordering::SeqCst) != generation {
                            history.compaction_pending.store(true, Ordering::SeqCst);
                        }
                    }
                }
                Ok(CompactionOutcome::CommittedSyncPending(error)) => {
                    tracing::warn!(
                        "history compaction was atomically renamed, but directory sync failed; \
                         durability is uncertain and cleanup remains pending: {error}"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        "history compaction failed before replacement; original path is unchanged: \
                         {error}"
                    );
                }
            }

            history.compaction_running.store(false, Ordering::SeqCst);
            if durable && history.compaction_pending.load(Ordering::SeqCst) {
                history.start_pending_compaction();
            }
        });
    }

    fn install_canonical_replay_if_unchanged(&self, replay: store::Replay, expected_revision: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.revision != expected_revision {
            return;
        }
        inner.diagnostics = replay.diagnostics;
        inner.recovery_gaps = replay.gaps;
        inner.recovery_events = replay.diagnostic_events;
        inner.recovery_events_complete = true;
        inner.revision = inner.revision.wrapping_add(1);
    }

    fn compact_file(&self, cutoff: u64) -> std::io::Result<CompactionOutcome> {
        let Some(path) = &self.file else {
            return Ok(CompactionOutcome::Durable);
        };
        let _filesystem = self.filesystem.lock().unwrap();

        let mut baseline: Option<(u64, String)> = None;
        let mut boot: Option<(u64, String)> = None;
        for line in std::io::BufReader::new(fs::File::open(path)?).lines() {
            let line = line?;
            let Ok(record) = serde_json::from_str::<StoredRecord>(&line) else {
                continue;
            };
            let Some((t, kind)) = persisted_record_kind(&record) else {
                continue;
            };
            if t >= cutoff {
                continue;
            }
            let slot = match kind {
                PersistedRecordKind::Sample => &mut baseline,
                PersistedRecordKind::Boot => &mut boot,
            };
            if slot.as_ref().is_none_or(|(latest, _)| t >= *latest) {
                *slot = Some((t, line));
            }
        }

        let temporary = temporary_path(path);
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&temporary)?;

        let mut prefix = [baseline, boot].into_iter().flatten().collect::<Vec<_>>();
        prefix.sort_by_key(|(t, _)| *t);
        for (_, line) in prefix {
            writeln!(output, "{line}")?;
        }
        for line in std::io::BufReader::new(fs::File::open(path)?).lines() {
            let line = line?;
            let Ok(record) = serde_json::from_str::<StoredRecord>(&line) else {
                continue;
            };
            if persisted_record_kind(&record).is_some_and(|(t, _)| t >= cutoff) {
                writeln!(output, "{line}")?;
            }
        }
        output.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(directory_sync_outcome(sync_parent_directory(path)))
    }
}

#[derive(Default)]
struct BucketAccumulator {
    deltas: BTreeMap<MetricKey, f64>,
    gauges: BTreeMap<MetricKey, f64>,
    from: Option<u64>,
    to: Option<u64>,
    duration_seconds: u64,
    capacity_weight: f64,
    capacity_duration: u64,
    known_capacity_duration: u64,
    latest_rpms: Vec<usize>,
}

impl BucketAccumulator {
    fn finish(self) -> Option<RollupPoint> {
        let from = self.from?;
        let to = self.to?;
        let capacity = (self.capacity_duration > 0
            && self.known_capacity_duration == self.capacity_duration)
            .then(|| CapacityRollup {
                average_rpm: self.capacity_weight / self.capacity_duration as f64,
                latest_rpms: self.latest_rpms,
            });
        let mut values = self.deltas;
        values.extend(self.gauges);
        Some(RollupPoint {
            from,
            to,
            duration_seconds: self.duration_seconds,
            values: metric_values(values),
            capacity,
        })
    }
}

fn add_metrics(totals: &mut BTreeMap<MetricKey, f64>, values: &BTreeMap<MetricKey, f64>) {
    for (key, value) in values {
        *totals.entry(key.clone()).or_default() += value;
    }
}

fn metric_values(values: BTreeMap<MetricKey, f64>) -> Vec<MetricValue> {
    values
        .into_iter()
        .map(|(key, value)| MetricValue {
            metric: key.metric,
            labels: key.labels,
            value,
        })
        .collect()
}

fn decode_record(record: StoredRecord) -> Option<LoadedRecord> {
    let t = record.t?;
    match (record.v, record.kind.as_deref()) {
        (Some(2), Some("boot")) => {
            record.boot?;
            record.capacity?;
            Some(LoadedRecord::Boot { t })
        }
        (Some(2), None) => Some(LoadedRecord::Sample(RawPoint {
            t,
            m: record.m?,
            boot: Some(record.boot?),
            capacity: Some(record.capacity?),
        })),
        (None, None) => Some(LoadedRecord::Sample(RawPoint {
            t,
            m: record.m?,
            boot: None,
            capacity: None,
        })),
        _ => None,
    }
}

fn new_boot_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS RNG for history boot ID");
    let mut id = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut id, "{byte:02x}").expect("writing to String cannot fail");
    }
    id
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn append_line(path: &PathBuf, line: &str) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{line}"))
}

#[derive(Clone, Copy)]
enum PersistedRecordKind {
    Boot,
    Sample,
}

fn persisted_record_kind(record: &StoredRecord) -> Option<(u64, PersistedRecordKind)> {
    let t = record.t?;
    match (record.v, record.kind.as_deref()) {
        (Some(2), Some("boot"))
            if record.boot.is_some() && record.capacity.is_some() && record.m.is_none() =>
        {
            Some((t, PersistedRecordKind::Boot))
        }
        (Some(2), None)
            if record.boot.is_some() && record.capacity.is_some() && record.m.is_some() =>
        {
            Some((t, PersistedRecordKind::Sample))
        }
        (None, None)
            if record.boot.is_none() && record.capacity.is_none() && record.m.is_some() =>
        {
            Some((t, PersistedRecordKind::Sample))
        }
        _ => None,
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

fn directory_sync_outcome(result: std::io::Result<()>) -> CompactionOutcome {
    match result {
        Ok(()) => CompactionOutcome::Durable,
        Err(error) => CompactionOutcome::CommittedSyncPending(error),
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("history file has no parent directory"))?;
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capacity(rpm: usize) -> CapacitySnapshot {
        CapacitySnapshot {
            enabled_lanes: usize::from(rpm > 0),
            rpms: (rpm > 0).then_some(rpm).into_iter().collect(),
            capacity_rpm: rpm,
        }
    }

    fn memory_history(days: u64) -> Arc<History> {
        Arc::new(History::load(None, days, capacity(40)))
    }

    fn history_with_file(days: u64, file: PathBuf) -> Arc<History> {
        Arc::new(History {
            inner: Mutex::new(HistoryInner::default()),
            file: Some(file),
            days: AtomicU64::new(days),
            dropped_since_compact: Mutex::new(0),
            filesystem: Mutex::new(()),
            compaction_pending: AtomicBool::new(false),
            compaction_running: AtomicBool::new(false),
            compaction_cutoff: AtomicU64::new(0),
            compaction_generation: AtomicU64::new(0),
            compaction_control: Mutex::new(CompactionControl { generation: 0 }),
            boot_id: "00000000000000000000000000000000".to_owned(),
            boot_t: 0,
            initial_capacity: capacity(40),
            canonical: None,
            compaction_test_hook: Mutex::new(None),
        })
    }

    const SNAPSHOT: &str = r#"
# TYPE nimproxy_requests_total counter
nimproxy_requests_total{client="Mindmap",model="z-ai/glm-5.2",status="200"} 12
# TYPE nimproxy_active_requests gauge
nimproxy_active_requests 2
# TYPE nimproxy_ttft_seconds histogram
nimproxy_ttft_seconds_bucket{model="z-ai/glm-5.2",le="0.5"} 3
nimproxy_ttft_seconds_bucket{model="z-ai/glm-5.2",le="+Inf"} 4
nimproxy_ttft_seconds_sum{model="z-ai/glm-5.2"} 1.25
nimproxy_ttft_seconds_count{model="z-ai/glm-5.2"} 4
"#;

    #[test]
    fn parses_counter_gauge_and_histogram_series() {
        let parsed = parse_exposition(SNAPSHOT);
        assert_eq!(parsed.counters.len(), 5);
        assert_eq!(parsed.gauges.len(), 1);
        assert_eq!(parsed.skipped_lines, 0);
    }

    #[test]
    fn parses_escaped_label_values() {
        let series = r#"escaped_total{quote="say \"hi\"",path="c:\\tmp",line="a\nb"}"#;
        let parsed = parse_exposition(&format!("# TYPE escaped_total counter\n{series} 1\n"));
        assert_eq!(parsed.skipped_lines, 0);
        let key = parsed.counters.keys().next().unwrap();
        assert_eq!(key.labels["quote"], "say \"hi\"");
        assert_eq!(key.labels["path"], "c:\\tmp");
        assert_eq!(key.labels["line"], "a\nb");
    }

    #[test]
    fn parse_skips_nan_and_counts_malformed_lines() {
        let parsed = parse_exposition(
            "# TYPE a_total counter\n\
             a_total NaN\n\
             missing_value\n\
             bad-name_total 2\n",
        );
        assert!(parsed.counters.is_empty());
        assert_eq!(parsed.skipped_lines, 3);
    }

    #[test]
    fn parses_counter_suffix_without_type_metadata() {
        let parsed = parse_exposition("orphan_total{kind=\"legacy\"} 9\n");
        let (key, value) = parsed.counters.iter().next().unwrap();
        assert_eq!(key.metric, "orphan_total");
        assert_eq!(key.labels["kind"], "legacy");
        assert_eq!(*value, 9.0);
        assert!(parsed.gauges.is_empty());
        assert_eq!(parsed.skipped_lines, 0);
    }

    #[test]
    fn parse_counts_oversized_comments_as_skipped() {
        let exposition = format!(
            "#{}\nrequests_total 1\n",
            "x".repeat(MAX_EXPOSITION_LINE_BYTES)
        );
        let parsed = parse_exposition(&exposition);
        assert_eq!(parsed.skipped_lines, 1);
        assert_eq!(metric(&parsed.counters, "requests_total"), 1.0);
    }

    #[test]
    fn parse_rejects_oversized_type_metadata_before_classification() {
        let exposition = format!(
            "# TYPE forced_total{}gauge\nforced_total 1\n",
            " ".repeat(MAX_EXPOSITION_LINE_BYTES)
        );
        let parsed = parse_exposition(&exposition);
        assert_eq!(parsed.skipped_lines, 1);
        assert_eq!(metric(&parsed.counters, "forced_total"), 1.0);
        assert!(parsed.gauges.is_empty());
    }

    #[test]
    fn parse_caps_the_number_of_retained_series() {
        let mut exposition = String::new();
        for value in 0..=MAX_EXPOSITION_SERIES {
            writeln!(&mut exposition, "capped_total {value}").unwrap();
        }
        let parsed = parse_exposition(&exposition);
        assert_eq!(parsed.skipped_lines, 1);
        assert_eq!(
            metric(&parsed.counters, "capped_total"),
            (MAX_EXPOSITION_SERIES - 1) as f64
        );
    }

    #[test]
    fn parse_caps_retained_type_metadata() {
        let mut exposition = String::new();
        for value in 0..MAX_EXPOSITION_SERIES {
            writeln!(&mut exposition, "# TYPE metric_{value} gauge").unwrap();
        }
        exposition.push_str("# TYPE overflow_total gauge\noverflow_total 1\n");
        let parsed = parse_exposition(&exposition);
        assert_eq!(parsed.skipped_lines, 1);
        assert_eq!(metric(&parsed.counters, "overflow_total"), 1.0);
        assert!(parsed.gauges.is_empty());
    }

    fn metric(values: &BTreeMap<MetricKey, f64>, name: &str) -> f64 {
        values
            .iter()
            .find_map(|(key, value)| (key.metric == name).then_some(*value))
            .unwrap()
    }

    #[test]
    fn explicit_boot_change_counts_new_process_values() {
        let first = parse_exposition("# TYPE requests_total counter\nrequests_total 100\n");
        let second = parse_exposition("# TYPE requests_total counter\nrequests_total 7\n");
        let normalized = normalize(Some(&first), &second, true);
        assert_eq!(metric(&normalized.deltas, "requests_total"), 7.0);
    }

    #[test]
    fn one_legacy_counter_decrease_resets_the_snapshot_epoch() {
        let first = parse_exposition(
            "# TYPE a_total counter\na_total 100\n\
             # TYPE b_total counter\nb_total 20\n",
        );
        let second = parse_exposition(
            "# TYPE a_total counter\na_total 3\n\
             # TYPE b_total counter\nb_total 25\n",
        );
        let normalized = normalize(Some(&first), &second, false);
        assert!(normalized.inferred_reset);
        assert_eq!(metric(&normalized.deltas, "a_total"), 3.0);
        assert_eq!(metric(&normalized.deltas, "b_total"), 25.0);
    }

    #[test]
    fn normalize_subtracts_counters_without_a_reset() {
        let first = parse_exposition("# TYPE requests_total counter\nrequests_total 100\n");
        let second = parse_exposition("# TYPE requests_total counter\nrequests_total 107\n");
        let normalized = normalize(Some(&first), &second, false);
        assert!(!normalized.inferred_reset);
        assert_eq!(metric(&normalized.deltas, "requests_total"), 7.0);
    }

    #[test]
    fn normalize_counts_a_new_counter_from_zero() {
        let first = parse_exposition("# TYPE old_total counter\nold_total 4\n");
        let second = parse_exposition(
            "# TYPE old_total counter\nold_total 6\n\
             # TYPE new_total counter\nnew_total 9\n",
        );
        let normalized = normalize(Some(&first), &second, false);
        assert_eq!(metric(&normalized.deltas, "old_total"), 2.0);
        assert_eq!(metric(&normalized.deltas, "new_total"), 9.0);
    }

    #[test]
    fn normalize_copies_current_gauges() {
        let first = parse_exposition(
            "# TYPE active gauge\nactive 8\n\
             # TYPE requests_total counter\nrequests_total 3\n",
        );
        let second = parse_exposition(
            "# TYPE active gauge\nactive 2\n\
             # TYPE requests_total counter\nrequests_total 4\n",
        );
        let normalized = normalize(Some(&first), &second, false);
        assert_eq!(metric(&normalized.gauges, "active"), 2.0);
        assert_eq!(metric(&normalized.deltas, "requests_total"), 1.0);
    }

    #[test]
    fn retention_prunes_and_rollup_filters() {
        let h = memory_history(1);
        h.append(1_000, "old", capacity(40));
        h.append(200_000, "new", capacity(40)); // 1-day cutoff drops t=1000
        assert_eq!(h.inner.lock().unwrap().points.len(), 1);
        h.append(200_300, "newer", capacity(40));
        let rollup = h.rollup(199_999, 200_100, 100);
        assert_eq!(rollup.data.points.len(), 1);
        assert_eq!(rollup.data.points[0].to, 200_000);
    }

    #[test]
    fn append_saturates_an_extreme_retention_window() {
        let history = memory_history(u64::MAX);
        history.append(
            1,
            "# TYPE requests_total counter\nrequests_total 1\n",
            capacity(40),
        );

        assert_eq!(history.inner.lock().unwrap().points.len(), 1);
    }

    #[tokio::test]
    async fn retention_reconfiguration_prunes_indexed_queries_immediately() {
        let history = memory_history(0);
        history.append(
            1_000,
            "# TYPE requests_total counter\nrequests_total 5\n",
            capacity(40),
        );
        history.append(
            120_000,
            "# TYPE requests_total counter\nrequests_total 9\n",
            capacity(40),
        );
        history.append(
            200_000,
            "# TYPE requests_total counter\nrequests_total 12\n",
            capacity(40),
        );
        let before_revision = history.revision();

        history.clone().reconfigure_retention(1, 200_000);

        let rollup = history.rollup(0, u64::MAX, 100);
        assert_eq!(rollup.data.available_from, Some(120_000));
        assert_eq!(history.status().available_from, Some(120_000));
        assert_eq!(value(&rollup.data.totals, "requests_total"), 7.0);
        assert!(history.revision() > before_revision);
    }

    #[test]
    fn append_clamps_clock_rollback_to_canonical_order_and_persists_sample() {
        let dir = TestDir::new();
        let history = Arc::new(History::open(dir.0.clone(), 0, capacity(40)).unwrap());
        let boot_t = history.boot_t;
        history.append(
            boot_t.saturating_sub(60),
            "# TYPE requests_total counter\nrequests_total 15\n",
            capacity(40),
        );

        let records = fs::read_to_string(dir.0.join("history-v1.jsonl"))
            .unwrap()
            .lines()
            .map(|line| codec::decode_record(line.as_bytes()).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(records.last(), Some(codec::Record::Sample(sample))
            if sample.timestamp == boot_t));
        let timestamps = history
            .inner
            .lock()
            .unwrap()
            .points
            .iter()
            .map(|point| point.t)
            .collect::<Vec<_>>();
        assert_eq!(timestamps, [boot_t]);
        assert!(timestamps.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(history.status().available_to, Some(boot_t));
        let rollup = history.rollup(boot_t.saturating_sub(1), boot_t, 20);
        assert_eq!(value(&rollup.data.totals, "requests_total"), 15.0);
    }

    #[test]
    fn canonical_persistence_health_degrades_on_changed_sample_and_checkpoint_poison() {
        for (label, prime_first) in [("changed", false), ("checkpoint", true)] {
            let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
            let handle = recorder.handle();
            metrics::with_local_recorder(&recorder, || {
                let dir = TestDir::new();
                let history = Arc::new(History::open(dir.0.clone(), 0, capacity(40)).unwrap());
                let timestamp = history.boot_t;
                let path = dir.0.join("history-v1.jsonl");

                assert_eq!(history.status().persistence, "ok", "{label}: fresh store");
                if prime_first {
                    history.append(timestamp, SNAPSHOT, capacity(40));
                }
                let before_failure = fs::read(&path).unwrap();
                history
                    .canonical
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .arm_test_runtime_partial_append_once();

                let logs = Arc::new(Mutex::new(Vec::new()));
                let writer = logs.clone();
                let subscriber = tracing_subscriber::fmt()
                    .with_ansi(false)
                    .without_time()
                    .with_target(false)
                    .with_max_level(tracing::Level::ERROR)
                    .with_writer(move || SharedWriter(writer.clone()))
                    .finish();
                tracing::subscriber::with_default(subscriber, || {
                    history.append(timestamp, SNAPSHOT, capacity(40));
                    assert_eq!(
                        history.status().persistence,
                        "degraded",
                        "{label}: first failed durable append degrades persistence"
                    );
                    assert_eq!(
                        history.inner.lock().unwrap().points.len(),
                        if prime_first { 2 } else { 1 }
                    );
                    let after_failed_tick = fs::read(&path).unwrap();
                    assert!(
                        after_failed_tick.starts_with(&before_failure),
                        "{label}: partial append preserves existing canonical bytes"
                    );

                    history.append(timestamp, SNAPSHOT, capacity(40));
                    assert_eq!(
                        fs::read(&path).unwrap(),
                        after_failed_tick,
                        "{label}: later poisoned tick adds no bytes"
                    );
                });

                let log = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
                assert_eq!(
                    log.matches("canonical history persistence degraded")
                        .count(),
                    1,
                    "{label}: only the first poison transition is an error"
                );
                assert!(matches!(
                    history
                        .canonical
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .append_sample(timestamp, canonical_capacity(&[40]), Vec::new()),
                    Err(store::WriteError::Poisoned)
                ));
                assert_eq!(
                    handle
                        .render()
                        .lines()
                        .find(|line| line.starts_with("nimproxy_history_persistence_degraded ")),
                    Some("nimproxy_history_persistence_degraded 1"),
                    "{label}: the no-label gauge remains degraded"
                );

                let restarted = History::open(dir.0.clone(), 0, capacity(40)).unwrap();
                assert_eq!(
                    restarted.status().persistence,
                    "ok",
                    "{label}: only a fresh store after restart can return to ok"
                );
            });
        }
    }

    async fn wait_for_canonical_compaction_idle(history: &Arc<History>, label: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while history.compaction_running.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "{label}: canonical compaction did not become idle"
            );
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn canonical_compaction_tracks_startup_debt_through_restart() {
        let dir = TestDir::new();
        let now = unix_now();
        let day = 86_400_u64;
        let cutoff = now.saturating_sub(day);
        let capacity = canonical_capacity(&[40]);
        let initial = canonical_snapshot(&capacity);
        let old_boot = cutoff.saturating_sub(500);
        let old_sample = cutoff.saturating_sub(400);
        let baseline = cutoff.saturating_sub(100);
        let retained = cutoff.saturating_add(60);
        let records = [
            canonical_boot(old_boot, "boot-a", &capacity),
            canonical_sample(old_sample, "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(baseline, "boot-a", &capacity, 20.0, 1.0),
            canonical_sample(retained, "boot-a", &capacity, 30.0, 2.0),
        ];
        let mut bytes = Vec::new();
        for record in &records {
            bytes.extend(codec::encode_record(record).unwrap());
            bytes.push(b'\n');
        }
        let canonical = dir.0.join("history-v1.jsonl");
        let legacy = dir.0.join("history.jsonl");
        fs::write(&canonical, bytes).unwrap();

        let history = Arc::new(History::open(dir.0.clone(), 1, initial.clone()).unwrap());
        let startup = history.rollup(cutoff, history.boot_t, 100);
        assert!(
            startup.complete,
            "clean retained history is complete at startup"
        );
        assert_eq!(
            value(&startup.data.totals, "nimproxy_requests_total"),
            10.0,
            "the hidden full-sample baseline preserves the retained counter delta"
        );
        assert!(
            history.status().compaction_pending,
            "durable canonical retention debt is visible at startup"
        );

        let runtime = history.boot_t;
        history.append(
            runtime,
            "# TYPE nimproxy_requests_total counter\n\
             nimproxy_requests_total{client=\"equivalence\"} 40\n\
             # TYPE nimproxy_active_requests gauge\n\
             nimproxy_active_requests{client=\"equivalence\"} 4\n",
            initial.clone(),
        );
        wait_for_canonical_compaction_idle(&history, "startup debt").await;
        assert!(
            !history.status().compaction_pending,
            "compaction clears durable retention debt after the fresh full sample"
        );
        let compacted = fs::read_to_string(&canonical).unwrap();
        assert!(
            !compacted.contains(&format!("\"timestamp\":{old_sample},")),
            "the unnecessary older sample is removed"
        );
        for timestamp in [old_boot, baseline, retained, history.boot_t, runtime] {
            assert!(
                compacted.contains(&format!("\"timestamp\":{timestamp},")),
                "required boot, baseline, retained, and live context is preserved at {timestamp}"
            );
        }
        assert!(
            compacted
                .lines()
                .all(|line| codec::decode_record(line.as_bytes()).is_ok()),
            "compaction leaves only valid canonical rows"
        );
        assert!(
            !legacy.exists(),
            "canonical compaction never touches legacy history"
        );

        drop(history);
        let reopened = Arc::new(History::open(dir.0.clone(), 1, initial).unwrap());
        assert!(
            !reopened.status().compaction_pending,
            "the required pre-cutoff baseline is not new restart debt"
        );
        let after_restart = reopened.rollup(cutoff, reopened.boot_t, 100);
        assert!(
            after_restart.complete,
            "retained history remains complete after restart"
        );
        assert_eq!(
            value(&after_restart.data.totals, "nimproxy_requests_total"),
            50.0,
            "the compacted baseline and retained/live samples preserve totals after restart"
        );
        assert!(
            fs::read_to_string(&canonical)
                .unwrap()
                .lines()
                .all(|line| codec::decode_record(line.as_bytes()).is_ok()),
            "restart keeps canonical bytes valid"
        );
        assert!(!legacy.exists(), "restart never creates legacy history");
    }

    #[tokio::test]
    async fn canonical_compaction_defers_recovery_until_gap_leaves_horizon() {
        let dir = TestDir::new();
        let now = unix_now();
        let day = 86_400_u64;
        let cutoff_two_days = now.saturating_sub(day.saturating_mul(2));
        let cutoff_one_day = now.saturating_sub(day);
        let old_boot = cutoff_two_days.saturating_sub(100);
        let old_sample = cutoff_two_days.saturating_sub(50);
        let corrupt_at = cutoff_two_days.saturating_add(100);
        let recovery_sample = cutoff_one_day.saturating_add(10);
        let capacity = canonical_capacity(&[40]);
        let initial = canonical_snapshot(&capacity);
        let corrupt = format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":{corrupt_at},"broken":true}}"#
        );
        let records = [
            canonical_boot(old_boot, "boot-a", &capacity),
            canonical_sample(old_sample, "boot-a", &capacity, 10.0, 1.0),
            canonical_boot(cutoff_one_day, "boot-b", &capacity),
            canonical_sample(recovery_sample, "boot-b", &capacity, 5.0, 2.0),
        ];
        let canonical = dir.0.join("history-v1.jsonl");
        let legacy = dir.0.join("history.jsonl");
        let mut bytes = Vec::new();
        for (index, record) in records.iter().enumerate() {
            if index == 2 {
                bytes.extend(corrupt.as_bytes());
                bytes.push(b'\n');
            }
            bytes.extend(codec::encode_record(record).unwrap());
            bytes.push(b'\n');
        }
        fs::write(&canonical, bytes).unwrap();

        let history = Arc::new(History::open(dir.0.clone(), 2, initial.clone()).unwrap());
        let crossing = history.rollup(corrupt_at.saturating_sub(1), recovery_sample, 100);
        assert!(
            !crossing.complete,
            "a query crossing supported-v1 recovery evidence is incomplete"
        );
        assert!(
            history.status().compaction_pending,
            "intersecting recovery evidence remains visible as canonical compaction debt"
        );

        let runtime = history.boot_t;
        let mut expected_after_live_append = fs::read(&canonical).unwrap();
        expected_after_live_append.extend(
            codec::encode_record(&canonical_sample(
                runtime,
                &history.boot_id,
                &capacity,
                9.0,
                4.0,
            ))
            .unwrap(),
        );
        expected_after_live_append.push(b'\n');
        history.append(
            runtime,
            "# TYPE nimproxy_requests_total counter\n\
             nimproxy_requests_total{client=\"equivalence\"} 9\n\
             # TYPE nimproxy_active_requests gauge\n\
             nimproxy_active_requests{client=\"equivalence\"} 4\n",
            initial.clone(),
        );
        wait_for_canonical_compaction_idle(&history, "intersecting recovery gap").await;
        assert!(
            history.status().compaction_pending,
            "deferred compaction remains pending while the recovery gap intersects retention"
        );
        assert_eq!(
            fs::read(&canonical).unwrap(),
            expected_after_live_append,
            "deferred compaction preserves all canonical evidence byte-for-byte after the live append"
        );

        history.clone().reconfigure_retention(1, now);
        wait_for_canonical_compaction_idle(&history, "safe recovery gap").await;
        assert!(
            !history.status().compaction_pending,
            "a gap ending exactly at the cutoff is safe to compact"
        );
        let compacted = fs::read_to_string(&canonical).unwrap();
        assert!(
            !compacted.contains(&corrupt),
            "safe compaction removes corrupt and outside evidence"
        );
        for timestamp in [old_boot, old_sample] {
            assert!(
                !compacted.contains(&format!("\"timestamp\":{timestamp},")),
                "safe compaction removes outside-horizon record at {timestamp}"
            );
        }
        assert!(
            compacted
                .lines()
                .all(|line| codec::decode_record(line.as_bytes()).is_ok()),
            "safe compaction leaves valid canonical bytes"
        );
        assert!(
            !legacy.exists(),
            "canonical compaction never touches legacy history"
        );
        let current = history.rollup(cutoff_one_day, runtime, 100);
        assert!(
            current.complete,
            "the retained query is complete after safe compaction"
        );
        assert_eq!(
            value(&current.data.totals, "nimproxy_requests_total"),
            14.0,
            "the recovery epoch and live reset preserve retained totals"
        );

        drop(history);
        let reopened = Arc::new(History::open(dir.0.clone(), 1, initial).unwrap());
        assert!(
            !reopened.status().compaction_pending,
            "safe compacted recovery history has no restart debt"
        );
        let after_restart = reopened.rollup(cutoff_one_day, reopened.boot_t, 100);
        assert!(
            after_restart.complete,
            "safe retained history stays complete after restart"
        );
        assert_eq!(
            value(&after_restart.data.totals, "nimproxy_requests_total"),
            14.0,
            "restart preserves safe-compaction totals"
        );
        assert!(!legacy.exists(), "restart never creates legacy history");
    }

    #[tokio::test]
    async fn canonical_compaction_review_preserves_a_superseding_generation() {
        let now = unix_now();
        let day = 86_400_u64;
        let cutoff = now.saturating_sub(day);
        let capacity = canonical_capacity(&[40]);
        let initial = canonical_snapshot(&capacity);
        let records = [
            canonical_boot(cutoff.saturating_sub(500), "boot-a", &capacity),
            canonical_sample(cutoff.saturating_sub(400), "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(cutoff.saturating_sub(100), "boot-a", &capacity, 20.0, 1.0),
            canonical_sample(cutoff.saturating_add(60), "boot-a", &capacity, 30.0, 2.0),
        ];
        let (_dir, history) = open_canonical_equivalence_history(&records, 1, initial.clone());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_passes = Arc::clone(&passes);
        let hook_release = Arc::clone(&release_rx);
        *history.compaction_test_hook.lock().unwrap() = Some(Arc::new(move |point| {
            if point == CompactionTestPoint::BeforeCanonicalPendingClear {
                let pass = hook_passes.fetch_add(1, Ordering::SeqCst) + 1;
                entered_tx.send(pass).unwrap();
                if pass == 1 {
                    hook_release.lock().unwrap().recv().unwrap();
                }
            }
        }));

        let runtime = history.boot_t;
        history.append(
            runtime,
            "# TYPE nimproxy_requests_total counter\n\
             nimproxy_requests_total{client=\"equivalence\"} 40\n\
             # TYPE nimproxy_active_requests gauge\n\
             nimproxy_active_requests{client=\"equivalence\"} 4\n",
            initial,
        );
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("first canonical durable pass reaches the generation-clear hook"),
            1
        );
        assert!(history.compaction_running.load(Ordering::SeqCst));
        let superseding_cutoff = runtime.saturating_sub(day.saturating_sub(1));
        history.request_compaction(superseding_cutoff);
        let superseding_generation = history.compaction_generation.load(Ordering::SeqCst);
        release_tx.send(()).unwrap();
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the superseding generation reaches the generation-clear hook"),
            2
        );
        wait_for_canonical_compaction_idle(&history, "superseding generation").await;

        assert_eq!(
            passes.load(Ordering::SeqCst),
            2,
            "a superseding generation starts a second canonical compaction pass"
        );
        assert!(!history.status().compaction_pending);
        assert_eq!(
            history.compaction_generation.load(Ordering::SeqCst),
            superseding_generation
        );
        assert_eq!(
            history.compaction_cutoff.load(Ordering::SeqCst),
            superseding_cutoff
        );
    }

    #[tokio::test]
    async fn canonical_compaction_review_preserves_concurrent_append_diagnostics_and_events() {
        let now = unix_now();
        let day = 86_400_u64;
        let cutoff = now.saturating_sub(day);
        let capacity = canonical_capacity(&[40]);
        let initial = canonical_snapshot(&capacity);
        let records = [
            canonical_boot(cutoff.saturating_sub(500), "boot-a", &capacity),
            canonical_sample(cutoff.saturating_sub(400), "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(cutoff.saturating_sub(100), "boot-a", &capacity, 20.0, 1.0),
            canonical_sample(cutoff.saturating_add(60), "boot-a", &capacity, 30.0, 2.0),
        ];
        let (_dir, history) = open_canonical_equivalence_history(&records, 1, initial.clone());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        *history.compaction_test_hook.lock().unwrap() = Some(Arc::new(move |point| {
            if point == CompactionTestPoint::CanonicalReplayCaptured {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
        }));

        let runtime = history.boot_t;
        history.append(
            runtime,
            "# TYPE nimproxy_requests_total counter\n\
             nimproxy_requests_total{client=\"equivalence\"} 40\n\
             # TYPE nimproxy_active_requests gauge\n\
             nimproxy_active_requests{client=\"equivalence\"} 4\n",
            initial.clone(),
        );
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("canonical replay capture releases the store before installation");
        history.compaction_pending.store(false, Ordering::SeqCst);
        history.append(
            runtime,
            "# TYPE nimproxy_requests_total counter\n\
             nimproxy_requests_total{client=\"equivalence\"} 50\n\
             # TYPE nimproxy_active_requests gauge\n\
             nimproxy_active_requests{client=\"equivalence\"} 5\n",
            initial,
        );
        let (diagnostics, recovery_events, revision) = {
            let inner = history.inner.lock().unwrap();
            (
                inner.diagnostics.clone(),
                inner.recovery_events.len(),
                inner.revision,
            )
        };
        release_tx.send(()).unwrap();
        wait_for_canonical_compaction_idle(&history, "concurrent append replay install").await;
        let after = history.inner.lock().unwrap();
        assert!(
            after.diagnostics.valid_samples >= diagnostics.valid_samples
                && after.diagnostics.normalized_series >= diagnostics.normalized_series
                && after.recovery_events.len() >= recovery_events
                && after.revision >= revision,
            "canonical replay installation preserves concurrent append diagnostics, events, and revision"
        );
        drop(after);
        assert!(
            value(
                &history.rollup(cutoff, runtime, 100).data.totals,
                "nimproxy_requests_total"
            ) >= 10.0,
            "the concurrent appended counter delta remains in the rollup"
        );
    }

    #[tokio::test]
    async fn canonical_compaction_review_unlimited_transition_cancels_stale_finite_work() {
        let now = unix_now();
        let capacity = canonical_capacity(&[40]);
        let initial = canonical_snapshot(&capacity);
        let records = [
            canonical_boot(now.saturating_sub(90_000), "boot-a", &capacity),
            canonical_sample(now.saturating_sub(89_000), "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(now.saturating_sub(100), "boot-a", &capacity, 20.0, 2.0),
        ];
        let (dir, history) = open_canonical_equivalence_history(&records, 0, initial);
        let canonical = dir.0.join("history-v1.jsonl");
        let legacy = dir.0.join("history.jsonl");
        let before = fs::read(&canonical).unwrap();
        let store = history.canonical.as_ref().unwrap().lock().unwrap();
        history.clone().reconfigure_retention(1, now);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !history.compaction_running.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "finite canonical compaction did not claim the running state"
            );
            std::thread::yield_now();
        }
        history.clone().reconfigure_retention(0, now);
        drop(store);
        wait_for_canonical_compaction_idle(&history, "unlimited retention transition").await;

        assert_eq!(history.days.load(Ordering::Relaxed), 0);
        assert!(
            !history.status().compaction_pending,
            "unlimited retention cancels pending finite canonical compaction"
        );
        assert_eq!(
            fs::read(&canonical).unwrap(),
            before,
            "unlimited retention leaves canonical bytes unchanged"
        );
        assert!(
            !legacy.exists(),
            "canonical compaction never touches legacy history"
        );
    }

    #[tokio::test]
    async fn canonical_compaction_review_reschedules_newer_generation_after_sync_failure() {
        let now = unix_now();
        let day = 86_400_u64;
        let cutoff = now.saturating_sub(day);
        let capacity = canonical_capacity(&[40]);
        let initial = canonical_snapshot(&capacity);
        let records = [
            canonical_boot(cutoff.saturating_sub(500), "boot-a", &capacity),
            canonical_sample(cutoff.saturating_sub(400), "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(cutoff.saturating_sub(100), "boot-a", &capacity, 20.0, 1.0),
            canonical_sample(cutoff.saturating_add(60), "boot-a", &capacity, 30.0, 2.0),
        ];
        let (_dir, history) = open_canonical_equivalence_history(&records, 1, initial.clone());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(2);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(2);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_passes = Arc::clone(&passes);
        let hook_release = Arc::clone(&release_rx);
        *history.compaction_test_hook.lock().unwrap() = Some(Arc::new(move |point| {
            if point == CompactionTestPoint::CanonicalReplayCaptured {
                let pass = hook_passes.fetch_add(1, Ordering::SeqCst) + 1;
                entered_tx.send(pass).unwrap();
                if pass <= 2 {
                    hook_release.lock().unwrap().recv().unwrap();
                }
            }
        }));

        history
            .canonical
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .inject_test_compaction_directory_sync_failure();
        let runtime = history.boot_t;
        history.append(
            runtime,
            "# TYPE nimproxy_requests_total counter\n\
             nimproxy_requests_total{client=\"equivalence\"} 40\n\
             # TYPE nimproxy_active_requests gauge\n\
             nimproxy_active_requests{client=\"equivalence\"} 4\n",
            initial.clone(),
        );
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the injected directory-sync failure reaches replay capture"),
            1
        );
        assert_eq!(
            passes.load(Ordering::SeqCst),
            1,
            "the unchanged sync-failure generation has one committed pass"
        );
        release_tx.send(()).unwrap();
        wait_for_canonical_compaction_idle(&history, "unchanged sync failure").await;
        assert_eq!(
            passes.load(Ordering::SeqCst),
            1,
            "the unchanged sync-failure generation is not retried"
        );
        assert!(
            history.status().compaction_pending,
            "the unsynced committed generation remains pending without a retry spin"
        );

        history
            .canonical
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .inject_test_compaction_directory_sync_failure();
        history.request_compaction(cutoff);
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the second injected directory-sync failure reaches replay capture"),
            2
        );
        let superseding_cutoff = runtime.saturating_sub(day.saturating_sub(1));
        history.request_compaction(superseding_cutoff);
        let superseding_generation = history.compaction_generation.load(Ordering::SeqCst);
        release_tx.send(()).unwrap();
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the superseding generation reaches replay capture"),
            3
        );
        wait_for_canonical_compaction_idle(&history, "sync-failure superseding generation").await;

        assert_eq!(
            passes.load(Ordering::SeqCst),
            3,
            "a newer finite generation starts and completes after the sync-failure worker releases"
        );
        assert!(!history.status().compaction_pending);
        assert_eq!(
            history.compaction_generation.load(Ordering::SeqCst),
            superseding_generation
        );
        assert_eq!(
            history.compaction_cutoff.load(Ordering::SeqCst),
            superseding_cutoff
        );
    }

    /// A unique per-test scratch dir (std-only; removed on drop).
    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "nimproxy-history-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn load_reads_v1_and_v2_boot_epochs() {
        let dir = TestDir::new();
        let path = dir.0.join("history.jsonl");
        fs::write(
            &path,
            "{\"t\":10,\"m\":\"# TYPE requests_total counter\\nrequests_total 5\\n\"}\n\
             {\"t\":20,\"m\":\"# TYPE requests_total counter\\nrequests_total 8\\n\"}\n\
             {\"v\":2,\"t\":21,\"boot\":\"boot-b\",\"kind\":\"boot\",\"capacity\":{\"enabled_lanes\":2,\"rpms\":[40,30],\"capacity_rpm\":70}}\n\
             {\"v\":2,\"t\":30,\"boot\":\"boot-b\",\"capacity\":{\"enabled_lanes\":2,\"rpms\":[40,30],\"capacity_rpm\":70},\"m\":\"# TYPE requests_total counter\\nrequests_total 2\\n\"}\n",
        )
        .unwrap();

        let h = History::load(
            Some(dir.0.clone()),
            0,
            CapacitySnapshot {
                enabled_lanes: 1,
                rpms: vec![40],
                capacity_rpm: 40,
            },
        );
        let inner = h.inner.lock().unwrap();
        assert_eq!(inner.points.len(), 3, "boot marker is not a metric sample");
        assert_eq!(
            inner
                .points
                .iter()
                .map(|point| metric(&point.deltas, "requests_total"))
                .sum::<f64>(),
            10.0
        );
        assert_eq!(inner.points[2].capacity.as_ref().unwrap().rpms, [40, 30]);
        assert_eq!(inner.diagnostics.excluded_epochs, 0);
    }

    #[test]
    fn load_infers_reset_when_legacy_counters_empty_then_reappear() {
        let dir = TestDir::new();
        let path = dir.0.join("history.jsonl");
        fs::write(
            &path,
            "{\"t\":10,\"m\":\"# TYPE requests_total counter\\nrequests_total 5\\n\"}\n\
             {\"t\":20,\"m\":\"# TYPE active gauge\\nactive 1\\n\"}\n\
             {\"t\":30,\"m\":\"# TYPE requests_total counter\\nrequests_total 2\\n\"}\n",
        )
        .unwrap();

        let history = History::load(Some(dir.0.clone()), 0, capacity(40));
        let inner = history.inner.lock().unwrap();
        let total = inner
            .points
            .iter()
            .filter_map(|point| {
                point
                    .deltas
                    .iter()
                    .find_map(|(key, value)| (key.metric == "requests_total").then_some(*value))
            })
            .sum::<f64>();
        assert_eq!(total, 7.0);
        assert_eq!(inner.diagnostics.excluded_epochs, 0);
    }

    fn history_with_points() -> Arc<History> {
        let history = memory_history(0);
        history.append(
            100,
            "# TYPE requests_total counter\n\
             requests_total 10\n\
             # TYPE prompt_tokens_total counter\n\
             prompt_tokens_total{model=\"alpha\"} 100\n\
             prompt_tokens_total{model=\"beta\"} 50\n\
             # TYPE latency_seconds histogram\n\
             latency_seconds_bucket{le=\"0.5\"} 3\n\
             latency_seconds_bucket{le=\"+Inf\"} 10\n\
             latency_seconds_sum 4\n\
             latency_seconds_count 10\n\
             # TYPE active gauge\n\
             active 1\n",
            capacity(40),
        );
        history.append(
            200,
            "# TYPE requests_total counter\n\
             requests_total 25\n\
             # TYPE prompt_tokens_total counter\n\
             prompt_tokens_total{model=\"alpha\"} 150\n\
             prompt_tokens_total{model=\"beta\"} 80\n\
             # TYPE latency_seconds histogram\n\
             latency_seconds_bucket{le=\"0.5\"} 8\n\
             latency_seconds_bucket{le=\"+Inf\"} 25\n\
             latency_seconds_sum 11\n\
             latency_seconds_count 25\n\
             # TYPE active gauge\n\
             active 2\n",
            CapacitySnapshot {
                enabled_lanes: 2,
                rpms: vec![40, 40],
                capacity_rpm: 80,
            },
        );
        history.append(
            300,
            "# TYPE requests_total counter\n\
             requests_total 40\n\
             # TYPE prompt_tokens_total counter\n\
             prompt_tokens_total{model=\"alpha\"} 240\n\
             prompt_tokens_total{model=\"beta\"} 110\n\
             # TYPE latency_seconds histogram\n\
             latency_seconds_bucket{le=\"0.5\"} 12\n\
             latency_seconds_bucket{le=\"+Inf\"} 40\n\
             latency_seconds_sum 18\n\
             latency_seconds_count 40\n\
             # TYPE active gauge\n\
             active 3\n",
            CapacitySnapshot {
                enabled_lanes: 2,
                rpms: vec![40, 40],
                capacity_rpm: 80,
            },
        );
        history
    }

    fn canonical_capacity(rpms: &[usize]) -> codec::Capacity {
        codec::Capacity {
            capacity_rpm: rpms.iter().sum(),
            enabled_keys: rpms.len(),
            key_rpms: rpms.to_vec(),
        }
    }

    fn canonical_snapshot(capacity: &codec::Capacity) -> CapacitySnapshot {
        CapacitySnapshot {
            enabled_lanes: capacity.enabled_keys,
            rpms: capacity.key_rpms.clone(),
            capacity_rpm: capacity.capacity_rpm,
        }
    }

    fn canonical_state(counter: f64, gauge: f64) -> Vec<codec::StateEntry> {
        let labels: BTreeMap<_, _> = [("client".to_owned(), "equivalence".to_owned())]
            .into_iter()
            .collect();
        vec![
            codec::StateEntry {
                kind: codec::StateKind::Counter,
                metric: "nimproxy_requests_total".to_owned(),
                labels: labels.clone(),
                value: counter,
            },
            codec::StateEntry {
                kind: codec::StateKind::Gauge,
                metric: "nimproxy_active_requests".to_owned(),
                labels,
                value: gauge,
            },
        ]
    }

    fn canonical_boot(t: u64, boot_id: &str, capacity: &codec::Capacity) -> codec::Record {
        codec::Record::Boot(codec::BootRecord {
            timestamp: t,
            boot_id: boot_id.to_owned(),
            capacity: capacity.clone(),
        })
    }

    fn canonical_sample(
        t: u64,
        boot_id: &str,
        capacity: &codec::Capacity,
        counter: f64,
        gauge: f64,
    ) -> codec::Record {
        codec::Record::Sample(codec::SampleRecord {
            timestamp: t,
            boot_id: boot_id.to_owned(),
            capacity: capacity.clone(),
            state: canonical_state(counter, gauge),
        })
    }

    fn canonical_checkpoint(t: u64, boot_id: &str, capacity: &codec::Capacity) -> codec::Record {
        codec::Record::Checkpoint(codec::CheckpointRecord {
            timestamp: t,
            boot_id: boot_id.to_owned(),
            capacity: capacity.clone(),
        })
    }

    fn open_canonical_equivalence_history(
        records: &[codec::Record],
        days: u64,
        initial_capacity: CapacitySnapshot,
    ) -> (TestDir, Arc<History>) {
        let dir = TestDir::new();
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend(codec::encode_record(record).unwrap());
            bytes.push(b'\n');
        }
        fs::write(dir.0.join("history-v1.jsonl"), bytes).unwrap();
        let history = Arc::new(History::open(dir.0.clone(), days, initial_capacity).unwrap());
        (dir, history)
    }

    fn rollup_difference(
        left: &HistoryWindow<Rollup>,
        right: &HistoryWindow<Rollup>,
    ) -> Option<&'static str> {
        if left.complete != right.complete {
            Some("complete")
        } else if left.data.available_from != right.data.available_from
            || left.data.available_to != right.data.available_to
        {
            Some("available_bounds")
        } else if left.data.effective_from != right.data.effective_from
            || left.data.effective_to != right.data.effective_to
        {
            Some("effective_bounds")
        } else if left.data.totals != right.data.totals {
            Some("totals")
        } else if left.data.latest != right.data.latest {
            Some("latest")
        } else if left.data.points != right.data.points {
            Some("points")
        } else if left.data.history_revision != right.data.history_revision {
            Some("history_revision")
        } else {
            None
        }
    }

    fn assert_rollup_equivalent(
        label: &str,
        left: &HistoryWindow<Rollup>,
        right: &HistoryWindow<Rollup>,
    ) {
        assert_eq!(
            rollup_difference(left, right),
            None,
            "{label}: paired canonical encodings diverged"
        );
    }

    fn assert_tail_equivalent(label: &str, left: &Tail, right: &Tail) {
        assert_eq!(
            left.base_history_revision, right.base_history_revision,
            "{label}: revision"
        );
        assert_eq!(left.from, right.from, "{label}: from");
        assert_eq!(left.to, right.to, "{label}: to");
        assert_eq!(left.totals, right.totals, "{label}: totals");
    }

    #[test]
    fn canonical_checkpoint_encoding_matches_repeated_samples_across_resets_capacities_and_budgets()
    {
        let now = unix_now();
        let start = now - 100;
        let first_capacity = canonical_capacity(&[40]);
        let changed_capacity = canonical_capacity(&[30, 30]);
        let reset_capacity = canonical_capacity(&[20, 30]);
        let live_checkpoint_capacity = canonical_capacity(&[35, 35]);
        let repeated = [
            canonical_boot(start, "boot-a", &first_capacity),
            canonical_sample(start + 10, "boot-a", &first_capacity, 10.0, 1.0),
            canonical_sample(start + 20, "boot-a", &changed_capacity, 10.0, 1.0),
            canonical_sample(start + 30, "boot-a", &changed_capacity, 15.0, 2.0),
            canonical_boot(start + 40, "boot-b", &reset_capacity),
            canonical_sample(start + 50, "boot-b", &reset_capacity, 3.0, 3.0),
            canonical_sample(start + 60, "boot-b", &live_checkpoint_capacity, 3.0, 3.0),
        ];
        let checkpointed = [
            canonical_boot(start, "boot-a", &first_capacity),
            canonical_sample(start + 10, "boot-a", &first_capacity, 10.0, 1.0),
            canonical_checkpoint(start + 20, "boot-a", &changed_capacity),
            canonical_sample(start + 30, "boot-a", &changed_capacity, 15.0, 2.0),
            canonical_boot(start + 40, "boot-b", &reset_capacity),
            canonical_sample(start + 50, "boot-b", &reset_capacity, 3.0, 3.0),
            canonical_checkpoint(start + 60, "boot-b", &live_checkpoint_capacity),
        ];
        let initial_capacity = canonical_snapshot(&first_capacity);
        let (_repeated_dir, repeated_history) =
            open_canonical_equivalence_history(&repeated, 0, initial_capacity.clone());
        let (_checkpointed_dir, checkpointed_history) =
            open_canonical_equivalence_history(&checkpointed, 0, initial_capacity);

        for budget in [2, 1000] {
            let repeated_rollup = repeated_history.rollup(start, start + 60, budget);
            let checkpointed_rollup = checkpointed_history.rollup(start, start + 60, budget);
            assert_rollup_equivalent(
                "checkpoint equivalence",
                &repeated_rollup,
                &checkpointed_rollup,
            );
            assert_ne!(
                repeated_rollup.diagnostics, checkpointed_rollup.diagnostics,
                "physical record diagnostics distinguish samples from checkpoints"
            );
            assert_eq!(
                value(&repeated_rollup.data.totals, "nimproxy_requests_total"),
                18.0
            );
            assert_eq!(
                value(&repeated_rollup.data.latest, "nimproxy_active_requests"),
                3.0
            );
        }

        let coarse = repeated_history.rollup(start, start + 60, 2);
        let fine = repeated_history.rollup(start, start + 60, 1000);
        assert_eq!(coarse.data.totals, fine.data.totals);
        assert_eq!(coarse.data.latest, fine.data.latest);
        assert_eq!(coarse.data.history_revision, fine.data.history_revision);
        assert!(coarse.data.points.len() <= 2);
        assert!(fine.data.points.len() > coarse.data.points.len());
        assert_eq!(
            fine.data.points.last().unwrap().capacity,
            Some(CapacityRollup {
                average_rpm: 70.0,
                latest_rpms: vec![35, 35],
            })
        );

        let runtime = unix_now() + 2;
        let runtime_snapshot = "# TYPE nimproxy_requests_total counter\n\
            nimproxy_requests_total{client=\"equivalence\"} 4\n\
            # TYPE nimproxy_active_requests gauge\n\
            nimproxy_active_requests{client=\"equivalence\"} 4\n";
        let before_repeated = repeated_history.current(runtime, || runtime_snapshot.to_owned());
        let before_checkpointed =
            checkpointed_history.current(runtime, || runtime_snapshot.to_owned());
        assert_eq!(
            before_repeated.tail.base_history_revision,
            repeated_history.revision()
        );
        assert_tail_equivalent(
            "current tail before append",
            &before_repeated.tail,
            &before_checkpointed.tail,
        );
        repeated_history.append(
            runtime,
            runtime_snapshot,
            canonical_snapshot(&live_checkpoint_capacity),
        );
        checkpointed_history.append(
            runtime,
            runtime_snapshot,
            canonical_snapshot(&live_checkpoint_capacity),
        );
        let after_repeated = repeated_history.current(runtime + 1, || runtime_snapshot.to_owned());
        let after_checkpointed =
            checkpointed_history.current(runtime + 1, || runtime_snapshot.to_owned());
        assert_tail_equivalent(
            "current tail after append",
            &after_repeated.tail,
            &after_checkpointed.tail,
        );
        assert_eq!(
            after_repeated.tail.base_history_revision,
            before_repeated.tail.base_history_revision + 1,
            "append advances the current-tail baseline revision"
        );
    }

    #[test]
    fn canonical_checkpoint_encoding_preserves_the_retention_boundary_baseline() {
        let now = unix_now();
        let cutoff = now - 86_400;
        let initial_capacity = canonical_capacity(&[40]);
        let old = cutoff - 100;
        let retained = cutoff + 10;
        let repeated = [
            canonical_boot(old - 10, "boot-a", &initial_capacity),
            canonical_sample(old, "boot-a", &initial_capacity, 10.0, 1.0),
            canonical_sample(old + 10, "boot-a", &initial_capacity, 10.0, 1.0),
            canonical_sample(retained, "boot-a", &initial_capacity, 15.0, 2.0),
        ];
        let checkpointed = [
            canonical_boot(old - 10, "boot-a", &initial_capacity),
            canonical_sample(old, "boot-a", &initial_capacity, 10.0, 1.0),
            canonical_checkpoint(old + 10, "boot-a", &initial_capacity),
            canonical_sample(retained, "boot-a", &initial_capacity, 15.0, 2.0),
        ];
        let initial_snapshot = canonical_snapshot(&initial_capacity);
        let (_repeated_dir, repeated_history) =
            open_canonical_equivalence_history(&repeated, 1, initial_snapshot.clone());
        let (_checkpointed_dir, checkpointed_history) =
            open_canonical_equivalence_history(&checkpointed, 1, initial_snapshot);

        for budget in [2, 1000] {
            let repeated_rollup = repeated_history.rollup(old - 10, retained, budget);
            let checkpointed_rollup = checkpointed_history.rollup(old - 10, retained, budget);
            assert_rollup_equivalent(
                "retention-boundary equivalence",
                &repeated_rollup,
                &checkpointed_rollup,
            );
            assert_eq!(repeated_rollup.data.available_from, Some(retained));
            assert_eq!(repeated_rollup.data.effective_from, Some(retained));
            assert_eq!(
                repeated_rollup.data.totals,
                vec![MetricValue {
                    labels: [("client".to_owned(), "equivalence".to_owned())]
                        .into_iter()
                        .collect(),
                    metric: "nimproxy_requests_total".to_owned(),
                    value: 5.0,
                }]
            );
        }
    }

    #[test]
    fn rollup_equivalence_checker_rejects_a_semantically_different_stream_by_totals() {
        let start = unix_now() - 30;
        let capacity = canonical_capacity(&[40]);
        let expected = [
            canonical_boot(start, "boot-a", &capacity),
            canonical_sample(start + 10, "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(start + 20, "boot-a", &capacity, 15.0, 1.0),
        ];
        let different = [
            canonical_boot(start, "boot-a", &capacity),
            canonical_sample(start + 10, "boot-a", &capacity, 10.0, 1.0),
            canonical_sample(start + 20, "boot-a", &capacity, 16.0, 1.0),
        ];
        let initial = canonical_snapshot(&capacity);
        let (_expected_dir, expected_history) =
            open_canonical_equivalence_history(&expected, 0, initial.clone());
        let (_different_dir, different_history) =
            open_canonical_equivalence_history(&different, 0, initial);

        assert_eq!(
            rollup_difference(
                &expected_history.rollup(start, start + 20, 1000),
                &different_history.rollup(start, start + 20, 1000),
            ),
            Some("totals")
        );
    }

    fn value(values: &[MetricValue], name: &str) -> f64 {
        values
            .iter()
            .find_map(|metric| (metric.metric == name).then_some(metric.value))
            .unwrap()
    }

    fn labeled_value(values: &[MetricValue], name: &str, label: &str, label_value: &str) -> f64 {
        values
            .iter()
            .find_map(|metric| {
                (metric.metric == name
                    && metric.labels.get(label).map(String::as_str) == Some(label_value))
                .then_some(metric.value)
            })
            .unwrap()
    }

    #[test]
    fn rollup_totals_do_not_change_with_point_budget() {
        let h = history_with_points();
        let coarse = h.rollup(99, 300, 2);
        let fine = h.rollup(99, 300, 1000);
        assert_eq!(coarse.data.totals, fine.data.totals);
        assert_eq!(value(&coarse.data.totals, "requests_total"), 40.0);
        assert!(coarse.data.points.len() <= 2);
    }

    #[test]
    fn rollup_uses_open_closed_sample_boundaries() {
        let h = history_with_points();
        let rollup = h.rollup(100, 200, 20);
        assert_eq!(value(&rollup.data.totals, "requests_total"), 15.0);
    }

    #[test]
    fn rollup_preserves_grouped_labels() {
        let rollup = history_with_points().rollup(99, 300, 20);
        assert_eq!(
            labeled_value(&rollup.data.totals, "prompt_tokens_total", "model", "alpha"),
            240.0
        );
        assert_eq!(
            labeled_value(&rollup.data.totals, "prompt_tokens_total", "model", "beta"),
            110.0
        );
    }

    #[test]
    fn rollup_keeps_latest_gauges_and_histogram_buckets() {
        let rollup = history_with_points().rollup(99, 300, 20);
        assert_eq!(value(&rollup.data.latest, "active"), 3.0);
        assert_eq!(
            labeled_value(&rollup.data.totals, "latency_seconds_bucket", "le", "0.5"),
            12.0
        );
        assert_eq!(
            labeled_value(&rollup.data.totals, "latency_seconds_bucket", "le", "+Inf"),
            40.0
        );
    }

    #[test]
    fn rollup_empty_window_retains_only_available_bounds() {
        let rollup = history_with_points().rollup(300, 400, 20);
        assert_eq!(rollup.data.available_from, Some(100));
        assert_eq!(rollup.data.available_to, Some(300));
        assert_eq!(rollup.data.effective_from, None);
        assert_eq!(rollup.data.effective_to, None);
        assert!(rollup.data.totals.is_empty());
        assert!(rollup.data.latest.is_empty());
        assert!(rollup.data.points.is_empty());
    }

    #[test]
    fn rollup_reports_effective_and_available_bounds() {
        let rollup = history_with_points().rollup(0, 250, 20);
        assert_eq!(rollup.data.available_from, Some(100));
        assert_eq!(rollup.data.available_to, Some(300));
        assert_eq!(rollup.data.effective_from, Some(100));
        assert_eq!(rollup.data.effective_to, Some(200));
    }

    #[test]
    fn rollup_does_not_invent_capacity_before_the_first_retained_sample() {
        let rollup = history_with_points().rollup(0, 100, 20);

        assert_eq!(value(&rollup.data.totals, "requests_total"), 10.0);
        assert_eq!(rollup.data.effective_from, Some(100));
        assert_eq!(rollup.data.points.len(), 1);
        assert_eq!(rollup.data.points[0].from, 100);
        assert_eq!(rollup.data.points[0].to, 100);
        assert_eq!(rollup.data.points[0].duration_seconds, 0);
        assert_eq!(rollup.data.points[0].capacity, None);
    }

    #[test]
    fn rollup_time_weights_capacity() {
        let rollup = history_with_points().rollup(99, 300, 2);
        let weighted_rpm = rollup
            .data
            .points
            .iter()
            .filter_map(|point| {
                point
                    .capacity
                    .as_ref()
                    .map(|capacity| capacity.average_rpm * point.duration_seconds as f64)
            })
            .sum::<f64>();
        let duration = rollup
            .data
            .points
            .iter()
            .map(|point| point.duration_seconds)
            .sum::<u64>();
        assert_eq!(duration, 200);
        assert!((weighted_rpm / duration as f64 - 80.0).abs() < 1e-12);
        assert_eq!(
            rollup
                .data
                .points
                .last()
                .unwrap()
                .capacity
                .as_ref()
                .unwrap()
                .latest_rpms,
            [40, 40]
        );
    }

    #[test]
    fn tail_reports_only_metrics_since_the_latest_indexed_sample() {
        let history = history_with_points();
        let current = history.current(350, || {
            "# TYPE requests_total counter\n\
                 requests_total 43\n\
                 # TYPE active gauge\n\
                 active 7\n"
                .to_owned()
        });
        assert_eq!(value(&current.tail.totals, "requests_total"), 3.0);
        assert_eq!(value(&current.metrics, "requests_total"), 43.0);
        assert_eq!(value(&current.metrics, "active"), 7.0);
        assert_eq!(current.tail.from, Some(300));
        assert_eq!(current.tail.to, 350);
        assert_eq!(current.tail.base_history_revision, history.revision());
    }

    #[test]
    fn tail_counts_a_new_process_value_from_zero() {
        let history = history_with_points();
        history.inner.lock().unwrap().last_sample_boot = Some("previous-process".to_owned());
        let current = history.current(350, || {
            "# TYPE requests_total counter\nrequests_total 2\n".to_owned()
        });
        assert_eq!(value(&current.tail.totals, "requests_total"), 2.0);
    }

    #[test]
    fn tail_polling_does_not_accumulate_previous_polls() {
        let history = history_with_points();
        let exposition = "# TYPE requests_total counter\nrequests_total 43\n";
        let first = history.current(350, || exposition.to_owned());
        let second = history.current(351, || exposition.to_owned());
        assert_eq!(value(&first.tail.totals, "requests_total"), 3.0);
        assert_eq!(value(&second.tail.totals, "requests_total"), 3.0);
        assert_eq!(history.revision(), 3);
    }

    #[test]
    fn current_render_holds_history_generation_through_bounds_and_baseline() {
        let history = memory_history(0);
        history.append(
            100,
            "# TYPE requests_total counter\nrequests_total 10\n",
            capacity(40),
        );
        let (render_started_tx, render_started_rx) = std::sync::mpsc::channel();
        let (release_render_tx, release_render_rx) = std::sync::mpsc::channel();
        let current_history = history.clone();
        let current_thread = std::thread::spawn(move || {
            current_history.current(150, || {
                render_started_tx.send(()).unwrap();
                release_render_rx.recv().unwrap();
                "# TYPE requests_total counter\nrequests_total 13\n".to_owned()
            })
        });
        render_started_rx.recv().unwrap();

        let (append_started_tx, append_started_rx) = std::sync::mpsc::channel();
        let (append_done_tx, append_done_rx) = std::sync::mpsc::channel();
        let append_history = history.clone();
        let append_thread = std::thread::spawn(move || {
            append_started_tx.send(()).unwrap();
            append_history.append(
                200,
                "# TYPE requests_total counter\nrequests_total 14\n",
                capacity(40),
            );
            append_done_tx.send(()).unwrap();
        });
        append_started_rx.recv().unwrap();
        assert!(
            append_done_rx
                .recv_timeout(Duration::from_millis(250))
                .is_err(),
            "append advanced while current metrics were rendering"
        );

        release_render_tx.send(()).unwrap();
        let current = current_thread.join().unwrap();
        assert_eq!(current.available_from, Some(100));
        assert_eq!(current.available_to, Some(100));
        assert_eq!(current.tail.from, Some(100));
        assert_eq!(current.tail.base_history_revision, 1);
        assert_eq!(value(&current.tail.totals, "requests_total"), 3.0);
        append_done_rx.recv().unwrap();
        append_thread.join().unwrap();
    }

    #[test]
    fn load_reads_existing_snapshots_and_skips_junk() {
        let dir = TestDir::new();
        let path = dir.0.join("history.jsonl");
        // Two valid lines, one unparseable line (skipped), one out-of-order.
        fs::write(
            &path,
            "{\"t\":10,\"m\":\"a\"}\nnot json\n{\"t\":20,\"m\":\"b\"}\n{\"t\":5,\"m\":\"c\"}\n",
        )
        .unwrap();
        // days = 0 exercises the "infinite" retention log branch.
        let h = History::load(Some(dir.0.clone()), 0, capacity(40));
        let timestamps = h
            .inner
            .lock()
            .unwrap()
            .points
            .iter()
            .map(|point| point.t)
            .collect::<Vec<_>>();
        assert_eq!(timestamps.len(), 3, "3 valid lines parsed, junk skipped");
        assert_eq!(timestamps[0], 5, "snapshots sorted by timestamp on load");
        // Finite retention is also enforced while loading so a compacted
        // pre-cutoff baseline stays hidden after restart.
        let h2 = History::load(Some(dir.0.clone()), 7, capacity(40));
        assert!(h2.inner.lock().unwrap().points.is_empty());
        assert_eq!(h2.status().available_from, None);
    }

    #[tokio::test]
    async fn retention_load_marks_stale_file_pending_until_next_append_compacts() {
        let dir = TestDir::new();
        let path = dir.0.join("history.jsonl");
        let now = unix_now();
        let cutoff = now - 86_400;
        let old = cutoff - 200;
        let baseline = cutoff - 100;
        let retained = cutoff + 10;
        fs::write(
            &path,
            format!(
                "{{\"t\":{old},\"m\":\"# TYPE requests_total counter\\nrequests_total 5\\n\"}}\n\
                 {{\"t\":{baseline},\"m\":\"# TYPE requests_total counter\\nrequests_total 8\\n\"}}\n\
                 {{\"t\":{retained},\"m\":\"# TYPE requests_total counter\\nrequests_total 10\\n\"}}\n"
            ),
        )
        .unwrap();

        let history = Arc::new(History::load(Some(dir.0.clone()), 1, capacity(40)));
        assert_eq!(history.status().available_from, Some(retained));
        assert!(
            history.status().compaction_pending,
            "startup-filtered records are durable cleanup debt"
        );
        assert_eq!(*history.dropped_since_compact.lock().unwrap(), 2);
        assert!((cutoff..=cutoff + 1).contains(&history.compaction_cutoff.load(Ordering::SeqCst)));

        history.append(
            now,
            "# TYPE requests_total counter\nrequests_total 1\n",
            capacity(40),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while history.status().compaction_pending {
            assert!(
                std::time::Instant::now() < deadline,
                "startup retention debt did not compact on the next append"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let compacted = fs::read_to_string(path).unwrap();
        assert!(!compacted
            .lines()
            .any(|line| line.contains(&format!("\"t\":{old},"))));
        assert!(compacted
            .lines()
            .any(|line| line.contains(&format!("\"t\":{baseline},"))));
        assert!(compacted
            .lines()
            .any(|line| line.contains(&format!("\"t\":{retained},"))));
    }

    #[tokio::test]
    async fn compaction_preserves_hidden_baseline_boot_and_retained_totals() {
        let dir = TestDir::new();
        let path = dir.0.join("history.jsonl");
        let now = unix_now();
        let cutoff = now - 86_400;
        let old = cutoff - 400;
        let boot = cutoff - 300;
        let baseline = cutoff - 200;
        let retained_one = cutoff + 10;
        let retained_two = now - 10;
        fs::write(
            &path,
            format!(
                "{{\"t\":{old},\"m\":\"# TYPE requests_total counter\\nrequests_total 5\\n\"}}\n\
                 {{\"v\":2,\"t\":{boot},\"boot\":\"boot-a\",\"kind\":\"boot\",\"capacity\":{{\"enabled_lanes\":1,\"rpms\":[40],\"capacity_rpm\":40}}}}\n\
                 {{\"v\":2,\"t\":{baseline},\"boot\":\"boot-a\",\"capacity\":{{\"enabled_lanes\":1,\"rpms\":[40],\"capacity_rpm\":40}},\"m\":\"# TYPE requests_total counter\\nrequests_total 50\\n\"}}\n\
                 {{\"v\":2,\"t\":{retained_one},\"boot\":\"boot-a\",\"capacity\":{{\"enabled_lanes\":1,\"rpms\":[40],\"capacity_rpm\":40}},\"m\":\"# TYPE requests_total counter\\nrequests_total 60\\n\"}}\n\
                 {{\"v\":2,\"t\":{retained_two},\"boot\":\"boot-a\",\"capacity\":{{\"enabled_lanes\":1,\"rpms\":[40],\"capacity_rpm\":40}},\"m\":\"# TYPE requests_total counter\\nrequests_total 70\\n\"}}\n"
            ),
        )
        .unwrap();

        let history = Arc::new(History::load(Some(dir.0.clone()), 30, capacity(40)));
        let temporary = temporary_path(&path);
        fs::write(&temporary, "stale partial rewrite").unwrap();
        history.clone().reconfigure_retention(1, now);
        let expected = history.rollup(0, u64::MAX, 100).data.totals;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while history.status().compaction_pending {
            assert!(
                std::time::Instant::now() < deadline,
                "history compaction did not finish"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let contents = fs::read_to_string(&path).unwrap();
        let records: Vec<serde_json::Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(!records.iter().any(|record| record["t"] == old));
        assert!(records.iter().any(|record| record["t"] == baseline));
        assert!(records
            .iter()
            .any(|record| record["t"] == boot && record["kind"] == "boot"));
        assert!(records.iter().any(|record| record["t"] == retained_one));
        assert!(records.iter().any(|record| record["t"] == retained_two));
        assert!(
            !temporary.exists(),
            "successful rename consumes the temp file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let reloaded = History::load(Some(dir.0.clone()), 1, capacity(40));
        let after = reloaded.rollup(0, u64::MAX, 100);
        assert_eq!(after.data.available_from, Some(retained_one));
        assert_eq!(after.data.totals, expected);
    }

    #[tokio::test]
    async fn compaction_failure_preserves_file_and_retries_on_append() {
        let dir = TestDir::new();
        let path = dir.0.join("history.jsonl");
        fs::write(
            &path,
            "{\"t\":1,\"m\":\"# TYPE requests_total counter\\nrequests_total 5\\n\"}\n\
             {\"t\":2,\"m\":\"# TYPE requests_total counter\\nrequests_total 8\\n\"}\n",
        )
        .unwrap();
        let history = Arc::new(History::load(Some(dir.0.clone()), 30, capacity(40)));
        let original = fs::read_to_string(&path).unwrap();
        let temporary = temporary_path(&path);
        fs::create_dir(&temporary).unwrap();

        history.clone().reconfigure_retention(1, unix_now());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while history.compaction_running.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "failed compaction did not finish"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            history.status().compaction_pending,
            "a failed rewrite must remain pending for retry"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "a temp-file failure must not replace or truncate durable history"
        );

        fs::remove_dir(&temporary).unwrap();
        history.append(
            unix_now(),
            "# TYPE requests_total counter\nrequests_total 1\n",
            capacity(40),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while history.status().compaction_pending {
            assert!(
                std::time::Instant::now() < deadline,
                "history compaction retry did not finish"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let compacted = fs::read_to_string(&path).unwrap();
        assert!(!compacted.lines().any(|line| line.contains("\"t\":1,")));
        assert!(compacted.lines().any(|line| line.contains("\"t\":2,")));
        assert!(compacted
            .lines()
            .all(|line| { serde_json::from_str::<serde_json::Value>(line).is_ok() }));
    }

    #[test]
    fn compaction_directory_sync_error_is_committed_but_pending() {
        let outcome = directory_sync_outcome(Err(std::io::Error::other(
            "injected directory sync failure",
        )));
        assert!(
            matches!(outcome, CompactionOutcome::CommittedSyncPending(_)),
            "a post-rename sync error is not a pre-commit failure"
        );
        assert!(matches!(
            directory_sync_outcome(Ok(())),
            CompactionOutcome::Durable
        ));
    }

    #[test]
    fn load_disables_persistence_when_the_dir_cant_be_created() {
        let dir = TestDir::new();
        // A file sits where the history directory should be, so create_dir_all
        // fails and persistence falls back to in-memory only.
        let blocker = dir.0.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let h = History::load(Some(blocker.join("sub")), 1, capacity(40));
        assert!(
            h.file.is_none(),
            "persistence disabled on dir-create failure"
        );
    }

    #[test]
    fn load_disables_persistence_when_the_path_isnt_a_writable_file() {
        let dir = TestDir::new();
        // A directory sits where history.jsonl should be, so opening it for
        // append fails (EISDIR) and persistence stays in-memory.
        fs::create_dir_all(dir.0.join("history.jsonl")).unwrap();
        let h = History::load(Some(dir.0.clone()), 1, capacity(40));
        assert!(
            h.file.is_none(),
            "persistence disabled when the path isn't a file"
        );
    }

    #[test]
    fn append_survives_a_write_failure_without_panicking() {
        let dir = TestDir::new();
        // `file` points at a directory: the append write errors and is logged,
        // but the in-memory record still updates and nothing panics.
        let h = history_with_file(0, dir.0.clone());
        h.append(1, "snap", capacity(40));
        assert_eq!(
            h.inner.lock().unwrap().points.len(),
            1,
            "in-memory append still works"
        );
    }
}
