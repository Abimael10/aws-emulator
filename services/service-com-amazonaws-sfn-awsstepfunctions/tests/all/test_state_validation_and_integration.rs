// SPDX-License-Identifier: CC0-1.0

//! What `TestState` makes of the definition, the variables and the Task it is given,
//! driven over this service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions and KMS mounted on one engine.
//!
//! The rules under test:
//!
//! 1. the state under test is held to the Amazon States Language's own schema, so a
//!    field the language does not declare for the type the state declares is refused
//!    with the modelled `InvalidDefinition` — and `Assign` is a field of every type
//!    EXCEPT the two that end an execution, because a variable is assigned for a later
//!    state to read and nothing runs after a `Succeed` or a `Fail`;
//! 2. `variables` is a "JSON object literal that sets variables used in the state under
//!    test. Object keys are the variable names": text no JSON parse reads, a document
//!    that is not an object, and an object whose key is not a variable name are each
//!    refused in the service's own sentence for that defect, which quotes nothing of
//!    the `@sensitive` document except a key that is not a name;
//! 3. "you can test [...] an Amazon Web Services service integration request and
//!    response": the call a `Task` state's `Resource` names is MADE, as the role the
//!    request named — so a role this service cannot assume fails the state with the
//!    task's own failure rather than with work nothing ran.
//!
//! The rows differ along the state TYPE carrying the unsupported field, along the way
//! `variables` fails to be a variables document, and along whether the role the test
//! names can be assumed.
//!
//! The harness (`Emulator`, `string`, `error_code`) is the one
//! `state_machine_lifecycle` builds; `tests/all.rs` is this binary's crate root, so it
//! is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{error_code, string, Emulator, ROLE_ARN};

/// `TestState` with the request members `extra` carries beside `definition`.
async fn test_state(emulator: &Emulator, definition: &str, extra: Value) -> (u16, Value) {
    let mut request = json!({ "definition": definition });
    for (member, value) in extra.as_object().expect("the row's members") {
        request[member] = value.clone();
    }
    emulator.call("TestState", &request).await
}

/// A state of each type the Amazon States Language declares `Assign` for, carrying it,
/// is tested; a `Succeed` or a `Fail` state carrying it is refused, because the
/// language declares the field for neither.
#[tokio::test]
async fn assign_is_a_field_of_every_type_but_the_two_that_end_an_execution() {
    let emulator = Emulator::new();

    // The rows differ along the state's TYPE: the ones that carry on to another state
    // declare `Assign`, and the two that end the execution do not.
    let accepted: Vec<(&str, String)> = vec![
        (
            "a Pass state",
            json!({ "Type": "Pass", "Assign": { "w": 1 }, "End": true }).to_string(),
        ),
        (
            "a Wait state",
            json!({ "Type": "Wait", "Seconds": 0, "Assign": { "w": 1 }, "End": true }).to_string(),
        ),
        (
            "a Choice state",
            json!({
                "Type": "Choice",
                "Assign": { "w": 1 },
                "Choices": [{ "Variable": "$.a", "NumericEquals": 1, "Next": "Onward" }],
                "Default": "Onward",
            })
            .to_string(),
        ),
    ];
    for (case, definition) in accepted {
        let (status, body) = test_state(&emulator, &definition, json!({ "input": "{\"a\":1}" })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the language declares Assign here, so the state is tested, got \
             {status} with body {body:?}"
        );
    }

    // And the two that end an execution: each is refused as a definition defect, and
    // the refusal names the field it could not accept.
    let refused: Vec<(&str, String)> = vec![
        (
            "a Succeed state",
            json!({ "Type": "Succeed", "Assign": { "w": 1 } }).to_string(),
        ),
        (
            "a Fail state",
            json!({ "Type": "Fail", "Error": "Boom", "Assign": { "w": 1 } }).to_string(),
        ),
    ];
    for (case, definition) in refused {
        let (status, body) = test_state(&emulator, &definition, json!({})).await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with body {body:?}"
        );
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("'Assign'"),
            "case {case:?}: the refusal must name the field it could not accept, got {message:?}"
        );
    }
}

