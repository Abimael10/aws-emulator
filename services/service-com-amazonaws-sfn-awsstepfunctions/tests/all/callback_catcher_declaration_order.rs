// SPDX-License-Identifier: CC0-1.0

//! WHICH catcher of a `Task` state takes over the closure of a callback
//! (`.waitForTaskToken`) task's own clock, driven over both services' own wire protocols
//! through the production `MultiServiceRouter` with Step Functions and Amazon SQS mounted
//! on ONE engine — so the integration call that hands the token out is really made, the
//! bound really runs out with nothing answering the token, and the branch the execution
//! leaves the state by is the one the interpreter really chose.
//!
//! RULE: `Catch` is "an array of objects, called catchers", and the interpreter scans it
//! in the order the definition declares the catchers in: the FIRST catcher whose
//! `ErrorEquals` holds ANY name the raised error answers to takes the failure over, never
//! the one that names it most specifically. A `Task` state whose `HeartbeatSeconds` window
//! lapses raises a closure the language gives TWO names — "States.Timeout — a Task state
//! either ran longer than the TimeoutSeconds value, or failed to send a heartbeat for a
//! period longer than the HeartbeatSeconds value" and "States.HeartbeatTimeout — a Task
//! state failed to send a heartbeat for a period longer than the HeartbeatSeconds value" —
//! so a catcher naming either answers for it, and only its POSITION in the array decides
//! which of two catchers that both name it wins. The state's `Retry` is matched the same
//! way, because it is the same question asked of the same entries.
//!
//! The rows span what the rule generalises over: both orders of the same two catchers, the
//! `States.ALL` wildcard where the language allows it ("States.ALL must appear alone and at
//! end of list") before and after a catcher naming the heartbeat, a catcher that names
//! several errors of which only the last matches, the one wildcard the language documents
//! as NOT matching this class ("States.TaskFailed […] acts as a wildcard that matches any
//! known error name except States.Timeout"), and the other bound of the same state — a
//! `TimeoutSeconds` whose closure answers to `States.Timeout` alone, so a
//! `States.HeartbeatTimeout` catcher listed first takes nothing over.
//!
//! The harness is `callback_timeout_error_handling`'s `Emulator` (Step Functions and
//! Amazon SQS over one engine); `tests/all/main.rs` declares every suite of this target,
//! so it is reached through `crate::callback_timeout_error_handling`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    HEARTBEAT_TIMEOUT_ERROR, TIMEOUT_ERROR,
};

use crate::callback_timeout_error_handling::Emulator;

/// The state whose callback task every row waits at.
const WAITING_STATE: &str = "WaitForCallback";

/// The bound each row declares, in seconds: the smallest a definition may state, so a row
/// waits for the condition it is about and no longer.
const BOUND_SECONDS: u64 = 1;

/// The wildcard the language documents as matching "any known error name"
/// (its "Error names" documentation).
const ALL_ERRORS: &str = "States.ALL";

/// The OTHER wildcard it documents: "States.TaskFailed […] When used in a retry or catch,
/// States.TaskFailed acts as a wildcard that matches any known error name except
/// States.Timeout" — so it answers for neither name of a closure of the task's own clock.
const TASK_FAILED_ERROR: &str = "States.TaskFailed";

/// An error name of the invoked service, which no closure of a bound answers to: a catcher
/// naming it beside a name that DOES match must still match, because a catcher's
/// `ErrorEquals` is a list of names and any one of them is enough.
const SERVICE_ERROR: &str = "SQS.AmazonSQSException";

/// Each branch a catcher may carry the execution on at, and the literal its `Pass` state
/// puts out — which is what the execution ends with, so the output names the branch that
/// really ran.
const BRANCHES: &[(&str, &str)] = &[
    ("CaughtByHeartbeatTimeout", "heartbeat-timeout-catcher-won"),
    ("CaughtByTimeout", "timeout-catcher-won"),
    ("CaughtByWildcard", "wildcard-catcher-won"),
    ("CaughtByTaskFailed", "task-failed-catcher-won"),
];

/// The literal the branch `state` puts out ([`BRANCHES`]).
fn literal_of(state: &str) -> &'static str {
    BRANCHES
        .iter()
        .find(|(name, _)| *name == state)
        .map(|(_, literal)| *literal)
        .unwrap_or_else(|| panic!("{state} is one of this suite's branches"))
}

/// One catcher of a row: the names it lists in `ErrorEquals`, and the branch it carries
/// the execution on at.
struct Catcher {
    errors: &'static [&'static str],
    next: &'static str,
}

