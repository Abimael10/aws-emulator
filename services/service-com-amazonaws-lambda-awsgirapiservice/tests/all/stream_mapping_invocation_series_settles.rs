// SPDX-License-Identifier: CC0-1.0

//! What the `AWS/Lambda` `Invocations` series of a function driven by an event source mapping
//! on an Amazon DynamoDB table's STREAM is worth to the client reading it, end to end over the
//! wire, with DynamoDB, DynamoDB Streams and CloudWatch mounted beside Lambda on ONE engine.
//!
//! The RULE asserted over the table below: draining a backlog of `n` stream records under a
//! mapping whose `BatchSize` is `b` takes at least `n / b` invocations rounded up and never more
//! than one per record — "The maximum number of records in each batch that Lambda pulls from
//! your stream or queue and sends to your function"
//! (`CreateEventSourceMappingRequest$BatchSize`), and a batch is never empty — and EVERY one of
//! those invocations, and only those, is metered as one `Invocations` datapoint of the invoked
//! function. `Invocations` is "the number of invocations" of a function (the AWS Lambda metric
//! reference; the series itself is a recorded behaviour and is not in the Smithy model), so a
//! `Sum` read over a window that holds the whole drain equals the number of invocations the
//! function really received: it stops where the backlog stops rather than going on advancing,
//! and it never counts an invocation nothing made.
//!
//! Two consequences a client depends on follow and are asserted with it: the figure is
//! COMPARABLE across mappings — a mapping that takes one record at a time is metered at one
//! invocation per record, while a mapping whose ceiling spans several of them is metered
//! strictly fewer times over the same burst, which is what makes `Invocations` the series a
//! caller tunes a mapping's batching on — and the figure SETTLES: once the backlog has been
//! delivered, the series holds exactly the invocations the handler itself counted and no more,
//! so a caller waiting for a stream-driven count to stop moving before acting reaches that
//! point instead of watching it climb.
//!
//! What makes this more than a restatement of the delivery: how many records Lambda hands over
//! per invocation is the MAPPING's own setting, while the metered count belongs to the FUNCTION,
//! so the two can disagree — a series that counted batches read, polls made or records
//! delivered would be a plausible figure that is not the number of invocations, and a series
//! that kept being appended to after the drain would never settle. Both halves are therefore
//! read against the handler's OWN census of what it was handed: one row per delivered record,
//! keyed on the record's own `eventID`, and one counter row it increments once per invocation.
//! A handler that never ran writes nothing and the row fails.
//!
//! The rows differ along what the rule generalises over: the `BatchSize` the mapping carries
//! (one record at a time, a ceiling the backlog fills twice over, and a ceiling far above the
//! whole backlog) and how many records the burst holds (two counts, so no row's figure can be a
//! constant). Every burst is written BEFORE its mapping is created and read from
//! `TRIM_HORIZON`, so what each ceiling is asked to batch is the whole backlog rather than
//! whatever had arrived by the time a poller first looked.
//!
//! The fixtures — the engine, the tables, the function, the mapping and the observing handler —
//! are [`crate::dynamodb_stream_event_source_delivery`]'s and
//! [`crate::dynamodb_stream_event_source_batch_size`]'s, so all three suites drive the same
//! wire: that suite pins the CEILING on one invocation's batch, and this one pins what the
//! invocations it forces are metered as. The throttle half of the same namespace — a request
//! refused by the function's own reservation, counted in `Throttles` instead — is
//! [`crate::invocation_outcome_series`]'s.
//!
//! Every wait here is for a CONDITION, looked at again until it holds or a generous ceiling
//! passes: the handler's census reaching the burst's size, and the series catching up with the
//! invocations the handler counted. Nothing is asserted about how long any of it took, so how
//! fast or how loaded the host is decides no outcome here.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_batch_size::{
    delivered, observations, observing_handler, wait_for_delivery, ATTEMPTS_ATTRIBUTE, COUNTER,
};
use crate::dynamodb_stream_event_source_delivery::{
    emulator, given_function, given_mapping, given_table, invocation_count, stop_mapping,
    write_item, Write, BUDGET, KEY, LOOK_AGAIN, NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};

/// One row: how many records the burst holds and the ceiling the mapping reads it under.
struct Row {
    case: &'static str,
    /// How many items are written into the source table before the mapping exists, one stream
    /// record each.
    records: usize,
    /// The `BatchSize` the mapping is created with.
    batch_size: i64,
}

/// The rows: each burst read at the smallest ceiling the model admits (`BatchSize` is
/// `@range(min: 1, ...)`), at one the burst fills more than once, and at one far above the whole
/// burst — over two burst sizes, so the figure each row expects is the row's own.
const ROWS: &[Row] = &[
    Row {
        case: "four-records-one-at-a-time",
        records: 4,
        batch_size: 1,
    },
    Row {
        case: "four-records-two-at-a-time",
        records: 4,
        batch_size: 2,
    },
    Row {
        case: "four-records-under-a-ceiling-far-above-them",
        records: 4,
        batch_size: 100,
    },
    Row {
        case: "six-records-one-at-a-time",
        records: 6,
        batch_size: 1,
    },
    Row {
        case: "six-records-under-a-ceiling-far-above-them",
        records: 6,
        batch_size: 100,
    },
];

