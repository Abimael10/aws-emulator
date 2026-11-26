// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the document a JSONata `Task` state hands its work, driven over
//! this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: `Arguments` is the REQUEST the task is started with, so a
//! whole-field expression that evaluated to a value that is no document names no request
//! to make — the state fails with `States.Runtime` and the cause is the service's own
//! sentence about a request a task could not be started with: the value as the compact
//! JSON of what the expression produced, and then, in brackets, the member's own rule — a
//! STRING is a type the field cannot carry at all, and every other value is told the
//! field must be an object.
//!
//! Three things follow from the failure being the STATE's own refusal rather than the
//! expression misevaluating: nothing is handed over, the history carries the state's
//! entered event and the execution's failure with no evaluation-failure event between
//! them, and a `Catch` on `States.ALL` does not intercept it — `States.Runtime` is
//! neither retried nor caught.
//!
//! The rows differ along what the rule generalises over: the JSON type the expression
//! produced (string, empty string, number, boolean, array, array of objects), how the
//! expression produced it (reading the input, computing it) and the name of the state
//! that asked. The control is the other side of the same rule: an expression that
//! produced an OBJECT really hands that document to the work, which a worker claiming
//! the task reads back.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds, the activity helpers are
//! `activity_tasks`' and the history reader is `execution_history`'s;
//! `tests/all/main.rs` picks this file up, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;

use crate::activity_tasks::{claimed, create_activity};
use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// How long a row waits for an execution to reach a status the clock owes: generous,
/// because what it waits for is the emulator's own settle delay and not the host's speed.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// Poll `execution` until it reports a status other than `RUNNING`, and answer that
/// record. Panics naming `case` when [`SETTLE_DEADLINE`] passes first.
async fn settled(emulator: &Emulator, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = describe_execution(emulator, execution).await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to reach a terminal status, got {described:?}"
        );
        ::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A JSONata definition whose one `Task` state, named `state`, schedules the activity
/// `resource` and hands it the document `arguments` evaluates to; `catching` adds a
/// `States.ALL` catcher to it.
fn definition(state: &str, resource: &str, arguments: &str, catching: bool) -> String {
    let mut task = json!({
        "Type": "Task",
        "Resource": resource,
        "Arguments": arguments,
        "End": true,
    });
    let mut states = json!({});
    if catching {
        task["Catch"] = json!([{ "ErrorEquals": ["States.ALL"], "Next": "Caught" }]);
        states["Caught"] = json!({ "Type": "Pass", "Output": "caught", "End": true });
    }
    states[state] = task;
    json!({ "QueryLanguage": "JSONata", "StartAt": state, "States": states }).to_string()
}

/// The whole cause an execution whose `Arguments` produced `rendered` must carry, for the
/// state `state` entered at the second event of its history.
fn cause_for(state: &str, rendered: &str, reason: &str) -> String {
    format!(
        "An error occurred while executing the state '{state}' (entered at the event id #2). \
         The Parameters '{rendered}' could not be used to start the Task: [{reason}]"
    )
}

/// What the bracketed reason says about a STRING, and about every other value.
const CANNOT_BE_A_STRING: &str = "The value for the 'Arguments' field cannot be a string";
const MUST_BE_AN_OBJECT: &str = "The value for the 'Arguments' field must be an object";

/// One row: the case, the state's name, the `Arguments` expression, the input the run
/// carries, and the cause the execution must report.
struct Row {
    case: &'static str,
    state: &'static str,
    arguments: &'static str,
    input: Value,
    cause: String,
}

/// An `Arguments` expression that produced a value that is no document fails the
/// execution, and the cause names the request and the member's own rule.
#[tokio::test]
async fn arguments_that_produced_no_document_fail_the_state() {
    let rows: Vec<Row> = vec![
        Row {
            case: "a string read out of the input",
            state: "T",
            arguments: "{% $states.input.v %}",
            input: json!({ "v": "hello" }),
            cause: cause_for("T", "\"hello\"", CANNOT_BE_A_STRING),
        },
        Row {
            case: "the empty string, which is a string all the same",
            state: "T",
            arguments: "{% $states.input.v %}",
            input: json!({ "v": "" }),
            cause: cause_for("T", "\"\"", CANNOT_BE_A_STRING),
        },
        Row {
            case: "a string the expression COMPUTED rather than read",
            state: "HandOver",
            arguments: "{% $string($states.input.v) %}",
            input: json!({ "v": 7 }),
            cause: cause_for("HandOver", "\"7\"", CANNOT_BE_A_STRING),
        },
        Row {
            case: "an array of numbers",
            state: "T",
            arguments: "{% $states.input.v %}",
            input: json!({ "v": [1, 2, 3] }),
            cause: cause_for("T", "[1,2,3]", MUST_BE_AN_OBJECT),
        },
        Row {
            case: "an array of objects, which is still no object",
            state: "T",
            arguments: "{% [$states.input] %}",
            input: json!({ "v": 1 }),
            cause: cause_for("T", "[{\"v\":1}]", MUST_BE_AN_OBJECT),
        },
        Row {
            case: "a number",
            state: "T",
            arguments: "{% $states.input.v %}",
            input: json!({ "v": 42 }),
            cause: cause_for("T", "42", MUST_BE_AN_OBJECT),
        },
        Row {
            case: "a boolean",
            state: "Decide",
            arguments: "{% $states.input.v %}",
            input: json!({ "v": true }),
            cause: cause_for("Decide", "true", MUST_BE_AN_OBJECT),
        },
    ];

    let emulator = Emulator::new();
    let activity = create_activity(&emulator, "arguments-worker").await;
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("arguments-{index}"),
            &definition(row.state, &activity, row.arguments, false),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", &row.input.to_string()).await);
    }
    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described:?}"
        );
        let cause = string(&described, "cause");
        assert_eq!(
            cause, row.cause,
            "case {case:?}: expected the cause {:?}, got {cause:?}",
            row.cause
        );
    }
}

