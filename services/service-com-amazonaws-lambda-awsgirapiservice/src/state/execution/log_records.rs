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
//! frames the platform writes itself — of the INVOCATION, and of the INIT phase of the
//! execution environment it was served in ([`initialization`]) — and WHICH of a run's records
//! Lambda sends on to CloudWatch Logs at the two levels a function filters them at
//! ([`admitted_at`]): its `ApplicationLogLevel` over the handler's own records, and its
//! `SystemLogLevel` over the platform's. Both filters are on what Lambda SENDS ("set this
//! property to filter the application logs for your function that Lambda sends to CloudWatch",
//! and the same sentence for the system logs), so both are applied where the delivery is made
//! rather than inside the runtime that wrote the record.
//!
//! Nothing here is synthesised. Every field of a frame is the one the plain-text framing
//! states — the invocation's id, the version that executed, and what the run measured —
//! under the member names the platform record carries them with.

use std::time::{Duration, SystemTime};

use super::outcome::{Completion, Stop};

/// The member every platform record names its own kind under, and the prefix of every one of
/// those kinds.
const PLATFORM_TYPE_PREFIX: &str = "platform.";

/// The member a platform record of an environment's INITIALIZATION names that phase under,
/// and the name of the phase itself.
const PHASE_MEMBER: &str = "phase";
const INIT_PHASE: &str = "init";

/// The member such a record names WHICH kind of initialization it was under, which is the
/// same value the invocation's own `AWS_LAMBDA_INITIALIZATION_TYPE` states
/// ([`crate::state::execution_slot`]).
const INITIALIZATION_TYPE_MEMBER: &str = "initializationType";

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
    /// How long the INITIALIZATION of the execution environment took, in milliseconds, for a
    /// run that paid it — and `None` for one an already-initialized environment served, whose
    /// report states no such span.
    pub init_ms: Option<f64>,
}

/// What the record of an environment's INITIALIZATION states: which kind of environment it
/// is, the function it was built for, and the runtime version it stands on.
#[derive(Debug, Clone, Copy)]
pub(super) struct Initialization<'a> {
    /// Which kind of execution environment this is, as the admission that let the first
    /// invocation in decided.
    pub initialization_type: &'a str,
    /// The function the environment was built for.
    pub function_name: &'a str,
    /// The version of it the environment holds.
    pub function_version: &'a str,
    /// The managed runtime version the environment stands on.
    pub runtime_version: &'a str,
    /// The ARN that runtime version is addressed by, which is the one a read of the function
    /// reports for it.
    pub runtime_version_arn: &'a str,
    /// The execution environment this is the initialization of, as the log STREAM that
    /// environment writes to — which is what identifies one instance of a function's
    /// environment to a client reading its logs.
    pub instance_id: &'a str,
    /// The most memory that instance may hold, in BYTES: the function's own `MemorySize`,
    /// which is all an environment of it is given.
    pub instance_max_memory: i64,
}

/// The frame that opens the log of a new EXECUTION ENVIRONMENT, before the first invocation
/// that environment serves: Lambda states the initialization of an environment as a record of
/// its own, which is what lets a client tell a cold start from a warm one, attribute the
/// module-scope output that follows it to the initialization, and read which runtime version
/// the environment stands on.
pub(super) fn init_start(json: bool, at: SystemTime, about: &Initialization<'_>) -> String {
    if !json {
        return format!(
            "INIT_START Runtime Version: {}\tRuntime Version ARN: {}",
            about.runtime_version, about.runtime_version_arn
        );
    }
    platform(
        at,
        "initStart",
        ::serde_json::json!({
            "initializationType": about.initialization_type,
            "phase": INIT_PHASE,
            "functionName": about.function_name,
            "functionVersion": about.function_version,
            "instanceId": about.instance_id,
            "instanceMaxMemory": about.instance_max_memory,
            "runtimeVersion": about.runtime_version,
            "runtimeVersionArn": about.runtime_version_arn,
        }),
    )
}

/// The frame that OPENS a run's log: the invocation, the function it ran and the version that
/// executed it.
pub(super) fn start(
    json: bool,
    at: SystemTime,
    request_id: &str,
    function_arn: &str,
    version: &str,
) -> String {
    if !json {
        return format!("START RequestId: {request_id} Version: {version}");
    }
    platform(
        at,
        "start",
        ::serde_json::json!({
            "requestId": request_id,
            "functionArn": function_arn,
            "version": version,
        }),
    )
}

