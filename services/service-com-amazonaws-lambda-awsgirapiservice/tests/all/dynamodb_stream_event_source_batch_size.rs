// SPDX-License-Identifier: CC0-1.0

//! What the `BatchSize` of an event source mapping on an Amazon DynamoDB table's STREAM
//! bounds, end to end over the wire, with DynamoDB, DynamoDB Streams and CloudWatch mounted
//! beside Lambda on ONE engine.
//!
//! The RULE asserted over the table below: while such a mapping is `Enabled`, Lambda reads
//! the stream's records and invokes the mapped function with them ("Lambda reads items from
//! the event source and invokes the function", `CreateEventSourceMapping`) so that every
//! record of the stream is delivered, and NO single invocation is handed more records than
//! the mapping's `BatchSize` — "The maximum number of records in each batch that Lambda
//! pulls from your stream or queue and sends to your function"
//! (`CreateEventSourceMappingRequest$BatchSize`). Two consequences follow and are asserted
//! with it: a batch is never empty, and delivering `n` records under a ceiling of `b` takes
//! at least `n / b` invocations, rounded up.
//!
//! Every value asserted is observed from INSIDE the handler, which writes what it was handed
//! into a second table through the AWS SDK — one row per delivered record, keyed on the
//! record's own `eventID` and carrying how many records that one invocation got, plus one
//! counter row the handler increments once per invocation. A handler that was never invoked
//! writes nothing and the row fails; a handler handed a batch over its ceiling records the
//! size that breaks it.
//!
//! The rows differ along what the rule generalises over: the `BatchSize` the mapping carries
//! (one record at a time, a ceiling the backlog exceeds several times over, a ceiling that is
//! exactly the number of records written, and a ceiling far above it), how many records are
//! written under it, and which `StartingPosition` fixes the mapping's position.
//!
//! The suite that pins what one delivered record CARRIES is [`crate::
//! dynamodb_stream_event_source_delivery`], whose fixtures — the engine, the tables, the
//! function and the mapping — are the ones used here, so both suites drive the same wire.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::time::Instant;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, emulator, given_function, given_mapping, given_table, results_until, stop_mapping,
    write_item, Write, BUDGET, KEY, LATEST, LOOK_AGAIN, NEW_AND_OLD_IMAGES, RESULT_OK,
    TRIM_HORIZON,
};

/// The key of the one row the handler counts its invocations on. Nothing in the emulator
/// reads it: the handler writes it and the assertions read it back through DynamoDB.
pub(crate) const COUNTER: &str = "batch-attempts";

/// The prefix of the per-record rows the handler writes, one per delivered stream record.
const RECORD_PREFIX: &str = "rec-";

/// The attribute each per-record row carries the size of its own invocation's batch under.
const BATCH_SIZE_ATTRIBUTE: &str = "batch_size";

/// The attribute each per-record row carries the changed item's key under.
const KEY_ID_ATTRIBUTE: &str = "key_id";

/// The attribute the counter row counts invocations under.
pub(crate) const ATTEMPTS_ATTRIBUTE: &str = "attempts";

/// The handler every row runs: for each record of the batch it was handed it writes one row
/// keyed on that record's `eventID`, carrying the batch's size and the changed item's key,
/// and it increments one counter row once per invocation.
///
/// Keying on the record's own `eventID` is what makes the rows a census of the records
/// DELIVERED rather than of the invocations made: a record handed over twice (which the
/// contract allows) writes the same row again, while a record the mapping invented or split
/// would show up as a row of its own.
pub(crate) fn observing_handler() -> String {
    format!(
        r##"
import os

import boto3

OUT_TABLE = os.environ["OUT_TABLE"]
KEY = "{KEY}"
COUNTER = "{COUNTER}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    client = boto3.client("dynamodb")
    client.update_item(
        TableName=OUT_TABLE,
        Key={{KEY: {{"S": COUNTER}}}},
        UpdateExpression="ADD #a :one",
        ExpressionAttributeNames={{"#a": "{ATTEMPTS_ATTRIBUTE}"}},
        ExpressionAttributeValues={{":one": {{"N": "1"}}}},
    )
    size = len(records)
    for record in records:
        event_id = record["eventID"]
        if not event_id:
            raise ValueError("a record carries no eventID")
        key_id = record["dynamodb"]["Keys"][KEY]["S"]
        client.put_item(
            TableName=OUT_TABLE,
            Item={{
                KEY: {{"S": "{RECORD_PREFIX}" + event_id}},
                "{BATCH_SIZE_ATTRIBUTE}": {{"N": str(size)}},
                "{KEY_ID_ATTRIBUTE}": {{"S": key_id}},
            }},
        )
    return {{"delivered": size}}
"##
    )
}

/// Everything the observation table holds, read where a client reads it.
pub(crate) async fn observations(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
) -> Vec<Value> {
    let (status, read) = dynamodb(router, "Scan", &json!({ "TableName": table })).await;
    assert_eq!(status, 200, "case {case:?}: Scan answered {status} {read}");
    assert!(
        read["LastEvaluatedKey"].is_null(),
        "case {case:?}: the observation table must fit in one page, got {read}"
    );
    read["Items"].as_array().cloned().unwrap_or_default()
}

