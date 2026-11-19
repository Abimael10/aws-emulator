// SPDX-License-Identifier: CC0-1.0

//! How many of a `Map` state's items have their WORK being done at one instant, driven over
//! three services' own wire protocols through the production `MultiServiceRouter` with Step
//! Functions, Lambda and DynamoDB mounted on ONE engine: each item's `Task` really invokes the
//! function, and the runs count themselves in DynamoDB, so what a row reads is how many
//! invocations this emulator really had in flight together.
//!
//! RULE: "MaxConcurrency […] an upper bound on how many invocations of the Iterator may run in
//! parallel". A `Map` state's items are performed CONCURRENTLY up to that many at one instant,
//! and never more than that — so the concurrency a client configures is the concurrency its
//! items' work gets, and a limit of `1` runs them one at a time.
//!
//! The rows differ along what the rule generalises over: the `MaxConcurrency` declared (a limit
//! equal to the item count, and `1`), and whether the figure a row's runs wait for is one its
//! limit allows (where the rendezvous is what ends them, and the concurrency must REACH it) or
//! one it forbids (where each run is observed over its whole budget of looks, and nothing may
//! exceed the limit). The suite that asks the same question of the assembled emulator, over
//! more limits and item counts, is `core/router/tests/all/sfn_map_items_run_their_work_together`.
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `sfn`, `ddb`) is the one
//! `lambda_task_control_flow` builds for these services, and the table the runs count
//! themselves in is `saga_compensation_on_task_failure`'s; `tests/all/main.rs` declares every
//! suite of this target, so both are reached through `crate::<suite>`.

use serde_json::{json, Value};

use crate::lambda_task_control_flow::Chain;
use crate::saga_compensation_on_task_failure::{
    given_table, FUNCTION_TIMEOUT_SECONDS, KEY, TABLE,
};
use crate::state_machine_lifecycle::string;

/// The attribute the runs of one row count themselves in.
const RUNNING_ATTRIBUTE: &str = "running";

/// The `Resource` of the optimized Lambda integration, which names the service and the action
/// rather than the function.
const INVOKE_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// How long one run waits between two looks at how many invocations are in flight.
const SECONDS_BETWEEN_LOOKS: f64 = 0.1;

/// How many looks a run whose row's figure the limit ALLOWS may take: spent only when the
/// rendezvous does not happen, so it is generous — how fast a host gets through a handler is
/// the host's business, and no row may turn on it.
const RENDEZVOUS_LOOKS: u64 = 150;

/// How many a run of the row asking only about the BOUND may take: every run of that row spends
/// all of them, because the figure it waits for is one the limit forbids, so this is the window
/// those runs are observed over rather than a ceiling on a failure.
const BOUNDED_LOOKS: u64 = 40;

/// The handler every item of every row invokes: it counts itself into the row's tally of the
/// invocations in flight, reads that tally until it holds as many as the row asks for (or runs
/// out of looks), and counts itself out before it answers — so the tally is the number of this
/// function's invocations running at that instant, and what the handler reports is the greatest
/// number it ever read.
const WORKER_HANDLER: &str = r#"
import time

import boto3

CLIENT = boto3.client("dynamodb")


def counted(table, key, attribute, delta):
    CLIENT.update_item(
        TableName=table,
        Key={"pk": {"S": key}},
        UpdateExpression="ADD %s :delta" % attribute,
        ExpressionAttributeValues={":delta": {"N": str(delta)}},
    )


def running_now(table, key, attribute):
    read = CLIENT.get_item(
        TableName=table,
        Key={"pk": {"S": key}},
        ConsistentRead=True,
    )
    return int(read["Item"][attribute]["N"])


def handler(event, context):
    table = event["table"]
    key = event["run"]
    attribute = event["attribute"]
    want = int(event["want"])
    looks = int(event["looks"])
    pause = float(event["pause"])
    counted(table, key, attribute, 1)
    together = 0
    for taken in range(looks):
        seen = running_now(table, key, attribute)
        if seen > together:
            together = seen
        if together >= want:
            break
        time.sleep(pause)
    counted(table, key, attribute, -1)
    return {"item": event["item"], "together": together}
"#;

/// One row: the `MaxConcurrency` the state declares, how many items it is given, the figure its
/// runs wait for, how many looks each of them may take, and the two bounds the greatest figure
/// any of them read must satisfy.
struct Row {
    case: &'static str,
    concurrency: i64,
    items: usize,
    want: usize,
    looks: u64,
    /// The concurrency the limit allows over this many items, which must have been reached.
    at_least: usize,
    /// And the most any run may ever have read, which is that same limit.
    at_most: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two items under a limit of two: both invocations run together",
            concurrency: 2,
            items: 2,
            want: 2,
            looks: RENDEZVOUS_LOOKS,
            at_least: 2,
            at_most: 2,
        },
        Row {
            case: "two items under a limit of one, which runs them one at a time",
            concurrency: 1,
            items: 2,
            // One more than the limit allows, so neither run stops early and each is observed
            // over its whole budget of looks.
            want: 2,
            looks: BOUNDED_LOOKS,
            at_least: 1,
            at_most: 1,
        },
    ]
}

