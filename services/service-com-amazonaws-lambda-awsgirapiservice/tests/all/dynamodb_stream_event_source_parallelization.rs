// SPDX-License-Identifier: CC0-1.0

//! How many batches of ONE shard an event source mapping on an Amazon DynamoDB table's
//! stream processes at a time, end to end over the wire, with DynamoDB, DynamoDB Streams and
//! CloudWatch mounted beside Lambda on ONE engine.
//!
//! RULE: `ParallelizationFactor` is "(Kinesis and DynamoDB Streams only) the number of
//! batches to process from each shard concurrently"
//! (`smithy_query(query="doc", shape="CreateEventSourceMappingRequest",
//! member="ParallelizationFactor")`), so a mapping configuring more than one has that many
//! invocations of its function IN FLIGHT AT ONE INSTANT for one shard, while a mapping
//! configuring none keeps one. The concurrency reorders nothing a client may rely on: the
//! records of one partition key go to the SAME batch, so two changes to one item are never in
//! flight beside each other.
//!
//! Each row's evidence is read where a client reads it: the concurrency the function reached
//! comes off Lambda's own `ConcurrentExecutions` series, whose datapoints are the
//! observations of that gauge at the instants it rises, and the delivery itself comes off the
//! results table the handler writes one row into per record it was handed — so a row passes
//! only if the records really reached the handler.
//!
//! The first table's rows differ along what the rule generalises over: the
//! `ParallelizationFactor` configured (none, the published minimum, a value inside the
//! published range, the published maximum), how many records one batch holds, and how long
//! the backlog waiting in the shard is.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::time::Instant;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, emulator, given_function, given_table, write_item, Write, BUDGET, KEY, LOOK_AGAIN,
    NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};
use crate::event_source_mapping_scaling_concurrency::observed_concurrency;
use crate::event_source_mappings::{await_mapping_state, create_mapping, STATE_ENABLED};

/// The range `ParallelizationFactor` admits
/// (`smithy_query(query="shape", shape="ParallelizationFactor")`: `@range(min: 1, max: 10)`).
const PARALLELIZATION_MIN: i64 = 1;
/// See [`PARALLELIZATION_MIN`].
const PARALLELIZATION_MAX: i64 = 10;

/// How long the handler holds its execution environment, in the seconds `time.sleep` takes.
/// Short, because the run capacity of this process is shared with every other test beside it
/// — and it is the HANDLER's own run, not a wait this test makes: a mapping that handed its
/// batches over one after another would take one of these per batch, and one that hands them
/// over together takes one for all of them.
const HELD_SECONDS: f64 = 0.5;

/// The least concurrency a row accepts as evidence that the batches of one shard really were
/// in flight together: two invocations of the function at one instant.
const OVERLAP: f64 = 2.0;

/// The most concurrency a mapping that processes one batch at a time may be seen holding.
const SERIAL: f64 = 1.0;

/// One row: what the mapping asks for, how long the backlog in the shard is, and how much
/// concurrency its function must then be seen holding.
struct Row {
    case: &'static str,
    /// `ParallelizationFactor`, or `None` for a mapping that configures none.
    parallelization_factor: Option<i64>,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: i64,
    /// How many items are written to the source table before the mapping starts reading, each
    /// under its own key — so each is a record of its own partition key.
    waiting: usize,
    /// Whether the batches of one shard must be seen in flight together.
    overlaps: bool,
}

/// The catalog: a mapping configuring no `ParallelizationFactor`, one configuring each end of
/// the published range, and ones whose batches hold one or several records.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no parallelization configured",
            parallelization_factor: None,
            batch_size: 1,
            waiting: 3,
            overlaps: false,
        },
        Row {
            case: "the published minimum parallelization",
            parallelization_factor: Some(PARALLELIZATION_MIN),
            batch_size: 1,
            waiting: 3,
            overlaps: false,
        },
        Row {
            case: "two batches of one record from one shard",
            parallelization_factor: Some(2),
            batch_size: 1,
            waiting: 4,
            overlaps: true,
        },
        Row {
            case: "two batches of two records from one shard",
            parallelization_factor: Some(2),
            batch_size: 2,
            waiting: 4,
            overlaps: true,
        },
        Row {
            case: "the published maximum parallelization over a backlog of two",
            parallelization_factor: Some(PARALLELIZATION_MAX),
            batch_size: 1,
            waiting: 2,
            overlaps: true,
        },
    ]
}

/// The handler every row runs: it holds its execution environment for a span, then writes one
/// row into the results table per record it was handed — keyed by the record's own `eventID`,
/// carrying the key of the item that changed.
///
/// The hold is what makes the batches of one shard observable as concurrent at all, and the
/// rows are what makes the delivery observable: both are consequences of the records having
/// reached the handler.
fn recording_handler() -> String {
    format!(
        r#"
import os
import time

import boto3

OUT_TABLE = os.environ["OUT_TABLE"]
HELD_SECONDS = float(os.environ["HELD_SECONDS"])
KEY = "{KEY}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    time.sleep(HELD_SECONDS)
    client = boto3.client("dynamodb")
    for record in records:
        client.put_item(
            TableName=OUT_TABLE,
            Item={{
                KEY: {{"S": record["eventID"]}},
                "item": {{"S": record["dynamodb"]["Keys"][KEY]["S"]}},
            }},
        )
    return {{"records": len(records)}}
"#
    )
}

