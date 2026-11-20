// SPDX-License-Identifier: CC0-1.0

//! What `HeartbeatSeconds` really bounds on a callback (`.waitForTaskToken`) task, driven
//! over both services' own wire protocols through the production `MultiServiceRouter` with
//! Step Functions and Lambda mounted on ONE engine — so the worker really runs, really
//! takes the spans it is asked for, and the `SendTaskHeartbeat` this suite asserts on is
//! the one its own handler made.
//!
//! RULE: "this action resets the Heartbeat clock" (`SendTaskHeartbeat`), and
//! "HeartbeatSeconds […] if more time than the specified seconds elapses between
//! heartbeats from the task, this state fails with a States.Timeout error name". The
//! member therefore bounds the IDLE span between two reports of progress and not the span
//! the work takes: every heartbeat on a task's token starts the window again, so a worker
//! that keeps reporting progress inside it settles its task however long its work runs,
//! and only a window nothing reported progress inside closes the task — with
//! `States.Timeout` and the work family's own timeout event. What bounds a task's WHOLE
//! duration is the other member: "the Timeout of a task […] is its maximum allowed
//! duration, regardless of the number of SendTaskHeartbeat requests received"
//! (`TimeoutSeconds`), which no number of heartbeats lifts.
//!
//! The heartbeats of a callback task come from whoever the token went to, and for the
//! Lambda callback pattern that is the very invocation the hand-over made — so they arrive
//! while this service is still waiting for that invocation to answer, which is the window
//! this suite is about.
//!
//! A worker reports progress as soon as it HOLDS the token, before it does any of the work
//! the token was handed over for: the window the hand-over opens therefore covers only the
//! span this emulator itself takes to get the worker running, and every span the rule turns
//! on is one the worker's own clock measures from a report of progress it made itself. And
//! that start-up span is paid BEFORE the first row, by the warm-up invocation below: Lambda
//! builds an execution environment for the function once and keeps it standing, so each
//! row's own invocation is the handler's run alone. So no row's outcome is decided by how
//! fast — or how loaded — the host is.
//!
//! The rows differ along what the rule generalises over: HOW MANY heartbeats the worker
//! sends (none, one report of progress, one and then more), the length of the window they
//! are sent inside (two, six, eight and ten seconds), how much work the row does after the
//! last of them (less than a window, more than one), and whether the state also declares the
//! `TimeoutSeconds` that bounds the task's whole duration. Every row asserts both halves —
//! the status and error this service reports, and whether the history carries the timeout
//! event — so a window that is never reset and a window that is never enforced both fail
//! here.
//!
//! The harness is `lambda_task_control_flow`'s `Chain` (Step Functions and Lambda over one
//! engine); `tests/all/main.rs` declares every suite of this target, so it is reached
//! through `crate::lambda_task_control_flow`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::EVENT_TASK_TIMED_OUT;

use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The optimized Lambda invocation with the callback pattern: the `Task` hands the
/// function the token out of the context object and waits until it is redeemed.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::lambda:invoke.waitForTaskToken";

/// The ASL predefined error name a bound a `Task` state declared reports when it runs out,
/// which the language states for both of them ("States.Timeout — a Task state either ran
/// longer than the TimeoutSeconds value, or failed to send a heartbeat for a period longer
/// than the HeartbeatSeconds value").
const TIMEOUT_ERROR: &str = "States.Timeout";

/// The statuses an execution of these machines is read in (`ExecutionStatus`).
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";

/// One invocation's own allowance, as the function's `Timeout`: well past the longest a
/// row's worker works, because what is under test is which bound closes the task and not
/// how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT_SECONDS: u64 = 60;

/// What a served heartbeat is recorded as in the document the worker settles with, so the
/// execution's own output says how many of them this service answered.
const BEAT: &str = "beat";

/// The function the rows invoke, as `CreateFunction` names it — the name the warm-up
/// invocation addresses as well.
const WORKER: &str = "heartbeat-window-worker";

/// The member that asks the worker for NOTHING but to be running: no task of any row
/// exists when it is invoked with it, so the handler reports no progress and settles
/// nothing, and what the invocation leaves behind is the execution environment Lambda
/// keeps standing for the function ("Lambda maintains the execution environment for some
/// time in anticipation of another invocation", and an invocation that finds one "skips
/// the initialization phase").
///
/// Every row below is therefore served by a worker this emulator is ALREADY able to start,
/// which is what keeps the span between a hand-over and the first report of progress made
/// on its token a span of the handler's own run rather than of a package being expanded and
/// an interpreter being built.
const WARMING: &str = "warming";

/// The case the warm-up is asserted under, and the document its handler answers: the
/// warm-up is a run of the SAME handler, so an answer that is not this is an environment
/// that was never built and a suite that would be measuring the build inside its rows'
/// windows.
const WARM_UP_CASE: &str = "the warm-up invocation that builds the worker's environment";

