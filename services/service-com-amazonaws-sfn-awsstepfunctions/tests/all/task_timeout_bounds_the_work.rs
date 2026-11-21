// SPDX-License-Identifier: CC0-1.0

//! What a `Task` state's own `TimeoutSeconds` DOES to work that takes a while, driven over
//! both services' own wire protocols through the production `MultiServiceRouter` with Step
//! Functions and Lambda mounted on ONE engine — so the function really runs, really takes
//! the seconds the row asks of it, and the task's outcome is that run's own.
//!
//! RULE: "TimeoutSeconds […] If the task runs longer than the specified seconds, this state
//! fails with a States.Timeout error name" — so the figure bounds the task's work and
//! NOTHING shorter: work that finishes inside the span the state declared succeeds carrying
//! its own result, work still going at that span fails the state with the language's own
//! timeout error, and a state that declares no bound at all is held to neither.
//!
//! The rows differ along what the rule generalises over: the RELATION between the span the
//! work takes and the span the state declared (far inside it, past it, and no declared span
//! at all) and the magnitude of each — because a bound honoured as anything other than the
//! figure the state wrote makes a generous bound indistinguishable from an unconditional
//! failure.
//!
//! The harness (`Chain`, `given_function_of`, `invoked`, `ran`) is
//! `tests/all/lambda_task_control_flow.rs`'s, which mounts the same two services, and
//! `event_types` is `tests/all/lambda_function_timeout.rs`'s; `tests/all/main.rs` is this
//! binary's crate root, so each is reached through its module.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::TIMEOUT_ERROR;
use service_com_amazonaws_sfn_awsstepfunctions::state::{EVENT_TASK_SCHEDULED, EVENT_TASK_TIMED_OUT};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The `Resource` of the optimized Lambda integration, whose request is the `Parameters`
/// the state builds and whose result is the invocation's own SDK response.
const OPTIMIZED_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The member the optimized integration's result carries the function's own returned
/// document under (`Invoke`'s `Payload`).
const PAYLOAD_MEMBER: &str = "Payload";

/// The handler every row invokes: it sleeps for as many whole seconds as the EVENT asks of
/// it and reports that figure back, so the span the task waited is the span the row chose
/// and the result says the run really happened.
const SLEEPING_HANDLER: &str = r#"
import time


def handler(event, context):
    seconds = int(event.get("sleep_seconds", 0))
    time.sleep(seconds)
    return {"slept_seconds": seconds}
"#;

/// The function's OWN `Timeout`, in seconds: far past the longest sleep any row asks for,
/// so what decides a row is the span the STATE declared and never the function's bound.
const FUNCTION_TIMEOUT_SECONDS: u64 = 120;

/// The bound the rows that succeed declare, in seconds: an order of magnitude past the
/// seconds their work takes, so no row is decided by how long an interpreter takes to start
/// on a loaded host.
const GENEROUS_BOUND_SECONDS: u64 = 30;

/// One row: how long the work takes, what the state declares about bounding it, and whether
/// the work therefore finished inside that bound.
struct Row {
    case: &'static str,
    /// The whole seconds the invoked function runs for.
    sleeps: u64,
    /// The `TimeoutSeconds` the state declares, and `None` for a state declaring none.
    declares: Option<u64>,
    /// Whether the work finished inside the span the state declared.
    finishes: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "work of several seconds far inside a generous bound",
        sleeps: 3,
        declares: Some(GENEROUS_BOUND_SECONDS),
        finishes: true,
    },
    // The same work with no bound declared at all: nothing of the state's cuts it short
    // either.
    Row {
        case: "the same work under no declared bound at all",
        sleeps: 3,
        declares: None,
        finishes: true,
    },
    // The other side of the rule: the same work under a bound SHORTER than it, which is the
    // one case the language fails the state for.
    Row {
        case: "work still going at a bound shorter than it",
        sleeps: 3,
        declares: Some(1),
        finishes: false,
    },
    // And brief work under the generous bound, so the bound is not read as a span the task
    // waits out.
    Row {
        case: "work of one second under the same generous bound",
        sleeps: 1,
        declares: Some(GENEROUS_BOUND_SECONDS),
        finishes: true,
    },
];

