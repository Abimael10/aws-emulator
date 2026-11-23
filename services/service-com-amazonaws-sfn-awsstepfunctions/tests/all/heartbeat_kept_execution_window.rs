// SPDX-License-Identifier: CC0-1.0

//! The WINDOW an execution reports when the only reason it is still running is that its
//! worker keeps REPORTING PROGRESS: a callback (`.waitForTaskToken`) task whose
//! `HeartbeatSeconds` window is NARROWER than the work the token was handed to, driven over
//! both services' own wire protocols through the production `MultiServiceRouter` with Step
//! Functions and Lambda mounted on ONE engine — so the worker really runs, really takes the
//! spans it is asked for, and the heartbeats and settlement the window has to cover are its
//! own.
//!
//! RULE: `startDate` is "the date the execution is started" and `stopDate` "if the execution
//! ended, the date the execution stopped" (`DescribeExecutionOutput`), so the pair are
//! MEASURED marks of when the run began and when it ended and never a span this emulator
//! chose: a run suspended on a task token is running until that token is answered, so every
//! instant of the work the token reached lies between them. The pair belongs to the
//! execution and not to the reader, so every reader of it — a second `DescribeExecution`,
//! `ListExecutions`, and the `ExecutionStarted` and `ExecutionSucceeded` events
//! `GetExecutionHistory` reports — answers the same two instants.
//!
//! What this suite adds to its two siblings is the CONJUNCTION they each cover one half of:
//! `callback_heartbeat_window` asserts which bound closes a task whose window is narrower
//! than its work, and `callback_execution_window` asserts the reported window of a task
//! under a window wide enough never to bind. Here the window the state declares is narrower
//! than the work, so the run reaches its settlement only because the worker's heartbeats
//! reset that window — and the window the execution reports still has to cover the whole of
//! the wait, which is the span a client measures, bills or ages a run by.
//!
//! The rows differ along what the rule generalises over: the span the work takes, in how
//! many stretches, how many reports of progress arrive inside it, and how many
//! `HeartbeatSeconds` windows the work adds up to — every row's work outlasting the one
//! window its state declares, so no row's verdict could be reached by a worker that returned
//! at once. Each row asserts the window against two floors — the spans the row ASKED the
//! worker for, and the span the worker's OWN clock measured and reported back in its
//! settlement — and both are LOWER bounds, so a slow or loaded host makes the real window
//! wider and never narrower and no row's verdict is the host's.
//!
//! The harness is `lambda_task_control_flow`'s `Chain` (Step Functions and Lambda over one
//! engine); `tests/all/main.rs` declares every suite of this target, so it is reached
//! through `crate::lambda_task_control_flow`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_EXECUTION_STARTED, EVENT_EXECUTION_SUCCEEDED, EVENT_TASK_TIMED_OUT,
};

use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The optimized Lambda invocation with the callback pattern: the `Task` hands the function
/// the token out of the context object and waits until it is redeemed.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::lambda:invoke.waitForTaskToken";

/// The status an execution a worker settled is read in (`ExecutionStatus`).
const SUCCEEDED: &str = "SUCCEEDED";

/// One invocation's own allowance, as the function's `Timeout`: well past the longest a row's
/// worker works, because what is under test is the window the execution reports and not how
/// fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT_SECONDS: u64 = 60;

/// The function the rows invoke, as `CreateFunction` names it.
const WORKER: &str = "heartbeat-kept-window-worker";

/// The member that asks the worker for nothing but to be running, so the build of its
/// execution environment is paid before any row ("Lambda maintains the execution environment
/// for some time in anticipation of another invocation") rather than inside a row's own
/// heartbeat window.
const WARMING: &str = "warming";

/// The case the warm-up invocation is asserted under.
const WARM_UP_CASE: &str = "the warm-up invocation that builds the worker's environment";

/// What a SERVED heartbeat is recorded as in the document the worker settles with, so the
/// execution's own output says how many of them this service answered — and a refused one is
/// reported as its own error code rather than passing as a served one.
const BEAT: &str = "beat";

/// The worker every row invokes: it is handed a task token, reports progress on it as soon as
/// it holds it, works the spans its event names and reports progress after each of them, works
/// once more, and settles the token with the reports it got answered and the span ITS OWN
/// clock measured for the whole of that run.
///
/// The measured span is what makes each row's floor the work's own rather than this suite's:
/// a host that runs the handler slowly reports a longer span, and the window the execution
/// reports still has to cover it.
const WORKER_SOURCE: &str = r#"
import json
import time

import boto3
import botocore.exceptions


