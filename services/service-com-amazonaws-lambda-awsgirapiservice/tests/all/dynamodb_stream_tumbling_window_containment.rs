// SPDX-License-Identifier: CC0-1.0

//! Which WINDOW a tumbling-window invocation of a mapping on an Amazon DynamoDB table's
//! stream is stamped with, end to end over the wire, with DynamoDB, DynamoDB Streams and
//! CloudWatch mounted beside Lambda on ONE engine.
//!
//! The RULE asserted over the table below: a mapping carrying a `TumblingWindowInSeconds`
//! window — "(Kinesis and DynamoDB Streams only) The duration in seconds of a processing
//! window for DynamoDB and Kinesis Streams event sources. A value of 0 seconds indicates no
//! tumbling window" (`smithy_query(query="operation",
//! operation="CreateEventSourceMapping")`) — partitions the stream into fixed,
//! non-overlapping windows of that duration, and each invocation reports the window it is
//! aggregating, so every record that invocation carries belongs to it:
//!
//! ```text
//! window.start <= record.dynamodb.ApproximateCreationDateTime < window.end
//! ```
//!
//! with `window.end - window.start` the configured duration and `window.start` a whole
//! multiple of it since the epoch. The window is therefore a fact about WHEN THE RECORDS WERE
//! CREATED, not about when the poller happened to form the batch: a record created shortly
//! before a boundary and read shortly after it belongs to the window before that boundary.
//! That is what makes a handler keying its aggregate on `event['window']` count exactly the
//! records of the window it is told it is in — the whole point of the setting.
//!
//! Every value asserted is observed from INSIDE the handler, which writes one row per
//! delivered record into a second table through the AWS SDK, carrying the window it was told
//! it was in beside that record's own creation instant; a handler that was never invoked
//! writes nothing and the row fails. Two consequences are asserted with the containment: the
//! records behind a window boundary are still all delivered, and each window still ends in a
//! final invocation (`isFinalInvokeForWindow`), which is what a window a boundary split the
//! backlog at must come to.
//!
//! The rows differ along what the rule generalises over: the window's duration (the shortest
//! the member admits, the one the report cites, and the longest the member admits), how many
//! records the backlog holds against it, and the `BatchSize` it is read in. The duration's
//! published bounds and the value the model documents as no window are read off the member by
//! the unit table in `state/event_source_poller/tumbling_window.rs`; the rows here are the
//! ones that need a stream, a function and a delivery to decide.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, emulator, given_function, given_table, stop_mapping, write_item, Write, BUDGET, KEY,
    LOOK_AGAIN, NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};
use crate::event_source_mappings::{call, MAPPINGS_URI};

/// The prefix of the per-record rows the handler writes, one per delivered stream record.
const RECORD_PREFIX: &str = "rec-";

/// The key of the one row the handler counts the FINAL invocations of windows on.
const FINALS_COUNTER: &str = "finals";

/// The attributes each per-record row carries: the changed item's key, the instant the
/// record's source stamped it as created at, and the window the invocation that carried it
/// reported.
const KEY_ATTRIBUTE: &str = "key_id";
/// See [`KEY_ATTRIBUTE`].
const CREATED_ATTRIBUTE: &str = "created";
/// See [`KEY_ATTRIBUTE`].
const WINDOW_START_ATTRIBUTE: &str = "window_start";
/// See [`KEY_ATTRIBUTE`].
const WINDOW_END_ATTRIBUTE: &str = "window_end";
/// See [`KEY_ATTRIBUTE`].
const SHARD_ATTRIBUTE: &str = "shard_id";

/// The attribute the finals row counts window finalisations under.
const COUNT_ATTRIBUTE: &str = "count";

/// The handler every row runs: for each record of the batch it was handed it writes one row
/// keyed on that record's own `eventID`, carrying the record's creation instant and the
/// window the invocation was told it is in, and it counts the final invocation of each
/// window on one row of its own. It returns the running count as the window's next state, so
/// the aggregate really is carried from one invocation of a window to the next.
///
/// Everything it writes is taken out of the event it was handed, so no row can pass on a
/// value the handler was never given.
fn window_auditing_handler() -> String {
    format!(
        r##"
import os

import boto3

OUT_TABLE = os.environ["OUT_TABLE"]
KEY = "{KEY}"


def lambda_handler(event, context):
    client = boto3.client("dynamodb")
    window = event.get("window") or {{}}
    records = event.get("Records") or []
    state = event.get("state") or {{}}
    if event.get("isFinalInvokeForWindow"):
        client.update_item(
            TableName=OUT_TABLE,
            Key={{KEY: {{"S": "{FINALS_COUNTER}"}}}},
            UpdateExpression="ADD #c :one",
            ExpressionAttributeNames={{"#c": "{COUNT_ATTRIBUTE}"}},
            ExpressionAttributeValues={{":one": {{"N": "1"}}}},
        )
    for record in records:
        body = record["dynamodb"]
        client.put_item(
            TableName=OUT_TABLE,
            Item={{
                KEY: {{"S": "{RECORD_PREFIX}" + record["eventID"]}},
                "{KEY_ATTRIBUTE}": {{"S": body["Keys"][KEY]["S"]}},
                "{CREATED_ATTRIBUTE}": {{
                    "N": str(int(body["ApproximateCreationDateTime"]))
                }},
                "{WINDOW_START_ATTRIBUTE}": {{"S": window.get("start") or ""}},
                "{WINDOW_END_ATTRIBUTE}": {{"S": window.get("end") or ""}},
                "{SHARD_ATTRIBUTE}": {{"S": event.get("shardId") or ""}},
            }},
        )
    return {{"state": {{"seen": int(state.get("seen", 0)) + len(records)}}}}
"##
    )
}

