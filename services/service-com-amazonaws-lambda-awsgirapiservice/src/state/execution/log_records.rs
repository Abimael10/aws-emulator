// SPDX-License-Identifier: CC0-1.0

//! The FORM one run's log records take: the platform frames Lambda itself writes around a
//! handler's own output, rendered in the function's own `LoggingConfig$LogFormat`.
//!
//! `LoggingConfig$LogFormat` is "the format in which Lambda sends your function's application
//! and system logs to CloudWatch. Select between plain text and structured JSON", so the
//! format is not a property of one record but of the whole log a function leaves: under the
//! plain-text format the frames are the `START`/`END`/`REPORT` lines a client reads as text,
//! and under the structured one each is a JSON object naming the kind of platform record it
//! is and carrying that frame's own fields. A client that selected the structured format
//! parses every record it reads, so a text frame left among them is a record it cannot read.
//!
//! The APPLICATION records — what the handler itself logged — are rendered by the runtime
//! that produced them, which is the only place the level and the logger of a record are
//! known ([`super::bootstrap`] for a Python handler, [`super::node::runtime`] for a node
//! one); both read the format out of the invocation's own environment
//! ([`super::super::execution::runtime_environment`]). This module is the HOST's half: the
//! frames the platform writes itself, and WHICH of a run's records Lambda sends on to
//! CloudWatch Logs at the function's `ApplicationLogLevel` ([`admitted_at`]) — the filter is
//! on what Lambda SENDS ("set this property to filter the application logs for your function
//! that Lambda sends to CloudWatch"), so it is applied where the delivery is made rather than
//! inside the runtime that wrote the record.
//!
//! Nothing here is synthesised. Every field of a frame is the one the plain-text framing
//! states — the invocation's id, the version that executed, and what the run measured —
//! under the member names the platform record carries them with.

use std::time::SystemTime;

use super::outcome::{Completion, Stop};

/// The member every platform record names its own kind under, and the prefix of every one of
/// those kinds.
const PLATFORM_TYPE_PREFIX: &str = "platform.";

/// What the run came to, as a platform record states it: the invocation answered, the
/// function's own failure, the platform's, and a run stopped at the function's `Timeout`.
const STATUS_SUCCESS: &str = "success";
const STATUS_FAILURE: &str = "failure";
const STATUS_ERROR: &str = "error";
const STATUS_TIMEOUT: &str = "timeout";

/// What one run's REPORT states it spent: the figures the plain-text `REPORT` line carries,
/// which the structured record carries as its metrics.
#[derive(Debug, Clone, Copy)]
pub(super) struct Report {
    /// How long the run took, in milliseconds.
    pub duration_ms: f64,
    /// What it is billed for, in whole milliseconds.
    pub billed_ms: i64,
    /// The memory the function is configured for, in MB.
    pub memory_size: i32,
    /// The most memory the run was measured holding, in MB.
    pub used_mb: i64,
}

/// The frame that OPENS a run's log: the invocation and the version that executes it.
pub(super) fn start(json: bool, at: SystemTime, request_id: &str, version: &str) -> String {
    if !json {
        return format!("START RequestId: {request_id} Version: {version}");
    }
    platform(
        at,
        "start",
        ::serde_json::json!({ "requestId": request_id, "version": version }),
    )
}

/// The frame that states the handler's run is OVER, and how it ended.
pub(super) fn runtime_done(
    json: bool,
    at: SystemTime,
    request_id: &str,
    completion: &Completion,
) -> String {
    if !json {
        return format!("END RequestId: {request_id}");
    }
    platform(
        at,
        "runtimeDone",
        ::serde_json::json!({ "requestId": request_id, "status": status(completion) }),
    )
}

