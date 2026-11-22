// SPDX-License-Identifier: CC0-1.0

//! The WINDOW `DescribeExecution` reports for an execution that waited on a callback
//! (`.waitForTaskToken`) token, driven over both services' own wire protocols through the
//! production `MultiServiceRouter` with Step Functions and Lambda mounted on ONE engine —
//! so the worker really runs, really takes the spans it is asked for, and the
//! `SendTaskHeartbeat`/`SendTaskSuccess` the window has to cover are its own.
//!
//! RULE: `startDate` is "the date the execution is started" and `stopDate` "if the
//! execution ended, the date the execution stopped" (`DescribeExecutionOutput`), so the
//! two members bound the execution's REAL life: every instant of work the run performed
//! lies inside them, and a run suspended on a task token is still running while that token
//! is unanswered. The reported window is therefore never shorter than the span the work of
//! the execution took, whatever that span happens to be — it is measured and never a
//! constant.
//!
//! The rows differ along what the rule generalises over: how long the work the token was
//! handed to really takes (settling at once, one span, several spans adding up to seconds,
//! and the same shape an order of magnitude wider), in how many spans, and how many
//! heartbeats arrive inside it. Every row asserts
//! the window against two floors — the spans the row ASKED the worker for, and the span the
//! worker's own clock measured and reported back in its settlement — so a window derived
//! from a canned duration fails here however the host is loaded. Both are LOWER bounds: a
//! slow or loaded host makes the real window wider, never narrower, so no row's verdict is
//! the host's.
//!
//! The rule is about the two members and not about the callback pattern, so the second test
//! here asserts it where the same members are reported for an execution this service
//! started ITSELF: the child workflow executions of a Distributed Map, each of whose items
//! really invokes a function that works a span. A child's window has to cover its own
//! item's work for the same reason the parent's has to cover the whole run's.
//!
//! The harness is `lambda_task_control_flow`'s `Chain` (Step Functions and Lambda over one
//! engine); `tests/all/main.rs` declares every suite of this target, so it is reached
//! through `crate::lambda_task_control_flow`.

use serde_json::{json, Value};

use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The optimized Lambda invocation with the callback pattern: the `Task` hands the
/// function the token out of the context object and waits until it is redeemed.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::lambda:invoke.waitForTaskToken";

/// The status an execution a worker settled is read in (`ExecutionStatus`).
const SUCCEEDED: &str = "SUCCEEDED";

/// One invocation's own allowance, as the function's `Timeout`: well past the longest a
/// row's worker works, because what is under test is the window the execution reports and
/// not how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT_SECONDS: u64 = 60;

/// The window every row's `Task` declares as `HeartbeatSeconds`: far wider than any idle
/// span a row leaves, so no row is closed by that bound and the only thing deciding a row
/// is the window the execution reports.
const HEARTBEAT_SECONDS: u64 = 30;

/// The function the rows invoke, as `CreateFunction` names it.
const WORKER: &str = "callback-window-worker";

/// The same worker, under the name the Distributed Map suite below creates it as: each
/// test builds its own emulator, so each creates the function it invokes.
const MAP_WORKER: &str = "map-item-window-worker";

/// The optimized Lambda invocation a Distributed Map's item runs: request-response, so
/// the item's work is the invocation itself.
const INVOKE_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The span each item of that Map asks its invocation to work, in seconds, and the floor
/// in milliseconds it therefore puts under the window the item's own child execution
/// reports.
const ITEM_WORK_SECONDS: f64 = 2.0;
/// See [`ITEM_WORK_SECONDS`].
const ITEM_WORK_MS: i64 = 2_000;

/// How many items that Map fans out over, each its own child workflow execution.
const ITEMS: usize = 2;

/// The member that asks the worker for nothing but to be running, so the build of its
/// execution environment is paid before any row ("Lambda maintains the execution
/// environment for some time in anticipation of another invocation").
const WARMING: &str = "warming";

/// The case the warm-up invocation is asserted under.
const WARM_UP_CASE: &str = "the warm-up invocation that builds the worker's environment";

/// The worker every row invokes: it is handed a task token, works the spans its event
/// names and reports progress after each of them, works once more, and settles the token
/// with the number of reports it made and the span ITS OWN clock measured for the whole of
/// that run. An event carrying NO token is an item's own work instead: the handler works
/// the one span it names and answers the span it measured.
///
/// The measured span is what makes each row's floor the work's own rather than this
/// suite's: a host that runs the handler slowly reports a longer span, and the window the
/// execution reports still has to cover it.
const WORKER_SOURCE: &str = r#"
import json
import time

import boto3


