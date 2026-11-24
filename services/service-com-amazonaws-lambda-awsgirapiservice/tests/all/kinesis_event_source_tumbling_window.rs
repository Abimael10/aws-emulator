// SPDX-License-Identifier: CC0-1.0

//! The TUMBLING WINDOW of an event source mapping on an Amazon KINESIS data stream, end to
//! end over the wire with no client action after the records are put: Lambda reads the
//! stream's shards itself and invokes the mapped function once per batch, telling each
//! invocation which window it is aggregating into.
//!
//! The RULE asserted over the table below, for `TumblingWindowInSeconds` — "(Kinesis and
//! DynamoDB Streams only) The duration in seconds of a processing window for DynamoDB and
//! Kinesis Streams event sources. A value of 0 seconds indicates no tumbling window"
//! (`smithy_query(query="operation", operation="CreateEventSourceMapping")`), `@range(min: 0,
//! max: 900)`:
//!
//! * a mapping carrying a NON-ZERO window hands every invocation, beside its `Records`, the
//!   `window` it is aggregating into — `start` and `end` spanning exactly the configured
//!   duration, aligned to a whole multiple of it since the epoch — the `shardId` and
//!   `eventSourceARN` the records were read off, the `state` the window has reached,
//!   `isFinalInvokeForWindow` and `isWindowTerminatedEarly`;
//! * every record an invocation carries lies inside the window it reports, by that record's
//!   own approximate arrival instant, and every record put is delivered exactly once;
//! * the `state` one invocation of a window RETURNS is exactly the `state` the next
//!   invocation of that window is handed, the first invocation of a window opening with the
//!   empty state;
//! * a window nothing more aggregates into closes with exactly ONE further invocation,
//!   carrying no records, `isFinalInvokeForWindow` true and the state accumulated over that
//!   window;
//! * a mapping carrying NO window (the documented zero) carries none of those members at
//!   all, and makes no record-less invocation.
//!
//! Nothing here invokes the function: every record read off a sink stream was put there by a
//! handler the poller invoked, which writes ONE record per INVOCATION reporting what that
//! invocation was told about its window — so a mapping that delivers its records while
//! telling the handler nothing about the window leaves those members null in the sink and the
//! row fails, and a window that is never closed is a sink holding no record-less report.
//!
//! Nothing here waits a fixed span: every wait polls for the condition it is about until this
//! suite's own generous budget runs out.
//!
//! The rows differ along what the rule generalises over: the window's duration (the shortest
//! non-zero span the member admits, a span between the bounds, the longest it admits, and the
//! value documented as no window at all), how many records the backlog holds against it, and
//! the `BatchSize` the records are read in — so the windows of a row hold several invocations
//! each, or one, or none. The duration's own bounds and the value documented as no window are
//! read off the member by the unit table in `state/event_source_poller/tumbling_window.rs`;
//! the rows here are the ones that need a stream, a function and a delivery to decide.
//!
//! The Lambda, Kinesis and mapping helpers are the ones `kinesis_event_source_delivery.rs`
//! publishes, the emulator shape and the sink reader are
//! `kinesis_event_source_handler_writes_kinesis.rs`'s and the role fixture is
//! `event_source_mapping_role_access.rs`'s, so every suite over this surface drives the same
//! wire.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::STARTING_POSITION_TRIM_HORIZON;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, KINESIS_READS};
use crate::kinesis_event_source_delivery::{
    get_mapping, given_stream, lambda, put_records, LOOK_AGAIN, MAPPINGS_URI,
};
use crate::kinesis_event_source_handler_writes_kinesis::{emulator, SinkReader};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below is created with, and the module and attribute its
/// package exposes its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation of this handler is given, as the function's own `Timeout`: well
/// past what a run needs, because what is under test is the windowing and not how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long a wait for work nothing asked for — a stream or a function settling, a batch
/// handed over, a window closing, the handler's own write landing — is given before the row
/// is a failure. Well above the poller's interval, the state-settle span, the longest window
/// any row below has to see the end of and one interpreted run of the handler, so a row fails
/// only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(120);

