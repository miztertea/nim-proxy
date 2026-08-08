//! Private, side-band NIM response observations.
//!
//! The observer has no effect on response framing or relay bytes.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Observation {
    Measured(u64),
    Estimated(u64),
    Unavailable,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageObservations {
    pub(crate) cached_tokens: Observation,
    pub(crate) completion_tokens: Observation,
    pub(crate) prompt_tokens: Observation,
    pub(crate) reasoning_tokens: Observation,
    pub(crate) total_tokens: Observation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResponseObservations {
    pub(crate) finish_reasons: Vec<FinishObservation>,
    pub(crate) tool_calls: Observation,
    pub(crate) usage: UsageObservations,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageObservationMetric {
    pub(crate) field: &'static str,
    pub(crate) result: &'static str,
}

/// Map the finalized fixed usage fields to the only bounded observation metric
/// labels. This is the sole classification-to-metric boundary: callers must
/// not inspect response values or recreate these field/result pairs.
pub(crate) fn usage_observation_metrics(usage: &UsageObservations) -> Vec<UsageObservationMetric> {
    let result = |observation: &Observation| match observation {
        Observation::Measured(_) => "measured",
        Observation::Estimated(_) => "estimated",
        Observation::Unavailable => "unavailable",
        Observation::Invalid => "invalid",
    };
    vec![
        UsageObservationMetric {
            field: "prompt_tokens",
            result: result(&usage.prompt_tokens),
        },
        UsageObservationMetric {
            field: "completion_tokens",
            result: result(&usage.completion_tokens),
        },
        UsageObservationMetric {
            field: "total_tokens",
            result: result(&usage.total_tokens),
        },
        UsageObservationMetric {
            field: "cached_tokens",
            result: result(&usage.cached_tokens),
        },
        UsageObservationMetric {
            field: "reasoning_tokens",
            result: result(&usage.reasoning_tokens),
        },
    ]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FinishObservation {
    pub(crate) choice_index: u64,
    pub(crate) result: FinishResult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FinishResult {
    Measured(FinishReason),
    Unavailable,
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FinishReason {
    ContentFilter,
    FunctionCall,
    Length,
    Other,
    Stop,
    ToolCalls,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamOutcome {
    Completed,
    Disconnected,
    Truncated,
}

use std::collections::{BTreeMap, BTreeSet};

const MAX_EVENT_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Default)]
enum SeenNumber {
    #[default]
    Absent,
    Invalid,
    Value(u64),
}

impl SeenNumber {
    fn observe(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value else { return };
        let next = value.as_u64().map_or(Self::Invalid, Self::Value);
        *self = match (*self, next) {
            (Self::Absent, value) => value,
            (Self::Value(a), Self::Value(b)) if a == b => Self::Value(a),
            _ => Self::Invalid,
        };
    }

    fn invalidate(&mut self) {
        *self = Self::Invalid;
    }

    fn result(self) -> Observation {
        match self {
            Self::Absent => Observation::Unavailable,
            Self::Invalid => Observation::Invalid,
            Self::Value(value) => Observation::Measured(value),
        }
    }
}

#[derive(Default)]
struct UsageState {
    cached: SeenNumber,
    completion: SeenNumber,
    prompt: SeenNumber,
    reasoning: SeenNumber,
    total: SeenNumber,
}

impl UsageState {
    fn observe(&mut self, usage: Option<&serde_json::Value>) {
        let Some(usage) = usage else { return };
        if usage.is_null() {
            return;
        }
        let Some(usage) = usage.as_object() else {
            self.cached.invalidate();
            self.completion.invalidate();
            self.prompt.invalidate();
            self.reasoning.invalidate();
            self.total.invalidate();
            return;
        };
        self.prompt.observe(usage.get("prompt_tokens"));
        self.completion.observe(usage.get("completion_tokens"));
        self.total.observe(usage.get("total_tokens"));
        match usage.get("prompt_tokens_details") {
            None => {}
            Some(serde_json::Value::Object(details)) => {
                self.cached.observe(details.get("cached_tokens"));
            }
            Some(_) => self.cached.invalidate(),
        }
        match usage.get("completion_tokens_details") {
            None => {}
            Some(serde_json::Value::Object(details)) => {
                self.reasoning.observe(details.get("reasoning_tokens"));
            }
            Some(_) => self.reasoning.invalidate(),
        }
    }

    fn finish(self, estimate: Option<u64>) -> UsageObservations {
        let prompt = self.prompt.result();
        let mut completion = self.completion.result();
        let mut total = self.total.result();
        let mut cached = self.cached.result();
        let mut reasoning = self.reasoning.result();

        if let (Observation::Measured(prompt_value), Observation::Measured(completion_value)) =
            (&prompt, &completion)
        {
            match prompt_value.checked_add(*completion_value) {
                None => total = Observation::Invalid,
                Some(sum) if matches!(total, Observation::Measured(value) if value < sum) => {
                    total = Observation::Invalid;
                }
                Some(_) => {}
            }
        }
        if let Observation::Measured(value) = cached {
            if !matches!(prompt, Observation::Measured(parent) if value <= parent) {
                cached = Observation::Invalid;
            }
        }
        if let Observation::Measured(value) = reasoning {
            if !matches!(completion, Observation::Measured(parent) if value <= parent) {
                reasoning = Observation::Invalid;
            }
        }
        if matches!(completion, Observation::Unavailable) {
            if let Some(count) = estimate.filter(|count| *count > 0) {
                completion = Observation::Estimated(count);
            }
        }
        UsageObservations {
            cached_tokens: cached,
            completion_tokens: completion,
            prompt_tokens: prompt,
            reasoning_tokens: reasoning,
            total_tokens: total,
        }
    }
}

#[derive(Clone, Copy, Default)]
enum SeenFinish {
    #[default]
    Absent,
    Invalid,
    Value(FinishReason),
}

impl SeenFinish {
    fn observe(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value else { return };
        if value.is_null() {
            return;
        }
        let next = match value.as_str() {
            Some("content_filter") => Self::Value(FinishReason::ContentFilter),
            Some("function_call") => Self::Value(FinishReason::FunctionCall),
            Some("length") => Self::Value(FinishReason::Length),
            Some("stop") => Self::Value(FinishReason::Stop),
            Some("tool_calls") => Self::Value(FinishReason::ToolCalls),
            Some(_) => Self::Value(FinishReason::Other),
            None => Self::Invalid,
        };
        *self = match (*self, next) {
            (Self::Absent, value) => value,
            (Self::Value(a), Self::Value(b)) if a == b => Self::Value(a),
            _ => Self::Invalid,
        };
    }

    fn result(self, incomplete: bool) -> FinishResult {
        if incomplete {
            return FinishResult::Unavailable;
        }
        match self {
            Self::Absent => FinishResult::Unavailable,
            Self::Invalid => FinishResult::Invalid,
            Self::Value(reason) => FinishResult::Measured(reason),
        }
    }
}

#[derive(Default)]
struct ObservationState {
    finishes: BTreeMap<u64, SeenFinish>,
    buffered_tool_calls: u64,
    observed_stream: bool,
    tools_invalid: bool,
    tools_present: bool,
    tool_pairs: BTreeSet<(u64, u64)>,
    usage: UsageState,
}

impl ObservationState {
    fn observe_buffered(&mut self, value: &serde_json::Value) {
        self.usage.observe(value.get("usage"));
        let Some(choices) = value.get("choices") else {
            return;
        };
        let Some(choices) = choices.as_array() else {
            self.tools_invalid = true;
            return;
        };
        self.tools_present = true;
        let mut calls = 0u64;
        for choice in choices {
            let Some(choice) = choice.as_object() else {
                self.tools_invalid = true;
                continue;
            };
            let Some(index) = choice.get("index").and_then(serde_json::Value::as_u64) else {
                self.tools_invalid = true;
                continue;
            };
            self.finishes
                .entry(index)
                .or_default()
                .observe(choice.get("finish_reason"));
            match choice.get("message") {
                None => {}
                Some(serde_json::Value::Object(message)) => match message.get("tool_calls") {
                    None => {}
                    Some(serde_json::Value::Array(tool_calls)) => {
                        let Some(next) = calls.checked_add(tool_calls.len() as u64) else {
                            self.tools_invalid = true;
                            continue;
                        };
                        calls = next;
                    }
                    Some(_) => self.tools_invalid = true,
                },
                Some(_) => self.tools_invalid = true,
            }
        }
        self.buffered_tool_calls = calls;
    }

    fn observe_stream(&mut self, value: &serde_json::Value) -> bool {
        self.observed_stream = true;
        self.usage.observe(value.get("usage"));
        let Some(choices) = value.get("choices") else {
            return false;
        };
        let Some(choices) = choices.as_array() else {
            self.tools_invalid = true;
            return false;
        };
        self.tools_present = true;
        let mut countable = !choices.is_empty();
        for choice in choices {
            let Some(choice) = choice.as_object() else {
                self.tools_invalid = true;
                countable = false;
                continue;
            };
            let Some(index) = choice.get("index").and_then(serde_json::Value::as_u64) else {
                self.tools_invalid = true;
                countable = false;
                continue;
            };
            let finish = choice.get("finish_reason");
            self.finishes.entry(index).or_default().observe(finish);
            if !finish.is_none_or(serde_json::Value::is_null) {
                countable = false;
            }
            match choice.get("delta") {
                None => {}
                Some(serde_json::Value::Object(delta)) => match delta.get("tool_calls") {
                    None => {}
                    Some(serde_json::Value::Array(tool_calls)) => {
                        for tool_call in tool_calls {
                            let Some(tool_call) = tool_call.as_object() else {
                                self.tools_invalid = true;
                                continue;
                            };
                            let Some(tool_index) =
                                tool_call.get("index").and_then(serde_json::Value::as_u64)
                            else {
                                self.tools_invalid = true;
                                continue;
                            };
                            self.tool_pairs.insert((index, tool_index));
                        }
                    }
                    Some(_) => self.tools_invalid = true,
                },
                Some(_) => self.tools_invalid = true,
            }
        }
        countable
    }

    fn remember_choice_indexes(&mut self, value: &serde_json::Value) {
        let Some(choices) = value.get("choices").and_then(serde_json::Value::as_array) else {
            return;
        };
        for choice in choices {
            if let Some(index) = choice
                .as_object()
                .and_then(|choice| choice.get("index"))
                .and_then(serde_json::Value::as_u64)
            {
                self.finishes.entry(index).or_default();
            }
        }
    }

    fn finish(self, incomplete: bool, estimate: Option<u64>) -> ResponseObservations {
        let usage = if incomplete {
            unavailable_usage()
        } else {
            self.usage.finish(estimate)
        };
        let tool_calls = if incomplete {
            Observation::Unavailable
        } else if self.tools_invalid {
            Observation::Invalid
        } else if !self.tools_present {
            Observation::Unavailable
        } else if self.observed_stream {
            Observation::Measured(self.tool_pairs.len() as u64)
        } else {
            Observation::Measured(self.buffered_tool_calls)
        };
        ResponseObservations {
            finish_reasons: self
                .finishes
                .into_iter()
                .map(|(choice_index, result)| FinishObservation {
                    choice_index,
                    result: result.result(incomplete),
                })
                .collect(),
            tool_calls,
            usage,
        }
    }
}

#[derive(Default)]
pub(crate) struct SseObserver {
    state: ObservationState,
    current_data: Vec<u8>,
    current_line: Vec<u8>,
    discarded_event: bool,
    discarded_line_has_content: bool,
    discarded_line_ends_cr: bool,
    completion_events: u64,
}

impl SseObserver {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.finish_line();
                continue;
            }
            if self.discarded_event {
                if byte == b'\r' {
                    if self.discarded_line_ends_cr {
                        self.discarded_line_has_content = true;
                    }
                    self.discarded_line_ends_cr = true;
                } else {
                    self.discarded_line_has_content = true;
                    self.discarded_line_ends_cr = false;
                }
                continue;
            }
            if self.retained_len() == MAX_EVENT_BYTES {
                self.current_data.clear();
                self.current_line.clear();
                self.discarded_event = true;
                self.discarded_line_has_content = true;
                self.discarded_line_ends_cr = false;
                continue;
            }
            self.current_line.push(byte);
        }
    }

    fn finish_line(&mut self) {
        if self.discarded_event {
            self.current_line.clear();
            if !self.discarded_line_has_content {
                self.discarded_event = false;
            }
            self.discarded_line_has_content = false;
            self.discarded_line_ends_cr = false;
            return;
        }
        if self.current_line.last() == Some(&b'\r') {
            self.current_line.pop();
        }
        if self.current_line.is_empty() {
            self.classify_event();
            return;
        }
        if let Some(data) = self.current_line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            let separator = usize::from(!self.current_data.is_empty());
            if data.len().saturating_add(separator)
                > MAX_EVENT_BYTES.saturating_sub(self.current_data.len())
            {
                self.current_data.clear();
                self.current_line.clear();
                self.discarded_event = true;
                self.discarded_line_has_content = true;
                self.discarded_line_ends_cr = false;
                return;
            }
            if separator == 1 {
                self.current_data.push(b'\n');
            }
            self.current_data.extend_from_slice(data);
        }
        self.current_line.clear();
    }

    fn classify_event(&mut self) {
        if self.current_data != b"[DONE]" {
            if let Ok(data) = std::str::from_utf8(&self.current_data) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    if self.state.observe_stream(&value) {
                        self.completion_events = self.completion_events.saturating_add(1);
                    }
                }
            }
        }
        self.current_data.clear();
        self.current_line.clear();
    }

    fn retained_len(&self) -> usize {
        self.current_data.len() + self.current_line.len()
    }

    fn remember_unterminated_indexes(&mut self) {
        if let Ok(data) = std::str::from_utf8(&self.current_data) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                self.state.remember_choice_indexes(&value);
            }
        }
        if let Some(data) = self.current_line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if let Ok(data) = std::str::from_utf8(data) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    self.state.remember_choice_indexes(&value);
                }
            }
        }
    }

    pub(crate) fn finish(mut self, outcome: StreamOutcome) -> ResponseObservations {
        let unterminated =
            self.discarded_event || !self.current_data.is_empty() || !self.current_line.is_empty();
        if outcome == StreamOutcome::Completed && unterminated {
            self.remember_unterminated_indexes();
        }
        let incomplete = outcome != StreamOutcome::Completed || unterminated;
        self.state
            .finish(incomplete, (!incomplete).then_some(self.completion_events))
    }

    #[cfg(test)]
    fn retained_event_bytes(&self) -> usize {
        self.current_data.len() + self.current_line.len()
    }
}