/// The failure is the STATE's own: its history is the state entered and the execution
/// failed, with no evaluation-failure event, and a `States.ALL` catcher does not take it.
#[tokio::test]
async fn the_refusal_is_neither_an_evaluation_failure_nor_caught() {
    let emulator = Emulator::new();
    let activity = create_activity(&emulator, "arguments-history-worker").await;
    // (case, the Arguments expression, the input)
    let rows: [(&str, &str, Value); 2] = [
        ("a string", "{% $states.input.v %}", json!({ "v": "hello" })),
        ("an array", "{% $states.input.v %}", json!({ "v": [1, 2, 3] })),
    ];

    for (index, (case, arguments, input)) in rows.iter().enumerate() {
        // Uncaught: the whole history is the three events.
        let machine = create_machine(
            &emulator,
            &format!("arguments-history-{index}"),
            &definition("T", &activity, arguments, false),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", &input.to_string()).await;
        settled(&emulator, case, &execution).await;
        let types: Vec<String> = history(&emulator, &execution, json!({}))
            .await
            .iter()
            .map(|event| string(event, "type"))
            .collect();
        assert_eq!(
            types,
            vec![
                "ExecutionStarted".to_string(),
                "TaskStateEntered".to_string(),
                "ExecutionFailed".to_string(),
            ],
            "case {case:?}: expected the state entered and the execution failed, got {types:?}"
        );

        // Caught: the same definition carrying a `States.ALL` catcher fails the same way,
        // because the name this failure carries is neither retried nor caught.
        let catching = create_machine(
            &emulator,
            &format!("arguments-caught-{index}"),
            &definition("T", &activity, arguments, true),
        )
        .await;
        let caught = start_execution(&emulator, &catching, "only", &input.to_string()).await;
        let described = settled(&emulator, case, &caught).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected a States.ALL catcher not to intercept it, got {described:?}"
        );
    }
}

/// The control: an `Arguments` expression that produced an OBJECT is the document the
/// work is handed, which the worker that claims the task reads back — so the refusal is
/// about the VALUE the expression produced and not about the member carrying one
/// expression.
#[tokio::test]
async fn arguments_that_produced_a_document_are_handed_to_the_work() {
    let emulator = Emulator::new();
    // (case, the Arguments expression, the input, the document the work must be handed)
    let rows: [(&str, &str, Value, Value); 3] = [
        (
            "the state's own input",
            "{% $states.input %}",
            json!({ "v": 1 }),
            json!({ "v": 1 }),
        ),
        (
            "an object the expression builds",
            "{% { \"n\": $states.input.v * 2 } %}",
            json!({ "v": 21 }),
            json!({ "n": 42 }),
        ),
        (
            "an object read out of the input",
            "{% $states.input.inner %}",
            json!({ "inner": { "deep": true } }),
            json!({ "deep": true }),
        ),
    ];

    for (index, (case, arguments, input, handed)) in rows.iter().enumerate() {
        let activity = create_activity(&emulator, &format!("arguments-ok-worker-{index}")).await;
        let machine = create_machine(
            &emulator,
            &format!("arguments-ok-{index}"),
            &definition("T", &activity, arguments, false),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", &input.to_string()).await;
        let (_, claimed_input) = claimed(&emulator, case, &activity, None).await;
        assert_eq!(
            &claimed_input, handed,
            "case {case:?}: expected the work to be handed {handed:?}, got {claimed_input:?}"
        );
        let described = describe_execution(&emulator, &execution).await;
        assert_eq!(
            string(&described, "status"),
            "RUNNING",
            "case {case:?}: expected the execution to be waiting at its task, got {described:?}"
        );
    }
}
