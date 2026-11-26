// SPDX-License-Identifier: CC0-1.0

//! What a report of progress does to the window a `Task` state's `HeartbeatSeconds`
//! opens, and which FAMILY of history events records the closure when a bound the state
//! declared does run out — driven over both services' own wire protocols through the
//! production `MultiServiceRouter` with Step Functions and Lambda mounted on ONE engine,
//! so the worker really runs, really takes the spans it is asked for, and every heartbeat
//! asserted on here is one its own handler made.
//!
//! RULE: `SendTaskHeartbeat` "resets the Heartbeat clock", so `HeartbeatSeconds` bounds
//! the IDLE span between two reports of progress on a task's token rather than the span
//! the work takes ("if more time than the specified seconds elapses between heartbeats
//! from the task, this state fails with a States.Timeout error name"). A worker that
//! reports progress inside each window therefore settles its task however long its work
//! runs — the execution succeeds and no timeout event is recorded for it — and only a
//! window nothing reported progress inside closes the task, with `States.Timeout`.
//!
//! RULE: which family of events records that closure is decided by what the state's
//! `Resource` NAMES and not by what did the work: a `Resource` naming a service
//! integration — every `arn:aws:states:::…` form, the `.waitForTaskToken` callback one
//! included — has one API call as its unit of work and records it under the generic
//! `Task*` events, so its closure is `TaskTimedOut`; a `Resource` that is a Lambda
//! FUNCTION's own ARN has one invocation of that function as its unit of work and records
//! it under the `LambdaFunction*` events, so its closure is `LambdaFunctionTimedOut`. So
//! a callback task whose worker is a Lambda function is still closed under the generic
//! event, because what its `Resource` names is the integration and not the function.
//!
//! The rows differ along what the rules generalise over: how many reports of progress the
//! worker makes (none, one, three), the length of the window they are made inside (two,
//! three and four seconds), how much work the row does after the last of them (less than a
//! window, more than one), WHICH bound closes the task (the idle window, and the state's
//! own whole-duration `TimeoutSeconds`) and the form of the `Resource` the state declares.
//! Every row asserts the status and error this service reports, the hand-over event of the
//! family its `Resource` chose, and the timeout event of BOTH families — so a window that
//! is never reset, a window that is never enforced, and a closure recorded under the wrong
//! family all fail here.
//!
//! A worker reports progress as soon as it HOLDS the token, before doing any of the work
//! the token was handed over for, so every span a row turns on is one the worker's own
//! clock measured from a report it made itself; and the build of the worker's execution
//! environment is paid by the warm-up invocation below, before any row's window is open.
//!
//! The harness is `lambda_task_control_flow`'s `Chain` (Step Functions and Lambda over one
//! engine), with the history readers the sibling suites about the same two families
//! already carry; `tests/all/main.rs` declares every suite of this target, so they are
//! reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    asl::TIMEOUT_ERROR, EVENT_LAMBDA_FUNCTION_SCHEDULED, EVENT_LAMBDA_FUNCTION_TIMED_OUT,
    EVENT_TASK_SCHEDULED, EVENT_TASK_TIMED_OUT,
};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;
use crate::task_work_event_families::history;

/// The optimized Lambda invocation with the callback pattern: the `Task` hands the
/// function the token out of the context object and waits until it is redeemed. A
/// `Resource` of this form names the INTEGRATION, so its work is recorded under the
/// generic `Task*` events.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::lambda:invoke.waitForTaskToken";

/// The statuses an execution of these machines is read in (`ExecutionStatus`).
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";

/// One invocation's own allowance, as the function's `Timeout`: well past the longest a
/// row's worker works, so the bound that closes a row's task is one the STATE declared and
/// never the function's own.
const FUNCTION_TIMEOUT_SECONDS: u64 = 60;

/// What a served heartbeat is recorded as in the document the worker settles with, so a
/// settled execution's own output says how many of them this service answered.
const BEAT: &str = "beat";

/// The function the rows invoke, as `CreateFunction` names it.
const WORKER: &str = "heartbeat-family-worker";

/// The member that asks the worker for NOTHING but to be running: the invocation that
/// builds the execution environment Lambda then keeps standing for the function, so no
/// row's window covers that build.
const WARMING: &str = "warming";

/// The case the warm-up is asserted under, and the document its handler answers.
const WARM_UP_CASE: &str = "the warm-up invocation that builds the worker's environment";

/// The worker every row invokes.
///
/// Handed a token, it reports progress at once where its event asks it to, works the spans
/// its event names, reports progress after each of them, works once more and then settles
/// the task with the heartbeats it got answered. Handed NO token — the row whose
/// `Resource` names the function itself — it only works the span its event names, because
/// there is nothing to report progress on.
///
/// Whatever this service refuses it CATCHES and reports instead of raising, so a row whose
/// task a bound closed still runs its handler to the end.
const WORKER_SOURCE: &str = r#"
import json
import time

import boto3
import botocore.exceptions