pub(crate) fn observe_buffered(body: &[u8]) -> ResponseObservations {
    let mut state = ObservationState::default();
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        state.observe_buffered(&value);
    }
    state.finish(false, None)
}

fn unavailable_usage() -> UsageObservations {
    UsageObservations {
        cached_tokens: Observation::Unavailable,
        completion_tokens: Observation::Unavailable,
        prompt_tokens: Observation::Unavailable,
        reasoning_tokens: Observation::Unavailable,
        total_tokens: Observation::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        observe_buffered, usage_observation_metrics, FinishObservation, FinishReason, FinishResult,
        Observation, ResponseObservations, SseObserver, StreamOutcome, UsageObservationMetric,
        UsageObservations,
    };

    #[test]
    fn observation_metric_records_each_final_usage_result_with_closed_labels() {
        // Mutation caught: omitting the Task 16 counter, or emitting a raw
        // response/request value instead of one final bounded field/result pair.
        let usage = UsageObservations {
            prompt_tokens: Observation::Measured(0),
            completion_tokens: Observation::Estimated(3),
            total_tokens: Observation::Invalid,
            cached_tokens: Observation::Unavailable,
            reasoning_tokens: Observation::Measured(2),
        };

        assert_eq!(
            usage_observation_metrics(&usage),
            vec![
                UsageObservationMetric {
                    field: "prompt_tokens",
                    result: "measured"
                },
                UsageObservationMetric {
                    field: "completion_tokens",
                    result: "estimated"
                },
                UsageObservationMetric {
                    field: "total_tokens",
                    result: "invalid"
                },
                UsageObservationMetric {
                    field: "cached_tokens",
                    result: "unavailable"
                },
                UsageObservationMetric {
                    field: "reasoning_tokens",
                    result: "measured"
                },
            ],
            "nimproxy_usage_observations_total must have only literal field/result labels"
        );
    }

    #[test]
    fn observation_metric_finalizes_disconnected_stream_as_unavailable() {
        // Mutation caught: discarding the finalized observer result on a
        // client disconnect instead of counting all five unavailable fields.
        let mut observer = SseObserver::default();
        observer.push(b"data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n");
        let usage = observer.finish(StreamOutcome::Disconnected).usage;

        assert_eq!(
            usage_observation_metrics(&usage),
            vec![
                UsageObservationMetric {
                    field: "prompt_tokens",
                    result: "unavailable"
                },
                UsageObservationMetric {
                    field: "completion_tokens",
                    result: "unavailable"
                },
                UsageObservationMetric {
                    field: "total_tokens",
                    result: "unavailable"
                },
                UsageObservationMetric {
                    field: "cached_tokens",
                    result: "unavailable"
                },
                UsageObservationMetric {
                    field: "reasoning_tokens",
                    result: "unavailable"
                },
            ],
            "a disconnected stream has five final unavailable observations"
        );
    }

    #[test]
    fn observation_metric_records_successful_buffered_usage_as_measured() {
        // Mutation caught: bypassing final counter accounting for a successful
        // buffered response, including the zero-valued measured observation.
        let usage = observe_buffered(
            br#"{"usage":{"prompt_tokens":0,"completion_tokens":2,"total_tokens":2,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":1}}}"#,
        )
        .usage;

        assert_eq!(
            usage_observation_metrics(&usage),
            vec![
                UsageObservationMetric {
                    field: "prompt_tokens",
                    result: "measured"
                },
                UsageObservationMetric {
                    field: "completion_tokens",
                    result: "measured"
                },
                UsageObservationMetric {
                    field: "total_tokens",
                    result: "measured"
                },
                UsageObservationMetric {
                    field: "cached_tokens",
                    result: "measured"
                },
                UsageObservationMetric {
                    field: "reasoning_tokens",
                    result: "measured"
                },
            ],
            "a successful buffered response finalizes all five usage fields"
        );
    }

    #[test]
    fn observation_metric_finalizes_truncated_or_unterminated_stream_as_unavailable() {
        // Mutation caught: treating an upstream truncation or nominal EOF with
        // an unterminated SSE event as a completed measured observation.
        for outcome in [StreamOutcome::Truncated, StreamOutcome::Completed] {
            let mut observer = SseObserver::default();
            let event = if outcome == StreamOutcome::Truncated {
                b"data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n".as_slice()
            } else {
                b"data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}".as_slice()
            };
            observer.push(event);

            assert_eq!(
                usage_observation_metrics(&observer.finish(outcome).usage),
                vec![
                    UsageObservationMetric {
                        field: "prompt_tokens",
                        result: "unavailable"
                    },
                    UsageObservationMetric {
                        field: "completion_tokens",
                        result: "unavailable"
                    },
                    UsageObservationMetric {
                        field: "total_tokens",
                        result: "unavailable"
                    },
                    UsageObservationMetric {
                        field: "cached_tokens",
                        result: "unavailable"
                    },
                    UsageObservationMetric {
                        field: "reasoning_tokens",
                        result: "unavailable"
                    },
                ],
                "{outcome:?} has five final unavailable observations"
            );
        }
    }

    fn fixture_body(name: &str) -> Vec<u8> {
        let fixture = match name {
            "buffered-basic" => {
                include_str!("../tests/fixtures/nim-observations/buffered-basic.json")
            }
            "buffered-tools" => {
                include_str!("../tests/fixtures/nim-observations/buffered-tools.json")
            }
            "streamed-basic" => {
                include_str!("../tests/fixtures/nim-observations/streamed-basic.json")
            }
            "streamed-tools" => {
                include_str!("../tests/fixtures/nim-observations/streamed-tools.json")
            }
            _ => panic!("unknown literal evidence fixture: {name}"),
        };
        serde_json::from_str::<serde_json::Value>(fixture).unwrap()["body"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    fn assert_fixture_outcome(
        actual: ResponseObservations,
        finish: FinishReason,
        tools: u64,
        production_mutation: &str,
    ) {
        assert_eq!(
            actual.usage.prompt_tokens,
            Observation::Measured(1),
            "{production_mutation}: prompt must come from the literal fixture"
        );
        assert_eq!(
            actual.usage.completion_tokens,
            Observation::Measured(2),
            "{production_mutation}: completion must come from the literal fixture"
        );
        assert_eq!(
            actual.usage.total_tokens,
            Observation::Measured(3),
            "{production_mutation}: total must come from the literal fixture"
        );
        assert_eq!(actual.usage.cached_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Unavailable);
        assert_eq!(actual.tool_calls, Observation::Measured(tools));
        assert_eq!(
            actual.finish_reasons,
            vec![FinishObservation {
                choice_index: 0,
                result: FinishResult::Measured(finish),
            }],
            "{production_mutation}: finish observation must stay choice-indexed"
        );
    }

    #[test]
    fn buffered_fixture_conformance_rejects_old_first_choice_and_usage_shortcuts() {
        // Mutation caught: retaining the old ad-hoc buffered extraction loses
        // typed unavailable fields and does not make choice/tool classification
        // a checked observation boundary.
        assert_fixture_outcome(
            observe_buffered(&fixture_body("buffered-basic")),
            FinishReason::Stop,
            0,
            "old buffered shortcut",
        );
        assert_fixture_outcome(
            observe_buffered(&fixture_body("buffered-tools")),
            FinishReason::ToolCalls,
            1,
            "old buffered shortcut",
        );
    }

    #[test]
    fn streamed_fixture_conformance_rejects_old_line_scanner() {
        // Mutation caught: the previous line scanner treats a null progress
        // reason and the usage-only final event as unrelated observations.
        for (name, finish, tools) in [
            ("streamed-basic", FinishReason::Stop, 0),
            ("streamed-tools", FinishReason::ToolCalls, 1),
        ] {
            let mut observer = SseObserver::default();
            observer.push(&fixture_body(name));
            assert_fixture_outcome(
                observer.finish(StreamOutcome::Completed),
                finish,
                tools,
                "old SseScan line scanner",
            );
        }
    }

    #[test]
    fn usage_integer_boundaries_reject_non_u64_without_silent_casting() {
        // Mutation caught: serde Value::as_u64-style extraction silently drops
        // malformed presentations rather than emitting Invalid for that field.
        for (body, expected) in [
            (
                br#"{"usage":{"prompt_tokens":0,"completion_tokens":18446744073709551615}}"#
                    .as_slice(),
                (Observation::Measured(0), Observation::Measured(u64::MAX)),
            ),
            (
                br#"{"usage":{"prompt_tokens":18446744073709551616,"completion_tokens":-1}}"#
                    .as_slice(),
                (Observation::Invalid, Observation::Invalid),
            ),
            (
                br#"{"usage":{"prompt_tokens":1.0,"completion_tokens":"2"}}"#.as_slice(),
                (Observation::Invalid, Observation::Invalid),
            ),
            (
                br#"{"usage":{"prompt_tokens":null,"completion_tokens":{}}}"#.as_slice(),
                (Observation::Invalid, Observation::Invalid),
            ),
        ] {
            let actual = observe_buffered(body);
            assert_eq!(
                actual.usage.prompt_tokens, expected.0,
                "usage integer boundary"
            );
            assert_eq!(
                actual.usage.completion_tokens, expected.1,
                "usage integer boundary"
            );
        }

        let actual = observe_buffered(br#"{"usage":[]}"#);
        assert_eq!(actual.usage.prompt_tokens, Observation::Invalid);
        assert_eq!(actual.usage.completion_tokens, Observation::Invalid);
        assert_eq!(actual.usage.total_tokens, Observation::Invalid);
        assert_eq!(actual.usage.cached_tokens, Observation::Invalid);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Invalid);
    }

    #[test]
    fn stream_observer_reassembles_crlf_multiline_comments_and_every_split() {
        // Mutation caught: line-local parsing misses a valid event split at an
        // arbitrary transport boundary or changes the standard SSE data join.
        let bytes = b": keepalive\r\n\r\ndata: {\"choices\":[{\"index\":0,\r\ndata: \"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\r\n\r\n";
        for split in 0..=bytes.len() {
            let mut observer = SseObserver::default();
            observer.push(&bytes[..split]);
            observer.push(&bytes[split..]);
            assert_fixture_outcome(
                observer.finish(StreamOutcome::Completed),
                FinishReason::Stop,
                0,
                "split/CRLF/multi-line SSE parser",
            );
        }
    }

    #[test]
    fn stream_observer_invalidates_duplicate_conflicts_and_relationships() {
        // Mutation caught: first-value-wins scanning accepts contradictory
        // duplicate usage, cached, or reasoning presentations.
        let mut observer = SseObserver::default();
        observer.push(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":3},\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n");
        let actual = observer.finish(StreamOutcome::Completed);
        assert_eq!(actual.usage.prompt_tokens, Observation::Invalid);
        assert_eq!(actual.usage.completion_tokens, Observation::Measured(2));
        assert_eq!(actual.usage.cached_tokens, Observation::Invalid);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Invalid);

        let mut equal_duplicates = SseObserver::default();
        equal_duplicates.push(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n");
        let actual = equal_duplicates.finish(StreamOutcome::Completed);
        assert_eq!(actual.usage.prompt_tokens, Observation::Measured(1));
        assert_eq!(actual.usage.completion_tokens, Observation::Measured(2));
    }

    #[test]
    fn relationship_matrix_invalidates_only_dependent_usage_fields() {
        // Mutation caught: accepting unchecked totals or discarding healthy
        // siblings after one sum/parent relationship is invalid.
        let actual = observe_buffered(b"{\"usage\":{\"prompt_tokens\":18446744073709551615,\"completion_tokens\":1,\"total_tokens\":18446744073709551615}}");
        assert_eq!(actual.usage.prompt_tokens, Observation::Measured(u64::MAX));
        assert_eq!(actual.usage.completion_tokens, Observation::Measured(1));
        assert_eq!(actual.usage.total_tokens, Observation::Invalid);
        assert_eq!(actual.usage.cached_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Unavailable);

        let actual = observe_buffered(
            b"{\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":2}}",
        );
        assert_eq!(actual.usage.prompt_tokens, Observation::Measured(1));
        assert_eq!(actual.usage.completion_tokens, Observation::Measured(2));
        assert_eq!(actual.usage.total_tokens, Observation::Invalid);

        let actual = observe_buffered(
            b"{\"usage\":{\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":1}}}",
        );
        assert_eq!(actual.usage.prompt_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.completion_tokens, Observation::Measured(2));
        assert_eq!(actual.usage.cached_tokens, Observation::Invalid);

        let actual = observe_buffered(
            b"{\"usage\":{\"prompt_tokens\":\"1\",\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":1}}}",
        );
        assert_eq!(actual.usage.prompt_tokens, Observation::Invalid);
        assert_eq!(actual.usage.completion_tokens, Observation::Measured(2));
        assert_eq!(actual.usage.cached_tokens, Observation::Invalid);

        let actual = observe_buffered(
            b"{\"usage\":{\"prompt_tokens\":1,\"completion_tokens_details\":{\"reasoning_tokens\":1}}}",
        );
        assert_eq!(actual.usage.prompt_tokens, Observation::Measured(1));
        assert_eq!(actual.usage.completion_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Invalid);

        let actual = observe_buffered(
            b"{\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":\"2\",\"completion_tokens_details\":{\"reasoning_tokens\":1}}}",
        );
        assert_eq!(actual.usage.prompt_tokens, Observation::Measured(1));
        assert_eq!(actual.usage.completion_tokens, Observation::Invalid);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Invalid);

        let mut observer = SseObserver::default();
        observer.push(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"completion_tokens_details\":{\"reasoning_tokens\":1}}}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":3}}\n\n");
        let actual = observer.finish(StreamOutcome::Completed);
        assert_eq!(actual.usage.prompt_tokens, Observation::Measured(1));
        assert_eq!(actual.usage.completion_tokens, Observation::Invalid);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Invalid);
    }

    #[test]
    fn choice_and_tool_observations_reject_old_first_choice_and_max_index_rules() {
        // Mutation caught: first-choice-only finish extraction and max-index
        // tool counting lose valid choices, accept malformed shapes, and count
        // duplicate stream fragments as separate calls.
        let actual = observe_buffered(b"{\"choices\":[{\"index\":1,\"message\":{},\"finish_reason\":\"length\"},{\"index\":0,\"message\":{},\"finish_reason\":\"content_filter\"}]} ");
        assert_eq!(
            actual.finish_reasons,
            vec![
                FinishObservation {
                    choice_index: 0,
                    result: FinishResult::Measured(FinishReason::ContentFilter),
                },
                FinishObservation {
                    choice_index: 1,
                    result: FinishResult::Measured(FinishReason::Length),
                },
            ]
        );
        assert_eq!(actual.tool_calls, Observation::Measured(0));

        let actual = observe_buffered(b"{\"choices\":[]}");
        assert_eq!(actual.tool_calls, Observation::Measured(0));
        let actual = observe_buffered(b"{}");
        assert_eq!(actual.tool_calls, Observation::Unavailable);
        let actual = observe_buffered(b"{\"choices\":{}}");
        assert_eq!(actual.tool_calls, Observation::Invalid);

        let actual = observe_buffered(b"{\"choices\":[{\"index\":0,\"message\":{\"tool_calls\":[{},{}]}},{\"index\":1,\"message\":{\"tool_calls\":[{},{},{}]}}]}");
        assert_eq!(actual.tool_calls, Observation::Measured(5));

        let actual = observe_buffered(b"{\"choices\":[7]}");
        assert_eq!(actual.tool_calls, Observation::Invalid);
        assert!(actual.finish_reasons.is_empty());

        let actual = observe_buffered(
            b"{\"choices\":[{\"index\":0,\"message\":7,\"finish_reason\":\"stop\"}]}",
        );
        assert_eq!(actual.tool_calls, Observation::Invalid);
        assert_eq!(
            actual.finish_reasons,
            vec![FinishObservation {
                choice_index: 0,
                result: FinishResult::Measured(FinishReason::Stop),
            }]
        );

        let actual = observe_buffered(
            b"{\"choices\":[{\"index\":0,\"message\":{\"tool_calls\":{}},\"finish_reason\":\"stop\"}]}",
        );
        assert_eq!(actual.tool_calls, Observation::Invalid);
        assert_eq!(
            actual.finish_reasons,
            vec![FinishObservation {
                choice_index: 0,
                result: FinishResult::Measured(FinishReason::Stop),
            }]
        );

        for body in [
            b"{\"choices\":[{\"message\":{}}]}".as_slice(),
            b"{\"choices\":[{\"index\":-1,\"message\":{}}]}".as_slice(),
            b"{\"choices\":[{\"index\":1.5,\"message\":{}}]}".as_slice(),
            b"{\"choices\":[{\"index\":18446744073709551616,\"message\":{}}]}".as_slice(),
        ] {
            let actual = observe_buffered(body);
            assert_eq!(actual.tool_calls, Observation::Invalid);
            assert!(
                actual.finish_reasons.is_empty(),
                "unrepresentable choice index must not fabricate a finish observation"
            );
        }

        let mut observer = SseObserver::default();
        observer.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0},{\"index\":0}]}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1}]}}]}\n\n");
        assert_eq!(
            observer.finish(StreamOutcome::Completed).tool_calls,
            Observation::Measured(2)
        );

        let mut malformed = SseObserver::default();
        malformed.push(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":-1}]}}]}\n\n",
        );
        assert_eq!(
            malformed.finish(StreamOutcome::Completed).tool_calls,
            Observation::Invalid
        );

        for bytes in [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[7]}}]}\n\n"
                .as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{}]}}]}\n\n"
                .as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":-1}]}}]}\n\n"
                .as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1.5}]}}]}\n\n"
                .as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":18446744073709551616}]}}]}\n\n"
                .as_slice(),
        ] {
            let mut observer = SseObserver::default();
            observer.push(bytes);
            assert_eq!(
                observer.finish(StreamOutcome::Completed).tool_calls,
                Observation::Invalid,
                "malformed streamed tool-call presentation must be invalid"
            );
        }

        for bytes in [
            b"data: {\"choices\":{}}\n\n".as_slice(),
            b"data: {\"choices\":[7]}\n\n".as_slice(),
        ] {
            let mut observer = SseObserver::default();
            observer.push(bytes);
            let actual = observer.finish(StreamOutcome::Completed);
            assert_eq!(actual.tool_calls, Observation::Invalid);
            assert!(
                actual.finish_reasons.is_empty(),
                "malformed streamed choices must not fabricate finish observations"
            );
        }
    }

    #[test]
    fn finish_taxonomy_and_completed_estimate_reject_silent_terminal_fallbacks() {
        // Mutation caught: treating unknown or malformed terminal values as a
        // first-choice string and estimating regardless of terminal validity.
        let actual = observe_buffered(b"{\"choices\":[{\"index\":0,\"message\":{},\"finish_reason\":\"function_call\"},{\"index\":1,\"message\":{},\"finish_reason\":\"future_reason\"},{\"index\":2,\"message\":{},\"finish_reason\":1},{\"index\":3,\"message\":{}}]}");
        assert_eq!(
            actual.finish_reasons,
            vec![
                FinishObservation {
                    choice_index: 0,
                    result: FinishResult::Measured(FinishReason::FunctionCall),
                },
                FinishObservation {
                    choice_index: 1,
                    result: FinishResult::Measured(FinishReason::Other),
                },
                FinishObservation {
                    choice_index: 2,
                    result: FinishResult::Invalid,
                },
                FinishObservation {
                    choice_index: 3,
                    result: FinishResult::Unavailable,
                },
            ]
        );

        let mut observer = SseObserver::default();
        observer.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
        assert_eq!(
            observer
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Estimated(1)
        );
    }

    #[test]
    fn completed_stream_estimate_counts_only_valid_nonterminal_choice_events() {
        // Mutation caught: the old scanner counts every data line, estimates
        // zero, or replaces invalid/measured completion with an estimate.
        let mut zero = SseObserver::default();
        zero.push(b"data: [DONE]\n\ndata: {\"choices\":[]}\n\n");
        assert_eq!(
            zero.finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Unavailable
        );

        let mut measured = SseObserver::default();
        measured.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\ndata: {\"choices\":[],\"usage\":{\"completion_tokens\":7}}\n\n");
        assert_eq!(
            measured
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Measured(7)
        );

        let mut invalid = SseObserver::default();
        invalid.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{}}],\"usage\":{\"completion_tokens\":\"2\"}}\n\n");
        assert_eq!(
            invalid
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Invalid
        );

        let mut conflicting = SseObserver::default();
        conflicting.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{}}],\"usage\":{\"completion_tokens\":2}}\n\ndata: {\"choices\":[],\"usage\":{\"completion_tokens\":3}}\n\n");
        assert_eq!(
            conflicting
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Invalid
        );

        let mut usage_only = SseObserver::default();
        usage_only.push(b"data: {\"choices\":[],\"usage\":{\"completion_tokens\":2}}\n\n");
        assert_eq!(
            usage_only
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Measured(2),
            "usage-only event supplies measured completion but does not estimate"
        );

        let mut comment_only = SseObserver::default();
        comment_only.push(b": keepalive\n\n");
        assert_eq!(
            comment_only
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Unavailable,
            "comment-only event must not count toward completion estimate"
        );

        let mut json_error = SseObserver::default();
        json_error.push(b"data: {\"error\":{\"message\":\"redacted\"}}\n\n");
        assert_eq!(
            json_error
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Unavailable,
            "parsed JSON error event must not count toward completion estimate"
        );

        let mut usage_without_completion = SseObserver::default();
        usage_without_completion
            .push(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1}}\n\n");
        assert_eq!(
            usage_without_completion
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Unavailable,
            "usage-only event without completion tokens must not estimate completion"
        );

        for bytes in [
            b"data: {]\n\n".as_slice(),
            b"data: \xff\n\n".as_slice(),
            b"data: [DONE]\n\n".as_slice(),
            b"data: {\"choices\":[]}\n\n".as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n".as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"finish_reason\":1}]}\n\n".as_slice(),
        ] {
            let mut observer = SseObserver::default();
            observer.push(bytes);
            assert_eq!(
                observer
                    .finish(StreamOutcome::Completed)
                    .usage
                    .completion_tokens,
                Observation::Unavailable,
                "non-countable literal SSE event must not estimate completion"
            );
        }

        let mut counted = SseObserver::default();
        counted.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\ndata: {\"choices\":[{\"index\":1,\"delta\":{}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
        assert_eq!(
            counted
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Estimated(3)
        );
    }

    #[test]
    fn completed_unterminated_final_event_has_truncated_observation_outcome() {
        // Mutation caught: clean EOF publishes a partial final SSE event as
        // measured or estimated instead of treating the observation as truncated.
        let mut observer = SseObserver::default();
        observer.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}");
        let actual = observer.finish(StreamOutcome::Completed);
        assert_eq!(actual.usage.prompt_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.completion_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.total_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.cached_tokens, Observation::Unavailable);
        assert_eq!(actual.usage.reasoning_tokens, Observation::Unavailable);
        assert_eq!(actual.tool_calls, Observation::Unavailable);
        assert_eq!(
            actual.finish_reasons,
            vec![FinishObservation {
                choice_index: 0,
                result: FinishResult::Unavailable,
            }]
        );
    }

    #[test]
    fn stream_observer_discards_numeric_partial_observations_on_disconnect_or_truncation() {
        // Mutation caught: end-of-loop accounting publishes partial measured or
        // estimated usage after a client disconnect or unterminated final event.
        for outcome in [StreamOutcome::Disconnected, StreamOutcome::Truncated] {
            let mut observer = SseObserver::default();
            observer.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n");
            let actual = observer.finish(outcome);
            assert_eq!(actual.usage.prompt_tokens, Observation::Unavailable);
            assert_eq!(actual.usage.completion_tokens, Observation::Unavailable);
            assert_eq!(actual.tool_calls, Observation::Unavailable);
            assert_eq!(
                actual.finish_reasons,
                vec![FinishObservation {
                    choice_index: 0,
                    result: FinishResult::Unavailable,
                }]
            );
        }
    }

    #[test]
    fn unobservable_events_are_bounded_noncountable_and_do_not_block_recovery() {
        // Mutation caught: retaining an over-bound event or counting malformed
        // bytes makes the observer exceed its memory bound or inflate estimates.
        let mut observer = SseObserver::default();
        let mut over_bound = b"data: ".to_vec();
        over_bound.extend(std::iter::repeat_n(b'x', 1_048_577));
        observer.push(&over_bound);
        assert!(
            observer.retained_event_bytes() <= 1_048_576,
            "over-bound current SSE event must be discarded rather than retained"
        );
        observer.push(b"\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
        assert_eq!(
            observer
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Estimated(1),
            "discarded over-bound event must not count and later framed input must recover"
        );

        let mut malformed = SseObserver::default();
        malformed.push(b"data: {]\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
        assert_eq!(
            malformed
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Estimated(1),
            "malformed JSON event must be unobservable and non-countable"
        );

        let mut invalid_utf8 = SseObserver::default();
        invalid_utf8.push(b"data: \xff\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
        assert_eq!(
            invalid_utf8
                .finish(StreamOutcome::Completed)
                .usage
                .completion_tokens,
            Observation::Estimated(1),
            "invalid UTF-8 event must be unobservable and non-countable"
        );
    }

    #[test]
    fn over_bound_event_stays_unobservable_through_its_blank_line_delimiter() {
        // Mutation caught: leaving discard mode at the first physical newline
        // lets a later data line in the same over-bound event fabricate usage,
        // finish, or tool observations.
        for line_end in [b"\n".as_slice(), b"\r\n".as_slice()] {
            let mut bytes = b"data: ".to_vec();
            bytes.extend(std::iter::repeat_n(b'x', 1_048_577));
            bytes.extend_from_slice(line_end);
            bytes.extend_from_slice(b"data: {\"choices\":[{\"index\":7,\"delta\":{\"tool_calls\":[{\"index\":0}]},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":9}}");
            bytes.extend_from_slice(line_end);
            bytes.extend_from_slice(line_end);
            bytes.extend_from_slice(b"data: {\"choices\":[{\"index\":0,\"delta\":{}}]}");
            bytes.extend_from_slice(line_end);
            bytes.extend_from_slice(line_end);

            let mut observer = SseObserver::default();
            observer.push(&bytes);
            let actual = observer.finish(StreamOutcome::Completed);
            assert_eq!(actual.usage.prompt_tokens, Observation::Unavailable);
            assert_eq!(actual.usage.completion_tokens, Observation::Estimated(1));
            assert_eq!(actual.tool_calls, Observation::Measured(0));
            assert_eq!(
                actual.finish_reasons,
                vec![FinishObservation {
                    choice_index: 0,
                    result: FinishResult::Unavailable,
                }]
            );
        }
    }
}