/// The worker every row invokes: it is handed a task token, reports progress on it at once
/// where its event asks it to, works the spans its event names, reports progress after each
/// of them, works once more and then settles the task with the heartbeats it got answered.
///
/// The report it makes BEFORE working is what a worker does first — it holds the token, so
/// it is alive — and it is what leaves every later span of the row measured from an instant
/// this handler's own clock saw.
///
/// Whatever this service refuses it CATCHES and reports instead of raising, so a row whose
/// task a bound closed still runs its handler to the end rather than failing the
/// invocation.
const WORKER_SOURCE: &str = r#"
import json
import time

import boto3
import botocore.exceptions


def handler(event, context):
    states = boto3.client("stepfunctions")
    # The warm-up event, which carries no token: this run exists only so that the
    # environment it is served in is standing before any row's window opens.
    if event.get("warming"):
        return {"warmed": True}
    token = event["token"]
    reported = []

    def report():
        try:
            states.send_task_heartbeat(taskToken=token)
            reported.append("beat")
        except botocore.exceptions.ClientError as refused:
            reported.append(refused.response["Error"]["Code"])

    if event["reports_at_once"]:
        report()
    for pause in event["beats"]:
        time.sleep(pause)
        report()
    time.sleep(event["then"])
    try:
        states.send_task_success(
            taskToken=token,
            output=json.dumps({"beats": reported}),
        )
        return {"settled": True, "beats": reported}
    except botocore.exceptions.ClientError as refused:
        return {
            "settled": False,
            "beats": reported,
            "code": refused.response["Error"]["Code"],
        }
"#;

/// What the rule says one row's execution must end as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// Every idle span stayed inside the window the state declared, and the task's whole
    /// duration inside whatever bounded it: the settlement the worker sent decides, so the
    /// execution succeeds carrying the document that settlement named.
    Settled,
    /// A bound the state declared really did run out, so the task is closed by it.
    TimedOut,
}

/// One row: the bounds a callback `Task` declares, and how its worker spends them.
struct Row {
    case: &'static str,
    /// `HeartbeatSeconds`: the window one report of progress opens.
    heartbeat_seconds: u64,
    /// `TimeoutSeconds` where the row declares one, which bounds the task's whole duration
    /// however often it is heartbeated.
    timeout_seconds: Option<u64>,
    /// Whether the worker reports progress AS SOON AS it holds the token, before doing any
    /// of the work — so the window the hand-over opened has to cover nothing but this
    /// emulator's own start-up of the invocation, and every span below is measured from a
    /// report the worker made itself.
    reports_at_once: bool,
    /// The span the worker works before each further heartbeat it sends, in seconds.
    beats: &'static [f64],
    /// And the span it works after the last of them, before settling the task.
    then: f64,
    ends: Ends,
}

impl Row {
    /// How many heartbeats of this row's worker this service must answer: the one it makes
    /// on holding the token, where it makes one, and one after each span in [`Self::beats`].
    fn heartbeats(&self) -> usize {
        usize::from(self.reports_at_once) + self.beats.len()
    }
}

const ROWS: &[Row] = &[
    // The case the rule is stated about: one further report of progress inside the window,
    // with the work on either side of it adding up to more than one window. Each of those
    // two spans is a good deal shorter than the window it is inside, so a host that runs
    // the worker slowly cannot decide the row.
    Row {
        case: "one heartbeat inside the window, with the work around it outlasting it",
        heartbeat_seconds: 8,
        timeout_seconds: None,
        reports_at_once: true,
        beats: &[5.0],
        then: 4.0,
        ends: Ends::Settled,
    },
    // Two windows' worth of work, kept alive by heartbeats three times a window: the member
    // bounds idle time, so it caps the task's duration at nothing.
    Row {
        case: "heartbeats kept up over several windows' worth of work",
        heartbeat_seconds: 6,
        timeout_seconds: None,
        reports_at_once: true,
        beats: &[2.0, 2.0, 2.0, 2.0, 2.0],
        then: 2.0,
        ends: Ends::Settled,
    },
    // The other side of the same rule: a window nothing reported progress inside closes
    // the task, which is what the member is for.
    Row {
        case: "a worker that never heartbeats at all",
        heartbeat_seconds: 2,
        timeout_seconds: None,
        reports_at_once: false,
        beats: &[],
        then: 5.0,
        ends: Ends::TimedOut,
    },
    // A heartbeat starts the window again and no more than that: a worker that stops
    // reporting progress is closed one window after its last heartbeat, however long the
    // work it is still doing runs. The window is well past this emulator's own start-up of
    // the worker, and that this service really ANSWERS a report made on holding the token is
    // what the settled rows above assert.
    Row {
        case: "a worker that heartbeats once and then goes quiet for longer than the window",
        heartbeat_seconds: 6,
        timeout_seconds: None,
        reports_at_once: true,
        beats: &[],
        then: 10.0,
        ends: Ends::TimedOut,
    },
    // And the member that DOES bound the whole duration: heartbeats inside the window keep
    // the heartbeat clock open, and the task's own `TimeoutSeconds` closes it anyway. The
    // window is far longer than that bound here, so the bound that fires can only be the
    // whole-duration one.
    Row {
        case: "heartbeats inside the window past the task's own TimeoutSeconds",
        heartbeat_seconds: 10,
        timeout_seconds: Some(3),
        reports_at_once: true,
        beats: &[1.0, 1.0, 1.0, 1.0, 1.0],
        then: 0.0,
        ends: Ends::TimedOut,
    },
];

