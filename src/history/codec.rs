//! Canonical `nimproxy-history/v1` record serialization and validation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

const FORMAT: &str = "nimproxy-history";
const VERSION: u64 = 1;

#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Boot(BootRecord),
    Sample(SampleRecord),
    Checkpoint(CheckpointRecord),
}

#[derive(Clone, Debug, PartialEq)]
pub struct BootRecord {
    pub timestamp: u64,
    pub boot_id: String,
    pub capacity: Capacity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleRecord {
    pub timestamp: u64,
    pub boot_id: String,
    pub capacity: Capacity,
    pub state: Vec<StateEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckpointRecord {
    pub timestamp: u64,
    pub boot_id: String,
    pub capacity: Capacity,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Capacity {
    pub capacity_rpm: usize,
    pub enabled_keys: usize,
    pub key_rpms: Vec<usize>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StateKind {
    Counter,
    Gauge,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateEntry {
    pub kind: StateKind,
    pub metric: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidUtf8,
    InvalidJson,
    UnsupportedFormat,
    UnsupportedVersion,
    InvalidRecord,
    InvalidRecordKind,
    InvalidState,
    DuplicateSeries,
}

impl DecodeError {
    #[must_use]
    pub const fn diagnostic(&self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "invalid_utf8",
            Self::InvalidJson => "invalid_json",
            Self::UnsupportedFormat => "unsupported_format",
            Self::UnsupportedVersion => "unsupported_version",
            Self::InvalidRecord => "invalid_record",
            Self::InvalidRecordKind => "invalid_record_kind",
            Self::InvalidState => "invalid_state",
            Self::DuplicateSeries => "duplicate_series",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    InvalidState,
    DuplicateSeries,
}

#[derive(Deserialize)]
struct RawRecord {
    format: String,
    v: u64,
    kind: String,
    timestamp: u64,
    boot_id: String,
    capacity: Capacity,
    #[serde(default)]
    state: StateField,
}

#[derive(Default)]
enum StateField {
    #[default]
    Missing,
    Null,
    Entries(Vec<RawStateEntry>),
}

impl<'de> Deserialize<'de> for StateField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<Vec<RawStateEntry>>::deserialize(deserializer)
            .map(|state| state.map_or(Self::Null, Self::Entries))
    }
}

#[derive(Deserialize)]
struct RawStateEntry {
    kind: String,
    metric: String,
    labels: BTreeMap<String, String>,
    value: f64,
}

pub fn decode_record(line: &[u8]) -> Result<Record, DecodeError> {
    if std::str::from_utf8(line).is_err() {
        return Err(DecodeError::InvalidUtf8);
    }
    let raw: RawRecord = serde_json::from_slice(line).map_err(|error| {
        if error.is_eof()
            || (error.is_syntax() && !error.to_string().contains("number out of range"))
        {
            DecodeError::InvalidJson
        } else {
            DecodeError::InvalidRecord
        }
    })?;
    if raw.format != FORMAT {
        return Err(DecodeError::UnsupportedFormat);
    }
    if raw.v != VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }

    let base = BootRecord {
        timestamp: raw.timestamp,
        boot_id: raw.boot_id,
        capacity: raw.capacity,
    };
    match raw.kind.as_str() {
        "boot" if matches!(&raw.state, StateField::Missing) => Ok(Record::Boot(base)),
        "checkpoint" if matches!(&raw.state, StateField::Missing) => {
            Ok(Record::Checkpoint(CheckpointRecord {
                timestamp: base.timestamp,
                boot_id: base.boot_id,
                capacity: base.capacity,
            }))
        }
        "sample" => Ok(Record::Sample(SampleRecord {
            timestamp: base.timestamp,
            boot_id: base.boot_id,
            capacity: base.capacity,
            state: decode_state(match raw.state {
                StateField::Entries(state) => state,
                StateField::Missing | StateField::Null => return Err(DecodeError::InvalidRecord),
            })?,
        })),
        "boot" | "checkpoint" => Err(DecodeError::InvalidRecord),
        _ => Err(DecodeError::InvalidRecordKind),
    }
}

pub fn encode_record(record: &Record) -> Result<Vec<u8>, EncodeError> {
    let encoded = match record {
        Record::Boot(record) => serde_json::to_vec(&EncodedBoot {
            format: FORMAT,
            v: VERSION,
            kind: "boot",
            timestamp: record.timestamp,
            boot_id: &record.boot_id,
            capacity: &record.capacity,
        }),
        Record::Sample(record) => serde_json::to_vec(&EncodedSample {
            format: FORMAT,
            v: VERSION,
            kind: "sample",
            timestamp: record.timestamp,
            boot_id: &record.boot_id,
            capacity: &record.capacity,
            state: encode_state(&record.state)?,
        }),
        Record::Checkpoint(record) => serde_json::to_vec(&EncodedCheckpoint {
            format: FORMAT,
            v: VERSION,
            kind: "checkpoint",
            timestamp: record.timestamp,
            boot_id: &record.boot_id,
            capacity: &record.capacity,
        }),
    };
    encoded.map_err(|_| EncodeError::InvalidState)
}

fn decode_state(raw: Vec<RawStateEntry>) -> Result<Vec<StateEntry>, DecodeError> {
    let mut series = BTreeSet::new();
    raw.into_iter()
        .map(|entry| {
            let kind = match entry.kind.as_str() {
                "counter" => StateKind::Counter,
                "gauge" => StateKind::Gauge,
                _ => return Err(DecodeError::InvalidState),
            };
            if !entry.value.is_finite() {
                return Err(DecodeError::InvalidState);
            }
            let key = (kind.clone(), entry.metric.clone(), entry.labels.clone());
            if !series.insert(key) {
                return Err(DecodeError::DuplicateSeries);
            }
            Ok(StateEntry {
                kind,
                metric: entry.metric,
                labels: entry.labels,
                value: entry.value,
            })
        })
        .collect()
}

fn encode_state(state: &[StateEntry]) -> Result<Vec<EncodedStateEntry<'_>>, EncodeError> {
    let mut series = BTreeSet::new();
    state
        .iter()
        .map(|entry| {
            if !entry.value.is_finite() {
                return Err(EncodeError::InvalidState);
            }
            let key = (
                entry.kind.clone(),
                entry.metric.clone(),
                entry.labels.clone(),
            );
            if !series.insert(key) {
                return Err(EncodeError::DuplicateSeries);
            }
            Ok(EncodedStateEntry {
                kind: match entry.kind {
                    StateKind::Counter => "counter",
                    StateKind::Gauge => "gauge",
                },
                metric: &entry.metric,
                labels: &entry.labels,
                value: entry.value,
            })
        })
        .collect()
}

#[derive(Serialize)]
struct EncodedBoot<'a> {
    format: &'a str,
    v: u64,
    kind: &'a str,
    timestamp: u64,
    boot_id: &'a str,
    capacity: &'a Capacity,
}

#[derive(Serialize)]
struct EncodedSample<'a> {
    format: &'a str,
    v: u64,
    kind: &'a str,
    timestamp: u64,
    boot_id: &'a str,
    capacity: &'a Capacity,
    state: Vec<EncodedStateEntry<'a>>,
}