/// A `variables` value that is not a JSON object literal whose keys are variable names
/// is refused, each defect in the sentence the service words for it.
#[tokio::test]
async fn variables_must_be_an_object_of_variable_names() {
    let emulator = Emulator::new();
    let definition = json!({ "Type": "Pass", "Result": { "ok": true }, "End": true }).to_string();

    // The rows differ along HOW the value fails to be a variables document: text no
    // parse reads, a document of another JSON type, and an object whose key is not a
    // name a state could have assigned.
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "text that is not JSON at all",
            "{\"a\": ",
            "The value of 'variables' is not valid JSON.".to_string(),
        ),
        (
            "an empty value",
            "",
            "The value of 'variables' is not valid JSON.".to_string(),
        ),
        (
            "a JSON array",
            "[1, 2]",
            "The value of 'variables' must be a valid JSON object.".to_string(),
        ),
        (
            "a JSON string",
            "\"w\"",
            "The value of 'variables' must be a valid JSON object.".to_string(),
        ),
        (
            "a key that begins with a digit",
            "{\"1bad\": 1}",
            "Invalid variable name '1bad': the variable name contains invalid characters."
                .to_string(),
        ),
        (
            "a key carrying a character a name may not",
            "{\"a-b\": 1}",
            "Invalid variable name 'a-b': the variable name contains invalid characters."
                .to_string(),
        ),
        (
            "the reserved context object's own name",
            "{\"states\": 1}",
            "Invalid variable name 'states': the variable name contains invalid characters."
                .to_string(),
        ),
    ];
    for (case, variables, expected) in cases {
        let (status, body) = test_state(&emulator, &definition, json!({ "variables": variables })).await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code} with body \
             {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }

    // And an object whose keys ARE variable names sets them, which the state reads.
    let answered = emulator
        .ok(
            "TestState",
            &json!({
                "definition": json!({
                    "Type": "Pass",
                    "QueryLanguage": "JSONata",
                    "Output": "{% { 'seen': $w } %}",
                    "End": true,
                })
                .to_string(),
                "variables": "{\"w\": 7, \"_also_ok2\": 1}",
            }),
        )
        .await;
    assert_eq!(
        (
            answered.get("status").and_then(Value::as_str),
            string(&answered, "output").as_str(),
        ),
        (Some("SUCCEEDED"), r#"{"seen":7}"#),
        "a variables document of names sets them for the state, got {answered:?}"
    );
}

/// A `Task` state's integration call is MADE, as the role the request named: a role
/// this service cannot assume fails the state with the task's own failure, while the
/// same state tested under a role it can assume runs the call.
#[tokio::test]
async fn a_task_states_integration_call_is_made_as_the_role_the_request_named() {
    let emulator = Emulator::new();
    // The target of the call: an ACTIVITY is work a worker does, so the one integration
    // every mounting of this service can serve is a call on Step Functions itself —
    // here a listing of this emulator's own activities.
    let definition = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::aws-sdk:sfn:listActivities",
        "End": true,
    })
    .to_string();

    // The rows differ along the role the test names: none at all — which the operation
    // refuses, because the call is made AS a role and a `Task` state under test names
    // none of its own — and one the request does name, under which the call is made.
    let (status, body) = test_state(&emulator, &definition, json!({ "input": "{}" })).await;
    let code = error_code(&body);
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "a Task state tested with no role has nobody to make its call, got {status} {code} with \
         body {body:?}"
    );

    // Under a role the request names, the call IS made: the state's result is the
    // operation's own response document, not a stand-in for one.
    let (status, body) = test_state(
        &emulator,
        &definition,
        json!({ "input": "{}", "roleArn": ROLE_ARN }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a Task state tested under a role is run as that role, got {status} with body {body:?}"
    );
    assert_eq!(
        body.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "the call must be made under the role the request named, got {body:?}"
    );
    let output: Value = serde_json::from_str(&string(&body, "output")).expect("the output is JSON");
    assert!(
        output.get("Activities").is_some(),
        "the state's result is the operation's own response document, got {output:?}"
    );
}