/// The frame that states what the run SPENT.
pub(super) fn report(
    json: bool,
    at: SystemTime,
    request_id: &str,
    completion: &Completion,
    measured: Report,
) -> String {
    let Report {
        duration_ms,
        billed_ms,
        memory_size,
        used_mb,
    } = measured;
    if !json {
        let mut line = format!(
            "REPORT RequestId: {request_id}\tDuration: {duration_ms:.2} ms\tBilled Duration: \
             {billed_ms} ms\tMemory Size: {memory_size} MB\tMax Memory Used: {used_mb} MB"
        );
        if let Some(status) = reported_status(completion) {
            line.push_str("\tStatus: ");
            line.push_str(status);
        }
        return line;
    }
    platform(
        at,
        "report",
        ::serde_json::json!({
            "requestId": request_id,
            "metrics": {
                "durationMs": round_hundredths(duration_ms),
                "billedDurationMs": billed_ms,
                "memorySizeMB": memory_size,
                "maxMemoryUsedMB": used_mb,
            },
            "status": status(completion),
        }),
    )
}

/// The status the plain-text `REPORT` line closes on, or `None` for a run that carries none.
///
/// The line states a status for a run the PLATFORM ended — one stopped at the function's own
/// `Timeout`, or one whose environment failed — because that outcome is reported nowhere else
/// in the plain-text framing: the handler never answered, so there is no error payload to read
/// it off, and a client telling a timed-out invocation from a slow one that answered has only
/// this line. A run the HANDLER itself completed, returning or raising, carries no status
/// clause: what it came to is in its own answer.
///
/// The status named is the one the structured record states for the same run ([`status`]), so
/// the two formats report one outcome rather than two.
fn reported_status(completion: &Completion) -> Option<&'static str> {
    let status = status(completion);
    (status == STATUS_TIMEOUT || status == STATUS_ERROR).then_some(status)
}

/// ONE platform record: the instant it was written, the kind of record it is, and the fields
/// that kind carries.
fn platform(at: SystemTime, kind: &str, record: ::serde_json::Value) -> String {
    ::serde_json::json!({
        "time": iso_millis(at),
        "type": format!("{PLATFORM_TYPE_PREFIX}{kind}"),
        "record": record,
    })
    .to_string()
}

/// The member an application record carries the level of the logger that emitted it under.
const LEVEL_MEMBER: &str = "level";

/// The member a platform record names its own kind under.
const TYPE_MEMBER: &str = "type";

/// How much DETAIL each level names, in the order `ApplicationLogLevel` documents — "`TRACE`
/// is the highest level and `FATAL` is the lowest" — together with the names the runtimes'
/// own logging interfaces give the same levels, because a record states the level of the
/// logger that emitted it rather than the filter's spelling.
///
/// Source: the model's `ApplicationLogLevel` enum (`TRACE | DEBUG | INFO | WARN | ERROR |
/// FATAL`), and the two extra names the Python logging library gives the lowest two levels.
const LEVELS: &[(&str, u8)] = &[
    ("TRACE", 0),
    ("DEBUG", 1),
    ("INFO", 2),
    ("WARN", 3),
    ("WARNING", 3),
    ("ERROR", 4),
    ("FATAL", 5),
    ("CRITICAL", 5),
];

/// How little detail `level` names, or `None` for a level name no ladder above places — a
/// level a handler defined itself.
fn severity(level: &str) -> Option<u8> {
    LEVELS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(level))
        .map(|(_, severity)| *severity)
}

/// The level of detail a record that states none of its own counts as: the level AWS
/// documents for output a handler wrote straight to its standard streams, which is the level
/// an application log carries by default ([`crate::state::DEFAULT_LOG_LEVEL`]) — so a filter
/// more restrictive than that withholds such a line exactly as it withholds a record emitted
/// at it.
fn unlevelled_severity() -> u8 {
    severity(crate::state::DEFAULT_LOG_LEVEL).unwrap_or(0)
}

/// Whether the record `message` is one Lambda SENDS to CloudWatch Logs at the application log
/// level `filter`: "Lambda only sends application logs at the selected level of detail and
/// lower".
///
/// A PLATFORM frame is a system record rather than an application one, so the application
/// level filters none of them — what a function's `SystemLogLevel` admits of them is a
/// reading of its own ([`crate::state::configured_system_log_level`], which a read of the
/// function reports) and no frame of a run is withheld here; everything else is an
/// application record, judged at the level it states and at [`unlevelled_severity`] when it
/// states none.
pub(super) fn admitted_at(message: &str, filter: Option<&str>) -> bool {
    let Some(threshold) = filter.and_then(severity) else {
        return true;
    };
    let record = match ::serde_json::from_str(message) {
        Ok(::serde_json::Value::Object(members)) => Some(members),
        _ => None,
    };
    let platform_frame = record.as_ref().is_some_and(|members| {
        members
            .get(TYPE_MEMBER)
            .and_then(::serde_json::Value::as_str)
            .is_some_and(|kind| kind.starts_with(PLATFORM_TYPE_PREFIX))
    });
    if platform_frame {
        return true;
    }
    let level = record
        .as_ref()
        .and_then(|members| members.get(LEVEL_MEMBER))
        .and_then(::serde_json::Value::as_str)
        .and_then(severity)
        .unwrap_or_else(unlevelled_severity);
    level >= threshold
}