#[derive(Serialize)]
struct EncodedCheckpoint<'a> {
    format: &'a str,
    v: u64,
    kind: &'a str,
    timestamp: u64,
    boot_id: &'a str,
    capacity: &'a Capacity,
}

#[derive(Serialize)]
struct EncodedStateEntry<'a> {
    kind: &'a str,
    metric: &'a str,
    labels: &'a BTreeMap<String, String>,
    value: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_records_have_the_locked_field_order() {
        for fixture in [
            "valid-boot.json",
            "valid-sample.json",
            "valid-checkpoint.json",
        ] {
            let path = format!("tests/fixtures/history-v1/{fixture}");
            let expected = std::fs::read(path).expect("fixture must exist");
            let record = decode_record(&expected).expect("the sanitized golden record must decode");
            assert_eq!(
                encode_record(&record).expect("the decoded record must encode"),
                expected.strip_suffix(b"\n").unwrap_or(&expected),
            );
        }
    }

    #[test]
    fn negative_fixtures_report_their_declared_diagnostic() {
        for (fixture, expected) in [
            ("truncated-json.json", "invalid_json"),
            ("invalid-utf8.bin", "invalid_utf8"),
            ("wrong-scalar-type.json", "invalid_record"),
            ("non-finite-number.json", "invalid_record"),
            ("duplicate-semantic-series.json", "duplicate_series"),
            ("unknown-state-kind.json", "invalid_state"),
            ("state-null-on-boot.json", "invalid_record"),
            ("unknown-record-kind.json", "invalid_record_kind"),
            ("unknown-format.json", "unsupported_format"),
            ("unknown-version.json", "unsupported_version"),
        ] {
            let path = format!("tests/fixtures/history-v1/{fixture}");
            let bytes = std::fs::read(path).expect("fixture must exist");
            assert_eq!(decode_record(&bytes).unwrap_err().diagnostic(), expected);
        }
    }

    #[test]
    fn reordered_non_state_fields_decode_but_reencode_canonically() {
        let record = decode_record(include_bytes!(
            "../../tests/fixtures/history-v1/reordered-object-fields.json"
        ))
        .expect("unknown non-state fields are ignored");
        assert_eq!(
            encode_record(&record).expect("the valid record must encode"),
            include_bytes!("../../tests/fixtures/history-v1/valid-boot.json")
                .strip_suffix(b"\n")
                .unwrap(),
        );
    }
}