/// The one `State` a mapping that is reading rests in.
const STATE_ENABLED: &str = "Enabled";

/// The published bounds of `TumblingWindowInSeconds` (`@range(min: 0, max: 900)`), where the
/// minimum is the value documented as no tumbling window at all, and the shortest span and a
/// span between the bounds that a row can see the end of.
const NO_WINDOW: i64 = 0;
/// See [`NO_WINDOW`].
const WINDOW_MAX: i64 = 900;
/// See [`NO_WINDOW`].
const WINDOW_SHORTEST: i64 = 1;
/// See [`NO_WINDOW`].
const WINDOW_MID: i64 = 10;

/// The prefix every shard of a Kinesis data stream is named with, which is what an invocation
/// inside a window names its records' shard by.
const SHARD_PREFIX: &str = "shardId-";

/// The member each report carries the window's own bounds under.
const START: &str = "start";
/// See [`START`].
const END: &str = "end";

/// The handler every function below runs: it puts ONE record onto the sink stream per
/// INVOCATION, carrying everything that invocation was told about its tumbling window beside
/// the arrival instant of each record it was handed, and it returns a running count of the
/// records of the window as that window's next state.
///
/// It asserts nothing: a record read off the sink is what ONE invocation was handed, so every
/// value this suite checks is the emulator's own.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3

SINK = os.environ["SINK_STREAM"]
CLIENT = boto3.client("kinesis")


def lambda_handler(event, context):
    records = event.get("Records") or []
    state = event.get("state")
    report = {
        "window": event.get("window"),
        "stateIn": state,
        "shardId": event.get("shardId"),
        "eventSourceARN": event.get("eventSourceARN"),
        "final": event.get("isFinalInvokeForWindow"),
        "early": event.get("isWindowTerminatedEarly"),
        "size": len(records),
        "ids": [record["eventID"] for record in records],
        "arrivals": [
            record["kinesis"].get("approximateArrivalTimestamp") for record in records
        ],
    }
    CLIENT.put_record(
        StreamName=SINK,
        Data=json.dumps(report).encode("utf-8"),
        PartitionKey="report",
    )
    seen = 0
    if isinstance(state, dict):
        seen = int(state.get("seen", 0))
    return {"state": {"seen": seen + len(records)}}
"#;