/// The one-state definition a row runs: a callback `Task` that hands the worker the token
/// out of the context object together with the spans that worker works, under the bounds
/// the row declares.
fn definition(row: &Row, function_arn: &str) -> Value {
    let mut task = json!({
        "Type": "Task",
        "Resource": CALLBACK_RESOURCE,
        "Parameters": {
            "FunctionName": function_arn,
            "Payload": {
                "token.$": "$$.Task.Token",
                "reports_at_once": row.reports_at_once,
                "beats": row.beats,
                "then": row.then,
            },
        },
        "HeartbeatSeconds": row.heartbeat_seconds,
        "End": true,
    });
    if let Some(seconds) = row.timeout_seconds {
        task["TimeoutSeconds"] = json!(seconds);
    }
    json!({ "StartAt": "AwaitWorker", "States": { "AwaitWorker": task } })
}

/// The types of the events `execution`'s history carries, in order.
async fn history_types(chain: &Chain, case: &str, execution: &str) -> Vec<String> {
    let history = chain
        .sfn(
            case,
            "GetExecutionHistory",
            &json!({ "executionArn": execution }),
        )
        .await;
    history
        .get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected an events list in {history}"))
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Every row: a heartbeat inside the window starts it again, so a worker that reports
/// progress settles its task however long its work runs, while a window nothing reported
/// progress inside — and the task's own whole-duration bound — close it with
/// `States.Timeout`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heartbeat_inside_the_window_keeps_a_callback_task_alive() {
    let chain = Chain::new();
    let function = chain
        .given_function_of(WORKER, WORKER_SOURCE, Some(FUNCTION_TIMEOUT_SECONDS))
        .await;
    // The build of the worker's execution environment is paid HERE, where no task's window
    // is open, rather than inside the first window of the first row ([`WARMING`]).
    let warmed = chain
        .invoked(WARM_UP_CASE, WORKER, &json!({ WARMING: true }))
        .await;
    assert_eq!(
        warmed,
        json!({ "warmed": true }),
        "case {WARM_UP_CASE:?}: the warm-up must be the worker's own run, so that every row \
         below is served by an environment this emulator already has standing, got {warmed}"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let settled = chain
            .ran(
                case,
                &format!("heartbeat-window-{index}"),
                &definition(row, &function),
                &json!({}),
            )
            .await;
        let execution = string(&settled, "executionArn");
        let types = history_types(&chain, case, &execution).await;
        let timed_out = types.iter().any(|event| event == EVENT_TASK_TIMED_OUT);
        match row.ends {
            Ends::Settled => {
                assert_eq!(
                    settled.get("status").and_then(Value::as_str),
                    Some(SUCCEEDED),
                    "case {case:?}: expected {SUCCEEDED} for a task heartbeated inside its \
                     {}s window, got {settled}",
                    row.heartbeat_seconds
                );
                // The document the worker settled with says how many of its heartbeats this
                // service answered: one entry per heartbeat the row sends, so a heartbeat
                // that was refused is reported here as the refusal's own code rather than
                // passing as a served one.
                let carried: Value = settled
                    .get("output")
                    .and_then(Value::as_str)
                    .map(|text| {
                        serde_json::from_str(text)
                            .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"))
                    })
                    .unwrap_or_else(|| panic!("case {case:?}: expected an output in {settled}"));
                let answered: Vec<String> =
                    (0..row.heartbeats()).map(|_| BEAT.to_string()).collect();
                assert_eq!(
                    carried,
                    json!({ "beats": answered }),
                    "case {case:?}: expected every heartbeat answered and the settlement's own \
                     document, got {carried}"
                );
                assert!(
                    !timed_out,
                    "case {case:?}: no {EVENT_TASK_TIMED_OUT} may be recorded for a task whose \
                     window was reset in time, got {types:?}"
                );
            }
            Ends::TimedOut => {
                assert_eq!(
                    settled.get("status").and_then(Value::as_str),
                    Some(FAILED),
                    "case {case:?}: expected {FAILED} once a bound the state declared ran out, \
                     got {settled}"
                );
                assert_eq!(
                    settled.get("error").and_then(Value::as_str),
                    Some(TIMEOUT_ERROR),
                    "case {case:?}: expected {TIMEOUT_ERROR}, got {settled}"
                );
                assert!(
                    timed_out,
                    "case {case:?}: expected a {EVENT_TASK_TIMED_OUT} event for the closed task, \
                     got {types:?}"
                );
            }
        }
    }
}