def handler(event, context):
    # The warm-up event: this run exists only so that the environment it is served in is
    # standing before any row's window opens.
    if event.get("warming"):
        return {"warmed": True}
    token = event.get("token")
    if token is None:
        # A task whose Resource names the function itself hands over no token, so this run
        # just works for the span it was asked for.
        time.sleep(event["then"])
        return {"worked": True}
    states = boto3.client("stepfunctions")
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

/// What a row's `Resource` names, which is what decides the family its work is recorded
/// under and which bound the row declares.
#[derive(Clone, Copy, Debug)]
enum Work {
    /// The callback integration: the token is handed to the worker, whose reports of
    /// progress move the window `HeartbeatSeconds` opens on.
    Callback { heartbeat_seconds: u64 },
    /// The function's own ARN, with the state's whole-duration bound: no token is handed
    /// over, so nothing reports progress and what closes the task is that bound.
    Function { timeout_seconds: u64 },
}

/// What the rules say one row's execution must end as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// Every idle span stayed inside the window the state declared, so the settlement the
    /// worker sent decides: the execution succeeds carrying the document it named, and
    /// neither family's timeout event is recorded.
    Settled,
    /// A bound the state declared really did run out, so the task is closed by it and the
    /// closure is recorded under the family the `Resource` chose.
    ClosedBy(Family),
}

/// The family of history events a `Task` state's work is recorded under, as the events
/// this suite reads name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    /// One API call of a service integration: `TaskScheduled` … `TaskTimedOut`.
    Integration,
    /// One invocation of the function the `Resource` named: `LambdaFunctionScheduled` …
    /// `LambdaFunctionTimedOut`.
    Invocation,
}

impl Family {
    /// The event this family records the HAND-OVER of the work with.
    fn scheduled(self) -> &'static str {
        match self {
            Self::Integration => EVENT_TASK_SCHEDULED,
            Self::Invocation => EVENT_LAMBDA_FUNCTION_SCHEDULED,
        }
    }

    /// The event this family records a bound running OUT with.
    fn timed_out(self) -> &'static str {
        match self {
            Self::Integration => EVENT_TASK_TIMED_OUT,
            Self::Invocation => EVENT_LAMBDA_FUNCTION_TIMED_OUT,
        }
    }

    /// Both families, so every row asserts the timeout event of the one it expects AND the
    /// absence of the other's.
    const BOTH: [Self; 2] = [Self::Integration, Self::Invocation];
}

/// One row: what the state's `Resource` names and what it bounds, and how its worker
/// spends that bound.
struct Row {
    case: &'static str,
    work: Work,
    /// Whether the worker reports progress AS SOON AS it holds the token, before doing any
    /// of the work — so the window the hand-over opened has to cover nothing but this
    /// emulator's own start-up of the invocation.
    reports_at_once: bool,
    /// The span the worker works before each further report of progress it makes, in
    /// seconds.
    beats: &'static [f64],
    /// And the span it works after the last of them, before settling the task.
    then: f64,
    ends: Ends,
}

impl Row {
    /// The family this row's `Resource` puts its work in.
    fn family(&self) -> Family {
        match self.work {
            Work::Callback { .. } => Family::Integration,
            Work::Function { .. } => Family::Invocation,
        }
    }

    /// How many heartbeats of this row's worker this service must answer: the one it makes
    /// on holding the token, where it makes one, and one after each span in [`Self::beats`].
    fn heartbeats(&self) -> usize {
        usize::from(self.reports_at_once) + self.beats.len()
    }

    /// The one-state definition this row runs.
    fn definition(&self, function_arn: &str) -> Value {
        let task = match self.work {
            Work::Callback { heartbeat_seconds } => json!({
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "FunctionName": function_arn,
                    "Payload": {
                        "token.$": "$$.Task.Token",
                        "reports_at_once": self.reports_at_once,
                        "beats": self.beats,
                        "then": self.then,
                    },
                },
                "HeartbeatSeconds": heartbeat_seconds,
                "End": true,
            }),
            Work::Function { timeout_seconds } => json!({
                "Type": "Task",
                "Resource": function_arn,
                "TimeoutSeconds": timeout_seconds,
                "End": true,
            }),
        };
        json!({ "StartAt": "Work", "States": { "Work": task } })
    }

    /// The execution input: the spans the worker of a row whose `Resource` names the
    /// function itself is asked for, which a callback row's `Parameters` carry instead.
    fn input(&self) -> Value {
        match self.work {
            Work::Callback { .. } => json!({}),
            Work::Function { .. } => json!({ "then": self.then }),
        }
    }
}