def handler(event, context):
    if event.get("warming"):
        return {"warmed": True}
    began = time.monotonic()
    token = event.get("token")
    if token is None:
        # An item's own work: there is no token to redeem, so the span this run took is
        # reported in the answer the invocation returns.
        time.sleep(event["then"])
        return {"worked_ms": int((time.monotonic() - began) * 1000)}
    states = boto3.client("stepfunctions")
    beats = 0
    for pause in event["beats"]:
        time.sleep(pause)
        states.send_task_heartbeat(taskToken=token)
        beats += 1
    time.sleep(event["then"])
    worked_ms = int((time.monotonic() - began) * 1000)
    states.send_task_success(
        taskToken=token,
        output=json.dumps({"beats": beats, "worked_ms": worked_ms}),
    )
    return {"settled": True}
"#;

/// One row: how long the work the token is handed to takes, and in how many spans.
struct Row {
    case: &'static str,
    /// The span the worker works before each report of progress it makes, in seconds.
    beats: &'static [f64],
    /// And the span it works after the last of them, before settling the token.
    then: f64,
}

impl Row {
    /// The floor the row's OWN asks put under the execution's reported window, in
    /// milliseconds: every span the worker was asked to spend lies between the start of
    /// the execution and its settlement.
    fn asked_ms(&self) -> i64 {
        let seconds: f64 = self.beats.iter().sum::<f64>() + self.then;
        (seconds * 1000.0).round() as i64
    }
}

const ROWS: &[Row] = &[
    // The degenerate end of the class: a worker that settles as soon as it holds the
    // token, so the window covers the hand-over and nothing else.
    Row {
        case: "a worker that settles the token as soon as it holds it",
        beats: &[],
        then: 0.0,
    },
    // One span of work and no report of progress at all.
    Row {
        case: "a worker that works one span before settling, reporting no progress",
        beats: &[],
        then: 2.0,
    },
    // And the shape the rule is usually met in: several spans of work, each followed by a
    // report of progress, and more work after the last of them.
    Row {
        case: "a worker that reports progress after each of three spans, then works on",
        beats: &[1.0, 1.0, 1.0],
        then: 1.0,
    },
    // The same shape an order of magnitude wider, so the window a row reports is a span of
    // the work and not of anything this emulator holds a started execution for.
    Row {
        case: "a worker whose three spans and final stretch add up to several seconds",
        beats: &[5.0, 5.0, 5.0],
        then: 2.0,
    },
];

/// The one-state definition a row runs: a callback `Task` that hands the worker the token
/// out of the context object together with the spans that worker works.
fn definition(row: &Row, function_arn: &str) -> Value {
    json!({
        "StartAt": "AwaitWorker",
        "States": {
            "AwaitWorker": {
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "FunctionName": function_arn,
                    "Payload": {
                        "token.$": "$$.Task.Token",
                        "beats": row.beats,
                        "then": row.then,
                    },
                },
                "HeartbeatSeconds": HEARTBEAT_SECONDS,
                "End": true,
            },
        },
    })
}

/// The definition the Distributed Map test runs: a Map over [`ITEMS`] literal items whose
/// `ItemProcessor` invokes the worker for the one span each item works, keeping the span the
/// invocation measured as the item's own result.
fn map_definition(function_arn: &str) -> Value {
    let items: Vec<Value> = (0..ITEMS).map(|item| json!({ "item": item })).collect();
    json!({
        "StartAt": "FanOut",
        "States": {
            "FanOut": {
                "Type": "Map",
                "Items": items,
                "ItemProcessor": {
                    "ProcessorConfig": { "Mode": "DISTRIBUTED" },
                    "StartAt": "Work",
                    "States": {
                        "Work": {
                            "Type": "Task",
                            "Resource": INVOKE_RESOURCE,
                            "Parameters": {
                                "FunctionName": function_arn,
                                "Payload": { "then": ITEM_WORK_SECONDS },
                            },
                            "ResultSelector": { "worked_ms.$": "$.Payload.worked_ms" },
                            "End": true,
                        },
                    },
                },
                "End": true,
            },
        },
    })
}

/// The timestamp member `name` of `body`, in whole milliseconds since the epoch.
///
/// An `awsJson1_0` timestamp is epoch SECONDS with a fractional part, so both instants a
/// window is measured between are read through this one reader.
fn epoch_millis(case: &str, body: &Value, name: &str) -> i64 {
    let seconds = body
        .get(name)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a timestamp {name:?} in {body}"));
    (seconds * 1000.0).round() as i64
}

/// The document a settled execution carries as its `output`.
fn output(case: &str, settled: &Value) -> Value {
    settled
        .get("output")
        .and_then(Value::as_str)
        .map(|text| {
            serde_json::from_str(text)
                .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"))
        })
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {settled}"))
}