/// The per-record rows of `items`, which are the stream records that were delivered.
pub(crate) fn delivered(items: &[Value]) -> Vec<&Value> {
    items
        .iter()
        .filter(|item| {
            item[KEY]["S"]
                .as_str()
                .is_some_and(|key| key.starts_with(RECORD_PREFIX))
        })
        .collect()
}

/// Wait until the handler has recorded at least `expected` delivered records, answering
/// everything the observation table then holds.
pub(crate) async fn wait_for_delivery(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    expected: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let items = observations(router, case, table).await;
        if delivered(&items).len() >= expected {
            return items;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must come to deliver {expected} record(s) to its \
             function, but the handler recorded {} of them: {items:?}",
            delivered(&items).len()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: the ceiling the mapping carries, how many records are written under it, and the
/// position it reads the stream from.
struct Row {
    case: &'static str,
    /// The `BatchSize` the mapping is created with.
    batch_size: i64,
    /// How many items are written into the source table, one record each.
    records: usize,
    /// The `StartingPosition` the mapping is created with.
    starting_position: &'static str,
}

/// RULE: every record of a mapped DynamoDB stream is delivered to the mapped function, and
/// no single invocation is handed more records than the mapping's `BatchSize`.
#[tokio::test]
async fn no_invocation_is_handed_more_records_than_the_mappings_batch_size() {
    let rows = vec![
        // The recorded shape: a ceiling of three, with a backlog that takes more than two
        // batches to drain, read from the newest position.
        Row {
            case: "a-ceiling-of-three-over-seven-records",
            batch_size: 3,
            records: 7,
            starting_position: LATEST,
        },
        // The smallest ceiling the model admits (`BatchSize` is `@range(min: 1, ...)`): one
        // record per invocation.
        Row {
            case: "one-record-at-a-time",
            batch_size: 1,
            records: 3,
            starting_position: TRIM_HORIZON,
        },
        // A ceiling that is exactly the number of records written: one batch may hold all
        // of them, and none may hold more.
        Row {
            case: "a-ceiling-the-backlog-exactly-fills",
            batch_size: 4,
            records: 4,
            starting_position: TRIM_HORIZON,
        },
        // A ceiling far above the backlog: the records are still all delivered, and the
        // ceiling bounds nothing.
        Row {
            case: "a-ceiling-far-above-the-backlog",
            batch_size: 100,
            records: 2,
            starting_position: LATEST,
        },
    ];

    for row in rows {
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
        let uuid = given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.starting_position,
            row.batch_size,
            false,
        )
        .await;

        // The writes the mapping has to deliver, each one item and so one stream record.
        let written: Vec<String> = (0..row.records).map(|at| format!("item-{at:02}")).collect();
        for key in &written {
            write_item(&router, case, &source, key, Write::Insert).await;
        }

        let items = wait_for_delivery(&router, case, &observed, row.records).await;
        let records = delivered(&items);

        // Every record written was delivered, and no record no write made was: the keys the
        // handler saw are exactly the keys written.
        let mut keys_delivered: Vec<String> = records
            .iter()
            .filter_map(|record| record[KEY_ID_ATTRIBUTE]["S"].as_str().map(str::to_string))
            .collect();
        keys_delivered.sort();
        keys_delivered.dedup();
        assert_eq!(
            keys_delivered, written,
            "case {case:?}: every written item's record must reach the function exactly once, \
             but the handler was handed the keys {keys_delivered:?} for the writes {written:?}"
        );
        assert_eq!(
            records.len(),
            row.records,
            "case {case:?}: {} write(s) make {} stream record(s), but the handler recorded {} \
             distinct ones: {records:?}",
            row.records,
            row.records,
            records.len()
        );

        // The ceiling itself: every invocation's own count of records, as the handler
        // counted it, is at least one and at most the mapping's `BatchSize`.
        for record in &records {
            let size = record[BATCH_SIZE_ATTRIBUTE]["N"]
                .as_str()
                .and_then(|size| size.parse::<i64>().ok())
                .unwrap_or_else(|| panic!("case {case:?}: no batch size in {record:?}"));
            assert!(
                (1..=row.batch_size).contains(&size),
                "case {case:?}: an invocation must be handed between 1 and {} record(s), but \
                 one was handed {size}: {record:?}",
                row.batch_size
            );
        }

        // And what the ceiling forces: draining the backlog under it took at least as many
        // invocations as the records need batches, counted by the handler itself.
        let attempts = items
            .iter()
            .find(|item| item[KEY]["S"] == json!(COUNTER))
            .and_then(|item| item[ATTEMPTS_ATTRIBUTE]["N"].as_str())
            .and_then(|made| made.parse::<usize>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no invocation counter in {items:?}"));
        let batches_needed = row.records.div_ceil(
            usize::try_from(row.batch_size).expect("a row's batch size must be a count"),
        );
        assert!(
            attempts >= batches_needed,
            "case {case:?}: {} record(s) under a ceiling of {} need at least {batches_needed} \
             invocation(s), but the handler counted {attempts}",
            row.records,
            row.batch_size
        );

        // The delivery is also what the mapping reports it came to, read where a client
        // reads it.
        let seen = results_until(&router, case, &uuid, RESULT_OK, Instant::now() + BUDGET).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(RESULT_OK),
            "case {case:?}: a mapping whose batches were processed must report it, saw {seen:?}"
        );

        // The trigger is torn down as a client tears one down, which is also what stops the
        // mapping from reading its source any further.
        stop_mapping(&router, case, &uuid).await;
    }
}