/// The definition a row runs: ONE `Map` state over the items its input carries, declaring the
/// row's `MaxConcurrency`, whose processor is a single `Task` invoking the function with the
/// document the `ItemSelector` built for that item.
fn definition(concurrency: i64, function_arn: &str) -> Value {
    json!({
        "StartAt": "Fan",
        "States": {
            "Fan": {
                "Type": "Map",
                "ItemsPath": "$.items",
                "MaxConcurrency": concurrency,
                "ItemSelector": {
                    "item.$": "$$.Map.Item.Value",
                    "table": TABLE,
                    "attribute": RUNNING_ATTRIBUTE,
                    "run.$": "$.run",
                    "want.$": "$.want",
                    "looks.$": "$.looks",
                    "pause.$": "$.pause",
                },
                "ItemProcessor": {
                    "StartAt": "Work",
                    "States": {
                        "Work": {
                            "Type": "Task",
                            "Resource": INVOKE_RESOURCE,
                            "Parameters": {
                                "FunctionName": function_arn,
                                "Payload.$": "$",
                            },
                            "End": true,
                        },
                    },
                },
                "End": true,
            },
        },
    })
}

/// Seed the tally `key` at nothing, so the runs that count themselves into it read a figure
/// rather than nothing.
async fn given_tally(chain: &Chain, case: &str, key: &str) {
    chain
        .ddb(
            case,
            "PutItem",
            &json!({
                "TableName": TABLE,
                "Item": {
                    KEY: { "S": key },
                    RUNNING_ATTRIBUTE: { "N": "0" },
                },
            }),
        )
        .await;
}

/// What the items of one execution of `definition` read, in the order the items are listed: the
/// greatest number of invocations each of them saw in flight.
async fn observed(chain: &Chain, case: &str, name: &str, row: &Row, function_arn: &str) -> Vec<usize> {
    given_tally(chain, case, name).await;
    let items: Vec<Value> = (0..row.items).map(|item| json!(item)).collect();
    let described = chain
        .ran(
            case,
            name,
            &definition(row.concurrency, function_arn),
            &json!({
                "run": name,
                "want": row.want,
                "looks": row.looks,
                "pause": SECONDS_BETWEEN_LOOKS,
                "items": items,
            }),
        )
        .await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the run to succeed, got {described}"
    );
    let output: Value = serde_json::from_str(&string(&described, "output"))
        .unwrap_or_else(|error| panic!("case {case:?}: expected JSON output, got {error}"));
    let read: Vec<usize> = output
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an array, got {output}"))
        .iter()
        .map(|item| {
            usize::try_from(item["Payload"]["together"].as_u64().unwrap_or_default())
                .unwrap_or_default()
        })
        .collect();
    // Every run counted itself out again, so the tally is back where it started: what the runs
    // read was the invocations in flight and nothing left over.
    let tally = chain
        .ddb(
            case,
            "GetItem",
            &json!({
                "TableName": TABLE,
                "Key": { KEY: { "S": name } },
                "ConsistentRead": true,
            }),
        )
        .await;
    assert_eq!(
        tally["Item"][RUNNING_ATTRIBUTE]["N"].as_str(),
        Some("0"),
        "case {case:?}: expected every run to have counted itself out again, got {tally}"
    );
    read
}

/// A `Map` state has up to `MaxConcurrency` of its items' work in flight at one instant, and
/// never more: each row's runs report how many of them were really running together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn map_items_have_their_work_in_flight_up_to_max_concurrency() {
    let chain = Chain::with_dynamodb();
    let setup = "the table and the function every row runs on";
    given_table(&chain, setup).await;
    let function_arn = chain
        .given_function_of(
            "map-concurrency-worker",
            WORKER_HANDLER,
            Some(FUNCTION_TIMEOUT_SECONDS),
        )
        .await;

    // An execution environment is built for the first invocation it serves and then stands for
    // the next one, so the rows below are served by standing ones: this run makes two of them
    // stand, with each of its runs waiting for nothing but its own count, so no row turns on
    // how long building an interpreter takes on this host. It asserts nothing about
    // concurrency — it is a `given`.
    let warming = Row {
        case: "warming",
        concurrency: 2,
        items: 2,
        want: 1,
        looks: RENDEZVOUS_LOOKS,
        at_least: 1,
        at_most: 2,
    };
    observed(&chain, setup, "warming", &warming, &function_arn).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let read = observed(
            &chain,
            case,
            &format!("map-concurrency-{index}"),
            &row,
            &function_arn,
        )
        .await;
        let most = read.iter().copied().max().unwrap_or_default();
        assert_eq!(
            most, row.at_least,
            "case {case:?}: expected {} of this state's items to have their work in flight \
             together under MaxConcurrency {}, got {read:?}",
            row.at_least, row.concurrency
        );
        assert!(
            read.iter().all(|seen| *seen <= row.at_most),
            "case {case:?}: MaxConcurrency {} is an upper bound, so no run may read more than \
             {} invocations in flight, got {read:?}",
            row.concurrency,
            row.at_most
        );
    }
}