/// A function whose package RUNS [`HANDLER_SOURCE`], running as `role_arn` and told through
/// its environment which stream to write, waited out to `Active`.
async fn given_handler_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    sink: &str,
) {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { "SINK_STREAM": sink } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration answered {status} {read}"
        );
        if read["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function never became Active: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create an `Enabled` mapping from `stream_arn` to `function`, reading from the oldest record
/// the stream holds, with the row's `BatchSize` and `TumblingWindowInSeconds`, waited out to
/// the one `State` a reading mapping rests in.
///
/// The create's own answer is checked for the window it was given, so a row cannot pass
/// against a mapping that never carried one.
async fn given_windowed_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    batch_size: i64,
    window_seconds: i64,
) {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": STARTING_POSITION_TRIM_HORIZON,
            "BatchSize": batch_size,
            "TumblingWindowInSeconds": window_seconds,
            "Enabled": true,
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    assert_eq!(
        created["TumblingWindowInSeconds"],
        json!(window_seconds),
        "case {case:?}: the mapping must report the window it was created with, got {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = get_mapping(router, case, &uuid).await;
        if read["State"] == json!(STATE_ENABLED) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported {STATE_ENABLED}: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Everything one row needs: the source stream records are put to, its ARN, and the reader of
/// the sink the handler writes one report per invocation to.
struct Mapped {
    /// The source stream's name, which is what records are put to.
    stream: String,
    /// That stream's ARN, which is what an invocation inside a window must name as the source
    /// its records were read from.
    stream_arn: String,
    /// The reader of the sink the handler writes one report per invocation to.
    sink: SinkReader,
}

/// A source stream, a sink stream, a role carrying both grants, the handler function and an
/// `Enabled` mapping wiring them together, under names derived from `label` so one row's
/// resources are its own.
async fn given_mapped_stream(
    router: &MultiServiceRouter,
    case: &str,
    label: &str,
    batch_size: i64,
    window_seconds: i64,
) -> Mapped {
    let stream = format!("{label}-source");
    let sink = format!("{label}-sink");
    let function = format!("{label}-aggregate");
    let source_arn = given_stream(router, case, &stream, 1).await;
    let sink_arn = given_stream(router, case, &sink, 1).await;
    // Lambda reads the source acting as the function's execution role, and the handler writes
    // the sink as itself: the role carries both grants, as a client wiring this pipeline gives
    // it.
    let role = given_role(
        router,
        case,
        &format!("{label}-role"),
        Some(json!([
            logs_only(),
            allow("kinesis", &KINESIS_READS, &source_arn),
            allow("kinesis", &["PutRecord"], &sink_arn),
        ])),
    )
    .await;
    given_handler_function(router, case, &function, &role, &sink).await;
    let reader = SinkReader::of(router, case, &sink).await;
    given_windowed_mapping(
        router,
        case,
        &function,
        &source_arn,
        batch_size,
        window_seconds,
    )
    .await;
    Mapped {
        stream,
        stream_arn: source_arn,
        sink: reader,
    }
}

/// How many records the report of one invocation says it was handed.
fn size_of(case: &str, report: &Value) -> usize {
    report["size"]
        .as_u64()
        .and_then(|size| usize::try_from(size).ok())
        .unwrap_or_else(|| panic!("case {case:?}: a report carries no size: {report}"))
}

/// Whether the report of one invocation says it was the FINAL invocation of its window.
fn is_final(report: &Value) -> bool {
    report["final"] == json!(true)
}

/// Wait until the reports the handler wrote account for `expected` records, and — where the
/// row's windows are ones it can see the end of — for at least one window's final
/// invocation; answers the reports in the order the handler wrote them, and what it had
/// written when the budget ran out, which is what the row then reports.
async fn wait_for_reports(
    mapped: &mut Mapped,
    router: &MultiServiceRouter,
    case: &str,
    expected: usize,
    awaits_final: bool,
) -> Vec<Value> {
    let mut held: Vec<Value> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        held.extend(mapped.sink.drain(router, case).await);
        let records: usize = held.iter().map(|report| size_of(case, report)).sum();
        let finals = held.iter().filter(|report| is_final(report)).count();
        if records >= expected && (!awaits_final || finals >= 1) {
            return held;
        }
        if Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One bound of the window a report carries, in whole seconds since the epoch; a bound it
/// carries nothing for, or one that is no instant, fails the row where it stands.
fn window_bound(case: &str, report: &Value, member: &str) -> i64 {
    let held = report["window"][member].as_str().unwrap_or_else(|| {
        panic!(
            "case {case:?}: an invocation inside a window must report the window's {member}, got \
             {report}"
        )
    });
    ::aws_smithy_types::DateTime::from_str(held, ::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: an invocation inside a window must report {member} as an \
                 ISO-8601 instant, got {held:?} ({error}): {report}"
            )
        })
        .secs()
}

/// Every `eventID` the reports say was delivered, in the order the invocations carried them.
fn delivered_ids(reports: &[Value]) -> Vec<String> {
    reports
        .iter()
        .flat_map(|report| report["ids"].as_array().cloned().unwrap_or_default())
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// One row: the window the mapping aggregates over, how many records the backlog holds, and
/// the size they are read in.
struct Row {
    case: &'static str,
    /// The `TumblingWindowInSeconds` the mapping is created with.
    window_seconds: i64,
    /// How many records are put onto the source stream, before the mapping exists.
    records: usize,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// Whether the windows the backlog falls into END while the row runs, which is what makes
    /// their final invocation due: a window the clock is still inside owes none yet, so a row
    /// whose window outlasts the run does not wait for one.
    finalises: bool,
}

/// RULE: every invocation of a mapping carrying a non-zero `TumblingWindowInSeconds` is told
/// which window it aggregates into — the fixed span of the configured duration, aligned to a
/// whole multiple of it since the epoch, that holds its records' own arrival instants — is
/// handed the state the previous invocation of that window returned, and each such window
/// closes with exactly one further invocation carrying no records, `isFinalInvokeForWindow`
/// and the state the window accumulated.
#[tokio::test]
async fn a_kinesis_window_is_carried_threaded_and_closed_once() {
    let rows = [
        // The shortest window the member's range admits beside the documented zero, read two
        // records at a time, so one window holds SEVERAL invocations whose state must thread.
        Row {
            case: "the shortest non-zero window, several invocations deep",
            window_seconds: WINDOW_SHORTEST,
            records: 6,
            batch_size: 2,
            finalises: true,
        },
        // A window between the bounds, whose end the row still sees: the records go over in
        // batches of three and the window then closes.
        Row {
            case: "a window between the bounds, closed once its end has passed",
            window_seconds: WINDOW_MID,
            records: 5,
            batch_size: 3,
            finalises: true,
        },
        // The longest window the member's range admits, which the whole backlog fits inside
        // and whose end the clock is still short of, so the row asserts the window and its
        // state without waiting for a close.
        Row {
            case: "the longest window the range admits, still open",
            window_seconds: WINDOW_MAX,
            records: 3,
            batch_size: 10,
            finalises: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let mut mapped = given_mapped_stream(
            &router,
            case,
            &format!("win-{index}"),
            row.batch_size,
            row.window_seconds,
        )
        .await;
        // The records are put BEFORE the mapping reads, so every window they fall into is
        // decided by their own arrival instants rather than by when the poller got to them.
        put_records(&router, case, &mapped.stream, row.records).await;

        let reports =
            wait_for_reports(&mut mapped, &router, case, row.records, row.finalises).await;
        assert!(
            !reports.is_empty(),
            "case {case:?}: the mapping must invoke its function for the {} record(s) put",
            row.records
        );

        // Every record put was delivered exactly once: grouping the records of one window into
        // one invocation leaves none of them behind and repeats none of them.
        let ids = delivered_ids(&reports);
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            (ids.len(), unique.len()),
            (row.records, row.records),
            "case {case:?}: each of the {} record(s) put must be delivered exactly once, got \
             the deliveries {ids:?}",
            row.records
        );

        // THE RULE, for every invocation: it is told the window it aggregates into, which
        // shard and source its records came off, and that the window is inside its bound —
        // and every record it carries lies inside that window.
        for report in &reports {
            let start = window_bound(case, report, START);
            let end = window_bound(case, report, END);
            assert_eq!(
                end - start,
                row.window_seconds,
                "case {case:?}: the window reported must span the {} second(s) the mapping \
                 configures, got {start}..{end}: {report}",
                row.window_seconds
            );
            assert_eq!(
                start % row.window_seconds,
                0,
                "case {case:?}: the windows of one duration tile the clock from a whole \
                 multiple of it, got {start}..{end}: {report}"
            );
            assert!(
                report["shardId"]
                    .as_str()
                    .is_some_and(|shard| shard.starts_with(SHARD_PREFIX)),
                "case {case:?}: an invocation inside a window names the shard its records came \
                 off, got {report}"
            );
            assert_eq!(
                report["eventSourceARN"],
                json!(mapped.stream_arn),
                "case {case:?}: an invocation inside a window names the stream its records \
                 came from, got {report}"
            );
            assert_eq!(
                report["early"],
                json!(false),
                "case {case:?}: a window whose state stayed inside its bound is terminated \
                 early for no invocation, got {report}"
            );
            for arrival in report["arrivals"].as_array().cloned().unwrap_or_default() {
                let at = arrival.as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: a delivered record carries no arrival instant: {report}")
                });
                #[allow(clippy::cast_precision_loss)]
                let (from, until) = (start as f64, end as f64);
                assert!(
                    from <= at && at < until,
                    "case {case:?}: the record that arrived at {at} must lie inside the window \
                     {start}..{end} the invocation carrying it reports: {report}"
                );
            }
        }

        // THE RULE, per window: the state one invocation returned is what the next invocation
        // of that window is handed — the handler returns the records it has seen in the
        // window, so the state each invocation opens with is the count of the records the
        // invocations before it carried — and a window that closed did so with exactly one
        // record-less final invocation carrying the whole window's state.
        let mut windows: BTreeMap<i64, Vec<&Value>> = BTreeMap::new();
        for report in &reports {
            windows
                .entry(window_bound(case, report, START))
                .or_default()
                .push(report);
        }
        for (start, invocations) in &windows {
            let mut seen = 0usize;
            for (at, report) in invocations.iter().enumerate() {
                let expected = if at == 0 {
                    json!({})
                } else {
                    json!({ "seen": seen })
                };
                assert_eq!(
                    report["stateIn"], expected,
                    "case {case:?}: invocation {at} of the window at {start} must be handed the \
                     state the invocation before it returned, expected {expected}, got {report}"
                );
                seen += size_of(case, report);
            }
            let closes = invocations
                .iter()
                .filter(|report| is_final(report))
                .count();
            assert!(
                closes <= 1,
                "case {case:?}: a window closes with exactly one final invocation, but the \
                 window at {start} made {closes} of them: {invocations:?}"
            );
            if closes == 1 {
                let closing = invocations.last().copied().unwrap_or(&Value::Null);
                assert_eq!(
                    (is_final(closing), size_of(case, closing)),
                    (true, 0),
                    "case {case:?}: the final invocation of the window at {start} is its last \
                     one and carries no records, got {invocations:?}"
                );
            }
        }
        if row.finalises {
            assert!(
                reports.iter().any(is_final),
                "case {case:?}: a window the clock has passed the end of must close with a \
                 final invocation, got {reports:?}"
            );
        }
    }
}