/// Every row: the window `DescribeExecution` reports covers the whole of the run, so it is
/// never shorter than the work the token was handed to really took.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reported_window_covers_the_wait_on_a_task_token() {
    let chain = Chain::new();
    let function = chain
        .given_function_of(WORKER, WORKER_SOURCE, Some(FUNCTION_TIMEOUT_SECONDS))
        .await;
    // The build of the worker's execution environment is paid HERE rather than inside the
    // first row's own run.
    let warmed = chain
        .invoked(WARM_UP_CASE, WORKER, &json!({ WARMING: true }))
        .await;
    assert_eq!(
        warmed,
        json!({ "warmed": true }),
        "case {WARM_UP_CASE:?}: the warm-up must be the worker's own run, got {warmed}"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let settled = chain
            .ran(
                case,
                &format!("callback-window-{index}"),
                &definition(row, &function),
                &json!({}),
            )
            .await;
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {case:?}: expected {SUCCEEDED} once the worker settled the token, got {settled}"
        );
        let carried = output(case, &settled);
        assert_eq!(
            carried.get("beats").and_then(Value::as_i64),
            Some(row.beats.len() as i64),
            "case {case:?}: expected every report of progress answered, got {carried}"
        );
        let window = epoch_millis(case, &settled, "stopDate")
            - epoch_millis(case, &settled, "startDate");
        // The spans the row asked the worker for all happened between the two instants.
        let asked = row.asked_ms();
        assert!(
            window >= asked,
            "case {case:?}: expected a window of at least the {asked} ms of work the row asked \
             for, got {window} ms in {settled}"
        );
        // And the span the worker's own clock measured, which is the work as it really ran
        // on this host.
        let worked = carried
            .get("worked_ms")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a worked_ms in {carried}"));
        assert!(
            window >= worked,
            "case {case:?}: expected a window covering the {worked} ms the worker reports it \
             took, got {window} ms in {settled}"
        );
    }
}

/// The same rule where this service starts the executions itself: every CHILD workflow
/// execution of a Distributed Map reports a window covering the work its own item did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_map_childs_window_covers_the_work_its_item_did() {
    let case = "two items whose own invocation really works a span";
    let chain = Chain::new();
    let function = chain
        .given_function_of(MAP_WORKER, WORKER_SOURCE, Some(FUNCTION_TIMEOUT_SECONDS))
        .await;
    let warmed = chain
        .invoked(WARM_UP_CASE, MAP_WORKER, &json!({ WARMING: true }))
        .await;
    assert_eq!(
        warmed,
        json!({ "warmed": true }),
        "case {WARM_UP_CASE:?}: the warm-up must be the worker's own run, got {warmed}"
    );

    let settled = chain
        .ran(case, "map-item-window", &map_definition(&function), &json!({}))
        .await;
    assert_eq!(
        settled.get("status").and_then(Value::as_str),
        Some(SUCCEEDED),
        "case {case:?}: expected {SUCCEEDED} once every item is done, got {settled}"
    );
    let execution = string(&settled, "executionArn");
    // The parent's own window covers the items' work, exactly as it covers a token's wait.
    let window =
        epoch_millis(case, &settled, "stopDate") - epoch_millis(case, &settled, "startDate");
    assert!(
        window >= ITEM_WORK_MS,
        "case {case:?}: expected the parent's window to cover the {ITEM_WORK_MS} ms its items \
         worked, got {window} ms in {settled}"
    );

    let runs = chain
        .sfn(case, "ListMapRuns", &json!({ "executionArn": execution }))
        .await;
    let map_run = runs
        .get("mapRuns")
        .and_then(Value::as_array)
        .and_then(|runs| runs.first())
        .map(|run| string(run, "mapRunArn"))
        .unwrap_or_else(|| panic!("case {case:?}: expected one Map Run in {runs}"));
    let listed = chain
        .sfn(case, "ListExecutions", &json!({ "mapRunArn": map_run }))
        .await;
    let children = listed
        .get("executions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the children in {listed}"));
    assert_eq!(
        children.len(),
        ITEMS,
        "case {case:?}: expected one child per item, got {listed}"
    );
    for child in &children {
        let arn = string(child, "executionArn");
        let described = chain.settled(case, &arn).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {case:?}: expected {SUCCEEDED} for the child {arn}, got {described}"
        );
        let carried = output(case, &described);
        let worked = carried
            .get("worked_ms")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a worked_ms in {carried}"));
        let child_window = epoch_millis(case, &described, "stopDate")
            - epoch_millis(case, &described, "startDate");
        assert!(
            child_window >= ITEM_WORK_MS,
            "case {case:?}: expected the child {arn} to report a window of at least the \
             {ITEM_WORK_MS} ms its item was asked to work, got {child_window} ms in {described}"
        );
        assert!(
            child_window >= worked,
            "case {case:?}: expected the child {arn} to report a window covering the {worked} ms \
             its invocation reports it took, got {child_window} ms in {described}"
        );
    }
}