/// What the records that CLOSE an environment's INIT phase state: which kind of
/// initialization it was, what the phase came to, and the span it was measured taking.
#[derive(Debug, Clone, Copy)]
pub(super) struct InitDone<'a> {
    /// Which kind of initialization it was — the value the invocation's own
    /// `AWS_LAMBDA_INITIALIZATION_TYPE` states.
    pub initialization_type: &'a str,
    /// How long the phase took, as its own report states it.
    pub took: Duration,
    /// Whether the phase FAILED — the runtime could not resolve the handler it was
    /// configured with — which is the status its records state.
    ///
    /// No engine of this emulator reports such a phase to be framed yet: both report the
    /// span of a phase only once the runtime has begun the handler's own, which a phase that
    /// failed never reaches, so the records of one are written from the unit table alone
    /// until an engine latches the span where the resolution failure is detected.
    pub failed: bool,
}

/// The frames that CLOSE the INIT phase an execution environment was built in, after the
/// frame that opened it ([`init_start`]) and before the records of the first invocation served
/// in it: the runtime reporting its half of the phase done, and the report of what the phase
/// spent.
///
/// They are the PLATFORM's structured records: under the plain-text format the span of the
/// phase is stated on that run's own `REPORT` line instead ([`Report::init_ms`]), which is
/// where a client of that format reads it.
///
/// Every field is the phase's own: the kind of initialization it was, what the phase came to
/// and the span it was measured taking.
pub(super) fn init_done(at: SystemTime, phase: &InitDone<'_>) -> Vec<String> {
    let status = if phase.failed {
        STATUS_ERROR
    } else {
        STATUS_SUCCESS
    };
    let kind = phase.initialization_type;
    let duration_ms = round_hundredths(phase.took.as_secs_f64() * 1_000.0);
    vec![
        platform(
            at,
            "initRuntimeDone",
            ::serde_json::json!({
                INITIALIZATION_TYPE_MEMBER: kind,
                PHASE_MEMBER: INIT_PHASE,
                "status": status,
            }),
        ),
        platform(
            at,
            "initReport",
            ::serde_json::json!({
                INITIALIZATION_TYPE_MEMBER: kind,
                PHASE_MEMBER: INIT_PHASE,
                "status": status,
                "metrics": { "durationMs": duration_ms },
            }),
        ),
    ]
}

/// What the record that closes the handler's run states it produced: the figures that
/// record's own metrics carry.
#[derive(Debug, Clone, Copy)]
pub(super) struct RuntimeDone {
    /// How long the runtime took over the invocation, in milliseconds.
    pub duration_ms: f64,
    /// How many bytes of response the runtime produced for it.
    pub produced_bytes: usize,
}