/// The row's mapping on `stream_arn`, carrying the tumbling window and the batch size the
/// row states; read back from the create's own answer, so a row cannot pass against a window
/// the mapping never carried.
async fn given_windowed_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    window_seconds: i64,
    batch_size: i64,
) -> String {
    let created = call(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": batch_size,
            "TumblingWindowInSeconds": window_seconds,
            "Enabled": true,
        }),
    )
    .await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: CreateEventSourceMapping answered {} {}",
        created.status,
        created.text()
    );
    let configuration = created.json();
    assert_eq!(
        configuration["TumblingWindowInSeconds"],
        json!(window_seconds),
        "case {case:?}: the mapping must carry the window it was created with, got \
         {configuration}"
    );
    configuration["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {configuration}"))
        .to_string()
}

/// Everything the audit table holds, read where a client reads it.
async fn audited(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<Value> {
    let (status, read) = dynamodb(router, "Scan", &json!({ "TableName": table })).await;
    assert_eq!(status, 200, "case {case:?}: Scan answered {status} {read}");
    assert!(
        read["LastEvaluatedKey"].is_null(),
        "case {case:?}: the audit table must fit in one page, got {read}"
    );
    read["Items"].as_array().cloned().unwrap_or_default()
}

/// The per-record rows of `items`, which are the stream records that were delivered.
fn delivered(items: &[Value]) -> Vec<&Value> {
    items
        .iter()
        .filter(|item| {
            item[KEY]["S"]
                .as_str()
                .is_some_and(|key| key.starts_with(RECORD_PREFIX))
        })
        .collect()
}

/// How many windows have been finalised, as the handler counted them.
fn finalised(items: &[Value]) -> i64 {
    items
        .iter()
        .find(|item| item[KEY]["S"] == json!(FINALS_COUNTER))
        .and_then(|item| item[COUNT_ATTRIBUTE]["N"].as_str())
        .and_then(|count| count.parse::<i64>().ok())
        .unwrap_or_default()
}

/// Wait until the handler has audited at least `expected` delivered records, and the final
/// invocation of at least one window where the row's windows end inside the run, answering
/// everything the audit table then holds; what it last held when it does not, which is what
/// the row then reports.
async fn wait_for_audit(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    expected: usize,
    awaits_final: bool,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET + BUDGET;
    loop {
        let items = audited(router, case, table).await;
        let finals_in = !awaits_final || finalised(&items) >= 1;
        if delivered(&items).len() >= expected && finals_in {
            return items;
        }
        if Instant::now() >= deadline {
            return items;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One instant the handler audited, in whole seconds since the epoch; a bound it audited
/// nothing for fails the row where it stands.
fn window_bound(case: &str, record: &Value, member: &str) -> i64 {
    let held = record[member]["S"].as_str().unwrap_or_default();
    ::aws_smithy_types::DateTime::from_str(held, ::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: an invocation inside a window must report {member} as an \
                 ISO-8601 instant, got {held:?} ({error}): {record}"
            )
        })
        .secs()
}

/// One row: the window the mapping aggregates over, how many records the backlog holds, and
/// the size they are read in.
struct Row {
    case: &'static str,
    /// The `TumblingWindowInSeconds` the mapping is created with.
    window_seconds: i64,
    /// How many items are written into the source table, one record each.
    records: usize,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// Whether the windows the backlog falls into END while the row runs, which is what
    /// makes their final invocation due: a window the clock is still inside owes none yet,
    /// so a row whose window outlasts the run asserts the containment alone.
    finalises: bool,
}

/// RULE: every record an invocation of a tumbling-window mapping carries lies inside the
/// window that invocation reports — the fixed window of the configured duration, aligned to
/// a whole multiple of it since the epoch, holding the instant the record's source stamped it
/// as created at — and the records on the far side of a window boundary are delivered under
/// the window they belong to rather than under the one the poller's clock was in.
#[tokio::test]
async fn every_record_of_a_windowed_invocation_lies_inside_the_window_it_reports() {
    let rows = vec![
        // The shortest window the member's range admits, over a backlog whose records are
        // written across several of its windows: every boundary of it falls inside what the
        // mapping reads.
        Row {
            case: "a-one-second-window-over-a-backlog",
            window_seconds: 1,
            records: 6,
            batch_size: 2,
            finalises: true,
        },
        // The recorded shape: a fifteen-second window read two records at a time, where a
        // record created seconds before a boundary is read after it.
        Row {
            case: "a-fifteen-second-window-read-two-at-a-time",
            window_seconds: 15,
            records: 8,
            batch_size: 2,
            finalises: true,
        },
        // The longest window the member's range admits, which the whole backlog fits inside:
        // one window carries every record, and the clock is still inside that window when the
        // row ends, so no final invocation is owed for it yet.
        Row {
            case: "the-largest-window-the-range-admits",
            window_seconds: 900,
            records: 3,
            batch_size: 10,
            finalises: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let source = format!("source-{case}");
        let audit = format!("audit-{case}");
        let function = format!("aggregate-{case}");
        let stream_arn = given_table(&router, case, &source, Some(NEW_AND_OLD_IMAGES)).await;
        given_table(&router, case, &audit, None).await;
        given_function(
            &router,
            case,
            &function,
            &window_auditing_handler(),
            json!({ "OUT_TABLE": audit }),
        )
        .await;

        // The records are written BEFORE the mapping exists, so the mapping reads them as a
        // backlog of windows the clock has already left behind: the window each record is
        // handed over under is then its own rather than one that merely looks plausible.
        let written: Vec<String> = (0..row.records).map(|at| format!("item-{at:02}")).collect();
        for key in &written {
            write_item(&router, case, &source, key, Write::Insert).await;
        }
        let uuid = given_windowed_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.window_seconds,
            row.batch_size,
        )
        .await;

        let items = wait_for_audit(&router, case, &audit, row.records, row.finalises).await;
        let records = delivered(&items);

        // Every record written reached the function: grouping the records of one window into
        // one invocation leaves none of them behind.
        let mut keys_delivered: Vec<String> = records
            .iter()
            .filter_map(|record| record[KEY_ATTRIBUTE]["S"].as_str().map(str::to_string))
            .collect();
        keys_delivered.sort();
        keys_delivered.dedup();
        assert_eq!(
            keys_delivered, written,
            "case {case:?}: every written item's record must reach the function, but the \
             handler was handed the keys {keys_delivered:?} for the writes {written:?}"
        );

        // THE RULE, for every record delivered: the invocation reported a window of the
        // configured duration, aligned to a whole multiple of it, and the record's own
        // creation instant lies inside it.
        for record in &records {
            let start = window_bound(case, record, WINDOW_START_ATTRIBUTE);
            let end = window_bound(case, record, WINDOW_END_ATTRIBUTE);
            let created = record[CREATED_ATTRIBUTE]["N"]
                .as_str()
                .and_then(|created| created.parse::<i64>().ok())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no creation instant audited for {record}")
                });
            assert_eq!(
                end - start,
                row.window_seconds,
                "case {case:?}: the window reported must span the {} second(s) the mapping \
                 configures, got {start}..{end}: {record}",
                row.window_seconds
            );
            assert_eq!(
                start % row.window_seconds,
                0,
                "case {case:?}: the windows of one duration tile the clock from a whole \
                 multiple of it, got {start}..{end}: {record}"
            );
            assert!(
                start <= created && created < end,
                "case {case:?}: the record created at {created} must lie inside the window \
                 {start}..{end} stamped on the invocation that carried it: {record}"
            );
            assert!(
                record[SHARD_ATTRIBUTE]["S"]
                    .as_str()
                    .is_some_and(|shard| shard.starts_with("shardId-")),
                "case {case:?}: an invocation inside a window names the shard its records \
                 came off, got {record}"
            );
        }

        // And each window the backlog fell into still ends in its own final invocation once
        // the clock has passed its end, so the handler that keys its aggregate on the window
        // is told when to write that window's result.
        if row.finalises {
            assert!(
                finalised(&items) >= 1,
                "case {case:?}: a window the clock has passed the end of must end in a final \
                 invocation, but the handler counted {} of them: {items:?}",
                finalised(&items)
            );
        }

        // The trigger is torn down as a client tears one down, which is also what stops the
        // mapping from reading its source any further.
        stop_mapping(&router, case, &uuid).await;
    }
}