/// What the run came to, as the platform records state it: the handler answered, the handler
/// failed, the run was stopped at the function's own `Timeout`, or the platform ended it.
fn status(completion: &Completion) -> &'static str {
    match completion {
        Completion::Returned(_) => STATUS_SUCCESS,
        Completion::Streamed { failure, .. } => {
            if failure.is_some() {
                STATUS_FAILURE
            } else {
                STATUS_SUCCESS
            }
        }
        Completion::Raised { .. } => STATUS_FAILURE,
        Completion::Stopped(Stop::Deadline) => STATUS_TIMEOUT,
        Completion::Stopped(_) | Completion::Internal(_) => STATUS_ERROR,
    }
}

/// A duration in milliseconds at the precision the plain-text framing states it to, so the
/// two formats report one measurement rather than two.
fn round_hundredths(milliseconds: f64) -> f64 {
    (milliseconds * 100.0).round() / 100.0
}

/// `at` as the instant a platform record carries: UTC to the millisecond, with the zero-offset
/// designator the runtime's own records state an instant with.
///
/// Rendered by the one instant renderer this crate has
/// ([`crate::state::designated_iso8601`], which the function-URL records also call for this
/// designator) rather than computed here, so every instant this service reports is the same
/// instant rendered the same way.
fn iso_millis(at: SystemTime) -> String {
    crate::state::designated_iso8601(at, RECORD_UTC_DESIGNATOR)
}