def handler(event, context):
    if event.get("warming"):
        return {"warmed": True}
    began = time.monotonic()
    states = boto3.client("stepfunctions")
    token = event["token"]
    reported = []

    def report():
        try:
            states.send_task_heartbeat(taskToken=token)
            reported.append("beat")
        except botocore.exceptions.ClientError as refused:
            reported.append(refused.response["Error"]["Code"])

    report()
    for pause in event["beats"]:
        time.sleep(pause)
        report()
    time.sleep(event["then"])
    worked_ms = int((time.monotonic() - began) * 1000)
    states.send_task_success(
        taskToken=token,
        output=json.dumps({"beats": reported, "worked_ms": worked_ms}),
    )
    return {"settled": True}
"#;

/// One row: the window the state declares, and how the work the token is handed to spends it.
struct Row {
    case: &'static str,
    /// `HeartbeatSeconds`: the IDLE span one report of progress opens, which every row's work
    /// outlasts — so the row reaches its settlement only because the worker keeps resetting
    /// it.
    heartbeat_seconds: u64,
    /// The span the worker works before each further report of progress it makes, in seconds.
    beats: &'static [f64],
    /// And the span it works after the last of them, before settling the token.
    then: f64,
}

impl Row {
    /// The floor the row's OWN asks put under the execution's reported window, in
    /// milliseconds: every span the worker was asked to spend lies between the start of the
    /// execution and its settlement.
    fn asked_ms(&self) -> i64 {
        let seconds: f64 = self.beats.iter().sum::<f64>() + self.then;
        (seconds * 1000.0).round() as i64
    }

    /// The window the state declares, in milliseconds.
    fn window_ms(&self) -> i64 {
        i64::try_from(self.heartbeat_seconds).unwrap_or(i64::MAX) * 1000
    }

    /// How many reports of progress this service must answer for the row: the one the worker
    /// makes on holding the token, and one after each span in [`Self::beats`].
    fn heartbeats(&self) -> usize {
        1 + self.beats.len()
    }
}

const ROWS: &[Row] = &[
    // The smallest shape the rule needs: one stretch of work, one report of progress inside
    // the window, and a last stretch that carries the run past that window.
    Row {
        case: "one report of progress inside a four-second window, with the work outlasting it",
        heartbeat_seconds: 4,
        beats: &[2.0],
        then: 3.0,
    },
    // Two reports of progress, and work adding up to well over one window.
    Row {
        case: "two reports of progress carrying the work past a six-second window",
        heartbeat_seconds: 6,
        beats: &[3.0, 3.0],
        then: 4.0,
    },
    // And the shape the rule is usually met in: several stretches of work, each followed by a
    // report of progress, adding up to seconds — so a window derived from anything but the
    // run's own instants is an order of magnitude short here.
    Row {
        case: "three reports of progress over work adding up to several windows",
        heartbeat_seconds: 8,
        beats: &[4.0, 4.0, 4.0],
        then: 5.0,
    },
];

/// The one-state definition a row runs: a callback `Task` that hands the worker the token out
/// of the context object together with the spans that worker works, under the row's own
/// window.
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
                "HeartbeatSeconds": row.heartbeat_seconds,
                "End": true,
            },
        },
    })
}

/// The timestamp member `name` of `body`, in whole milliseconds since the epoch.
///
/// An `awsJson1_0` timestamp is epoch SECONDS with a fractional part, so every instant this
/// suite reads — of an execution and of a history event alike — is read through this one
/// reader.
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

/// The events `execution`'s history carries, in order.
async fn history(chain: &Chain, case: &str, execution: &str) -> Vec<Value> {
    let reported = chain
        .sfn(
            case,
            "GetExecutionHistory",
            &json!({ "executionArn": execution }),
        )
        .await;
    reported
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an events list in {reported}"))
}

/// The entry `ListExecutions` reports for `execution` under its own state machine.
async fn listed(chain: &Chain, case: &str, machine: &str, execution: &str) -> Value {
    let reported = chain
        .sfn(
            case,
            "ListExecutions",
            &json!({ "stateMachineArn": machine }),
        )
        .await;
    reported
        .get("executions")
        .and_then(Value::as_array)
        .and_then(|executions| {
            executions
                .iter()
                .find(|entry| entry.get("executionArn").and_then(Value::as_str) == Some(execution))
                .cloned()
        })
        .unwrap_or_else(|| panic!("case {case:?}: expected {execution} listed in {reported}"))
}