/// The frame that states the handler's run is OVER, and how it ended.
pub(super) fn runtime_done(
    json: bool,
    at: SystemTime,
    request_id: &str,
    completion: &Completion,
    measured: RuntimeDone,
) -> String {
    if !json {
        return format!("END RequestId: {request_id}");
    }
    platform(
        at,
        "runtimeDone",
        ::serde_json::json!({
            "requestId": request_id,
            "status": status(completion),
            // What the runtime spent over the invocation and how much response it produced,
            // both measured: the span the run took and the size of the answer it carried.
            "metrics": {
                "durationMs": round_hundredths(measured.duration_ms),
                "producedBytes": measured.produced_bytes,
            },
        }),
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
        init_ms,
    } = measured;
    if !json {
        // Each figure is one FIELD of the line, closed by the separator the fields are laid
        // out with — so a client reading the report splits on that separator and finds the
        // same fields whether or not the run paid an initialization, which is the one extra
        // field a cold start states. The status clause CLOSES the line, so it carries no
        // separator of its own.
        let initialization =
            init_ms.map_or_else(String::new, |spent| format!("Init Duration: {spent:.2} ms\t"));
        let mut line = format!(
            "REPORT RequestId: {request_id}\tDuration: {duration_ms:.2} ms\tBilled Duration: \
             {billed_ms} ms\tMemory Size: {memory_size} MB\tMax Memory Used: {used_mb} \
             MB\t{initialization}"
        );
        if let Some(status) = reported_status(completion) {
            line.push_str("Status: ");
            line.push_str(status);
        }
        return line;
    }
    let mut metrics = ::serde_json::json!({
        "durationMs": round_hundredths(duration_ms),
        "billedDurationMs": billed_ms,
        "memorySizeMB": memory_size,
        "maxMemoryUsedMB": used_mb,
    });
    if let (Some(members), Some(spent)) = (metrics.as_object_mut(), init_ms) {
        members.insert(
            "initDurationMs".to_string(),
            ::serde_json::json!(round_hundredths(spent)),
        );
    }
    platform(
        at,
        "report",
        ::serde_json::json!({
            "requestId": request_id,
            "metrics": metrics,
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

/// Whether the record `message` is one Lambda SENDS to CloudWatch Logs at the levels a
/// function filters its records at: `application` for the records the HANDLER wrote ("Lambda
/// only sends application logs at the selected level of detail and lower") and `system` for
/// the platform's own frames ("set this property to filter the system logs for your function
/// that Lambda sends to CloudWatch").
///
/// A PLATFORM frame is a system record rather than an application one, so the application
/// level filters none of them — what a function's `SystemLogLevel` admits of them is a
/// reading of its own ([`system_admitted_at`]) and no frame of a run is withheld here;
/// everything else is an application record, judged at the level it states and at
/// [`unlevelled_severity`] when it states none.
pub(super) fn admitted_at(message: &str, filter: Option<&str>) -> bool {
    let Some(threshold) = filter.and_then(severity) else {
        return true;
    };
    let record = match ::serde_json::from_str(message) {
        Ok(::serde_json::Value::Object(members)) => Some(members),
        _ => None,
    };
    if platform_kind(message).is_some() {
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

/// Whether the record `message` is one Lambda SENDS to CloudWatch Logs at the SYSTEM log
/// level `filter`: `SystemLogLevel` is "set this property to filter the system logs for your
/// function that Lambda sends to CloudWatch", and it filters the platform records
/// INDEPENDENTLY of the level the application records are filtered at — so a function can
/// keep its application logs while suppressing the platform noise, or the other way round.
///
/// Only a PLATFORM record is a system record: an application record is judged by
/// [`admitted_at`] and never withheld here, whatever the system level is.
pub(super) fn system_admitted_at(message: &str, filter: Option<&str>) -> bool {
    let Some(threshold) = filter.and_then(severity) else {
        return true;
    };
    let Some(kind) = platform_kind(message) else {
        return true;
    };
    // A kind this ladder does not place is SENT: a record whose level is unknown is judged the
    // way a record stating no level is ([`admitted_at`]), so nothing is withheld for want of a
    // reading of it.
    platform_record_level(&kind)
        .and_then(severity)
        .is_none_or(|level| level >= threshold)
}

/// The kind a platform record names itself as (`platform.start`, …), or `None` for a record
/// that is no platform record at all.
fn platform_kind(message: &str) -> Option<String> {
    let record: ::serde_json::Map<String, ::serde_json::Value> =
        match ::serde_json::from_str(message) {
            Ok(::serde_json::Value::Object(members)) => members,
            _ => return None,
        };
    let kind = record.get(TYPE_MEMBER)?.as_str()?;
    kind.starts_with(PLATFORM_TYPE_PREFIX)
        .then(|| kind.to_string())
}

/// How much DETAIL each platform record names, on the ladder the system level filters by.
///
/// The frames of an ORDINARY run — the initialization of the execution environment, the
/// opening of an invocation, the end of the runtime's work on it and the report of what it
/// spent — are the records Lambda states about every invocation, so they are that ladder's
/// `INFO` records; a record about a log Lambda could not deliver is a warning about the log
/// itself.
///
/// Source: the three levels the model's `SystemLogLevel` enum declares (`DEBUG | INFO |
/// WARN`), against the platform record kinds this module writes ([`init_start`], [`start`],
/// [`runtime_done`], [`report`]).
const PLATFORM_RECORD_LEVELS: &[(&str, &str)] = &[
    ("platform.initStart", "INFO"),
    ("platform.initRuntimeDone", "INFO"),
    ("platform.initReport", "INFO"),
    ("platform.start", "INFO"),
    ("platform.runtimeDone", "INFO"),
    ("platform.report", "INFO"),
    ("platform.logsDropped", "WARN"),
];

/// The level the platform record `kind` is stated at, or `None` for a kind the ladder above
/// does not place — a record this reading says nothing about, which is therefore withheld by
/// no level.
fn platform_record_level(kind: &str) -> Option<&'static str> {
    PLATFORM_RECORD_LEVELS
        .iter()
        .find(|(named, _)| *named == kind)
        .map(|(_, level)| *level)
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

    /// One `Report`, as a run an already-initialized environment served leaves one.
    fn measured() -> Report {
        Report {
            duration_ms: 12.345,
            billed_ms: 13,
            memory_size: 256,
            used_mb: 42,
            init_ms: None,
        }
    }

    /// The same `Report` of a run that BUILT the environment it ran in, which states what the
    /// initialization took beside what the run did.
    fn initialized() -> Report {
        Report {
            init_ms: Some(123.456),
            ..measured()
        }
    }

    /// One `RuntimeDone`, as a run that answered leaves one.
    fn produced() -> RuntimeDone {
        RuntimeDone {
            duration_ms: 12.345,
            produced_bytes: 16,
        }
    }

    /// The ARN of the function every frame below is a frame of.
    const FUNCTION_ARN: &str = "arn:aws:lambda:us-east-1:000000000000:function:orders";

    /// RULE: the plain-text format keeps the runtime's own framing, line for line.
    #[test]
    fn the_plain_text_format_frames_a_run_in_the_runtimes_own_lines() {
        let completion = Completion::Returned("{}".to_string());
        // (case, the rendered frame, what it must be)
        let rows: &[(&str, String, &str)] = &[
            (
                "the opening frame",
                start(false, at(), "req-1", FUNCTION_ARN, "7"),
                "START RequestId: req-1 Version: 7",
            ),
            (
                "the frame that closes the handler's run",
                runtime_done(false, at(), "req-1", &completion, produced()),
                "END RequestId: req-1",
            ),
            (
                "the frame that states what the run spent",
                report(false, at(), "req-1", &completion, measured()),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB\t",
            ),
            (
                "the same frame for a run that built the environment it ran in",
                report(false, at(), "req-1", &completion, initialized()),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB\tInit Duration: 123.46 ms\t",
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
                        initialization: false,
                    },
                    measured(),
                ),
                "REPORT RequestId: req-1\tDuration: 12.35 ms\tBilled Duration: 13 ms\tMemory \
                 Size: 256 MB\tMax Memory Used: 42 MB\t",
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
                start(true, at(), "req-1", FUNCTION_ARN, "7"),
                "platform.start",
            ),
            (
                "the frame that closes the handler's run",
                runtime_done(true, at(), "req-1", &completion, produced()),
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

    /// One `Initialization`, as an environment built for an invocation states one.
    fn initializing(initialization_type: &str) -> Initialization<'_> {
        Initialization {
            initialization_type,
            function_name: "orders",
            function_version: "7",
            runtime_version: "python3.12",
            runtime_version_arn: "arn:aws:lambda:us-east-1::runtime:0123abcd",
            instance_id: "2026/10/26/[7]0123456789abcdef0123456789abcdef",
            instance_max_memory: 256 * 1024 * 1024,
        }
    }

    /// RULE: a run that BUILT its execution environment opens that environment's log with the
    /// platform record of the initialization, which names the kind of environment it is, the
    /// phase it states and the function it was built for.
    #[test]
    fn the_initialization_of_an_environment_is_a_platform_record_of_its_own() {
        // (case, the kind of environment the admission decided)
        let rows: &[(&str, &str)] = &[
            ("an environment started for an invocation", "on-demand"),
            (
                "one provisioned concurrency keeps initialized",
                "provisioned-concurrency",
            ),
        ];
        for (case, initialization_type) in rows {
            let about = initializing(initialization_type);
            let rendered = init_start(true, at(), &about);
            let parsed: ::serde_json::Value =
                ::serde_json::from_str(&rendered).expect("the record is JSON");
            assert_eq!(
                parsed["type"].as_str(),
                Some("platform.initStart"),
                "case {case:?}: expected the initialization record, got {rendered:?}"
            );
            // (the member, what it must carry)
            let members: &[(&str, &str)] = &[
                ("initializationType", initialization_type),
                ("phase", INIT_PHASE),
                ("functionName", about.function_name),
                ("functionVersion", about.function_version),
                ("instanceId", about.instance_id),
                ("runtimeVersion", about.runtime_version),
                ("runtimeVersionArn", about.runtime_version_arn),
            ];
            for (member, expected) in members {
                let actual = parsed["record"][*member].as_str();
                assert_eq!(
                    actual,
                    Some(*expected),
                    "case {case:?}: expected {member} to be {expected:?}, got {actual:?} in \
                     {rendered:?}"
                );
            }
            assert_eq!(
                parsed["time"].as_str(),
                Some("2026-10-26T07:33:20.123Z"),
                "case {case:?}: expected the instant the frame was written, got {rendered:?}"
            );
            // The memory the instance may hold is the function's own, in bytes.
            let memory = parsed["record"]["instanceMaxMemory"].as_i64();
            assert_eq!(
                memory,
                Some(about.instance_max_memory),
                "case {case:?}: expected the instance's memory in bytes, got {memory:?} in \
                 {rendered:?}"
            );
            // The same frame under the plain-text format is the runtime's own line, naming
            // the runtime version the environment stands on.
            let text = init_start(false, at(), &about);
            assert_eq!(
                text,
                format!(
                    "INIT_START Runtime Version: {}\tRuntime Version ARN: {}",
                    about.runtime_version, about.runtime_version_arn
                ),
                "case {case:?}: expected the plain-text initialization line, got {text:?}"
            );
        }
    }

    /// RULE: `SystemLogLevel` filters the PLATFORM records of a run independently of the level
    /// the application records are filtered at — one row per level the model declares for it,
    /// at and around each platform record's own level, plus an application record, which this
    /// filter never withholds.
    #[test]
    fn a_platform_record_is_sent_only_at_the_selected_system_level_and_lower() {
        let completion = Completion::Returned("{}".to_string());
        let application = ::serde_json::json!({
            "timestamp": "2026-10-26T07:33:20.123Z",
            "level": "ERROR",
            "message": "a line the handler wrote",
            "requestId": "req-1",
        })
        .to_string();
        let dropped = ::serde_json::json!({
            "time": "2026-10-26T07:33:20.123Z",
            "type": "platform.logsDropped",
            "record": { "reason": "Consumer seems to have fallen behind" },
        })
        .to_string();
        // (case, the record, the system level in force, whether Lambda sends it)
        let rows: Vec<(&str, String, Option<&str>, bool)> = vec![
            (
                "the frame that opens an environment's log, under the most detailed level",
                init_start(true, at(), &initializing("on-demand")),
                Some("DEBUG"),
                true,
            ),
            (
                "the frame that opens an invocation, at its own level",
                start(true, at(), "req-1", FUNCTION_ARN, "7"),
                Some("INFO"),
                true,
            ),
            (
                "the frame that closes the handler's run, one level higher in detail than the \
                 filter",
                runtime_done(true, at(), "req-1", &completion, produced()),
                Some("WARN"),
                false,
            ),
            (
                "the frame that states what the run spent, one level higher in detail",
                report(true, at(), "req-1", &completion, measured()),
                Some("WARN"),
                false,
            ),
            (
                "the frame that opens an environment's log, one level higher in detail",
                init_start(true, at(), &initializing("on-demand")),
                Some("WARN"),
                false,
            ),
            (
                "a record about a log Lambda could not deliver, at the least detailed level",
                dropped.clone(),
                Some("WARN"),
                true,
            ),
            (
                "the same record under a filter at the most detailed level",
                dropped,
                Some("DEBUG"),
                true,
            ),
            (
                "a platform record of a kind this reading does not place, which no level \
                 withholds",
                ::serde_json::json!({
                    "time": "2026-10-26T07:33:20.123Z",
                    "type": "platform.extension",
                    "record": { "name": "a telemetry extension" },
                })
                .to_string(),
                Some("WARN"),
                true,
            ),
            (
                "a frame of a function filtered at no system level at all",
                report(true, at(), "req-1", &completion, measured()),
                None,
                true,
            ),
            (
                "an application record, which the system level never withholds",
                application,
                Some("WARN"),
                true,
            ),
            (
                "the plain-text framing of a run, which states no record kind",
                "START RequestId: req-1 Version: 7".to_string(),
                Some("WARN"),
                true,
            ),
        ];
        for (case, message, filter, expected) in rows {
            let actual = system_admitted_at(&message, filter);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected sent = {expected} under {filter:?}, got {actual} for \
                 {message:?}"
            );
            // The two filters are independent: the application level judges the application
            // records and withholds no platform record.
            assert!(
                platform_kind(&message).is_none() || admitted_at(&message, Some("FATAL")),
                "case {case:?}: the application level withholds no platform record, got \
                 {message:?}"
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
                    initialization: false,
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
            let rendered = runtime_done(true, at(), "req-1", &completion, produced());
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
                start(true, at(), "req-1", FUNCTION_ARN, "7"),
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

    /// RULE: the records that CLOSE the INIT phase are the two the platform writes for it, in
    /// the order it writes them, each naming the phase and the kind of initialization it was,
    /// and the report states the span the phase was measured taking — one row per outcome the
    /// phase can have.
    #[test]
    fn the_init_phase_is_closed_by_its_own_platform_records() {
        // (case, whether the phase failed, the status its records state)
        let rows: &[(&str, bool, &str)] = &[
            (
                "a phase that stood the environment up",
                false,
                STATUS_SUCCESS,
            ),
            ("one whose handler could not be resolved", true, STATUS_ERROR),
        ];
        for (case, failed, status) in rows {
            let written = init_done(
                at(),
                &InitDone {
                    initialization_type: "provisioned-concurrency",
                    took: Duration::from_micros(123_450),
                    failed: *failed,
                },
            );
            let kinds: Vec<String> = written
                .iter()
                .map(|record| {
                    let parsed: ::serde_json::Value =
                        ::serde_json::from_str(record).unwrap_or_else(|error| {
                            panic!("case {case:?}: expected JSON, got {error}")
                        });
                    parsed[TYPE_MEMBER].as_str().unwrap_or_default().to_string()
                })
                .collect();
            let expected = vec![
                "platform.initRuntimeDone".to_string(),
                "platform.initReport".to_string(),
            ];
            assert_eq!(
                kinds, expected,
                "case {case:?}: expected {expected:?}, got {kinds:?}"
            );
            for record in &written {
                let parsed: ::serde_json::Value =
                    ::serde_json::from_str(record).expect("the record is JSON");
                let named = parsed["record"][PHASE_MEMBER].as_str();
                assert_eq!(
                    named,
                    Some(INIT_PHASE),
                    "case {case:?}: expected the record to name the phase, got {record:?}"
                );
                let kind = parsed["record"][INITIALIZATION_TYPE_MEMBER].as_str();
                assert_eq!(
                    kind,
                    Some("provisioned-concurrency"),
                    "case {case:?}: expected the kind of initialization, got {record:?}"
                );
                assert_eq!(
                    parsed["record"]["status"].as_str(),
                    Some(*status),
                    "case {case:?}: expected the status {status:?}, got {record:?}"
                );
            }
            let reported: ::serde_json::Value =
                ::serde_json::from_str(&written[1]).expect("the record is JSON");
            // The report states the span the phase was MEASURED taking, in the milliseconds a
            // platform record states a duration in.
            assert_eq!(
                reported["record"]["metrics"]["durationMs"].as_f64(),
                Some(123.45),
                "case {case:?}: expected the measured span, got {:?}",
                written[1]
            );
        }
    }

    /// RULE: the record that closes the handler's run states what the runtime SPENT over the
    /// invocation and how much response it produced — one row per class of answer a run
    /// leaves.
    #[test]
    fn the_record_that_closes_the_run_states_what_the_runtime_produced() {
        // (case, what the run came to, the bytes it produced)
        let rows: Vec<(&str, Completion, usize)> = vec![
            (
                "a handler that answered",
                Completion::Returned("{\"ok\":true}".to_string()),
                11,
            ),
            (
                "a handler that answered nothing at all",
                Completion::Returned(String::new()),
                0,
            ),
            (
                "a handler that raised, whose error document is the answer",
                Completion::Raised {
                    error_type: "ValueError".to_string(),
                    error_message: "no".to_string(),
                    stack_trace: Vec::new(),
                    initialization: false,
                },
                64,
            ),
        ];
        for (case, completion, produced_bytes) in rows {
            let rendered = runtime_done(
                true,
                at(),
                "req-1",
                &completion,
                RuntimeDone {
                    duration_ms: 12.345,
                    produced_bytes,
                },
            );
            let parsed: ::serde_json::Value =
                ::serde_json::from_str(&rendered).expect("the record is JSON");
            let metrics = &parsed["record"]["metrics"];
            assert_eq!(
                metrics["durationMs"].as_f64(),
                Some(12.35),
                "case {case:?}: expected the span the runtime took, got {rendered:?}"
            );
            assert_eq!(
                metrics["producedBytes"].as_u64(),
                u64::try_from(produced_bytes).ok(),
                "case {case:?}: expected {produced_bytes} bytes produced, got {rendered:?}"
            );
            assert!(
                metrics["producedBytes"].is_u64(),
                "case {case:?}: expected a whole number of bytes, got {rendered:?}"
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