/// The time-zone designator a log record states its instant with: the zero-offset `Z` form,
/// which is the form the runtimes' own records carry ([`super::bootstrap`]) and the one a
/// client parses a CloudWatch Logs record's instant as.
const RECORD_UTC_DESIGNATOR: &str = "Z";

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant every row below is written at.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + ::std::time::Duration::from_millis(1_793_000_000_123)
    }

    /// One `Report`, as a run that spent something leaves one.
    fn measured() -> Report {
        Report {
            duration_ms: 12.345,
            billed_ms: 13,
            memory_size: 256,
            used_mb: 42,
        }
    }

    /// RULE: the plain-text format keeps the runtime's own framing, line for line.
    #[test]
    fn the_plain_text_format_frames_a_run_in_the_runtimes_own_lines() {
        let completion = Completion::Returned("{}".to_string());
        // (case, the rendered frame, what it must be)
        let rows: &[(&str, String, &str)] = &[
            (
                "the opening frame",
                start(false, at(), "req-1", "7"),
                "START RequestId: req-1 Version: 7",
            ),
            (
                "the frame that closes the handler's run",
                runtime_done(false, at(), "req-1", &completion),
                "END RequestId: req-1",
            ),
            (
                "the frame that states what the run spent",
                report(false, at(), "req-1", &completion, measured()),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB",
            ),
            (
                "the same frame for a run stopped at the function's own Timeout, which \
                 states the status the handler never answered with",
                report(
                    false,
                    at(),
                    "req-1",
                    &Completion::Stopped(Stop::Deadline),
                    measured(),
                ),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB\tStatus: timeout",
            ),
            (
                "the same frame for a run whose environment failed",
                report(
                    false,
                    at(),
                    "req-1",
                    &Completion::Stopped(Stop::Memory),
                    measured(),
                ),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB\tStatus: error",
            ),
            (
                "the same frame for a handler that raised, which answered for itself",
                report(
                    false,
                    at(),
                    "req-1",
                    &Completion::Raised {
                        error_type: "ValueError".to_string(),
                        error_message: "no".to_string(),
                        stack_trace: Vec::new(),
                    },
                    measured(),
                ),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB",
            ),
        ];
        for (case, actual, expected) in rows {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the structured format renders every frame as ONE JSON object naming the platform
    /// record it is and carrying that frame's own fields.
    #[test]
    fn the_structured_format_renders_every_frame_as_a_platform_record() {
        let completion = Completion::Returned("{}".to_string());
        // (case, the rendered frame, the `type` it must name)
        let rows: &[(&str, String, &str)] = &[
            (
                "the opening frame",
                start(true, at(), "req-1", "7"),
                "platform.start",
            ),
            (
                "the frame that closes the handler's run",
                runtime_done(true, at(), "req-1", &completion),
                "platform.runtimeDone",
            ),
            (
                "the frame that states what the run spent",
                report(true, at(), "req-1", &completion, measured()),
                "platform.report",
            ),
        ];
        for (case, rendered, expected) in rows {
            let parsed: ::serde_json::Value = ::serde_json::from_str(rendered)
                .unwrap_or_else(|error| panic!("case {case:?}: expected JSON, got {error}"));
            assert_eq!(
                parsed["type"].as_str(),
                Some(*expected),
                "case {case:?}: expected the type {expected:?}, got {rendered:?}"
            );
            assert_eq!(
                parsed["record"]["requestId"].as_str(),
                Some("req-1"),
                "case {case:?}: expected the record to name the invocation, got {rendered:?}"
            );
            assert_eq!(
                parsed["time"].as_str(),
                Some("2026-10-26T07:33:20.123Z"),
                "case {case:?}: expected the instant the frame was written, got {rendered:?}"
            );
            assert!(
                !rendered.contains('\t'),
                "case {case:?}: no plain-text framing is left in a structured record, got \
                 {rendered:?}"
            );
        }
    }

    /// The structured frames carry the same measurements the plain-text `REPORT` line states.
    #[test]
    fn the_structured_report_carries_the_measurements_the_text_report_states() {
        let rendered = report(
            true,
            at(),
            "req-1",
            &Completion::Returned("{}".to_string()),
            measured(),
        );
        let parsed: ::serde_json::Value =
            ::serde_json::from_str(&rendered).expect("the record is JSON");
        let metrics = &parsed["record"]["metrics"];
        // (the member, the figure it must carry)
        let rows: &[(&str, f64)] = &[
            ("durationMs", 12.35),
            ("billedDurationMs", 13.0),
            ("memorySizeMB", 256.0),
            ("maxMemoryUsedMB", 42.0),
        ];
        for (member, expected) in rows {
            let actual = metrics[*member].as_f64();
            assert_eq!(
                actual,
                Some(*expected),
                "case {member:?}: expected {expected}, got {actual:?} in {rendered:?}"
            );
        }
        // Every figure but the duration is a whole number, exactly as the plain-text frame
        // states it.
        for member in ["billedDurationMs", "memorySizeMB", "maxMemoryUsedMB"] {
            assert!(
                metrics[member].is_i64(),
                "case {member:?}: expected a whole number, got {} in {rendered:?}",
                metrics[member]
            );
        }
    }

    /// RULE: a platform record states what the run came to — one row per class of outcome a
    /// run can have.
    #[test]
    fn a_platform_record_states_what_the_run_came_to() {
        // (case, the completion, the status it is stated as)
        let rows: Vec<(&str, Completion, &str)> = vec![
            (
                "a handler that answered",
                Completion::Returned("{}".to_string()),
                STATUS_SUCCESS,
            ),
            (
                "a streaming handler that wrote its whole answer",
                Completion::Streamed {
                    chunks: vec!["part".to_string()],
                    failure: None,
                },
                STATUS_SUCCESS,
            ),
            (
                "a handler that raised",
                Completion::Raised {
                    error_type: "ValueError".to_string(),
                    error_message: "no".to_string(),
                    stack_trace: Vec::new(),
                },
                STATUS_FAILURE,
            ),
            (
                "a run stopped at the function's own Timeout",
                Completion::Stopped(Stop::Deadline),
                STATUS_TIMEOUT,
            ),
            (
                "a run whose runtime was killed",
                Completion::Stopped(Stop::Memory),
                STATUS_ERROR,
            ),
            (
                "a run nothing could drive",
                Completion::Internal("the engine is gone".to_string()),
                STATUS_ERROR,
            ),
        ];
        for (case, completion, expected) in rows {
            let rendered = runtime_done(true, at(), "req-1", &completion);
            let parsed: ::serde_json::Value =
                ::serde_json::from_str(&rendered).expect("the record is JSON");
            let actual = parsed["record"]["status"].as_str();
            assert_eq!(
                actual,
                Some(expected),
                "case {case:?}: expected the status {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: Lambda sends an application record to CloudWatch Logs only at the selected level
    /// of detail and lower — one row per level the model declares, at and around the filter,
    /// plus every kind of delivered line that states no level of its own.
    #[test]
    fn a_record_is_sent_only_at_the_selected_level_of_detail_and_lower() {
        let application = |level: &str| {
            ::serde_json::json!({
                "timestamp": "2026-10-26T07:33:20.123Z",
                "level": level,
                "message": "a line the handler wrote",
                "requestId": "req-1",
            })
            .to_string()
        };
        // (case, the record, the filter in force, whether Lambda sends it)
        let rows: Vec<(&str, String, Option<&str>, bool)> = vec![
            (
                "the selected level itself",
                application("WARN"),
                Some("WARN"),
                true,
            ),
            (
                "the same level under the logging library's own name",
                application("WARNING"),
                Some("WARN"),
                true,
            ),
            (
                "one level lower in detail",
                application("ERROR"),
                Some("WARN"),
                true,
            ),
            (
                "the lowest level of all",
                application("CRITICAL"),
                Some("WARN"),
                true,
            ),
            (
                "one level higher in detail",
                application("INFO"),
                Some("WARN"),
                false,
            ),
            (
                "the highest level of all",
                application("TRACE"),
                Some("WARN"),
                false,
            ),
            (
                "a record below a filter at the lowest level",
                application("ERROR"),
                Some("FATAL"),
                false,
            ),
            (
                "every level under a filter at the highest",
                application("TRACE"),
                Some("TRACE"),
                true,
            ),
            (
                "a record of a function filtered at no level at all",
                application("DEBUG"),
                None,
                true,
            ),
            (
                "a level the ladder does not place, which counts as the default level of \
                 detail",
                application("AUDIT"),
                Some("FATAL"),
                false,
            ),
            (
                "a platform frame, which is a system record and not an application one",
                start(true, at(), "req-1", "7"),
                Some("FATAL"),
                true,
            ),
            (
                "a line written straight to the handler's standard output, under a filter \
                 above the default level of detail",
                "printed-marker".to_string(),
                Some("WARN"),
                false,
            ),
            (
                "the same line under a filter at the default level of detail",
                "printed-marker".to_string(),
                Some("INFO"),
                true,
            ),
            (
                "the plain-text framing of a function that carries no filter",
                "[DEBUG]\t2026-10-26T07:33:20.123Z\treq-1\ta line".to_string(),
                None,
                true,
            ),
        ];
        for (case, message, filter, expected) in rows {
            let actual = admitted_at(&message, filter);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected sent = {expected} under {filter:?}, got {actual} for \
                 {message:?}"
            );
        }
    }

    /// RULE: every level the model declares is placed on the ladder, so a filter the API
    /// accepts can always be judged — the generated catalog of the member's values is the
    /// source (`crate::ops::types::APPLICATION_LOG_LEVEL_VALUES`, which the model's
    /// `ApplicationLogLevel` enum renders), so a level the model gains is caught here.
    #[test]
    fn every_level_the_model_declares_is_placed_on_the_ladder() {
        for level in crate::ops::types::APPLICATION_LOG_LEVEL_VALUES {
            let placed = severity(level);
            assert!(
                placed.is_some(),
                "case {level:?}: expected the model's own level to be placed on the ladder, got \
                 {placed:?}"
            );
        }
        // The ladder runs from the most detailed level to the least, which is the order the
        // member documents ("TRACE is the highest level and FATAL is the lowest").
        let ends = (severity("TRACE"), severity("FATAL"));
        assert_eq!(
            ends,
            (Some(0), Some(5)),
            "expected the ladder to run from the highest level of detail to the lowest, got \
             {ends:?}"
        );
    }
}