/// Every row: an execution that stays alive only because its worker keeps reporting progress
/// inside a window narrower than its work reports a window covering the whole of that wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_window_covers_a_wait_only_heartbeats_kept_alive() {
    let chain = Chain::new();
    let function = chain
        .given_function_of(WORKER, WORKER_SOURCE, Some(FUNCTION_TIMEOUT_SECONDS))
        .await;
    // The build of the worker's execution environment is paid HERE, where no task's window is
    // open, rather than inside the first window of the first row.
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
        // What makes the row a row: its work outlasts the one window its state declares, so
        // the run can only reach a settlement by having its window reset inside it.
        let window_ms = row.window_ms();
        let asked = row.asked_ms();
        assert!(
            asked > window_ms,
            "case {case:?}: the row's own work must outlast its {window_ms} ms window, asked \
             for {asked} ms"
        );

        let settled = chain
            .ran(
                case,
                &format!("heartbeat-kept-window-{index}"),
                &definition(row, &function),
                &json!({}),
            )
            .await;
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {case:?}: expected {SUCCEEDED} for a task heartbeated inside its {} s \
             window, got {settled}",
            row.heartbeat_seconds
        );
        // The document the worker settled with says how many of its reports of progress this
        // service answered: one entry per report the row makes, so a refused one is reported
        // here as the refusal's own code rather than passing as a served one.
        let carried = output(case, &settled);
        let answered: Vec<String> = (0..row.heartbeats()).map(|_| BEAT.to_string()).collect();
        assert_eq!(
            carried.get("beats"),
            Some(&json!(answered)),
            "case {case:?}: expected every report of progress answered, got {carried}"
        );

        let execution = string(&settled, "executionArn");
        let start = epoch_millis(case, &settled, "startDate");
        let stop = epoch_millis(case, &settled, "stopDate");
        let window = stop - start;
        // The spans the row asked the worker for all happened between the two instants.
        assert!(
            window >= asked,
            "case {case:?}: expected a window of at least the {asked} ms of work the row asked \
             for, got {window} ms in {settled}"
        );
        // And the span the worker's own clock measured, which is the work as it really ran on
        // this host.
        let worked = carried
            .get("worked_ms")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a worked_ms in {carried}"));
        assert!(
            window >= worked,
            "case {case:?}: expected a window covering the {worked} ms the worker reports it \
             took, got {window} ms in {settled}"
        );
        // Nothing closed the task: the window the state declared was reset in time, every
        // time.
        let events = history(&chain, case, &execution).await;
        let types: Vec<&str> = events
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .collect();
        assert!(
            !types.contains(&EVENT_TASK_TIMED_OUT),
            "case {case:?}: no {EVENT_TASK_TIMED_OUT} may be recorded for a task whose window \
             was reset in time, got {types:?}"
        );

        // The pair belongs to the EXECUTION, so reading it again answers the same two
        // instants: a read never moves them.
        let again = chain
            .sfn(
                case,
                "DescribeExecution",
                &json!({ "executionArn": execution }),
            )
            .await;
        assert_eq!(
            (
                epoch_millis(case, &again, "startDate"),
                epoch_millis(case, &again, "stopDate")
            ),
            (start, stop),
            "case {case:?}: a second read must report the same two instants, got {again}"
        );
        // And so does every OTHER reader of them: the listing of the executions of this
        // state machine...
        let entry = listed(&chain, case, &string(&settled, "stateMachineArn"), &execution).await;
        assert_eq!(
            (
                epoch_millis(case, &entry, "startDate"),
                epoch_millis(case, &entry, "stopDate")
            ),
            (start, stop),
            "case {case:?}: the listed execution must report the same two instants, got {entry}"
        );
        // ...and the history's own account of when the run began and when it ended.
        let opened = events
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected an opening event in {events:?}"));
        assert_eq!(
            opened.get("type").and_then(Value::as_str),
            Some(EVENT_EXECUTION_STARTED),
            "case {case:?}: expected the history to open on {EVENT_EXECUTION_STARTED}, got \
             {types:?}"
        );
        assert_eq!(
            epoch_millis(case, opened, "timestamp"),
            start,
            "case {case:?}: expected {EVENT_EXECUTION_STARTED} at the execution's own \
             startDate {start}, got {opened}"
        );
        let ended = events
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: expected a terminal event in {events:?}"));
        assert_eq!(
            ended.get("type").and_then(Value::as_str),
            Some(EVENT_EXECUTION_SUCCEEDED),
            "case {case:?}: expected the history to end on {EVENT_EXECUTION_SUCCEEDED}, got \
             {types:?}"
        );
        assert_eq!(
            epoch_millis(case, ended, "timestamp"),
            stop,
            "case {case:?}: expected {EVENT_EXECUTION_SUCCEEDED} at the execution's own \
             stopDate {stop}, got {ended}"
        );
    }
}