/// RULE: "A value of 0 seconds indicates no tumbling window" — a mapping carrying it is
/// handed its records as any mapping is, and its invocations are told nothing about a window:
/// no `window`, no `state`, no `shardId`, no `isFinalInvokeForWindow`, no
/// `isWindowTerminatedEarly`, and no record-less invocation closing anything.
#[tokio::test]
async fn a_kinesis_mapping_with_no_window_carries_none_of_it() {
    let case = "no tumbling window configured";
    let records = 3;
    let router = emulator();
    let mut mapped = given_mapped_stream(&router, case, "unwindowed", 2, NO_WINDOW).await;
    put_records(&router, case, &mapped.stream, records).await;

    let reports = wait_for_reports(&mut mapped, &router, case, records, false).await;
    let ids = delivered_ids(&reports);
    assert_eq!(
        ids.len(),
        records,
        "case {case:?}: each of the {records} record(s) put must be delivered, got {ids:?}"
    );
    for report in &reports {
        assert!(
            size_of(case, report) > 0,
            "case {case:?}: a mapping with no window makes no record-less invocation, got \
             {report}"
        );
        for member in ["window", "stateIn", "shardId", "final", "early"] {
            assert_eq!(
                report[member],
                Value::Null,
                "case {case:?}: a mapping with no window tells its invocations nothing under \
                 {member}, got {report}"
            );
        }
    }
}