/// What a row's execution must end as.
enum Ends {
    /// The catcher of this branch took the closure over, so the execution ends SUCCEEDED
    /// with that branch's own literal.
    Caught(&'static str),
    /// No catcher of the state names the closure, so it is the execution's own failure.
    Failed,
}

/// One row: the bound that runs out, the catchers in the order the definition declares
/// them, and the branch the rule says must win.
struct Row {
    case: &'static str,
    /// Whether the bound that closes the task is the HEARTBEAT window (the closure both
    /// names answer to) or the state's whole-duration `TimeoutSeconds` (which answers to
    /// `States.Timeout` alone).
    heartbeat: bool,
    catchers: &'static [Catcher],
    ends: Ends,
}

const ROWS: &[Row] = &[
    // The rule, stated over both orders of the same two catchers: the same closure, the
    // same two names, and only the position in the array differs.
    Row {
        case: "the heartbeat's own name listed first",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
            Catcher {
                errors: &[TIMEOUT_ERROR],
                next: "CaughtByTimeout",
            },
        ],
        ends: Ends::Caught("CaughtByHeartbeatTimeout"),
    },
    Row {
        case: "the broader timeout name listed first, over the identical closure",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[TIMEOUT_ERROR],
                next: "CaughtByTimeout",
            },
            Catcher {
                errors: &[HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
        ],
        ends: Ends::Caught("CaughtByTimeout"),
    },
    // The wildcard is a name like any other, bar where it may be written: "States.ALL must
    // appear alone and at end of list", so a catcher naming it answers only for what no
    // catcher before it does.
    Row {
        case: "the heartbeat's own name ahead of the States.ALL wildcard",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
            Catcher {
                errors: &[ALL_ERRORS],
                next: "CaughtByWildcard",
            },
        ],
        ends: Ends::Caught("CaughtByHeartbeatTimeout"),
    },
    Row {
        case: "the wildcard at the end, with nothing before it naming the closure",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[TASK_FAILED_ERROR],
                next: "CaughtByTaskFailed",
            },
            Catcher {
                errors: &[ALL_ERRORS],
                next: "CaughtByWildcard",
            },
        ],
        ends: Ends::Caught("CaughtByWildcard"),
    },
    Row {
        case: "the heartbeat's own name in the middle, between a wildcard of each kind",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[TASK_FAILED_ERROR],
                next: "CaughtByTaskFailed",
            },
            Catcher {
                errors: &[HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
            Catcher {
                errors: &[ALL_ERRORS],
                next: "CaughtByWildcard",
            },
        ],
        ends: Ends::Caught("CaughtByHeartbeatTimeout"),
    },
    // A catcher's `ErrorEquals` is a LIST: the first catcher that holds any matching name
    // wins, wherever in its own list that name sits.
    Row {
        case: "a first catcher naming another service's error and then the heartbeat's",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[SERVICE_ERROR, HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
            Catcher {
                errors: &[TIMEOUT_ERROR],
                next: "CaughtByTimeout",
            },
        ],
        ends: Ends::Caught("CaughtByHeartbeatTimeout"),
    },
    // The wildcard that excludes this class is skipped however early it is listed, under
    // EITHER name the closure answers to.
    Row {
        case: "States.TaskFailed first, which answers for neither name of the closure",
        heartbeat: true,
        catchers: &[
            Catcher {
                errors: &[TASK_FAILED_ERROR],
                next: "CaughtByTaskFailed",
            },
            Catcher {
                errors: &[HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
        ],
        ends: Ends::Caught("CaughtByHeartbeatTimeout"),
    },
    // And the other bound of the same state: a whole-duration `TimeoutSeconds` that ran
    // out is no heartbeat lapse, so the heartbeat's own name takes nothing over even
    // listed first.
    Row {
        case: "a TimeoutSeconds closure, with the heartbeat's name listed first",
        heartbeat: false,
        catchers: &[
            Catcher {
                errors: &[HEARTBEAT_TIMEOUT_ERROR],
                next: "CaughtByHeartbeatTimeout",
            },
            Catcher {
                errors: &[TIMEOUT_ERROR],
                next: "CaughtByTimeout",
            },
        ],
        ends: Ends::Caught("CaughtByTimeout"),
    },
    Row {
        case: "a TimeoutSeconds closure with only the heartbeat's name declared",
        heartbeat: false,
        catchers: &[Catcher {
            errors: &[HEARTBEAT_TIMEOUT_ERROR],
            next: "CaughtByHeartbeatTimeout",
        }],
        ends: Ends::Failed,
    },
];

/// The `Catch` array a row declares, in the row's own order.
fn catch_array(catchers: &[Catcher]) -> Value {
    Value::Array(
        catchers
            .iter()
            .map(|catcher| json!({ "ErrorEquals": catcher.errors, "Next": catcher.next }))
            .collect(),
    )
}

/// A definition whose one callback `Task` hands its token to `queue_url` and is answered by
/// nobody, under the bound the row declares and with the row's catchers in its `Catch`.
///
/// Only the branches the catchers name are declared: a state no transition reaches is not
/// a definition this service accepts.
fn definition(queue_url: &str, heartbeat: bool, catchers: &[Catcher], retry: Option<Value>) -> Value {
    let bound = match heartbeat {
        true => "HeartbeatSeconds",
        false => "TimeoutSeconds",
    };
    let mut task = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
        "Parameters": {
            "QueueUrl": queue_url,
            "MessageBody": { "token.$": "$$.Task.Token" },
        },
        bound: BOUND_SECONDS,
        "Catch": catch_array(catchers),
        "End": true,
    });
    if let Some(retry) = retry {
        task.as_object_mut()
            .expect("the state is an object")
            .insert("Retry".to_string(), retry);
    }
    let mut states = json!({ WAITING_STATE: task });
    let declared = states.as_object_mut().expect("the states are an object");
    for catcher in catchers {
        // Each branch puts out its own literal, so the execution's output names the branch
        // that really ran.
        declared.insert(
            catcher.next.to_string(),
            json!({ "Type": "Pass", "Result": literal_of(catcher.next), "End": true }),
        );
    }
    json!({ "StartAt": WAITING_STATE, "States": states })
}

