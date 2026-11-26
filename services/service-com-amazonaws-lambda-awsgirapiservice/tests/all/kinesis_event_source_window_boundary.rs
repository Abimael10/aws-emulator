// SPDX-License-Identifier: CC0-1.0

//! WHICH tumbling window an event source mapping on an Amazon KINESIS data stream delivers
//! each record under, end to end over the wire with no client action after the records are
//! put — and in particular what happens to a record that arrives SHORTLY BEFORE a window
//! boundary and is read only after the clock has passed it.
//!
//! The RULE asserted over the table below, for `TumblingWindowInSeconds` — "(Kinesis and
//! DynamoDB Streams only) The duration in seconds of a processing window for DynamoDB and
//! Kinesis Streams event sources. A value of 0 seconds indicates no tumbling window"
//! (`smithy_query(query="operation", operation="CreateEventSourceMapping")`), `@range(min: 0,
//! max: 900)`: a mapping carrying a non-zero duration partitions the clock into fixed,
//! non-overlapping windows of that duration, and each record is delivered under the window
//! that CONTAINS the record's own approximate arrival instant:
//!
//! ```text
//! window.start == the whole multiple of the duration at or before the record's arrival
//! window.start <= record.kinesis.approximateArrivalTimestamp < window.end
//! ```
//!
//! The window is therefore a fact about WHEN THE RECORD ARRIVED, not about when the poller
//! happened to read it: a record that arrived a fraction of a second before a boundary and is
//! read after that boundary still belongs to the window before it, which is what makes a
//! handler keying its aggregate on `event['window']` count exactly the records of the window
//! it is told it is in.
//!
//! Where `kinesis_event_source_tumbling_window.rs` asserts the windowing over a backlog the
//! mapping is already reading, every row here forces the boundary case the rule is about: the
//! records are put in GROUPS, each group put only once the clock has left the window the group
//! before it fell in, and the mapping is created only after the last group is in — so every
//! record of every group but the last is read strictly after its own window has ended, and a
//! poller bucketing by its own read clock would collapse the whole backlog into one later
//! window.
//!
//! Every value asserted is observed from INSIDE the handler, which writes one report per
//! INVOCATION onto a second stream carrying the window it was told it was in beside the
//! arrival instant of each record it was handed; a mapping that delivers nothing leaves the
//! sink empty and the row fails.
//!
//! Nothing here waits a fixed span: each wait polls for the condition it is about — a boundary
//! the next group's records wait on, the reports the handler has written — until this suite's
//! generous budget runs out.
//!
//! The rows differ along what the rule generalises over: the window's duration, how many of
//! its windows the backlog is spread over, how many records fall in each of them, and the
//! `BatchSize` they are read in. The duration's published bounds and the value the model
//! documents as no window at all are read off the member by the unit table in
//! `state/event_source_poller/tumbling_window.rs` and by
//! `tests/all/event_source_mapping_source_shape_and_window.rs`; the rows here are the ones
//! that need a stream, a function and a delivery to decide.
//!
//! The Lambda, Kinesis and mapping helpers are the ones `kinesis_event_source_delivery.rs`
//! publishes, the emulator shape and the sink reader are
//! `kinesis_event_source_handler_writes_kinesis.rs`'s, the role fixture is
//! `event_source_mapping_role_access.rs`'s and the reporting handler, the windowed mapping and
//! the report readers are `kinesis_event_source_tumbling_window.rs`'s, so every suite over
//! this surface drives the same wire.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, KINESIS_READS};
use crate::kinesis_event_source_delivery::{given_stream, kinesis, LOOK_AGAIN};
use crate::kinesis_event_source_handler_writes_kinesis::{emulator, SinkReader};
use crate::kinesis_event_source_tumbling_window::{
    delivered_ids, given_handler_function, given_windowed_mapping, is_final, size_of,
    window_bound, BUDGET, END, START,
};

/// The prefix every shard of a Kinesis data stream is named with, which is what an invocation
/// inside a window names its records' shard by.
const SHARD_PREFIX: &str = "shardId-";

/// Everything one row needs before its mapping exists: the stream records are put to, its
/// ARN, the function the mapping will invoke, and the reader of the sink that function writes
/// one report per invocation to.
struct Pipeline {
    /// The source stream's name, which is what records are put to.
    stream: String,
    /// That stream's ARN, which is what the mapping's event source names.
    stream_arn: String,
    /// The function the mapping invokes, which reports what each invocation was told.
    function: String,
    /// The reader of the sink the handler writes one report per invocation to.
    sink: SinkReader,
}

/// A source stream, a sink stream, a role carrying both grants, the reporting handler function
/// and a reader of the sink — everything but the MAPPING, which a row creates only once its
/// records are in, so the backlog is read strictly after the windows it falls in have ended.
async fn given_pipeline(router: &MultiServiceRouter, case: &str, label: &str) -> Pipeline {
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
    Pipeline {
        stream,
        stream_arn: source_arn,
        function,
        sink: reader,
    }
}