/// How many invocations the handler itself counted, read out of the counter row it keeps.
fn attempts_recorded(case: &str, items: &[Value]) -> usize {
    items
        .iter()
        .find(|item| item[KEY]["S"] == json!(COUNTER))
        .and_then(|item| item[ATTEMPTS_ATTRIBUTE]["N"].as_str())
        .and_then(|made| made.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no invocation counter in {items:?}"))
}

/// Wait until the `Invocations` series of `function` holds exactly as many counts as the handler
/// recorded invocations, answering that figure.
///
/// Both sides are read again on every look: the metering happens as each invocation is admitted,
/// so the series can be ahead of a handler that has not finished writing its counter row yet,
/// and the mapping has been torn down before this is called, so the two converge on the
/// invocations that were really made rather than on a moving target. A series that stays ahead
/// of the census — an invocation metered twice, or one metered for work nothing did — never
/// converges and fails here.
async fn settled_invocation_count(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    table: &str,
) -> usize {
    let deadline = Instant::now() + BUDGET;
    loop {
        let items = observations(router, case, table).await;
        let attempts = attempts_recorded(case, &items);
        let metered = invocation_count(router, case, function).await;
        if (metered - attempts as f64).abs() < f64::EPSILON {
            return attempts;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: AWS/Lambda Invocations for {function:?} must come to sum to the \
             {attempts} invocation(s) the handler counted, and it summed to {metered}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: every invocation a DynamoDB-stream mapping makes to drain its backlog, and only those,
/// is metered as one `AWS/Lambda` `Invocations` datapoint of the invoked function — so the `Sum`
/// over the drain is the number of invocations the function received, it settles there once the
/// backlog is delivered, and a mapping batching several records per invocation is metered
/// strictly fewer times over the same burst than one taking a record at a time.
#[tokio::test]
async fn a_stream_mappings_invocations_are_metered_and_settle_at_what_its_batch_size_forced() {
    // What each burst size came to at the smallest ceiling, which is the figure the wider
    // ceilings over the same burst are compared against.
    let mut one_at_a_time: Vec<(usize, usize)> = Vec::new();

    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let source = format!("source-{case}");
        let observed = format!("observed-{case}");
        let function = format!("consume-{case}");
        let stream_arn = given_table(&router, case, &source, Some(NEW_AND_OLD_IMAGES)).await;
        given_table(&router, case, &observed, None).await;
        given_function(
            &router,
            case,
            &function,
            &observing_handler(),
            json!({ "OUT_TABLE": observed }),
        )
        .await;

        // The whole burst is written BEFORE the mapping exists, so the backlog every ceiling is
        // asked to batch is the same one.
        for at in 0..row.records {
            write_item(&router, case, &source, &format!("item-{at:02}"), Write::Insert).await;
        }
        let uuid = given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            TRIM_HORIZON,
            row.batch_size,
            false,
        )
        .await;

        // The burst reaching the handler is the work the series is about, so it is waited for
        // first — on the handler's own census of the records it was handed.
        let items = wait_for_delivery(&router, case, &observed, row.records).await;
        let records = delivered(&items).len();
        assert_eq!(
            records, row.records,
            "case {case:?}: the burst's {} record(s) must each reach the handler exactly once, \
             and it recorded {records} of them: {items:?}",
            row.records
        );

        // The trigger is torn down before the series is read, so nothing can add to the count
        // while the two sides are being compared.
        stop_mapping(&router, case, &uuid).await;
        let metered = settled_invocation_count(&router, case, &function, &observed).await;

        // What the ceiling forces, and what no ceiling can go past: a batch holds at most
        // `BatchSize` records and at least one, so draining the burst takes at least as many
        // invocations as it needs batches and at most one per record.
        let ceiling = usize::try_from(row.batch_size).expect("a row's ceiling is a count");
        let batches_needed = row.records.div_ceil(ceiling);
        assert!(
            (batches_needed..=row.records).contains(&metered),
            "case {case:?}: {} record(s) under a ceiling of {} must be metered as between \
             {batches_needed} and {} invocation(s), and the series summed to {metered}",
            row.records,
            row.batch_size,
            row.records
        );

        if row.batch_size == 1 {
            // A ceiling of one record per invocation: the count is one per record, which is the
            // figure the wider ceilings are read against.
            assert_eq!(
                metered, row.records,
                "case {case:?}: a ceiling of one record per invocation must be metered as one \
                 invocation per record, so {} and not {metered}",
                row.records
            );
            one_at_a_time.push((row.records, metered));
            continue;
        }

        // And the comparison a client tunes a mapping on: the same burst under a ceiling that
        // spans several records is metered strictly fewer times than under the smallest one.
        let baseline = one_at_a_time
            .iter()
            .find(|(records, _)| *records == row.records)
            .map(|(_, metered)| *metered)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no one-record-at-a-time row over {} record(s) to compare \
                     against",
                    row.records
                )
            });
        assert!(
            metered < baseline,
            "case {case:?}: a ceiling of {} over {} record(s) must be metered as fewer \
             invocations than the {baseline} a ceiling of one is, and the series summed to \
             {metered}",
            row.batch_size,
            row.records
        );
    }
}