const ROWS: &[Row] = &[
    // The case the heartbeat rule is stated about: one further report of progress inside
    // the window, with the work on either side of it adding up to more than one window.
    // Each of those two spans is a good deal shorter than the window it is inside, so a
    // host that runs the worker slowly cannot decide the row.
    Row {
        case: "a callback task whose worker reports progress once inside its window",
        work: Work::Callback {
            heartbeat_seconds: 4,
        },
        reports_at_once: true,
        beats: &[2.0],
        then: 2.0,
        ends: Ends::Settled,
    },
    // The same rule over SEVERAL windows' worth of work, reported progress on three times
    // a window: the member bounds idle time, so it caps the task's duration at nothing.
    Row {
        case: "a callback task reporting progress through several windows' worth of work",
        work: Work::Callback {
            heartbeat_seconds: 3,
        },
        reports_at_once: true,
        beats: &[1.0, 1.0, 1.0],
        then: 1.0,
        ends: Ends::Settled,
    },
    // The other side of it: a window nothing reported progress inside closes the task, and
    // the `Resource` of this task names the INTEGRATION, so the closure is the generic
    // event however much of the work a Lambda function did.
    Row {
        case: "a callback task whose worker never reports progress at all",
        work: Work::Callback {
            heartbeat_seconds: 2,
        },
        reports_at_once: false,
        beats: &[],
        then: 6.0,
        ends: Ends::ClosedBy(Family::Integration),
    },
    // And the other family: the same function, named by a `Resource` that IS its ARN, so
    // the unit of work is the invocation and the state's own whole-duration bound closes it
    // under that family's timeout event.
    Row {
        case: "a Resource naming the function itself, whose run outlasts the state's \
               TimeoutSeconds",
        work: Work::Function {
            timeout_seconds: 3,
        },
        reports_at_once: false,
        beats: &[],
        then: 10.0,
        ends: Ends::ClosedBy(Family::Invocation),
    },
];

/// How often `event_type` appears in `types`.
fn counted(types: &[String], event_type: &str) -> usize {
    types.iter().filter(|kind| *kind == event_type).count()
}

/// Every row: a report of progress inside the window starts it again, so a worker that
/// keeps reporting settles its task however long its work runs, while a bound that does run
/// out closes the task with `States.Timeout` recorded under the family its `Resource` chose.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_task_is_recorded_under_the_family_its_resource_names() {
    let chain = Chain::new();
    let function = chain
        .given_function_of(WORKER, WORKER_SOURCE, Some(FUNCTION_TIMEOUT_SECONDS))
        .await;
    // The build of the worker's execution environment is paid HERE, where no task's window
    // is open, rather than inside the first window of the first row.
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
                &format!("heartbeat-family-{index}"),
                &row.definition(&function),
                &row.input(),
            )
            .await;
        let execution = string(&settled, "executionArn");
        let events = history(&chain, case, &execution, json!({})).await;
        let types = event_types(&events);
        let family = row.family();

        // The work was handed over ONCE, under the family the `Resource` chose, and the
        // other family recorded no hand-over at all.
        assert_eq!(
            counted(&types, family.scheduled()),
            1,
            "case {case:?}: expected one {} for the family this Resource names, got {types:?}",
            family.scheduled()
        );
        for other in Family::BOTH.into_iter().filter(|kind| *kind != family) {
            assert_eq!(
                counted(&types, other.scheduled()),
                0,
                "case {case:?}: expected no {} beside the family this Resource names, got \
                 {types:?}",
                other.scheduled()
            );
        }

        match row.ends {
            Ends::Settled => {
                assert_eq!(
                    settled.get("status").and_then(Value::as_str),
                    Some(SUCCEEDED),
                    "case {case:?}: expected {SUCCEEDED} for a task whose worker reported \
                     progress inside every window, got {settled}"
                );
                // The document the worker settled with says how many of its heartbeats this
                // service answered: one entry per report it made, so a heartbeat that was
                // refused is reported here as the refusal's own code rather than passing as
                // a served one.
                let carried: Value = settled
                    .get("output")
                    .and_then(Value::as_str)
                    .map(|text| {
                        serde_json::from_str(text).unwrap_or_else(|why| {
                            panic!("case {case:?}: the output is JSON: {why}")
                        })
                    })
                    .unwrap_or_else(|| panic!("case {case:?}: expected an output in {settled}"));
                let answered: Vec<String> = (0..row.heartbeats()).map(|_| BEAT.to_string()).collect();
                assert_eq!(
                    carried,
                    json!({ "beats": answered }),
                    "case {case:?}: expected every heartbeat answered and the settlement's own \
                     document, got {carried}"
                );
            }
            Ends::ClosedBy(_) => {
                assert_eq!(
                    (
                        settled.get("status").and_then(Value::as_str),
                        settled.get("error").and_then(Value::as_str),
                    ),
                    (Some(FAILED), Some(TIMEOUT_ERROR)),
                    "case {case:?}: expected {FAILED} under {TIMEOUT_ERROR} once the bound the \
                     state declared ran out, got {settled}"
                );
            }
        }

        // The closure itself: the timeout event of the family the `Resource` named, and
        // never the other family's — and neither of them for a task no bound closed.
        for kind in Family::BOTH {
            let expected = usize::from(row.ends == Ends::ClosedBy(kind));
            assert_eq!(
                counted(&types, kind.timed_out()),
                expected,
                "case {case:?}: expected {expected} {} event(s), got {types:?}",
                kind.timed_out()
            );
        }
    }
}