/// The mapping a row asks for, created on `stream_arn` and settled into `Enabled`.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    batch_size: i64,
    parallelization_factor: Option<i64>,
) {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": TRIM_HORIZON,
        "BatchSize": batch_size,
        "Enabled": true,
    });
    if let Some(factor) = parallelization_factor {
        request["ParallelizationFactor"] = json!(factor);
    }
    let created = create_mapping(router, &request).await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: CreateEventSourceMapping answered {} {}",
        created.status,
        created.text()
    );
    let uuid = created
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
    await_mapping_state(router, case, &uuid, STATE_ENABLED).await;
}

/// The keys of the items the handler reported processing, read off the results table.
async fn processed_items(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<String> {
    let (status, scanned) = dynamodb(router, "Scan", &json!({ "TableName": table })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Scan answered {status} {scanned}"
    );
    scanned["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.pointer("/item/S")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until the results table holds a row for each of the `expected` records, answering the
/// item keys those rows carry.
async fn delivered_records(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let processed = processed_items(router, case, table).await;
        if processed.len() >= expected {
            return processed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} record(s) to reach the handler, got \
             {processed:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until the function has been seen holding at least `wanted` concurrent executions,
/// answering the most it was seen holding — the deadline is a ceiling on patience, never a
/// span anything counts on.
async fn concurrency_reached(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    wanted: f64,
) -> f64 {
    let deadline = Instant::now() + BUDGET;
    let mut observed = 0.0f64;
    loop {
        observed = observed.max(observed_concurrency(router, case, function).await);
        if observed >= wanted || Instant::now() >= deadline {
            return observed;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The concurrency a row's function was seen holding: a row that expects the batches of one
/// shard to overlap waits for that reading to appear, while one that expects none reads the
/// gauge once, after every record it wrote has reached the handler — by then every invocation
/// the mapping made has already been observed.
async fn concurrency_of(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    overlaps: bool,
) -> f64 {
    if overlaps {
        concurrency_reached(router, case, function, OVERLAP).await
    } else {
        observed_concurrency(router, case, function).await
    }
}

/// RULE: a mapping configuring a `ParallelizationFactor` above one has that many batches of
/// one shard in flight together, and one configuring none has one batch at a time — whatever
/// the batches hold and however long the backlog is.
#[tokio::test]
async fn one_shard_is_processed_as_many_batches_at_once_as_the_mapping_configures() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let source = format!("ddb-pf-source-{index}");
        let results = format!("ddb-pf-results-{index}");
        let function = format!("ddb-pf-fn-{index}");
        let stream_arn = given_table(&router, case, &source, Some(NEW_AND_OLD_IMAGES)).await;
        given_table(&router, case, &results, None).await;
        given_function(
            &router,
            case,
            &function,
            &recording_handler(),
            json!({ "OUT_TABLE": results, "HELD_SECONDS": HELD_SECONDS.to_string() }),
        )
        .await;
        // The backlog is in the shard BEFORE the mapping reads it, each item under its own
        // key: what the mapping does with several records waiting at once is the question the
        // row asks.
        let written: Vec<String> = (0..row.waiting)
            .map(|item| format!("item-{item}"))
            .collect();
        for key in &written {
            write_item(&router, case, &source, key, Write::Insert).await;
        }

        given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.batch_size,
            row.parallelization_factor,
        )
        .await;

        let delivered = delivered_records(&router, case, &results, written.len()).await;
        let mut delivered_keys = delivered.clone();
        delivered_keys.sort();
        delivered_keys.dedup();
        assert_eq!(
            delivered_keys, written,
            "case {case:?}: every record written must reach the handler, got {delivered:?}"
        );
        let observed = concurrency_of(&router, case, &function, row.overlaps).await;
        if row.overlaps {
            assert!(
                observed >= OVERLAP,
                "case {case:?}: expected the function to be seen holding at least {OVERLAP} \
                 concurrent executions, got {observed}"
            );
        } else {
            assert!(
                observed <= SERIAL,
                "case {case:?}: a mapping processing one batch at a time must never be seen \
                 holding more than {SERIAL} concurrent execution, got {observed}"
            );
        }
    }
}

/// RULE, the other half: the records of ONE partition key go to the same batch, so a mapping
/// configured for several concurrent batches still hands over the changes to one item one
/// after another — their order is what a client reads a stream for.
#[tokio::test]
async fn the_changes_to_one_item_are_never_in_flight_together() {
    let case = "four changes to one item, the published maximum parallelization";
    let router = emulator();
    let source = "ddb-pf-one-key-source";
    let results = "ddb-pf-one-key-results";
    let function = "ddb-pf-one-key-fn";
    let stream_arn = given_table(&router, case, source, Some(NEW_AND_OLD_IMAGES)).await;
    given_table(&router, case, results, None).await;
    given_function(
        &router,
        case,
        function,
        &recording_handler(),
        json!({ "OUT_TABLE": results, "HELD_SECONDS": HELD_SECONDS.to_string() }),
    )
    .await;
    // One item, changed again and again: every record the shard holds carries the same key.
    let changes = [
        Write::Insert,
        Write::Overwrite,
        Write::Update,
        Write::Overwrite,
    ];
    for change in changes {
        write_item(&router, case, source, "one-item", change).await;
    }

    given_mapping(
        &router,
        case,
        function,
        &stream_arn,
        1,
        Some(PARALLELIZATION_MAX),
    )
    .await;

    let delivered = delivered_records(&router, case, results, changes.len()).await;
    assert!(
        delivered.iter().all(|key| key == "one-item"),
        "case {case:?}: every record delivered must be a change to the one item written, got \
         {delivered:?}"
    );
    let observed = concurrency_of(&router, case, function, false).await;
    assert!(
        observed <= SERIAL,
        "case {case:?}: the changes to one item must not be in flight together, so the \
         function must never be seen holding more than {SERIAL} concurrent execution, got \
         {observed}"
    );
}