/// The definition one row runs: one `Task` state invoking the function with the row's own
/// sleep, under the row's own bound.
fn definition(function_arn: &str, sleeps: u64, declares: Option<u64>) -> Value {
    let mut task = json!({
        "Type": "Task",
        "Resource": OPTIMIZED_RESOURCE,
        "Parameters": {
            "FunctionName": function_arn,
            "Payload": { "sleep_seconds": sleeps },
        },
        "End": true,
    });
    if let Some(seconds) = declares {
        task["TimeoutSeconds"] = json!(seconds);
    }
    json!({ "StartAt": "Invoke", "States": { "Invoke": task } })
}

/// Every row: a `Task` state's `TimeoutSeconds` bounds the work's own duration, so work
/// that finishes inside it succeeds and only work still going at it is the state's timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tasks_timeout_seconds_bounds_the_work_and_nothing_shorter() {
    let chain = Chain::new();
    let function = chain
        .given_function_of(
            "sleeper",
            SLEEPING_HANDLER,
            Some(FUNCTION_TIMEOUT_SECONDS),
        )
        .await;
    // One invocation BEFORE any row, so the execution environment this function's runs need
    // is standing: a build paid inside a row's own bound would decide that row by how loaded
    // the host is rather than by the rule.
    chain
        .invoked("warming the function", "sleeper", &json!({ "sleep_seconds": 0 }))
        .await;

    for (index, row) in ROWS.iter().enumerate() {
        let Row {
            case,
            sleeps,
            declares,
            finishes,
        } = *row;
        let described = chain
            .ran(
                case,
                &format!("bounded-work-{index}"),
                &definition(&function, sleeps, declares),
                &json!({}),
            )
            .await;
        let answered = chain
            .sfn(
                case,
                "GetExecutionHistory",
                &json!({ "executionArn": string(&described, "executionArn") }),
            )
            .await;
        let types = event_types(
            &answered
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        // The work was handed over exactly once either way: what a row differs in is the
        // bound, and no bound asks for a second attempt.
        assert_eq!(
            types.iter().filter(|kind| *kind == EVENT_TASK_SCHEDULED).count(),
            1,
            "case {case:?}: expected the work handed over once, got {types:?}"
        );

        if finishes {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected work finishing inside its bound to succeed, got \
                 {described}"
            );
            // The run's own result, which says the function really slept the row's seconds
            // rather than the task reporting something nothing ran for.
            let output: Value = described
                .get("output")
                .and_then(Value::as_str)
                .map(|text| serde_json::from_str(text).expect("the output is JSON"))
                .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described}"));
            assert_eq!(
                output.get(PAYLOAD_MEMBER),
                Some(&json!({ "slept_seconds": sleeps })),
                "case {case:?}: expected the run's own answer under {PAYLOAD_MEMBER}, got {output}"
            );
            assert!(
                !types.iter().any(|kind| kind == EVENT_TASK_TIMED_OUT),
                "case {case:?}: expected no timeout of the state, got {types:?}"
            );
        } else {
            assert_eq!(
                (
                    described.get("status").and_then(Value::as_str),
                    described.get("error").and_then(Value::as_str),
                ),
                (Some("FAILED"), Some(TIMEOUT_ERROR)),
                "case {case:?}: expected work still going at its bound to fail with \
                 {TIMEOUT_ERROR}, got {described}"
            );
            assert!(
                types.iter().any(|kind| kind == EVENT_TASK_TIMED_OUT),
                "case {case:?}: expected the state's own timeout among its events, got {types:?}"
            );
        }
    }
}
