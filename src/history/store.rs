//! Durable publication and append boundary for canonical history-v1.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::codec::{
    decode_record, encode_record, BootRecord, Capacity, CheckpointRecord, DecodeError, Record,
    SampleRecord, StateEntry,
};
use super::HistoryDiagnostics;

const CANONICAL_NAME: &str = "history-v1.jsonl";
const TEMP_PREFIX: &str = "history-v1.jsonl.tmp-";

#[derive(Debug)]
pub struct HistoryStore {
    file: File,
    path: PathBuf,
    boot_id: String,
    last_state: Option<Vec<StateEntry>>,
    replay: Replay,
    last_timestamp: Option<u64>,
    poisoned: bool,
    #[cfg(test)]
    test_compaction_directory_sync_failure: bool,
    #[cfg(test)]
    test_runtime_partial_append_once: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Replay {
    pub records: Vec<Record>,
    pub diagnostics: HistoryDiagnostics,
    pub diagnostic_events: Vec<DiagnosticEvent>,
    pub gaps: Vec<RecoveryGap>,
    pub ordering_bound: Option<u64>,
    pub needs_separator: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RecoveryGap {
    pub from: u64,
    pub to: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DiagnosticEvent {
    pub at: u64,
    pub diagnostics: HistoryDiagnostics,
}

#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    EmptyCanonical,
    InvalidCanonical { line: usize, error: DecodeError },
    UnterminatedRecord,
    InvalidStream { line: usize, reason: &'static str },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "history store I/O error: {error}"),
            Self::EmptyCanonical => formatter.write_str("canonical history is empty"),
            Self::InvalidCanonical { line, error } => write!(
                formatter,
                "invalid canonical history at line {line}: {}",
                error.diagnostic()
            ),
            Self::UnterminatedRecord => {
                formatter.write_str("canonical history ends with an unterminated record")
            }
            Self::InvalidStream { line, reason } => {
                write!(
                    formatter,
                    "invalid canonical history stream at line {line}: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for OpenError {}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<WriteError> for OpenError {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::Io(error) => Self::Io(error),
            WriteError::Encode => Self::Io(io::Error::other("canonical history encode failure")),
            WriteError::Poisoned => {
                Self::Io(io::Error::other("canonical history store is poisoned"))
            }
            WriteError::TimestampRegression => {
                Self::Io(io::Error::other("canonical history timestamp regresses"))
            }
        }
    }
}

#[derive(Debug)]
pub enum WriteError {
    Encode,
    Io(io::Error),
    Poisoned,
    TimestampRegression,
}

#[derive(Debug)]
pub(crate) enum CompactionOutcome {
    Durable,
    Deferred,
    Superseded,
    CommittedSyncPending(io::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode => formatter.write_str("cannot encode canonical history record"),
            Self::Io(error) => write!(formatter, "history store I/O error: {error}"),
            Self::Poisoned => {
                formatter.write_str("canonical history store is poisoned after a failed append")
            }
            Self::TimestampRegression => {
                formatter.write_str("canonical history timestamp regresses")
            }
        }
    }
}

impl std::error::Error for WriteError {}

impl HistoryStore {
    pub fn open(data_dir: &Path, timestamp: u64, capacity: Capacity) -> Result<Self, OpenError> {
        Self::open_inner(data_dir, timestamp, capacity, None)
    }

    fn open_inner(
        data_dir: &Path,
        timestamp: u64,
        capacity: Capacity,
        mut failure: Option<&mut FailureSwitch>,
    ) -> Result<Self, OpenError> {
        let path = data_dir.join(CANONICAL_NAME);
        let boot_id = new_boot_id();
        let boot = Record::Boot(BootRecord {
            timestamp,
            boot_id: boot_id.clone(),
            capacity,
        });
        let boot_timestamp = record_timestamp(&boot);
        match OpenOptions::new().read(true).open(&path) {
            Ok(_) => {
                let mut replay = validate_canonical(&path)?;
                ensure_open_timestamp(replay.ordering_bound, &boot)?;
                let mut file = OpenOptions::new().append(true).open(&path)?;
                append_recovery_separator(&mut file, replay.needs_separator)?;
                append_record(&mut file, &boot, failure.as_deref_mut(), true)?;
                replay.records.push(boot);
                close_open_gap(&mut replay, boot_timestamp);
                let last_timestamp = replay.records.last().map(record_timestamp);
                Ok(Self {
                    file,
                    path: path.clone(),
                    boot_id,
                    last_state: None,
                    replay,
                    last_timestamp,
                    poisoned: false,
                    #[cfg(test)]
                    test_compaction_directory_sync_failure: false,
                    #[cfg(test)]
                    test_runtime_partial_append_once: false,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(data_dir)?;
                match publish_first_boot(&path, &boot, failure.as_deref_mut()) {
                    Ok(()) => {
                        let file = OpenOptions::new().append(true).open(&path)?;
                        let last_timestamp = Some(record_timestamp(&boot));
                        Ok(Self {
                            file,
                            path: path.clone(),
                            boot_id,
                            last_state: None,
                            replay: Replay {
                                records: vec![boot],
                                ..Replay::default()
                            },
                            last_timestamp,
                            poisoned: false,
                            #[cfg(test)]
                            test_compaction_directory_sync_failure: false,
                            #[cfg(test)]
                            test_runtime_partial_append_once: false,
                        })
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let mut replay = validate_canonical(&path)?;
                        ensure_open_timestamp(replay.ordering_bound, &boot)?;
                        let mut file = OpenOptions::new().append(true).open(&path)?;
                        append_recovery_separator(&mut file, replay.needs_separator)?;
                        append_record(&mut file, &boot, failure, true)?;
                        replay.records.push(boot);
                        close_open_gap(&mut replay, boot_timestamp);
                        let last_timestamp = replay.records.last().map(record_timestamp);
                        Ok(Self {
                            file,
                            path: path.clone(),
                            boot_id,
                            last_state: None,
                            replay,
                            last_timestamp,
                            poisoned: false,
                            #[cfg(test)]
                            test_compaction_directory_sync_failure: false,
                            #[cfg(test)]
                            test_runtime_partial_append_once: false,
                        })
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn append_sample(
        &mut self,
        timestamp: u64,
        capacity: Capacity,
        state: Vec<StateEntry>,
    ) -> Result<(), WriteError> {
        #[cfg(test)]
        {
            let mut failure = std::mem::take(&mut self.test_runtime_partial_append_once).then_some(
                FailureSwitch {
                    point: FailurePoint::RuntimePartialAppend,
                    fired: None,
                },
            );
            self.append_sample_inner(timestamp, capacity, state, failure.as_mut())
        }
        #[cfg(not(test))]
        {
            self.append_sample_inner(timestamp, capacity, state, None)
        }
    }

    fn append_sample_inner(
        &mut self,
        timestamp: u64,
        capacity: Capacity,
        state: Vec<StateEntry>,
        mut failure: Option<&mut FailureSwitch>,
    ) -> Result<(), WriteError> {
        if self.poisoned {
            return Err(WriteError::Poisoned);
        }
        if self.last_state.as_ref() == Some(&state) {
            return self.checkpoint_inner(timestamp, capacity, failure.take());
        }
        let record = Record::Sample(SampleRecord {
            timestamp,
            boot_id: self.boot_id.clone(),
            capacity,
            state: state.clone(),
        });
        ensure_runtime_timestamp(self.last_timestamp, &record)?;
        if let Err(error) = append_record(&mut self.file, &record, failure, false) {
            self.poisoned = true;
            return Err(error);
        }
        self.last_timestamp = Some(record_timestamp(&record));
        self.last_state = Some(state);
        Ok(())
    }

    #[allow(dead_code)] // Retained for direct store checks; append_sample forwards injected failures below.
    pub fn checkpoint(&mut self, timestamp: u64, capacity: Capacity) -> Result<(), WriteError> {
        self.checkpoint_inner(timestamp, capacity, None)
    }

    #[cfg(test)]
    pub(crate) fn compact(&mut self, cutoff: u64) -> io::Result<CompactionOutcome> {
        self.compact_inner(cutoff, None)
    }

    pub(crate) fn compact_if_authorized<T>(
        &mut self,
        cutoff: u64,
        authorize: impl FnOnce() -> Option<T>,
    ) -> io::Result<CompactionOutcome> {
        #[cfg(test)]
        if std::mem::take(&mut self.test_compaction_directory_sync_failure) {
            let mut failure = FailureSwitch {
                point: FailurePoint::CompactionDirectorySync,
                fired: None,
            };
            return self.compact_inner_if_authorized(cutoff, Some(&mut failure), authorize);
        }
        self.compact_inner_if_authorized(cutoff, None, authorize)
    }

    #[cfg(test)]
    pub(crate) fn inject_test_compaction_directory_sync_failure(&mut self) {
        self.test_compaction_directory_sync_failure = true;
    }

    pub(crate) fn compaction_needed(&self, cutoff: u64) -> bool {
        if !self.replay.gaps.is_empty()
            || self.replay.needs_separator
            || self.replay.diagnostics.excluded_epochs != 0
            || self.replay.diagnostics.excluded_records != 0
        {
            return true;
        }
        let (records, fresh_boot) = match self.replay.records.last() {
            Some(Record::Boot(boot))
                if boot.boot_id == self.boot_id && self.last_state.is_none() =>
            {
                (
                    &self.replay.records[..self.replay.records.len() - 1],
                    Some(boot.clone()),
                )
            }
            Some(_) | None => (&self.replay.records[..], None),
        };
        let mut retained = retained_compaction_records(records, cutoff, &self.boot_id, false);
        if let Some(boot) = fresh_boot {
            retained.push(Record::Boot(boot));
        }
        retained != self.replay.records
    }

    #[cfg(test)]
    fn compact_inner(
        &mut self,
        cutoff: u64,
        failure: Option<&mut FailureSwitch>,
    ) -> io::Result<CompactionOutcome> {
        self.compact_inner_if_authorized(cutoff, failure, || Some(()))
    }

    fn compact_inner_if_authorized<T>(
        &mut self,
        cutoff: u64,
        mut failure: Option<&mut FailureSwitch>,
        authorize: impl FnOnce() -> Option<T>,
    ) -> io::Result<CompactionOutcome> {
        let replay = validate_canonical(&self.path).map_err(open_error_to_io)?;
        if replay.gaps.iter().any(|gap| gap.to > cutoff) || self.last_state.is_none() {
            return Ok(CompactionOutcome::Deferred);
        }

        let retained = retained_compaction_records(
            &replay.records,
            cutoff,
            &self.boot_id,
            self.last_state.is_some(),
        );
        let directory = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("history path has no parent"))?
            .to_path_buf();
        let (temporary, mut output) =
            create_temporary(&directory, &mut failure, FailurePoint::CompactionTempCreate)?;
        let prepared = (|| -> io::Result<Option<(T, Replay)>> {
            for record in &retained {
                write_record(
                    &mut output,
                    record,
                    failure.as_deref_mut(),
                    Some(FailurePoint::CompactionTempWrite),
                )
                .map_err(write_error_to_io)?;
            }
            output.flush()?;
            fail(&mut failure, FailurePoint::CompactionTempSync)?;
            output.sync_all()?;
            let replacement_replay = validate_canonical(&temporary).map_err(open_error_to_io)?;
            if replacement_replay.records != retained
                || !replacement_replay.gaps.is_empty()
                || replacement_replay.needs_separator
                || replacement_replay.diagnostics.excluded_epochs != 0
                || replacement_replay.diagnostics.excluded_records != 0
            {
                return Err(io::Error::other(
                    "compaction replacement does not exactly validate as retained canonical history",
                ));
            }
            let Some(authorization) = authorize() else {
                return Ok(None);
            };
            fail(&mut failure, FailurePoint::CompactionRename)?;
            fs::rename(&temporary, &self.path)?;
            Ok(Some((authorization, replacement_replay)))
        })();
        let (authorization, replacement_replay) = match prepared {
            Err(error) => {
                drop(output);
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
            Ok(None) => {
                drop(output);
                let _ = fs::remove_file(&temporary);
                return Ok(CompactionOutcome::Superseded);
            }
            Ok(Some((authorization, replacement_replay))) => (authorization, replacement_replay),
        };
        drop(authorization);

        self.file = output;
        self.last_timestamp = replacement_replay.records.last().map(record_timestamp);
        self.last_state = replacement_replay
            .records
            .iter()
            .rev()
            .find_map(|record| match record {
                Record::Sample(sample) if sample.boot_id == self.boot_id => {
                    Some(sample.state.clone())
                }
                Record::Boot(_) | Record::Sample(_) | Record::Checkpoint(_) => None,
            });
        self.replay = replacement_replay;

        match fail(&mut failure, FailurePoint::CompactionDirectorySync)
            .and_then(|()| sync_directory(&directory))
        {
            Ok(()) => Ok(CompactionOutcome::Durable),
            Err(error) => Ok(CompactionOutcome::CommittedSyncPending(error)),
        }
    }

    fn checkpoint_inner(
        &mut self,
        timestamp: u64,
        capacity: Capacity,
        failure: Option<&mut FailureSwitch>,
    ) -> Result<(), WriteError> {
        if self.poisoned {
            return Err(WriteError::Poisoned);
        }
        let record = Record::Checkpoint(CheckpointRecord {
            timestamp,
            boot_id: self.boot_id.clone(),
            capacity,
        });
        ensure_runtime_timestamp(self.last_timestamp, &record)?;
        if let Err(error) = append_record(&mut self.file, &record, failure, false) {
            self.poisoned = true;
            return Err(error);
        }
        self.last_timestamp = Some(record_timestamp(&record));
        Ok(())
    }

    pub(crate) fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub(crate) fn last_timestamp(&self) -> Option<u64> {
        self.last_timestamp
    }

    pub(crate) fn poisoned(&self) -> bool {
        self.poisoned
    }

    #[cfg(test)]
    pub(crate) fn arm_test_runtime_partial_append_once(&mut self) {
        self.test_runtime_partial_append_once = true;
    }

    pub(crate) fn take_replay(&mut self) -> Replay {
        std::mem::take(&mut self.replay)
    }

    pub(crate) fn file_bytes(&self) -> io::Result<u64> {
        self.file.metadata().map(|metadata| metadata.len())
    }
}

pub(crate) fn stale_temporary_count(directory: &Path) -> io::Result<usize> {
    fs::read_dir(directory)?.try_fold(0, |count, entry| {
        let name = entry?.file_name();
        Ok(count + usize::from(name.to_string_lossy().starts_with(TEMP_PREFIX)))
    })
}

fn validate_canonical(path: &Path) -> Result<Replay, OpenError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut replay = Replay::default();
    let mut state = ScannerState::AwaitBoot;
    let mut line = Vec::new();
    let mut index = 0;
    let mut saw_nonblank = false;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        index += 1;
        if line.last() != Some(&b'\n') {
            saw_nonblank |= !line.iter().all(u8::is_ascii_whitespace);
            if let Err(error @ (DecodeError::UnsupportedFormat | DecodeError::UnsupportedVersion)) =
                decode_record(&line)
            {
                return Err(OpenError::InvalidCanonical { line: index, error });
            }
            let at = trustworthy_timestamp(&line);
            if let Some(timestamp) = at {
                advance_ordering_bound(&mut replay, timestamp);
            }
            replay.needs_separator = !line.iter().all(u8::is_ascii_whitespace);
            let decision_at = at.or(replay.ordering_bound).unwrap_or(0);
            invalidate(&mut state, &mut replay, decision_at);
            break;
        }
        line.pop();
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        saw_nonblank = true;
        let record = match decode_record(&line) {
            Ok(record) => record,
            Err(error @ (DecodeError::UnsupportedFormat | DecodeError::UnsupportedVersion)) => {
                return Err(OpenError::InvalidCanonical { line: index, error });
            }
            Err(_) => {
                let at = trustworthy_timestamp(&line);
                if let Some(timestamp) = at {
                    advance_ordering_bound(&mut replay, timestamp);
                }
                let decision_at = at.or(replay.ordering_bound).unwrap_or(0);
                invalidate(&mut state, &mut replay, decision_at);
                continue;
            }
        };
        let timestamp = record_timestamp(&record);
        if replay.ordering_bound.is_some_and(|bound| timestamp < bound) {
            invalidate(&mut state, &mut replay, timestamp);
            continue;
        }
        advance_ordering_bound(&mut replay, timestamp);
        if matches!(record, Record::Boot(_)) {
            start_boot(&mut state, &mut replay, record);
            continue;
        }
        ingest_record(&mut state, &mut replay, record);
    }
    match state {
        ScannerState::Usable(epoch) => commit_epoch(epoch, &mut replay),
        ScannerState::AwaitSample(epoch) => exclude_epoch(epoch, &mut replay, None),
        ScannerState::AwaitBoot | ScannerState::InvalidEpoch => {}
    }
    saw_nonblank
        .then_some(replay)
        .ok_or(OpenError::EmptyCanonical)
}

#[derive(Debug)]
enum ScannerState {
    AwaitBoot,
    AwaitSample(Epoch),
    Usable(Epoch),
    InvalidEpoch,
}

#[derive(Debug)]
struct Epoch {
    boot_id: String,
    from: u64,
    records: Vec<Record>,
    state: Option<Vec<StateEntry>>,
    diagnostic_events: Vec<DiagnosticEvent>,
}

impl Epoch {
    fn new(record: Record) -> Self {
        let Record::Boot(boot) = &record else {
            unreachable!()
        };
        Self {
            boot_id: boot.boot_id.clone(),
            from: boot.timestamp,
            records: vec![record],
            state: None,
            diagnostic_events: Vec::new(),
        }
    }
}

fn start_boot(state: &mut ScannerState, replay: &mut Replay, record: Record) {
    let previous = std::mem::replace(state, ScannerState::AwaitBoot);
    match previous {
        ScannerState::AwaitSample(epoch) => exclude_epoch(epoch, replay, None),
        ScannerState::Usable(epoch) => commit_epoch(epoch, replay),
        ScannerState::AwaitBoot | ScannerState::InvalidEpoch => {}
    }
    close_open_gap(replay, record_timestamp(&record));
    *state = ScannerState::AwaitSample(Epoch::new(record));
}

fn ingest_record(state: &mut ScannerState, replay: &mut Replay, record: Record) {
    let timestamp = record_timestamp(&record);
    let boot_id = record_boot_id(&record);
    let previous = std::mem::replace(state, ScannerState::AwaitBoot);
    match previous {
        ScannerState::AwaitSample(mut epoch) if epoch.boot_id == boot_id => match record {
            Record::Sample(sample) => {
                add_sample(&mut epoch, &sample);
                epoch.records.push(Record::Sample(sample));
                *state = ScannerState::Usable(epoch);
            }
            Record::Checkpoint(_) => {
                exclude_epoch(epoch, replay, Some(timestamp));
                *state = ScannerState::InvalidEpoch;
            }
            Record::Boot(_) => unreachable!(),
        },
        ScannerState::Usable(mut epoch) if epoch.boot_id == boot_id => match record {
            Record::Sample(sample) => {
                add_sample(&mut epoch, &sample);
                epoch.records.push(Record::Sample(sample));
                *state = ScannerState::Usable(epoch);
            }
            Record::Checkpoint(checkpoint) => {
                add_checkpoint(&mut epoch, &checkpoint);
                epoch.records.push(Record::Checkpoint(checkpoint));
                *state = ScannerState::Usable(epoch);
            }
            Record::Boot(_) => unreachable!(),
        },
        ScannerState::AwaitSample(epoch) | ScannerState::Usable(epoch) => {
            exclude_epoch(epoch, replay, Some(timestamp));
            *state = ScannerState::InvalidEpoch;
        }
        ScannerState::AwaitBoot | ScannerState::InvalidEpoch => {
            exclude_record(replay, timestamp);
            open_gap(replay, timestamp);
            *state = ScannerState::InvalidEpoch;
        }
    }
}

fn add_sample(epoch: &mut Epoch, sample: &SampleRecord) {
    epoch.state = Some(sample.state.clone());
    epoch.diagnostic_events.push(DiagnosticEvent {
        at: sample.timestamp,
        diagnostics: HistoryDiagnostics {
            normalized_series: sample.state.len(),
            valid_samples: 1,
            ..HistoryDiagnostics::default()
        },
    });
}

fn add_checkpoint(epoch: &mut Epoch, checkpoint: &CheckpointRecord) {
    let normalized_series = epoch.state.as_ref().map_or(0, Vec::len);
    epoch.diagnostic_events.push(DiagnosticEvent {
        at: checkpoint.timestamp,
        diagnostics: HistoryDiagnostics {
            normalized_series,
            valid_checkpoints: 1,
            ..HistoryDiagnostics::default()
        },
    });
}

fn invalidate(state: &mut ScannerState, replay: &mut Replay, at: u64) {
    let previous = std::mem::replace(state, ScannerState::InvalidEpoch);
    match previous {
        ScannerState::AwaitSample(epoch) | ScannerState::Usable(epoch) => {
            exclude_epoch(epoch, replay, Some(at));
        }
        ScannerState::AwaitBoot | ScannerState::InvalidEpoch => {
            exclude_record(replay, at);
            open_gap(replay, at);
        }
    }
}

fn exclude_epoch(epoch: Epoch, replay: &mut Replay, damaging_timestamp: Option<u64>) {
    let diagnostics = HistoryDiagnostics {
        excluded_epochs: 1,
        ..HistoryDiagnostics::default()
    };
    add_diagnostics(&mut replay.diagnostics, &diagnostics);
    replay.diagnostic_events.push(DiagnosticEvent {
        at: epoch.from,
        diagnostics,
    });
    for record in epoch.records {
        exclude_record(replay, record_timestamp(&record));
    }
    if let Some(timestamp) = damaging_timestamp {
        exclude_record(replay, timestamp);
    }
    open_gap(replay, epoch.from);
}

fn exclude_record(replay: &mut Replay, at: u64) {
    let diagnostics = HistoryDiagnostics {
        excluded_records: 1,
        ..HistoryDiagnostics::default()
    };
    add_diagnostics(&mut replay.diagnostics, &diagnostics);
    replay
        .diagnostic_events
        .push(DiagnosticEvent { at, diagnostics });
}

fn commit_epoch(epoch: Epoch, replay: &mut Replay) {
    for event in epoch.diagnostic_events {
        let diagnostics = event.diagnostics.clone();
        add_diagnostics(&mut replay.diagnostics, &diagnostics);
        replay.diagnostic_events.push(event);
    }
    replay.records.extend(epoch.records);
}

fn open_gap(replay: &mut Replay, from: u64) {
    if let Some(gap) = replay.gaps.last_mut().filter(|gap| gap.to == u64::MAX) {
        gap.from = gap.from.min(from);
    } else {
        replay.gaps.push(RecoveryGap { from, to: u64::MAX });
    }
}

fn close_open_gap(replay: &mut Replay, timestamp: u64) {
    if let Some(gap) = replay.gaps.iter_mut().rev().find(|gap| gap.to == u64::MAX) {
        gap.to = timestamp;
    }
}

fn record_boot_id(record: &Record) -> &str {
    match record {
        Record::Boot(record) => &record.boot_id,
        Record::Sample(record) => &record.boot_id,
        Record::Checkpoint(record) => &record.boot_id,
    }
}

fn trustworthy_timestamp(line: &[u8]) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(line)
        .ok()
        .and_then(|record| record.get("timestamp")?.as_u64())
}

fn advance_ordering_bound(replay: &mut Replay, timestamp: u64) {
    replay.ordering_bound = Some(
        replay
            .ordering_bound
            .map_or(timestamp, |previous| previous.max(timestamp)),
    );
}

fn add_diagnostics(total: &mut HistoryDiagnostics, delta: &HistoryDiagnostics) {
    total.excluded_epochs += delta.excluded_epochs;
    total.excluded_records += delta.excluded_records;
    total.normalized_series += delta.normalized_series;
    total.skipped_metric_lines += delta.skipped_metric_lines;
    total.valid_checkpoints += delta.valid_checkpoints;
    total.valid_samples += delta.valid_samples;
}

fn record_timestamp(record: &Record) -> u64 {
    match record {
        Record::Boot(record) => record.timestamp,
        Record::Sample(record) => record.timestamp,
        Record::Checkpoint(record) => record.timestamp,
    }
}

fn ensure_open_timestamp(last_timestamp: Option<u64>, next: &Record) -> Result<(), OpenError> {
    if last_timestamp.is_some_and(|previous| record_timestamp(next) < previous) {
        return Err(OpenError::InvalidStream {
            line: 0,
            reason: "timestamp regresses",
        });
    }
    Ok(())
}

fn ensure_runtime_timestamp(last_timestamp: Option<u64>, next: &Record) -> Result<(), WriteError> {
    if last_timestamp.is_some_and(|previous| record_timestamp(next) < previous) {
        return Err(WriteError::TimestampRegression);
    }
    Ok(())
}

fn append_recovery_separator(file: &mut File, needs_separator: bool) -> Result<(), OpenError> {
    if !needs_separator {
        return Ok(());
    }
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn publish_first_boot(
    path: &Path,
    boot: &Record,
    mut failure: Option<&mut FailureSwitch>,
) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("history path has no parent"))?;
    let (temporary, mut file) =
        create_temporary(directory, &mut failure, FailurePoint::TempCreate)?;
    write_record(
        &mut file,
        boot,
        failure.as_deref_mut(),
        Some(FailurePoint::TempWrite),
    )
    .map_err(write_error_to_io)?;
    file.flush()?;
    fail(&mut failure, FailurePoint::TempSync)?;
    file.sync_all()?;
    fail(&mut failure, FailurePoint::HardLink)?;
    fs::hard_link(&temporary, path)?;
    fs::remove_file(&temporary)?;
    fail(&mut failure, FailurePoint::ParentDirectorySync)?;
    sync_directory(directory)
}

fn create_temporary(
    directory: &Path,
    failure: &mut Option<&mut FailureSwitch>,
    failure_point: FailurePoint,
) -> io::Result<(PathBuf, File)> {
    for _ in 0..32 {
        let path = directory.join(format!("{TEMP_PREFIX}{}", new_boot_id()));
        fail(failure, failure_point)?;
        let mut options = OpenOptions::new();
        options.append(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unique history temporary unavailable",
    ))
}

fn retained_compaction_records(
    records: &[Record],
    cutoff: u64,
    live_boot_id: &str,
    live_epoch_usable: bool,
) -> Vec<Record> {
    let mut retained = Vec::new();
    let mut start = 0;
    while start < records.len() {
        let end = records[start + 1..]
            .iter()
            .position(|record| matches!(record, Record::Boot(_)))
            .map_or(records.len(), |offset| start + 1 + offset);
        let epoch = &records[start..end];
        let is_live_epoch = live_epoch_usable
            && matches!(epoch.first(), Some(Record::Boot(boot)) if boot.boot_id == live_boot_id);
        let has_retained_observation = epoch
            .iter()
            .any(|record| record_timestamp(record) >= cutoff);
        if is_live_epoch || has_retained_observation {
            let baseline = epoch
                .iter()
                .enumerate()
                .filter(|(_, record)| {
                    matches!(record, Record::Sample(sample) if sample.timestamp < cutoff)
                })
                .map(|(index, _)| index)
                .next_back();
            retained.extend(
                epoch
                    .iter()
                    .enumerate()
                    .filter(|&(index, record)| {
                        index == 0 || Some(index) == baseline || record_timestamp(record) >= cutoff
                    })
                    .map(|(_, record)| record.clone()),
            );
        }
        start = end;
    }
    retained
}

fn append_record(
    file: &mut File,
    record: &Record,
    mut failure: Option<&mut FailureSwitch>,
    startup_append: bool,
) -> Result<(), WriteError> {
    if startup_append {
        fail(&mut failure, FailurePoint::RestartBootAppend).map_err(WriteError::Io)?;
    }
    #[cfg(test)]
    if failure
        .as_deref()
        .is_some_and(|failure| failure.point == FailurePoint::RuntimePartialAppend)
    {
        return write_partial_record(file, record, &mut failure);
    }
    write_record(file, record, failure.as_deref_mut(), None)?;
    file.flush().map_err(WriteError::Io)?;
    if startup_append {
        fail(&mut failure, FailurePoint::RestartBootSync).map_err(WriteError::Io)?;
    }
    file.sync_all().map_err(WriteError::Io)
}

#[cfg(test)]
fn write_partial_record(
    file: &mut File,
    record: &Record,
    failure: &mut Option<&mut FailureSwitch>,
) -> Result<(), WriteError> {
    let encoded = encode_record(record).map_err(|_| WriteError::Encode)?;
    let partial = encoded.len().max(1) / 2;
    file.write_all(&encoded[..partial])
        .map_err(WriteError::Io)?;
    file.flush().map_err(WriteError::Io)?;
    fail(failure, FailurePoint::RuntimePartialAppend).map_err(WriteError::Io)
}

fn write_record(
    file: &mut File,
    record: &Record,
    mut failure: Option<&mut FailureSwitch>,
    failure_point: Option<FailurePoint>,
) -> Result<(), WriteError> {
    if let Some(point) = failure_point {
        fail(&mut failure, point).map_err(WriteError::Io)?;
    }
    let encoded = encode_record(record).map_err(|_| WriteError::Encode)?;
    file.write_all(&encoded).map_err(WriteError::Io)?;
    file.write_all(b"\n").map_err(WriteError::Io)
}

fn write_error_to_io(error: WriteError) -> io::Error {
    match error {
        WriteError::Encode => io::Error::other("canonical history encode failure"),
        WriteError::Io(error) => error,
        WriteError::Poisoned => io::Error::other("canonical history store is poisoned"),
        WriteError::TimestampRegression => {
            io::Error::other("canonical history timestamp regresses")
        }
    }
}

fn open_error_to_io(error: OpenError) -> io::Error {
    match error {
        OpenError::Io(error) => error,
        error => io::Error::other(error.to_string()),
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

fn new_boot_id() -> String {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS RNG for canonical history boot ID");
    let mut id = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut id, "{byte:02x}").expect("writing to String cannot fail");
    }
    id
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailurePoint {
    TempCreate,
    TempWrite,
    TempSync,
    CompactionTempCreate,
    CompactionTempWrite,
    CompactionTempSync,
    CompactionRename,
    CompactionDirectorySync,
    HardLink,
    ParentDirectorySync,
    RestartBootAppend,
    RestartBootSync,
    RuntimePartialAppend,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum FailurePoint {
    TempCreate,
    TempWrite,
    TempSync,
    CompactionTempCreate,
    CompactionTempWrite,
    CompactionTempSync,
    CompactionRename,
    CompactionDirectorySync,
    HardLink,
    ParentDirectorySync,
    RestartBootAppend,
    RestartBootSync,
}

struct FailureSwitch {
    #[cfg(test)]
    point: FailurePoint,
    #[cfg(test)]
    fired: Option<FailurePoint>,
}

fn fail(failure: &mut Option<&mut FailureSwitch>, point: FailurePoint) -> io::Result<()> {
    #[cfg(test)]
    if let Some(failure) = failure {
        if failure.point == point {
            failure.fired = Some(point);
            return Err(io::Error::other(format!("injected {point:?} failure")));
        }
    }
    let _ = (failure, point);
    Ok(())
}

#[cfg(test)]
struct InjectedOpen {
    result: Result<HistoryStore, OpenError>,
    fired: Option<FailurePoint>,
}

#[cfg(test)]
fn open_with_test_failure(
    data_dir: &Path,
    timestamp: u64,
    capacity: Capacity,
    point: FailurePoint,
) -> InjectedOpen {
    let mut failure = FailureSwitch { point, fired: None };
    let result = HistoryStore::open_inner(data_dir, timestamp, capacity, Some(&mut failure));
    InjectedOpen {
        result,
        fired: failure.fired,
    }
}

#[cfg(test)]
struct InjectedWrite {
    result: Result<(), WriteError>,
    fired: Option<FailurePoint>,
}

#[cfg(test)]
fn append_with_test_failure(
    store: &mut HistoryStore,
    timestamp: u64,
    capacity: Capacity,
    state: Vec<StateEntry>,
    point: FailurePoint,
) -> InjectedWrite {
    let mut failure = FailureSwitch { point, fired: None };
    let result = store.append_sample_inner(timestamp, capacity, state, Some(&mut failure));
    InjectedWrite {
        result,
        fired: failure.fired,
    }
}

#[cfg(test)]
struct InjectedCompaction {
    result: io::Result<CompactionOutcome>,
    fired: Option<FailurePoint>,
}

#[cfg(test)]
fn compact_with_test_failure(
    store: &mut HistoryStore,
    cutoff: u64,
    point: FailurePoint,
) -> InjectedCompaction {
    let mut failure = FailureSwitch { point, fired: None };
    let result = store.compact_inner(cutoff, Some(&mut failure));
    InjectedCompaction {
        result,
        fired: failure.fired,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{mpsc, Arc, Mutex, TryLockError};

    use super::{
        add_sample, append_with_test_failure, compact_with_test_failure, open_with_test_failure,
        stale_temporary_count, validate_canonical, CompactionOutcome, Epoch, FailurePoint,
        HistoryStore, OpenError, ScannerState,
    };
    use crate::history::{
        codec::{
            decode_record, BootRecord, Capacity, DecodeError, Record, SampleRecord, StateEntry,
            StateKind,
        },
        CapacitySnapshot, History, HistoryWindow, MetricValue,
    };

    fn capacity() -> Capacity {
        Capacity {
            capacity_rpm: 80,
            enabled_keys: 2,
            key_rpms: vec![40, 40],
        }
    }

    fn state(value: f64) -> Vec<StateEntry> {
        vec![StateEntry {
            kind: StateKind::Counter,
            metric: "nimproxy_requests_total".into(),
            labels: Default::default(),
            value,
        }]
    }

    fn runtime_sample(timestamp: u64, boot_id: &str, value: f64) -> Record {
        Record::Sample(SampleRecord {
            timestamp,
            boot_id: boot_id.to_owned(),
            capacity: capacity(),
            state: state(value),
        })
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        static SERIAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nim-proxy-history-store-{label}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn history_capacity() -> CapacitySnapshot {
        CapacitySnapshot {
            enabled_lanes: 2,
            rpms: vec![40, 40],
            capacity_rpm: 80,
        }
    }

    fn boot(timestamp: u64, boot_id: &str) -> String {
        format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"boot","timestamp":{timestamp},"boot_id":"{boot_id}","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}}}"#
        ) + "\n"
    }

    fn sample(timestamp: u64, boot_id: &str, value: f64) -> String {
        format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":{timestamp},"boot_id":"{boot_id}","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}},"state":[{{"kind":"counter","metric":"nimproxy_requests_total","labels":{{"client":"synthetic-client"}},"value":{value}}}]}}"#
        ) + "\n"
    }

    fn sample_with_gauge(timestamp: u64, boot_id: &str, counter: f64, gauge: f64) -> String {
        format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":{timestamp},"boot_id":"{boot_id}","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}},"state":[{{"kind":"counter","metric":"nimproxy_requests_total","labels":{{"client":"synthetic-client"}},"value":{counter}}},{{"kind":"gauge","metric":"nimproxy_active_requests","labels":{{"client":"synthetic-client"}},"value":{gauge}}}]}}"#
        ) + "\n"
    }

    fn metric_value(metric: &str, value: f64) -> MetricValue {
        MetricValue {
            labels: [("client".to_owned(), "synthetic-client".to_owned())]
                .into_iter()
                .collect(),
            metric: metric.to_owned(),
            value,
        }
    }

    fn checkpoint(timestamp: u64, boot_id: &str) -> String {
        format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"checkpoint","timestamp":{timestamp},"boot_id":"{boot_id}","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}}}"#
        ) + "\n"
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ExpectedDiagnostics {
        excluded_epochs: usize,
        excluded_records: usize,
        normalized_series: usize,
        skipped_metric_lines: usize,
        valid_checkpoints: usize,
        valid_samples: usize,
    }

    /// Task 12's file-order state table. This deliberately names the approved
    /// `HistoryWindow` contract before its production implementation exists:
    /// the initial RED is the compiler proving that the contract is absent.
    #[test]
    fn recovery_streams_preserve_only_usable_epochs_and_report_completeness() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = now.saturating_sub(100);
        let invalid_state = format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":{},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}},"state":[{{"kind":"unknown","metric":"nimproxy_requests_total","labels":{{"client":"synthetic-client"}},"value":1.0}}]}}"#,
            t + 2
        ) + "\n";
        let duplicate_series = format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":{},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}},"state":[{{"kind":"counter","metric":"nimproxy_requests_total","labels":{{"client":"synthetic-client"}},"value":1.0}},{{"kind":"counter","metric":"nimproxy_requests_total","labels":{{"client":"synthetic-client"}},"value":2.0}}]}}"#,
            t + 2
        ) + "\n";
        let unknown_kind = format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"unknown","timestamp":{},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}}}"#,
            t + 2
        ) + "\n";
        let future_format = format!(
            r#"{{"format":"future-history","v":1,"kind":"boot","timestamp":{t},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}}}"#
        ) + "\n";
        let future_version = format!(
            r#"{{"format":"nimproxy-history","v":2,"kind":"boot","timestamp":{t},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}}}"#
        ) + "\n";
        let cases = [
            (
                "corruption-before-first-boot",
                format!(
                    "{{not-json}}\n{}{}",
                    boot(t + 1, "boot-a"),
                    sample(t + 2, "boot-a", 1.0)
                ),
                false,
                false,
                1.0,
                1,
                ExpectedDiagnostics {
                    excluded_epochs: 0,
                    excluded_records: 1,
                    normalized_series: 1,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 1,
                },
            ),
            (
                "corruption-between-samples",
                format!(
                    "{}{}{{not-json}}\n{}",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0),
                    sample(t + 3, "boot-a", 5.0)
                ),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 4,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "corruption-after-checkpoint",
                format!(
                    "{}{}{}{{not-json}}\n",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0),
                    checkpoint(t + 2, "boot-a")
                ),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 4,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "timestamp-regression",
                format!(
                    "{}{}{}",
                    boot(t + 1, "boot-a"),
                    sample(t + 2, "boot-a", 2.0),
                    sample(t, "boot-a", 5.0)
                ),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 3,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "equal-timestamps-are-valid",
                format!(
                    "{}{}{}",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0),
                    sample(t + 1, "boot-a", 5.0)
                ),
                true,
                false,
                5.0,
                1,
                ExpectedDiagnostics {
                    excluded_epochs: 0,
                    excluded_records: 0,
                    normalized_series: 2,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 2,
                },
            ),
            (
                "valid-checkpoint-carries-state-within-usable-epoch",
                format!(
                    "{}{}{}",
                    boot(t, "boot-a"),
                    sample_with_gauge(t + 1, "boot-a", 5.0, 9.0),
                    checkpoint(t + 2, "boot-a")
                ),
                true,
                false,
                5.0,
                1,
                ExpectedDiagnostics {
                    excluded_epochs: 0,
                    excluded_records: 0,
                    normalized_series: 4,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 1,
                    valid_samples: 1,
                },
            ),
            (
                "invalid-state-entry-invalidates-whole-sample",
                format!("{}{}", boot(t, "boot-a"), invalid_state),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 2,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "duplicate-semantic-series-invalidates-whole-sample",
                format!("{}{}", boot(t, "boot-a"), duplicate_series),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 2,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "unknown-v1-kind-invalidates-current-epoch",
                format!(
                    "{}{}{}",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0),
                    unknown_kind
                ),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 3,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "new-boot-and-full-sample-reestablish-usable-epoch",
                format!(
                    "{}{}{{not-json}}\n{}{}",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0),
                    boot(t + 3, "boot-b"),
                    sample(t + 4, "boot-b", 7.0)
                ),
                false,
                false,
                7.0,
                1,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 3,
                    normalized_series: 1,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 1,
                },
            ),
            (
                "multiple-damaged-epochs",
                format!(
                    "{}{}{{not-json}}\n{}{}{{not-json}}\n{}{}",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0),
                    boot(t + 3, "boot-b"),
                    sample(t + 4, "boot-b", 3.0),
                    boot(t + 6, "boot-c"),
                    sample(t + 7, "boot-c", 4.0)
                ),
                false,
                false,
                4.0,
                1,
                ExpectedDiagnostics {
                    excluded_epochs: 2,
                    excluded_records: 6,
                    normalized_series: 1,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 1,
                },
            ),
            (
                "truncated-tail-invalidates-current-epoch",
                format!(
                    "{}{}{{\"format\":\"nimproxy-history\"",
                    boot(t, "boot-a"),
                    sample(t + 1, "boot-a", 2.0)
                ),
                false,
                false,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 3,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "unknown-format-remains-fatal",
                future_format,
                false,
                true,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 0,
                    excluded_records: 0,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
            (
                "future-version-remains-fatal",
                future_version,
                false,
                true,
                0.0,
                0,
                ExpectedDiagnostics {
                    excluded_epochs: 0,
                    excluded_records: 0,
                    normalized_series: 0,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 0,
                },
            ),
        ];

        for (name, bytes, complete, fatal, total, point_count, expected) in cases {
            let dir = test_dir(&format!("recovery-{name}"));
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, bytes).unwrap();
            let before = fs::read(&path).unwrap();
            let opened = History::open(dir.clone(), 0, history_capacity());
            if fatal {
                let expected = if name == "unknown-format-remains-fatal" {
                    DecodeError::UnsupportedFormat
                } else {
                    DecodeError::UnsupportedVersion
                };
                assert!(
                    matches!(opened, Err(OpenError::InvalidCanonical { error, .. }) if error == expected),
                    "{name}: exact unsupported wire diagnostic"
                );
                assert_eq!(
                    fs::read(&path).unwrap(),
                    before,
                    "{name}: bytes stay evidence"
                );
                let _ = fs::remove_dir_all(dir);
                continue;
            }
            let history = opened.unwrap_or_else(|error| {
                panic!(
                    "{name}: recoverable corruption must still expose valid later epochs: {error}"
                )
            });
            let HistoryWindow {
                complete: actual_complete,
                data,
                diagnostics,
                ..
            } = history.rollup(0, u64::MAX, 1000);
            assert_eq!(actual_complete, complete, "{name}: query completeness");
            assert_eq!(data.points.len(), point_count, "{name}: included points");
            let expected_totals = if total != 0.0 {
                vec![metric_value("nimproxy_requests_total", total)]
            } else {
                Vec::new()
            };
            assert_eq!(data.totals, expected_totals, "{name}: exact totals");
            let expected_latest = if name == "valid-checkpoint-carries-state-within-usable-epoch" {
                vec![metric_value("nimproxy_active_requests", 9.0)]
            } else {
                Vec::new()
            };
            assert_eq!(data.latest, expected_latest, "{name}: exact latest values");
            let expected_point_values: Vec<Vec<MetricValue>> = match name {
                "corruption-before-first-boot" => {
                    vec![vec![metric_value("nimproxy_requests_total", 1.0)]]
                }
                "equal-timestamps-are-valid" => {
                    vec![vec![metric_value("nimproxy_requests_total", 5.0)]]
                }
                "valid-checkpoint-carries-state-within-usable-epoch" => vec![vec![
                    metric_value("nimproxy_active_requests", 9.0),
                    metric_value("nimproxy_requests_total", 5.0),
                ]],
                "new-boot-and-full-sample-reestablish-usable-epoch" => {
                    vec![vec![metric_value("nimproxy_requests_total", 7.0)]]
                }
                "multiple-damaged-epochs" => {
                    vec![vec![metric_value("nimproxy_requests_total", 4.0)]]
                }
                _ => Vec::new(),
            };
            assert_eq!(
                data.points
                    .iter()
                    .map(|point| point.values.clone())
                    .collect::<Vec<_>>(),
                expected_point_values,
                "{name}: exact point values"
            );
            let expected_effective = match name {
                "corruption-before-first-boot" => Some((t + 2, t + 2)),
                "equal-timestamps-are-valid" => Some((t + 1, t + 1)),
                "valid-checkpoint-carries-state-within-usable-epoch" => Some((t + 1, t + 2)),
                "new-boot-and-full-sample-reestablish-usable-epoch" => Some((t + 4, t + 4)),
                "multiple-damaged-epochs" => Some((t + 7, t + 7)),
                _ => None,
            };
            assert_eq!(
                (data.effective_from, data.effective_to),
                expected_effective.map_or((None, None), |(from, to)| (Some(from), Some(to))),
                "{name}: exact effective bounds"
            );
            assert_eq!(
                (data.available_from, data.available_to),
                expected_effective.map_or((None, None), |(from, to)| (Some(from), Some(to))),
                "{name}: exact available bounds"
            );
            if let Some((from, to)) = expected_effective {
                assert_eq!(data.points[0].from, from, "{name}: point start");
                assert_eq!(data.points[0].to, to, "{name}: point end");
                assert_eq!(
                    data.points[0].duration_seconds,
                    to - from,
                    "{name}: point duration"
                );
            }
            if name == "valid-checkpoint-carries-state-within-usable-epoch" {
                assert_eq!(data.points[0].from, t + 1, "{name}: point start");
                assert_eq!(data.points[0].to, t + 2, "{name}: point end");
                assert_eq!(data.points[0].duration_seconds, 1, "{name}: point duration");
                assert_eq!(
                    data.points[0].capacity.as_ref().unwrap().average_rpm,
                    80.0,
                    "{name}: checkpoint capacity"
                );
                assert_eq!(
                    data.points[0].capacity.as_ref().unwrap().latest_rpms,
                    vec![40, 40],
                    "{name}: checkpoint key RPMs"
                );
            } else {
                assert!(
                    data.points.iter().all(|point| point.capacity.is_none()),
                    "{name}: no capacity is invented before a second observation"
                );
            }
            assert_eq!(
                diagnostics.excluded_epochs, expected.excluded_epochs,
                "{name}: excluded epochs"
            );
            assert_eq!(
                diagnostics.excluded_records, expected.excluded_records,
                "{name}: excluded records"
            );
            assert_eq!(
                diagnostics.normalized_series, expected.normalized_series,
                "{name}: normalized series"
            );
            assert_eq!(
                diagnostics.skipped_metric_lines, expected.skipped_metric_lines,
                "{name}: skipped Prometheus metric lines"
            );
            assert_eq!(
                diagnostics.valid_checkpoints, expected.valid_checkpoints,
                "{name}: valid checkpoints"
            );
            assert_eq!(
                diagnostics.valid_samples, expected.valid_samples,
                "{name}: valid samples"
            );
            if name == "new-boot-and-full-sample-reestablish-usable-epoch" {
                let HistoryWindow {
                    complete: usable_complete,
                    data: usable,
                    diagnostics: usable_diagnostics,
                    ..
                } = history.rollup(t + 3, t + 4, 1000);
                assert!(usable_complete, "later usable epoch is query-complete");
                assert_eq!(
                    usable.totals,
                    vec![metric_value("nimproxy_requests_total", 7.0)],
                    "usable epoch exact total"
                );
                assert!(usable.latest.is_empty(), "usable epoch has no gauge");
                assert_eq!(usable.points.len(), 1, "usable epoch exact point count");
                assert_eq!(
                    usable.points[0].values,
                    vec![metric_value("nimproxy_requests_total", 7.0)],
                    "usable epoch exact point payload"
                );
                assert_eq!(
                    (usable.available_from, usable.available_to),
                    (Some(t + 4), Some(t + 4)),
                    "usable epoch available bounds"
                );
                assert_eq!(
                    (usable.effective_from, usable.effective_to),
                    (Some(t + 4), Some(t + 4)),
                    "usable epoch effective bounds"
                );
                assert_eq!(
                    (
                        usable_diagnostics.excluded_epochs,
                        usable_diagnostics.excluded_records,
                        usable_diagnostics.normalized_series,
                        usable_diagnostics.skipped_metric_lines,
                        usable_diagnostics.valid_checkpoints,
                        usable_diagnostics.valid_samples,
                    ),
                    (1, 3, 1, 0, 0, 1),
                    "usable epoch diagnostics remain cumulative for the query result"
                );
                let HistoryWindow {
                    complete: unavailable_complete,
                    data: unavailable,
                    diagnostics: unavailable_diagnostics,
                    ..
                } = history.rollup(t + 20, t + 21, 1000);
                assert!(
                    !unavailable_complete,
                    "unavailable interval is never presented as complete"
                );
                assert!(
                    unavailable.totals.is_empty(),
                    "unavailable interval has no invented totals"
                );
                assert!(
                    unavailable.latest.is_empty(),
                    "unavailable interval has no invented latest values"
                );
                assert!(
                    unavailable.points.is_empty(),
                    "unavailable interval has no invented points"
                );
                assert_eq!(
                    (unavailable.effective_from, unavailable.effective_to),
                    (None, None)
                );
                assert_eq!(
                    (
                        unavailable_diagnostics.excluded_epochs,
                        unavailable_diagnostics.excluded_records,
                        unavailable_diagnostics.normalized_series,
                        unavailable_diagnostics.skipped_metric_lines,
                        unavailable_diagnostics.valid_checkpoints,
                        unavailable_diagnostics.valid_samples,
                    ),
                    (1, 3, 1, 0, 0, 1),
                    "unavailable interval diagnostics remain cumulative for the query result"
                );
            }
            drop(history);
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn first_open_publishes_one_synced_canonical_boot_record() {
        // This catches a missing safe-publication boundary, including an
        // accidental fallback to the legacy history.jsonl path.
        let dir = std::env::temp_dir().join(format!(
            "nim-proxy-history-store-red-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let result = HistoryStore::open(&dir, 1_000, capacity());
        assert!(
            result.is_ok(),
            "missing canonical path must publish a boot: {result:?}"
        );

        let bytes = fs::read(dir.join("history-v1.jsonl")).unwrap();
        let Record::Boot(boot) = decode_record(bytes.trim_ascii_end()).unwrap() else {
            panic!("publication must contain a canonical boot record");
        };
        assert_eq!(boot.timestamp, 1_000);
        assert_eq!(boot.capacity, capacity());
        assert!(!boot.boot_id.is_empty(), "store owns a fresh boot id");
        assert!(!dir.join("history.jsonl").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("history-v1.jsonl"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o600,
                "first publication must create history-v1.jsonl as 0600"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_canonical_history_recovers_by_appending_a_fresh_boot() {
        let dir = std::env::temp_dir().join(format!(
            "nim-proxy-history-store-red-invalid-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history-v1.jsonl");
        let before = b"{not canonical}\n";
        fs::write(&path, before).unwrap();

        HistoryStore::open(&dir, 1_000, capacity()).unwrap();
        assert!(fs::read(path).unwrap().starts_with(before));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn recoverable_canonical_stream_damage_appends_a_fresh_boot() {
        let boot = b"{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"boot\",\"timestamp\":10,\"boot_id\":\"boot-a\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}\n";
        let sample_a = b"{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"sample\",\"timestamp\":11,\"boot_id\":\"boot-a\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]},\"state\":[]}\n";
        let sample_b = b"{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"sample\",\"timestamp\":11,\"boot_id\":\"boot-b\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]},\"state\":[]}\n";
        let checkpoint_b = b"{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"checkpoint\",\"timestamp\":11,\"boot_id\":\"boot-b\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}\n";
        let checkpoint_a = b"{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"checkpoint\",\"timestamp\":11,\"boot_id\":\"boot-a\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}\n";
        let sample_mismatch = [boot.as_slice(), sample_b.as_slice()].concat();
        let checkpoint_mismatch = [boot.as_slice(), checkpoint_b.as_slice()].concat();
        let regression = [boot.as_slice(), sample_a.as_slice(), boot.as_slice()].concat();
        for (name, bytes) in [
            ("sample-before-boot", sample_a.as_slice()),
            ("sample-boot-mismatch", sample_mismatch.as_slice()),
            ("checkpoint-boot-mismatch", checkpoint_mismatch.as_slice()),
            ("timestamp-regression", regression.as_slice()),
        ] {
            let dir = test_dir(name);
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, bytes).unwrap();
            HistoryStore::open(&dir, 20, capacity()).unwrap();
            assert!(fs::read(&path).unwrap().starts_with(bytes), "{name}");
            let _ = fs::remove_dir_all(dir);
        }
        let dir = test_dir("checkpoint-after-boot");
        fs::write(
            dir.join("history-v1.jsonl"),
            [boot.as_slice(), checkpoint_a.as_slice()].concat(),
        )
        .unwrap();
        HistoryStore::open(&dir, 20, capacity()).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unterminated_canonical_tail_recovers_with_a_fresh_boot() {
        let dir = test_dir("unterminated-record");
        let path = dir.join("history-v1.jsonl");
        let bytes = b"{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"boot\",\"timestamp\":1,\"boot_id\":\"boot-a\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}";
        fs::write(&path, bytes).unwrap();
        HistoryStore::open(&dir, 2, capacity()).unwrap();
        assert!(fs::read(&path).unwrap().starts_with(bytes));
        HistoryStore::open(&dir, 3, capacity()).unwrap();
        assert!(fs::read(&path).unwrap().starts_with(bytes));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn boot_only_epoch_is_excluded_and_leaves_a_bounded_gap() {
        let dir = test_dir("boot-only-epoch");
        let path = dir.join("history-v1.jsonl");
        fs::write(
            &path,
            format!("{}{}", boot(10, "boot-a"), boot(20, "boot-b")),
        )
        .unwrap();

        let replay = validate_canonical(&path).unwrap();
        assert_eq!(replay.diagnostics.excluded_epochs, 2);
        assert_eq!(replay.diagnostics.excluded_records, 2);
        assert_eq!(replay.gaps.len(), 2);
        assert_eq!(replay.gaps[0].from, 10);
        assert_eq!(replay.gaps[0].to, 20);
        assert_eq!(replay.gaps[1].from, 20);
        assert_eq!(replay.gaps[1].to, u64::MAX);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn complete_unterminated_future_tail_refuses_without_mutating_evidence() {
        for (name, before, expected) in [
            (
                "future-version-tail",
                b"{\"format\":\"nimproxy-history\",\"v\":2,\"kind\":\"boot\",\"timestamp\":1,\"boot_id\":\"x\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}".as_slice(),
                DecodeError::UnsupportedVersion,
            ),
            (
                "future-format-tail",
                b"{\"format\":\"future-history\",\"v\":1,\"kind\":\"boot\",\"timestamp\":1,\"boot_id\":\"x\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}".as_slice(),
                DecodeError::UnsupportedFormat,
            ),
        ] {
            let dir = test_dir(name);
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, before).unwrap();

            assert!(matches!(
                HistoryStore::open(&dir, 2, capacity()),
                Err(OpenError::InvalidCanonical { error, .. }) if error == expected
            ));
            assert_eq!(fs::read(&path).unwrap(), before);
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn whitespace_only_canonical_refuses_without_mutating_evidence() {
        let dir = test_dir("whitespace-only");
        let path = dir.join("history-v1.jsonl");
        let before = b" \t\n\r\n";
        fs::write(&path, before).unwrap();

        assert!(matches!(
            HistoryStore::open(&dir, 2, capacity()),
            Err(OpenError::EmptyCanonical)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejected_higher_timestamp_still_bounds_startup_append() {
        let dir = test_dir("rejected-high-timestamp");
        let path = dir.join("history-v1.jsonl");
        let before = format!("{}{}", boot(10, "boot-a"), sample(100, "wrong-boot", 1.0));
        fs::write(&path, &before).unwrap();

        assert!(matches!(
            HistoryStore::open(&dir, 20, capacity()),
            Err(OpenError::InvalidStream {
                reason: "timestamp regresses",
                ..
            })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn repeated_damage_without_a_boot_creates_one_bounded_recovery_gap() {
        let dir = test_dir("bounded-recovery-gap");
        let path = dir.join("history-v1.jsonl");
        fs::write(
            &path,
            format!(
                "{}{{not-json}}\n{{not-json}}\n{}{}",
                boot(10, "boot-a"),
                boot(20, "boot-b"),
                sample(21, "boot-b", 1.0)
            ),
        )
        .unwrap();

        let replay = validate_canonical(&path).unwrap();
        assert_eq!(replay.gaps.len(), 1);
        assert_eq!(replay.gaps[0].from, 10);
        assert_eq!(replay.gaps[0].to, 20);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn diagnostics_are_cumulative_only_through_the_query_end() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = now.saturating_sub(100);
        let dir = test_dir("query-end-diagnostics");
        let path = dir.join("history-v1.jsonl");
        fs::write(
            &path,
            format!(
                "{}{}{{not-json}}\n{}{}",
                boot(t, "boot-a"),
                sample(t + 1, "boot-a", 2.0),
                boot(t + 3, "boot-b"),
                sample(t + 4, "boot-b", 7.0)
            ),
        )
        .unwrap();

        let history = History::open(dir.clone(), 0, history_capacity()).unwrap();
        let before_damage = history.rollup(t, t + 1, 1000).diagnostics;
        assert_eq!(before_damage.excluded_epochs, 1);
        assert_eq!(before_damage.excluded_records, 3);
        assert_eq!(before_damage.valid_samples, 0);
        let after_damage = history.rollup(t, t + 4, 1000).diagnostics;
        assert_eq!(after_damage.excluded_epochs, 1);
        assert_eq!(after_damage.excluded_records, 3);
        assert_eq!(after_damage.valid_samples, 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_sample_after_recovery_boot_is_complete_in_its_own_window() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let dir = test_dir("recovery-boot-closes-gap");
        fs::write(
            dir.join("history-v1.jsonl"),
            format!(
                "{}{}{{not-json}}\n",
                boot(now - 10, "boot-a"),
                sample(now - 9, "boot-a", 2.0)
            ),
        )
        .unwrap();
        let history = Arc::new(History::open(dir.clone(), 0, history_capacity()).unwrap());
        let sample_t = now + 2;
        history.append(sample_t, "nimproxy_requests_total 1\n", history_capacity());

        assert!(history.rollup(sample_t - 1, sample_t, 1000).complete);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_diagnostics_are_cumulative_only_through_the_query_end() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let dir = test_dir("runtime-query-end-diagnostics");
        let history = Arc::new(History::open(dir.clone(), 0, history_capacity()).unwrap());
        let first_t = now + 2;
        history.append(first_t, "nimproxy_requests_total 1\n", history_capacity());
        history.append(
            first_t + 1,
            "nimproxy_requests_total 2\n",
            history_capacity(),
        );

        let before_second = history.rollup(first_t - 1, first_t, 1000).diagnostics;
        assert_eq!(before_second.valid_samples, 1);
        assert_eq!(before_second.normalized_series, 1);
        let after_second = history.rollup(first_t - 1, first_t + 1, 1000).diagnostics;
        assert_eq!(after_second.valid_samples, 2);
        assert_eq!(after_second.normalized_series, 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invalidation_counts_buffered_records_only_at_their_physical_times() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = now.saturating_sub(100);
        let dir = test_dir("invalidation-physical-times");
        let damage = format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"unknown","timestamp":{},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}}}}"#,
            t + 13
        ) + "\n";
        fs::write(
            dir.join("history-v1.jsonl"),
            format!(
                "{}{}{}{}",
                boot(t + 10, "boot-a"),
                sample(t + 11, "boot-a", 1.0),
                sample(t + 12, "boot-a", 2.0),
                damage,
            ),
        )
        .unwrap();

        let history = History::open(dir.clone(), 0, history_capacity()).unwrap();
        let before_damage = history.rollup(t, t + 12, 1000);
        assert!(!before_damage.complete);
        assert_eq!(before_damage.diagnostics.excluded_epochs, 1);
        assert_eq!(before_damage.diagnostics.excluded_records, 3);
        let after_damage = history.rollup(t, t + 13, 1000);
        assert_eq!(after_damage.diagnostics.excluded_records, 4);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejected_high_timestamp_requires_a_later_recovery_boot() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = now.saturating_sub(200);
        let invalid_state = format!(
            r#"{{"format":"nimproxy-history","v":1,"kind":"sample","timestamp":{},"boot_id":"boot-a","capacity":{{"capacity_rpm":80,"enabled_keys":2,"key_rpms":[40,40]}},"state":[{{"kind":"unknown","metric":"nimproxy_requests_total","labels":{{}},"value":1.0}}]}}"#,
            t + 100
        );
        let dir = test_dir("rejected-high-timestamp-recovery");
        fs::write(
            dir.join("history-v1.jsonl"),
            format!(
                "{}{}\n{}{}{}{}",
                boot(t + 10, "boot-a"),
                invalid_state,
                boot(t + 20, "boot-b"),
                sample(t + 21, "boot-b", 2.0),
                boot(t + 100, "boot-c"),
                sample(t + 101, "boot-c", 7.0),
            ),
        )
        .unwrap();

        let history = History::open(dir.clone(), 0, history_capacity()).unwrap();
        let early = history.rollup(t, t + 21, 1000);
        assert!(early.data.points.is_empty());
        let recovered = history.rollup(t, t + 101, 1000);
        assert_eq!(
            recovered.data.totals,
            vec![metric_value("nimproxy_requests_total", 7.0)]
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn scanner_state_enum_covers_all_locked_transitions() {
        let waiting = Epoch::new(decode_record(boot(10, "boot-a").trim().as_bytes()).unwrap());
        let mut usable = Epoch::new(decode_record(boot(20, "boot-b").trim().as_bytes()).unwrap());
        let sample = decode_record(sample(21, "boot-b", 1.0).trim().as_bytes()).unwrap();
        let Record::Sample(sample) = sample else {
            unreachable!()
        };
        add_sample(&mut usable, &sample);
        usable.records.push(Record::Sample(sample));
        let states = [
            ScannerState::AwaitBoot,
            ScannerState::AwaitSample(waiting),
            ScannerState::Usable(usable),
            ScannerState::InvalidEpoch,
        ];
        assert!(matches!(states[0], ScannerState::AwaitBoot));
        assert!(matches!(states[1], ScannerState::AwaitSample(_)));
        assert!(matches!(states[2], ScannerState::Usable(_)));
        assert!(matches!(states[3], ScannerState::InvalidEpoch));
    }

    #[test]
    fn missing_parent_directory_is_created_for_first_publication() {
        // This catches assuming the caller pre-created DATA_DIR.
        let dir = test_dir("missing-parent").join("new").join("data");
        HistoryStore::open(&dir, 1, capacity()).unwrap();
        assert!(dir.join("history-v1.jsonl").is_file());
        let _ = fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn concurrent_first_open_has_one_published_boot_without_clobbering_it() {
        // This catches check-then-rename publication, which can replace a
        // concurrent creator's evidence.
        let dir = test_dir("concurrent");
        let first = std::thread::scope(|scope| {
            let left = scope.spawn(|| HistoryStore::open(&dir, 1, capacity()));
            let right = scope.spawn(|| HistoryStore::open(&dir, 1, capacity()));
            (left.join().unwrap(), right.join().unwrap())
        });
        assert!(
            first.0.is_ok() && first.1.is_ok(),
            "both creators can use the winner: left={:?} right={:?}",
            first.0,
            first.1
        );
        let records = fs::read_to_string(dir.join("history-v1.jsonl")).unwrap();
        assert_eq!(
            records.lines().count(),
            2,
            "each successful process owns one boot"
        );
        assert!(records
            .lines()
            .all(|line| decode_record(line.as_bytes()).is_ok()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_temporary_is_ignored_and_left_as_evidence() {
        // This catches parsing or deleting a stale publication temporary.
        let dir = test_dir("stale-temp");
        let stale = dir.join("history-v1.jsonl.tmp-stale");
        let stale_bytes = b"{partial canonical boot evidence";
        fs::write(&stale, stale_bytes).unwrap();
        HistoryStore::open(&dir, 1, capacity()).unwrap();
        assert_eq!(fs::read(stale).unwrap(), stale_bytes);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn valid_restart_has_a_distinct_boot_id_for_its_first_checkpoint_and_sample() {
        // This catches using a caller-provided boot id or crossing a reset
        // boundary before the new process has written one.
        let dir = test_dir("restart");
        let mut first = HistoryStore::open(&dir, 1, capacity()).unwrap();
        first.append_sample(2, capacity(), state(1.0)).unwrap();
        drop(first);
        let mut second = HistoryStore::open(&dir, 3, capacity()).unwrap();
        second.checkpoint(4, capacity()).unwrap();
        second.append_sample(5, capacity(), state(1.0)).unwrap();
        let records = fs::read_to_string(dir.join("history-v1.jsonl")).unwrap();
        let records: Vec<_> = records
            .lines()
            .map(|line| decode_record(line.as_bytes()).unwrap())
            .collect();
        let Record::Boot(first_boot) = &records[0] else {
            panic!("first record must boot")
        };
        let Record::Boot(second_boot) = &records[2] else {
            panic!("restart must boot")
        };
        let Record::Checkpoint(first_after_restart) = &records[3] else {
            panic!("first restart record after boot must exercise checkpoint")
        };
        let Record::Sample(second_after_restart) = &records[4] else {
            panic!("restart state must be a full sample")
        };
        assert_ne!(
            first_boot.boot_id, second_boot.boot_id,
            "each process owns a new reset id"
        );
        assert_eq!(first_after_restart.boot_id, second_boot.boot_id);
        assert_eq!(second_after_restart.boot_id, second_boot.boot_id);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_and_future_files_remain_exactly_unchanged() {
        for (name, before) in [
            ("empty", b"".as_slice()),
            ("future", b"{\"format\":\"nimproxy-history\",\"v\":2,\"kind\":\"boot\",\"timestamp\":1,\"boot_id\":\"x\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}\n".as_slice()),
        ] {
            let dir = test_dir(name);
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, before).unwrap();
            if name == "future" {
                assert_eq!(
                    decode_record(before.trim_ascii_end()).unwrap_err().diagnostic(),
                    "unsupported_version"
                );
            }
            assert!(HistoryStore::open(&dir, 2, capacity()).is_err());
            assert_eq!(fs::read(path).unwrap(), before, "{name} canonical evidence");
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn canonical_non_file_path_refuses_without_replacing_it() {
        // This is deterministic under privileged test runners, unlike chmod:
        // a directory cannot be opened or replaced as the canonical file.
        let dir = test_dir("canonical-directory");
        let canonical = dir.join("history-v1.jsonl");
        fs::create_dir(&canonical).unwrap();
        assert!(HistoryStore::open(&dir, 1, capacity()).is_err());
        assert!(
            canonical.is_dir(),
            "failed open must not replace path evidence"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_file_is_never_parsed_or_mutated_when_canonical_is_absent_or_present() {
        // This catches accidental migration or a compatibility reader.
        for canonical in [false, true] {
            let dir = test_dir(if canonical {
                "legacy-plus-canonical"
            } else {
                "legacy-only"
            });
            let legacy = dir.join("history.jsonl");
            let legacy_bytes = b"legacy bytes stay opaque\n";
            fs::write(&legacy, legacy_bytes).unwrap();
            if canonical {
                HistoryStore::open(&dir, 1, capacity()).unwrap();
            }
            HistoryStore::open(&dir, 2, capacity()).unwrap();
            assert_eq!(fs::read(legacy).unwrap(), legacy_bytes);
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn changed_state_writes_sample_unchanged_state_writes_checkpoint_and_capacity_is_live() {
        // This catches re-emitting full idle snapshots or freezing capacity at open.
        let dir = test_dir("idle");
        let mut store = HistoryStore::open(&dir, 1, capacity()).unwrap();
        store.append_sample(2, capacity(), state(1.0)).unwrap();
        let changed_capacity = Capacity {
            capacity_rpm: 20,
            enabled_keys: 1,
            key_rpms: vec![20],
        };
        store
            .append_sample(3, changed_capacity.clone(), state(1.0))
            .unwrap();
        store
            .append_sample(4, changed_capacity.clone(), state(2.0))
            .unwrap();
        let records = fs::read_to_string(dir.join("history-v1.jsonl")).unwrap();
        let records: Vec<_> = records
            .lines()
            .map(|line| decode_record(line.as_bytes()).unwrap())
            .collect();
        assert!(matches!(records[2], Record::Checkpoint(_)));
        let Record::Checkpoint(checkpoint) = &records[2] else {
            unreachable!()
        };
        assert_eq!(checkpoint.capacity, changed_capacity);
        assert!(matches!(records[3], Record::Sample(_)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn publication_failures_preserve_their_actual_crash_state_and_fire() {
        // This catches tests that merely configure a failure without proving
        // the intended operation was actually reached.
        for point in [
            FailurePoint::TempCreate,
            FailurePoint::TempWrite,
            FailurePoint::TempSync,
            FailurePoint::HardLink,
        ] {
            let dir = test_dir("injected");
            let canonical = dir.join("history-v1.jsonl");
            let failure = open_with_test_failure(&dir, 1, capacity(), point);
            assert!(failure.result.is_err(), "{point:?} must reject startup");
            assert_eq!(failure.fired, Some(point), "{point:?} injection must fire");
            assert!(!canonical.exists(), "{point:?} occurs before publication");
            if point != FailurePoint::TempCreate {
                assert!(
                    fs::read_dir(&dir).unwrap().any(|entry| entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("history-v1.jsonl.tmp-")),
                    "{point:?} leaves its temporary evidence"
                );
            }
            let _ = fs::remove_dir_all(dir);
        }

        let dir = test_dir("directory-sync");
        let canonical = dir.join("history-v1.jsonl");
        let failure =
            open_with_test_failure(&dir, 1, capacity(), FailurePoint::ParentDirectorySync);
        assert!(failure.result.is_err());
        assert_eq!(failure.fired, Some(FailurePoint::ParentDirectorySync));
        let Record::Boot(_) = decode_record(&fs::read(&canonical).unwrap()).unwrap() else {
            panic!("directory-sync failure retains the linked complete boot evidence")
        };
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn append_and_sync_failures_do_not_hide_partial_tail_evidence() {
        // This catches truncate/repair after a failed restart boot append.
        for point in [
            FailurePoint::RestartBootAppend,
            FailurePoint::RestartBootSync,
        ] {
            let dir = test_dir("restart-failure");
            HistoryStore::open(&dir, 1, capacity()).unwrap();
            let before = fs::read(dir.join("history-v1.jsonl")).unwrap();
            let failure = open_with_test_failure(&dir, 2, capacity(), point);
            assert!(failure.result.is_err());
            assert_eq!(failure.fired, Some(point));
            let after = fs::read(dir.join("history-v1.jsonl")).unwrap();
            assert!(after.starts_with(&before), "failed tail remains evidence");
            if point == FailurePoint::RestartBootAppend {
                assert_eq!(after, before, "append failure writes no new tail");
            } else {
                assert!(
                    after.len() > before.len(),
                    "sync failure retains the partial new boot tail"
                );
            }
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn runtime_partial_append_poison_prevents_the_next_tick_from_mutating_history() {
        let dir = test_dir("runtime-poison");
        let mut store = HistoryStore::open(&dir, 1, capacity()).unwrap();
        let path = dir.join("history-v1.jsonl");
        let before = fs::read(&path).unwrap();
        let failed = append_with_test_failure(
            &mut store,
            2,
            capacity(),
            state(1.0),
            FailurePoint::RuntimePartialAppend,
        );
        assert!(failed.result.is_err());
        assert_eq!(failed.fired, Some(FailurePoint::RuntimePartialAppend));
        let after_failed_tick = fs::read(&path).unwrap();
        assert!(after_failed_tick.starts_with(&before));
        assert!(after_failed_tick.len() > before.len());
        assert!(!after_failed_tick.ends_with(b"\n"));
        assert!(store.append_sample(3, capacity(), state(2.0)).is_err());
        assert_eq!(fs::read(path).unwrap(), after_failed_tick);
        let _ = fs::remove_dir_all(dir);
    }

    fn canonical_records(bytes: &[u8]) -> Vec<Record> {
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| decode_record(line).expect("test canonical fixture must decode"))
            .collect()
    }

    fn canonical_bytes(records: &[Record]) -> Vec<u8> {
        records
            .iter()
            .flat_map(|record| {
                let mut line = super::encode_record(record).expect("test record must encode");
                line.push(b'\n');
                line
            })
            .collect()
    }

    fn records_from_fixture(bytes: &str) -> Vec<Record> {
        canonical_records(bytes.as_bytes())
    }

    fn assert_complete_canonical_path(path: &std::path::Path, expected: &[Record], name: &str) {
        let actual_bytes = fs::read(path).unwrap();
        assert_eq!(
            canonical_records(&actual_bytes),
            expected,
            "{name}: canonical records retain exact kinds, timestamps, values, and physical order"
        );
        assert_eq!(
            actual_bytes,
            canonical_bytes(expected),
            "{name}: canonical path contains exactly the complete expected bytes"
        );
        validate_canonical(path).unwrap_or_else(|error| {
            panic!("{name}: observable canonical path must revalidate: {error}")
        });
    }

    fn assert_full_sample_baseline_is_not_omitted_or_replaced_by_checkpoint(
        records: &[Record],
        cutoff: u64,
        expected_value: f64,
        name: &str,
    ) {
        let baseline = records
            .iter()
            .filter_map(|record| match record {
                Record::Sample(sample) if sample.timestamp < cutoff => Some(sample),
                Record::Boot(_) | Record::Checkpoint(_) | Record::Sample(_) => None,
            })
            .next_back()
            .unwrap_or_else(|| panic!("{name}: pre-cutoff full-sample baseline is required"));
        assert_eq!(
            baseline.state,
            vec![StateEntry {
                kind: StateKind::Counter,
                metric: "nimproxy_requests_total".into(),
                labels: [("client".to_owned(), "synthetic-client".to_owned())]
                    .into_iter()
                    .collect(),
                value: expected_value,
            }],
            "{name}: the latest strictly pre-cutoff full sample, not a checkpoint, is retained"
        );
    }

    #[test]
    fn compaction_selection_keeps_boot_full_baseline_and_retained_physical_order() {
        // This table is intentionally physical: a selected checkpoint may
        // follow a same-timestamp sample, but it can never replace the latest
        // full sample that precedes the cutoff.
        let cases = [
            (
                "boundary-equal-timestamp-and-multiple-epochs",
                13,
                30,
                format!(
                    "{}{}{}{}{}{}{}{}{}{}",
                    boot(9, "boot-a"),
                    sample(10, "boot-a", 1.0),
                    checkpoint(11, "boot-a"),
                    sample(12, "boot-a", 2.0),
                    sample(13, "boot-a", 3.0),
                    checkpoint(13, "boot-a"),
                    sample(14, "boot-a", 4.0),
                    boot(15, "boot-b"),
                    sample(16, "boot-b", 5.0),
                    checkpoint(16, "boot-b")
                ),
                format!(
                    "{}{}{}{}{}{}{}{}",
                    boot(9, "boot-a"),
                    sample(12, "boot-a", 2.0),
                    sample(13, "boot-a", 3.0),
                    checkpoint(13, "boot-a"),
                    sample(14, "boot-a", 4.0),
                    boot(15, "boot-b"),
                    sample(16, "boot-b", 5.0),
                    checkpoint(16, "boot-b")
                ),
                2.0,
            ),
            (
                "cutoff-owning-boot-and-latest-precutoff-sample",
                40,
                60,
                format!(
                    "{}{}{}{}{}{}",
                    boot(30, "boot-c"),
                    sample(31, "boot-c", 7.0),
                    checkpoint(32, "boot-c"),
                    sample(39, "boot-c", 8.0),
                    sample(40, "boot-c", 9.0),
                    sample(41, "boot-c", 10.0)
                ),
                format!(
                    "{}{}{}{}",
                    boot(30, "boot-c"),
                    sample(39, "boot-c", 8.0),
                    sample(40, "boot-c", 9.0),
                    sample(41, "boot-c", 10.0)
                ),
                8.0,
            ),
        ];

        for (name, cutoff, opened_at, source, retained, baseline_value) in cases {
            let dir = test_dir(&format!("compaction-selection-{name}"));
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, source).unwrap();
            let mut store = HistoryStore::open(&dir, opened_at, capacity()).unwrap();
            let live_boot = canonical_records(&fs::read(&path).unwrap())
                .last()
                .cloned()
                .expect("open appends its fresh boot");
            assert!(matches!(&live_boot, Record::Boot(boot) if boot.timestamp == opened_at));
            store
                .append_sample(opened_at + 1, capacity(), state(99.0))
                .unwrap();
            let before = fs::read(&path).unwrap();
            let live_sample = canonical_records(&before)
                .last()
                .cloned()
                .expect("fresh runtime boot receives a full sample before compaction");

            assert!(matches!(
                store.compact(cutoff).unwrap(),
                CompactionOutcome::Durable
            ));

            let mut expected = records_from_fixture(&retained);
            expected.push(live_boot);
            expected.push(live_sample);
            assert_complete_canonical_path(&path, &expected, name);
            assert_full_sample_baseline_is_not_omitted_or_replaced_by_checkpoint(
                &expected,
                cutoff,
                baseline_value,
                name,
            );
            assert_ne!(
                fs::read(&path).unwrap(),
                before,
                "{name}: old bytes are bounded away"
            );
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn compaction_recovery_safety_defers_intersecting_evidence_and_discards_only_outside_gaps() {
        type RecoveryCase = (&'static str, u64, u64, String, bool, bool, Option<String>);

        let cases: [RecoveryCase; 3] = [
            (
                "damage-intersects-retained-horizon",
                13,
                20,
                format!(
                    "{}{}{{not-json}}\n{}{}",
                    boot(10, "boot-a"),
                    sample(11, "boot-a", 1.0),
                    boot(14, "boot-b"),
                    sample(15, "boot-b", 2.0)
                ),
                false,
                false,
                None,
            ),
            (
                "gap-ending-at-cutoff-is-outside-retained-horizon",
                13,
                20,
                format!(
                    "{}{}{{not-json}}\n{}{}",
                    boot(10, "boot-a"),
                    sample(11, "boot-a", 1.0),
                    boot(13, "boot-b"),
                    sample(14, "boot-b", 2.0)
                ),
                true,
                true,
                Some(format!(
                    "{}{}",
                    boot(13, "boot-b"),
                    sample(14, "boot-b", 2.0)
                )),
            ),
            (
                "fresh-boot-without-full-sample",
                13,
                20,
                format!("{}{}", boot(10, "boot-a"), sample(11, "boot-a", 1.0)),
                false,
                false,
                None,
            ),
        ];

        for (name, cutoff, opened_at, source, expect_durable, sample_fresh_boot, retained) in cases
        {
            let dir = test_dir(&format!("compaction-recovery-{name}"));
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, source).unwrap();
            let mut store = HistoryStore::open(&dir, opened_at, capacity()).unwrap();
            let live_boot = Record::Boot(BootRecord {
                timestamp: opened_at,
                boot_id: store.boot_id().to_owned(),
                capacity: capacity(),
            });

            if sample_fresh_boot {
                store
                    .append_sample(opened_at + 1, capacity(), state(9.0))
                    .unwrap();
            }
            if name == "fresh-boot-without-full-sample" {
                assert!(
                    store.replay.gaps.iter().all(|gap| gap.to <= cutoff),
                    "{name}: clean historical evidence has no recovery gap in the retained horizon"
                );
            }
            let before = fs::read(&path).unwrap();
            let live_sample =
                expect_durable.then(|| runtime_sample(opened_at + 1, store.boot_id(), 9.0));

            let outcome = store.compact(cutoff).unwrap();
            if !expect_durable {
                assert!(matches!(outcome, CompactionOutcome::Deferred));
                assert_eq!(
                    fs::read(&path).unwrap(),
                    before,
                    "{name}: deferred compaction preserves incomplete evidence byte-for-byte"
                );
                validate_canonical(&path).unwrap();
            } else {
                assert!(matches!(outcome, CompactionOutcome::Durable));
                let mut expected =
                    records_from_fixture(&retained.expect("durable case retains rows"));
                expected.push(live_boot);
                expected.push(live_sample.expect("durable case has a live full sample"));
                assert_complete_canonical_path(&path, &expected, name);
                assert_ne!(
                    fs::read(&path).unwrap(),
                    before,
                    "{name}: old gap bytes are removed"
                );
            }
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn compaction_crash_points_leave_only_old_or_complete_new_canonical_bytes() {
        let source = format!(
            "{}{}{}{}",
            boot(10, "boot-a"),
            sample(11, "boot-a", 1.0),
            checkpoint(12, "boot-a"),
            sample(13, "boot-a", 2.0)
        );
        let retained = format!(
            "{}{}{}",
            boot(10, "boot-a"),
            sample(11, "boot-a", 1.0),
            sample(13, "boot-a", 2.0)
        );

        for point in [
            FailurePoint::CompactionTempCreate,
            FailurePoint::CompactionTempWrite,
            FailurePoint::CompactionTempSync,
            FailurePoint::CompactionRename,
        ] {
            let dir = test_dir(&format!("compaction-crash-{point:?}"));
            let path = dir.join("history-v1.jsonl");
            fs::write(&path, &source).unwrap();
            let mut store = HistoryStore::open(&dir, 20, capacity()).unwrap();
            store.append_sample(21, capacity(), state(9.0)).unwrap();
            let before = fs::read(&path).unwrap();

            let injected = compact_with_test_failure(&mut store, 13, point);
            assert!(
                injected.result.is_err(),
                "{point:?}: replacement must fail before commit"
            );
            assert_eq!(
                injected.fired,
                Some(point),
                "{point:?}: the injected crash point must actually fire"
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                before,
                "{point:?}: canonical path remains the complete old bytes"
            );
            validate_canonical(&path).unwrap();
            assert_eq!(
                stale_temporary_count(&dir).unwrap(),
                0,
                "{point:?}: failed compaction must remove its attempt temporary"
            );
            let _ = fs::remove_dir_all(dir);
        }

        let dir = test_dir("compaction-crash-directory-sync");
        let path = dir.join("history-v1.jsonl");
        fs::write(&path, source).unwrap();
        let mut store = HistoryStore::open(&dir, 20, capacity()).unwrap();
        let live_boot = canonical_records(&fs::read(&path).unwrap())
            .last()
            .cloned()
            .expect("open appends its fresh boot");
        store.append_sample(21, capacity(), state(9.0)).unwrap();
        let before = fs::read(&path).unwrap();
        let live_sample = canonical_records(&before)
            .last()
            .cloned()
            .expect("fresh runtime boot receives a full sample before compaction");

        let injected =
            compact_with_test_failure(&mut store, 13, FailurePoint::CompactionDirectorySync);
        assert!(matches!(
            injected.result,
            Ok(CompactionOutcome::CommittedSyncPending(_))
        ));
        assert_eq!(
            injected.fired,
            Some(FailurePoint::CompactionDirectorySync),
            "directory-sync injection must fire after rename"
        );
        let mut expected = records_from_fixture(&retained);
        expected.push(live_boot);
        expected.push(live_sample);
        assert_complete_canonical_path(&path, &expected, "directory-sync failure");
        assert_ne!(
            fs::read(&path).unwrap(),
            before,
            "directory sync follows committed rename"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn compaction_restart_rollup_and_subsequent_append_use_the_replacement_inode() {
        let dir = test_dir("compaction-restart-replacement");
        let path = dir.join("history-v1.jsonl");
        fs::write(
            &path,
            format!(
                "{}{}{}{}",
                boot(10, "boot-a"),
                sample(11, "boot-a", 1.0),
                checkpoint(12, "boot-a"),
                sample(13, "boot-a", 2.0)
            ),
        )
        .unwrap();
        let mut store = HistoryStore::open(&dir, 20, capacity()).unwrap();
        store.append_sample(21, capacity(), state(9.0)).unwrap();
        #[cfg(unix)]
        let old_inode = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&path).unwrap().ino()
        };
        let live_boot_id = store.boot_id().to_owned();

        assert!(matches!(
            store.compact(13).unwrap(),
            CompactionOutcome::Durable
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "compaction replacement must preserve history-v1.jsonl mode"
            );
        }
        #[cfg(unix)]
        let replacement_inode = {
            use std::os::unix::fs::MetadataExt;
            let inode = fs::metadata(&path).unwrap().ino();
            assert_ne!(
                inode, old_inode,
                "compaction atomically replaces the canonical inode"
            );
            inode
        };
        store.append_sample(22, capacity(), state(10.0)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(&path).unwrap().ino(),
                replacement_inode,
                "the post-compaction append remains on the replacement inode"
            );
        }
        let records = canonical_records(&fs::read(&path).unwrap());
        assert!(matches!(
            records.last(),
            Some(Record::Sample(sample))
                if sample.timestamp == 22 && sample.boot_id == live_boot_id && sample.state == state(10.0)
        ));
        drop(store);

        let reopened = HistoryStore::open(&dir, 23, capacity()).unwrap();
        assert!(matches!(
            canonical_records(&fs::read(&path).unwrap()).last(),
            Some(Record::Boot(boot)) if boot.timestamp == 23
        ));
        drop(reopened);
        let history = History::open(dir.clone(), 0, history_capacity()).unwrap();
        assert_eq!(
            history.rollup(12, 13, 1000).data.totals,
            vec![metric_value("nimproxy_requests_total", 1.0)],
            "the pre-window full-sample baseline still normalizes the compacted sample"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn compaction_serializes_an_append_requested_while_it_owns_the_store() {
        let dir = test_dir("compaction-exclusive-writer");
        let path = dir.join("history-v1.jsonl");
        fs::write(
            &path,
            format!(
                "{}{}{}{}",
                boot(10, "boot-a"),
                sample(11, "boot-a", 1.0),
                checkpoint(12, "boot-a"),
                sample(13, "boot-a", 2.0)
            ),
        )
        .unwrap();
        let mut store = HistoryStore::open(&dir, 20, capacity()).unwrap();
        let live_boot_id = store.boot_id().to_owned();
        store.append_sample(21, capacity(), state(7.0)).unwrap();
        let shared = Arc::new(Mutex::new(store));
        let (locked_tx, locked_rx) = mpsc::channel();
        let (contention_tx, contention_rx) = mpsc::channel();
        let (start_compaction_tx, start_compaction_rx) = mpsc::sync_channel(0);

        std::thread::scope(|scope| {
            let compacting = Arc::clone(&shared);
            let compact = scope.spawn(move || {
                let mut store = compacting.lock().unwrap();
                locked_tx.send(()).unwrap();
                start_compaction_rx.recv().unwrap();
                store.compact(13)
            });
            locked_rx.recv().unwrap();

            let appending = Arc::clone(&shared);
            let append = scope.spawn(move || {
                match appending.try_lock() {
                    Err(TryLockError::WouldBlock) => {}
                    Err(TryLockError::Poisoned(_)) => {
                        panic!("the exclusive history writer must not poison the store mutex")
                    }
                    Ok(_) => panic!("append must contend while compaction owns the store"),
                }
                contention_tx.send(()).unwrap();
                appending
                    .lock()
                    .unwrap()
                    .append_sample(22, capacity(), state(9.0))
            });
            contention_rx.recv().unwrap();
            start_compaction_tx.send(()).unwrap();

            assert!(matches!(
                compact.join().unwrap().unwrap(),
                CompactionOutcome::Durable
            ));
            append.join().unwrap().unwrap();
        });

        let expected = vec![
            records_from_fixture(&boot(10, "boot-a"))[0].clone(),
            records_from_fixture(&sample(11, "boot-a", 1.0))[0].clone(),
            records_from_fixture(&sample(13, "boot-a", 2.0))[0].clone(),
            records_from_fixture(&boot(20, &live_boot_id))[0].clone(),
            runtime_sample(21, &live_boot_id, 7.0),
            runtime_sample(22, &live_boot_id, 9.0),
        ];
        assert_complete_canonical_path(&path, &expected, "serialized append");
        assert_eq!(
            canonical_records(&fs::read(&path).unwrap())
                .iter()
                .filter(|record| {
                    matches!(record, Record::Sample(sample)
                        if sample.timestamp == 22
                            && sample.boot_id == live_boot_id
                            && sample.state == state(9.0))
                })
                .count(),
            1,
            "the append requested during compaction appears exactly once after replacement"
        );
        let _ = fs::remove_dir_all(dir);
    }
}