/// Put `count` records onto the stream `name` names in ONE call, each labelled with the group
/// it belongs to, so the records of one group arrive together and share a window.
async fn put_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    group: usize,
    count: usize,
) {
    let records: Vec<Value> = (0..count)
        .map(|index| {
            let label = format!("{case}-{group}-{index}");
            json!({
                "Data": base64::engine::general_purpose::STANDARD.encode(label.as_bytes()),
                "PartitionKey": label,
            })
        })
        .collect();
    let (status, written) = kinesis(
        router,
        "PutRecords",
        &json!({ "StreamName": name, "Records": records }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRecords answered {status} {written}"
    );
    assert_eq!(
        written["FailedRecordCount"],
        json!(0),
        "case {case:?}: every record of group {group} must be placed, got {written}"
    );
}

/// The instant the window of `span` seconds the clock is in NOW ends at: the next whole
/// multiple of the span since the epoch.
fn next_boundary(span: u64) -> SystemTime {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    UNIX_EPOCH + Duration::from_secs(since_epoch - since_epoch % span + span)
}

/// Wait until the clock has LEFT the window of `window_seconds` it is in now, so a record put
/// after this call arrives in a later window than one put before it.
///
/// This polls for that instant to pass — the condition the next group's records wait on — and
/// gives it the suite's own generous budget, so the row fails only when the clock never got
/// there at all.
async fn wait_for_the_next_window(case: &str, window_seconds: i64) {
    let span = u64::try_from(window_seconds).unwrap_or(1).max(1);
    let boundary = next_boundary(span);
    let deadline = Instant::now() + BUDGET;
    while SystemTime::now() < boundary {
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock never left the window of {window_seconds} second(s) the \
             records just put fell in"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until the reports the handler wrote account for `expected` records and until every
/// window they say carried records has been closed by a final invocation — the windows of the
/// backlog have all ended, so each of them is owed one. Answers the reports in the order the
/// handler wrote them, and what it had written when the budget ran out, which is what the row
/// then reports.
async fn wait_for_reports(
    router: &MultiServiceRouter,
    case: &str,
    sink: &mut SinkReader,
    expected: usize,
    least_windows: usize,
) -> Vec<Value> {
    let mut held: Vec<Value> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        held.extend(sink.drain(router, case).await);
        let records: usize = held.iter().map(|report| size_of(case, report)).sum();
        let carrying = windows_of(case, &held, |report| size_of(case, report) > 0);
        let closed = windows_of(case, &held, is_final);
        let all_closed = carrying.iter().all(|start| closed.contains(start));
        if records >= expected && closed.len() >= least_windows && all_closed {
            return held;
        }
        if Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The windows the reports `holds` admits are about, as the whole epoch second each starts at.
fn windows_of(case: &str, reports: &[Value], holds: impl Fn(&Value) -> bool) -> BTreeSet<i64> {
    reports
        .iter()
        .filter(|report| holds(report))
        .map(|report| window_bound(case, report, START))
        .collect()
}

/// `at` seconds since the epoch, truncated to the whole second the windows of a mapping are
/// aligned to: a window bound is always a whole second, since a window spans a whole number of
/// them.
#[allow(clippy::cast_possible_truncation)]
fn whole_seconds(at: f64) -> i64 {
    at.floor() as i64
}

/// The window of `window_seconds` an arrival instant falls in, as the whole epoch second it
/// starts at: the whole multiple of the duration at or before that instant.
fn window_holding(arrival: f64, window_seconds: i64) -> i64 {
    let seconds = whole_seconds(arrival);
    seconds - seconds.rem_euclid(window_seconds)
}

/// Every arrival instant one report says the invocation was handed; a record the report
/// carries no instant for fails the row where it stands.
fn arrivals_of(case: &str, report: &Value) -> Vec<f64> {
    report["arrivals"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|arrival| {
            arrival.as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: a delivered record carries no arrival instant: {report}")
            })
        })
        .collect()
}

/// One row: the window the mapping aggregates over, how many of its windows the backlog is
/// spread over, how many records fall in each of them, and the size they are read in.
struct Row {
    case: &'static str,
    /// The `TumblingWindowInSeconds` the mapping is created with.
    window_seconds: i64,
    /// How many consecutive windows of that duration the records are spread over, each group
    /// put only once the clock has left the window the group before it fell in.
    groups: usize,
    /// How many records are put inside each of those windows.
    per_group: usize,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
}

/// RULE: a record of a mapping carrying a non-zero `TumblingWindowInSeconds` is delivered
/// under the window that CONTAINS its own approximate arrival instant — the fixed window of
/// the configured duration starting at the whole multiple of that duration at or before the
/// arrival — however long after that window ended the poller got to it, so a record that
/// arrived before a boundary is never delivered under the window that starts at it.
#[tokio::test]
async fn every_record_is_delivered_under_the_window_holding_its_arrival() {
    let rows = [
        // The shortest window the member's range admits beside the documented zero, spread
        // over three of its windows and read two records at a time: every record but the last
        // group's is read seconds after its own window closed.
        Row {
            case: "the shortest non-zero window, three of its windows deep",
            window_seconds: 1,
            groups: 3,
            per_group: 2,
            batch_size: 2,
        },
        // A longer window, read one record at a time, so each record of a window is handed
        // over by an invocation of its own and every one of them must still report that
        // window.
        Row {
            case: "a two-second window read one record at a time",
            window_seconds: 2,
            groups: 2,
            per_group: 2,
            batch_size: 1,
        },
        // A window longer than the batch it is read in, whose boundary the backlog straddles:
        // one window holds more records than one batch, so several invocations of the same
        // window carry records that all belong to it.
        Row {
            case: "a five-second window holding more records than one batch",
            window_seconds: 5,
            groups: 2,
            per_group: 3,
            batch_size: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let total = row.groups * row.per_group;
        let router = emulator();
        let mut pipeline = given_pipeline(&router, case, &format!("bound-{index}")).await;

        // The backlog is built BEFORE the mapping exists, one group per window: each group is
        // put only once the clock has left the window the group before it fell in, so the
        // groups arrive in distinct, consecutive windows and every group but the last has its
        // window behind it by the time anything reads the stream.
        for group in 0..row.groups {
            if group > 0 {
                wait_for_the_next_window(case, row.window_seconds).await;
            }
            put_group(&router, case, &pipeline.stream, group, row.per_group).await;
        }
        given_windowed_mapping(
            &router,
            case,
            &pipeline.function,
            &pipeline.stream_arn,
            row.batch_size,
            row.window_seconds,
        )
        .await;

        // Every window the backlog fell in has ended by the time the last one is read, so each
        // of them is owed its own final invocation.
        let reports = wait_for_reports(&router, case, &mut pipeline.sink, total, row.groups).await;
        assert!(
            !reports.is_empty(),
            "case {case:?}: the mapping must invoke its function for the {total} record(s) put"
        );

        // Every record put was delivered exactly once: splitting the backlog at its window
        // boundaries leaves none of the records behind and repeats none of them.
        let ids = delivered_ids(&reports);
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            (ids.len(), unique.len()),
            (total, total),
            "case {case:?}: each of the {total} record(s) put must be delivered exactly once, \
             got the deliveries {ids:?}"
        );

        // THE RULE, for every record delivered: the window its invocation reported is the
        // fixed window of the configured duration that holds that record's OWN arrival
        // instant.
        let mut windows_with_records: BTreeMap<i64, usize> = BTreeMap::new();
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
            let arrivals = arrivals_of(case, report);
            if !arrivals.is_empty() {
                *windows_with_records.entry(start).or_default() += arrivals.len();
            }
            for arrival in arrivals {
                assert_eq!(
                    window_holding(arrival, row.window_seconds),
                    start,
                    "case {case:?}: the record that arrived at {arrival} must be delivered \
                     under the window holding that instant, which starts at {}, not under the \
                     window {start}..{end} the invocation carrying it reports: {report}",
                    window_holding(arrival, row.window_seconds)
                );
                #[allow(clippy::cast_precision_loss)]
                let (from, until) = (start as f64, end as f64);
                assert!(
                    from <= arrival && arrival < until,
                    "case {case:?}: the record that arrived at {arrival} must lie inside the \
                     window {start}..{end} the invocation carrying it reports: {report}"
                );
            }
        }

        // The backlog arrived in `groups` distinct windows, so it is handed over under that
        // many: a poller bucketing its records by its own read clock would report fewer, every
        // record of the backlog falling in whichever window it happened to read in.
        assert!(
            windows_with_records.len() >= row.groups,
            "case {case:?}: the {total} record(s) arrived in {} distinct window(s) and must be \
             delivered under that many, but the invocations carrying records reported {:?}",
            row.groups,
            windows_with_records
        );

        // And every window that carried records still ends in exactly ONE record-less final
        // invocation, which is its last: a window a boundary split the backlog at is closed
        // once, so the handler writes that window's result once.
        let mut per_window: BTreeMap<i64, Vec<&Value>> = BTreeMap::new();
        for report in &reports {
            per_window
                .entry(window_bound(case, report, START))
                .or_default()
                .push(report);
        }
        for (start, invocations) in &per_window {
            let closes = invocations.iter().filter(|report| is_final(report)).count();
            assert_eq!(
                closes, 1,
                "case {case:?}: the window at {start} must close with exactly one final \
                 invocation, got {invocations:?}"
            );
            let closing = invocations.last().copied().unwrap_or(&Value::Null);
            assert_eq!(
                (is_final(closing), size_of(case, closing)),
                (true, 0),
                "case {case:?}: the final invocation of the window at {start} is its last one \
                 and carries no records, got {invocations:?}"
            );
        }
    }
}