/// The document `settled` ended with, parsed.
fn output_of(settled: &Value, case: &str) -> Value {
    let output = settled
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {settled}"));
    serde_json::from_str(output)
        .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"))
}

/// Every row: the FIRST catcher that names the closure of the task's own clock takes it
/// over, by position in the `Catch` array and not by how specifically it names the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_catcher_that_names_the_closure_takes_it_over() {
    let emulator = Emulator::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let queue_url = emulator.queue(&format!("catcher-order-{index}")).await;
        let execution_arn = emulator
            .started(
                &format!("catcher-order-{index}"),
                &definition(&queue_url, row.heartbeat, row.catchers, None),
            )
            .await;
        let settled = emulator.settled(&execution_arn, case).await;
        let status = settled.get("status").and_then(Value::as_str);
        match row.ends {
            Ends::Caught(branch) => {
                assert_eq!(
                    status,
                    Some("SUCCEEDED"),
                    "case {case:?}: expected SUCCEEDED through the {branch} branch, got {settled}"
                );
                let expected = json!(literal_of(branch));
                let output = output_of(&settled, case);
                assert_eq!(
                    output, expected,
                    "case {case:?}: expected the {branch} branch to have run, whose output is \
                     {expected}, got {output}"
                );
            }
            Ends::Failed => {
                assert_eq!(
                    (status, settled.get("error").and_then(Value::as_str)),
                    (Some("FAILED"), Some(TIMEOUT_ERROR)),
                    "case {case:?}: expected FAILED with {TIMEOUT_ERROR} where no catcher names \
                     the closure, got {settled}"
                );
            }
        }
    }
}

/// The state's `Retry` is matched against the same names its `Catch` is: a retrier naming
/// the heartbeat's own name answers for a heartbeat window that lapsed, which makes the
/// state's integration call again — handing out a token of its own — before the `Catch` of
/// the attempt that follows it answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retrier_naming_the_heartbeat_closure_makes_the_call_again() {
    let case = "a retrier naming the heartbeat's own name, and then a catcher";
    let emulator = Emulator::new();
    let queue_url = emulator.queue("catcher-order-retried").await;
    let catchers = &[Catcher {
        errors: &[TIMEOUT_ERROR],
        next: "CaughtByTimeout",
    }];
    // One attempt more than the first, taken as soon as the closure is reported.
    let retry = json!([{
        "ErrorEquals": [HEARTBEAT_TIMEOUT_ERROR],
        "IntervalSeconds": 0,
        "MaxAttempts": 1,
    }]);
    let execution_arn = emulator
        .started(
            "catcher-order-retried",
            &definition(&queue_url, true, catchers, Some(retry)),
        )
        .await;

    let settled = emulator.settled(&execution_arn, case).await;
    assert_eq!(
        settled.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED through the catcher of the retried attempt, got \
         {settled}"
    );
    let expected = json!(literal_of("CaughtByTimeout"));
    let output = output_of(&settled, case);
    assert_eq!(
        output, expected,
        "case {case:?}: expected the catcher's branch to have run, whose output is {expected}, \
         got {output}"
    );
    // Two attempts, each handing a token of its own to the queue: the retrier really made
    // the state's call again rather than the catcher answering the first closure.
    let bodies = emulator.bodies(&queue_url, 2).await;
    assert_eq!(
        bodies.len(),
        2,
        "case {case:?}: expected the retrier's attempt to hand over a second token, got {bodies:?}"
    );
}
