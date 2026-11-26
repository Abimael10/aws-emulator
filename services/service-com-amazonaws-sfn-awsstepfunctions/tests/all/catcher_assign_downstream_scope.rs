// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH reads of a variable the create-time analysis refuses,
//! driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: a read of a variable is a defect of the SUBMITTED DOCUMENT only
//! when no run that reaches the field could have assigned it — there is then nothing for
//! the language to resolve on any run, whatever data the run carries. A variable SOME run
//! reaching the field has assigned and another has not is a read that resolves on some
//! runs and not on others: the definition is created, the diagnostic is the warning that
//! says so, and the runs that did assign it read it.
//!
//! A `Catch` catcher's `Assign` is exactly that case — it runs only where the state's own
//! work failed and that catcher took over — so the whole documented pattern of capturing a
//! caught error into a variable and reading it downstream is created and runs.
//!
//! The rows differ along what the rule generalises over: WHERE the assignment sits (a
//! catcher of a `Parallel`, a `Task` and a `Map`, a `Choice` rule, a state's own
//! `Assign`), whether the reading state is reached by a path that does not assign it, and
//! which field reads it (a payload template's reference field, a `*Path` member, a
//! `Choice` rule's `Variable`).
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`, `error_code`) is the one `state_machine_lifecycle` builds and the validation
//! readers are `definition_validation`'s; `tests/all/main.rs` picks this file up, so each
//! is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    POSSIBLY_UNDEFINED_VARIABLE, UNDEFINED_VARIABLE,
};
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    VALIDATION_RESULT_FAIL, VALIDATION_RESULT_OK,
};

use crate::definition_validation::{members, result_of, validate};
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, error_code, start_execution, string, Emulator, ROLE_ARN,
};

/// How long a row waits for an execution to reach a terminal status: generous, because
/// what it waits for is the emulator's own settle delay and not the host's speed.
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

/// What `CreateStateMachine` answers for `definition`: the status, the error code and the
/// message.
async fn created(emulator: &Emulator, name: &str, definition: &Value) -> (u16, String, String) {
    let (status, answered) = emulator
        .call(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": definition.to_string(),
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    let message = match status {
        200 => String::new(),
        _ => string(&answered, "message"),
    };
    (status, error_code(&answered), message)
}

/// A `Parallel` state whose one branch fails, carrying `members` beside its branches.
fn failing_parallel(members: Value) -> Value {
    let mut state = json!({
        "Type": "Parallel",
        "Branches": [{
            "StartAt": "Boom",
            "States": { "Boom": { "Type": "Fail", "Error": "Boom", "Cause": "the branch failed" } },
        }],
    });
    for (member, value) in members.as_object().expect("the extra members are an object") {
        state[member] = value.clone();
    }
    state
}

/// A definition whose state reads `$errVar` on a path that assigns it and on one that
/// does not: the `Catch` catcher's own `Assign` writes it, and the state's success path
/// reaches the same reader.
fn joined_after_a_catcher(reader: Value) -> Value {
    json!({
        "StartAt": "Attempt",
        "States": {
            "Attempt": failing_parallel(json!({
                "Catch": [{
                    "ErrorEquals": ["States.ALL"],
                    "Assign": { "errVar.$": "$.Error" },
                    "Next": "Emit",
                }],
                "Next": "Emit",
            })),
            "Emit": reader,
        },
    })
}

/// A definition whose reader is reached ONLY through the catcher that assigns `$errVar`.
fn only_after_a_catcher(reader: Value) -> Value {
    json!({
        "StartAt": "Attempt",
        "States": {
            "Attempt": failing_parallel(json!({
                "Catch": [{
                    "ErrorEquals": ["States.ALL"],
                    "Assign": { "errVar.$": "$.Error" },
                    "Next": "Emit",
                }],
                "End": true,
            })),
            "Emit": reader,
        },
    })
}

/// A definition that reads a variable NOTHING in it assigns, on the same shape.
fn nothing_assigns_it(reader: Value) -> Value {
    json!({
        "StartAt": "Attempt",
        "States": {
            "Attempt": failing_parallel(json!({
                "Catch": [{ "ErrorEquals": ["States.ALL"], "Next": "Emit" }],
                "Next": "Emit",
            })),
            "Emit": reader,
        },
    })
}

/// The reader every row above shares: a payload template's reference field on `$errVar`.
fn template_reader() -> Value {
    json!({ "Type": "Pass", "Parameters": { "errVar.$": "$errVar" }, "End": true })
}

/// A read of a variable only SOME path to the reading state assigns is created, and the
/// diagnostic is the warning that says the read resolves on some runs.
#[tokio::test]
async fn a_read_some_path_assigns_is_created_with_a_warning() {
    // (case, the definition)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a catcher's Assign read by the state the success path also reaches",
            joined_after_a_catcher(template_reader()),
        ),
        (
            "the same read through a *Path member",
            joined_after_a_catcher(json!({
                "Type": "Pass",
                "InputPath": "$errVar",
                "End": true,
            })),
        ),
        (
            "the same read through a Choice rule's Variable",
            json!({
                "StartAt": "Attempt",
                "States": {
                    "Attempt": failing_parallel(json!({
                        "Catch": [{
                            "ErrorEquals": ["States.ALL"],
                            "Assign": { "errVar.$": "$.Error" },
                            "Next": "Route",
                        }],
                        "Next": "Route",
                    })),
                    "Route": {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$errVar", "StringEquals": "Boom", "Next": "Emit" }],
                        "Default": "Emit",
                    },
                    "Emit": { "Type": "Pass", "End": true },
                },
            }),
        ),
        (
            "a Task state's catcher, which is the same position on another state kind",
            json!({
                "StartAt": "Attempt",
                "States": {
                    "Attempt": {
                        "Type": "Task",
                        "Resource": "arn:aws:states:::aws-sdk:sts:getCallerIdentity",
                        "Catch": [{
                            "ErrorEquals": ["States.ALL"],
                            "Assign": { "errVar.$": "$.Error" },
                            "Next": "Emit",
                        }],
                        "Next": "Emit",
                    },
                    "Emit": template_reader(),
                },
            }),
        ),
        (
            "a Map state's catcher",
            json!({
                "StartAt": "Attempt",
                "States": {
                    "Attempt": {
                        "Type": "Map",
                        "ItemsPath": "$.items",
                        "ItemProcessor": {
                            "ProcessorConfig": { "Mode": "INLINE" },
                            "StartAt": "Each",
                            "States": { "Each": { "Type": "Pass", "End": true } },
                        },
                        "Catch": [{
                            "ErrorEquals": ["States.ALL"],
                            "Assign": { "errVar.$": "$.Error" },
                            "Next": "Emit",
                        }],
                        "Next": "Emit",
                    },
                    "Emit": template_reader(),
                },
            }),
        ),
        (
            "a Choice rule's own Assign, read by the state the Default also reaches",
            json!({
                "StartAt": "Route",
                "States": {
                    "Route": {
                        "Type": "Choice",
                        "Choices": [{
                            "Variable": "$.go",
                            "BooleanEquals": true,
                            "Assign": { "errVar": "chosen" },
                            "Next": "Emit",
                        }],
                        "Default": "Emit",
                    },
                    "Emit": template_reader(),
                },
            }),
        ),
        (
            "a reader reached ONLY through the catcher that assigns it",
            only_after_a_catcher(template_reader()),
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition)) in rows.iter().enumerate() {
        let (status, code, message) =
            created(&emulator, &format!("some-path-{index}"), definition).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the definition to be served, got {status} {code} with \
             {message:?}"
        );
        let validated = validate(
            &emulator,
            case,
            &json!({ "definition": definition.to_string(), "severity": "WARNING" }),
        )
        .await;
        assert_eq!(
            result_of(&validated),
            VALIDATION_RESULT_OK,
            "case {case:?}: expected {VALIDATION_RESULT_OK}, got {validated:?}"
        );
        assert!(
            !members(&validated, "code").contains(&UNDEFINED_VARIABLE.to_string()),
            "case {case:?}: expected no {UNDEFINED_VARIABLE} diagnostic, got {validated:?}"
        );
    }
}

/// The control: a read of a variable NOTHING the definition declares assigns is refused
/// where the definition is submitted — the same shapes, with the assignment removed.
#[tokio::test]
async fn a_read_no_path_assigns_is_still_refused() {
    // (case, the definition)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a catcher that assigns nothing, read downstream of it",
            nothing_assigns_it(template_reader()),
        ),
        (
            "the same read through a *Path member",
            nothing_assigns_it(json!({
                "Type": "Pass",
                "InputPath": "$errVar",
                "End": true,
            })),
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition)) in rows.iter().enumerate() {
        let (status, code, message) =
            created(&emulator, &format!("no-path-{index}"), definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with {message:?}"
        );
        assert!(
            message.contains(&format!("{UNDEFINED_VARIABLE}: ")),
            "case {case:?}: expected the refusal to state {UNDEFINED_VARIABLE}, got {message:?}"
        );
        let validated = validate(
            &emulator,
            case,
            &json!({ "definition": definition.to_string() }),
        )
        .await;
        assert_eq!(
            result_of(&validated),
            VALIDATION_RESULT_FAIL,
            "case {case:?}: expected {VALIDATION_RESULT_FAIL}, got {validated:?}"
        );
    }
}

/// And the run really reads it: the caught path assigns the error it caught, and the
/// state the catcher transitions to puts that value out.
#[tokio::test]
async fn the_caught_path_reads_the_variable_its_catcher_assigned() {
    let emulator = Emulator::new();
    // (case, the definition, the value the reader must put out)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "the state the success path also reaches",
            joined_after_a_catcher(template_reader()),
            json!({ "errVar": "Boom" }),
        ),
        (
            "a state reached only through the catcher",
            only_after_a_catcher(template_reader()),
            json!({ "errVar": "Boom" }),
        ),
    ];

    let mut started = Vec::with_capacity(rows.len());
    for (index, (_, definition, _)) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("caught-read-{index}"),
            &definition.to_string(),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", "{}").await);
    }
    for ((case, _, expected), execution) in rows.iter().zip(started) {
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the caught path to run, got {described:?}"
        );
        let output: Value = serde_json::from_str(&string(&described, "output"))
            .unwrap_or_else(|why| panic!("case {case:?}: expected JSON output: {why}"));
        assert_eq!(
            &output, expected,
            "case {case:?}: expected the reader to put out {expected:?}, got {output:?}"
        );
    }
}

/// The warning the create is served with is the one the language words for a read that
/// resolves on some runs, located at the field that reads it.
#[tokio::test]
async fn the_warning_locates_the_field_that_reads_it() {
    let emulator = Emulator::new();
    let definition = joined_after_a_catcher(template_reader());
    let validated = validate(
        &emulator,
        "a catcher's Assign read on both paths",
        &json!({ "definition": definition.to_string(), "severity": "WARNING" }),
    )
    .await;
    assert!(
        members(&validated, "code").contains(&POSSIBLY_UNDEFINED_VARIABLE.to_string()),
        "expected a {POSSIBLY_UNDEFINED_VARIABLE} diagnostic, got {validated:?}"
    );
    assert!(
        members(&validated, "location").contains(&"/States/Emit/Parameters/errVar.$".to_string()),
        "expected the diagnostic at the field that reads it, got {validated:?}"
    );
}
