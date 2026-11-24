// SPDX-License-Identifier: CC0-1.0

//! The CLOSED field set of every object the Amazon States Language declares one for,
//! judged where the definition is submitted.
//!
//! The rule under test: the language declares, per object of a definition, the fields
//! that object may carry; a field outside that set is not a field the language defines
//! THERE, so the definition is refused `InvalidDefinition` with the field named at the
//! object that carries it — and with the field it is one edit from suggested. The rule
//! holds for every such object, which is what the table below spans: the state machine
//! object (the definition's own and each one nested in a state), a state, a retrier, a
//! catcher, a `Choice` rule and the rules nested in one, the four objects a `Map` state
//! carries beside its processor and the two configurations inside them, and the
//! `Credentials` a `Task` state's call is made under.
//!
//! Driven end to end over this service's own wire protocol through the production
//! router, so what is asserted is what a signed SDK request observes.
//!
//! The harness (`Emulator`, `error_code`, `ROLE_ARN`) is the one
//! `state_machine_lifecycle` builds and the validation readers are
//! `definition_validation`'s; `tests/all/main.rs` picks this file up, so each is
//! reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    INVALID_STATE_MACHINE_DEFINITION, VALIDATION_RESULT_FAIL, VALIDATION_RESULT_OK,
};

use crate::definition_validation::{members, result_of, validate};
use crate::state_machine_lifecycle::{error_code, string, Emulator, ROLE_ARN};

/// What `CreateStateMachine` answers for `definition`: the status, the error code and
/// the message.
async fn created(emulator: &Emulator, name: &str, definition: &Value) -> (u16, String, String) {
    let body = json!({
        "name": name,
        "definition": definition.to_string(),
        "roleArn": ROLE_ARN,
    });
    let (status, answered) = emulator.call("CreateStateMachine", &body).await;
    let message = match status {
        200 => String::new(),
        _ => string(&answered, "message"),
    };
    (status, error_code(&answered), message)
}

/// The refusal message `CreateStateMachine` states for a definition carrying the one
/// problem `subject` states.
fn refusal(subject: &str) -> String {
    format!("{INVALID_STATE_MACHINE_DEFINITION}: '{subject}'")
}

/// The subject a field outside the set of the object at `location` earns, with the field
/// `meant` suggested where the name is one edit from it.
fn unsupported(field: &str, meant: Option<&str>, location: &str) -> String {
    let suggestion = match meant {
        Some(meant) => format!(". Did you mean '{meant}'?"),
        None => String::new(),
    };
    format!("SCHEMA_VALIDATION_FAILED: Field '{field}' is not supported{suggestion} at {location}")
}

/// A `Map` state whose processor does nothing, carrying `members` beside it.
fn map_state(members: Value) -> Value {
    let mut state = json!({
        "Type": "Map",
        "ItemProcessor": { "StartAt": "P", "States": { "P": { "Type": "Pass", "End": true } } },
        "End": true,
    });
    for (member, value) in members.as_object().expect("the members of a Map state") {
        state[member] = value.clone();
    }
    state
}

/// A `Task` state carrying `members` beside the `Resource` every one of them needs.
fn task_state(members: Value) -> Value {
    let mut state = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::aws-sdk:sts:getCallerIdentity",
        "End": true,
    });
    for (member, value) in members.as_object().expect("the members of a Task state") {
        state[member] = value.clone();
    }
    state
}

/// A one-state definition: the state `name`, of the shape `state`.
fn one_state(name: &str, state: Value) -> Value {
    json!({ "StartAt": name, "States": { name: state } })
}

/// Every object position the language declares a field set for is judged against that
/// set, and the field the unknown name is one edit from is suggested.
///
/// The rows differ along the OBJECT the unknown field sits in — twelve positions, from
/// the definition's own root object down to the configuration of a `Map` state's result
/// writer and the credentials of a `Task` state's call — and along whether the name is a
/// near miss of a field that object declares (suggested) or of none (named alone).
#[tokio::test]
async fn a_field_outside_a_declared_objects_set_is_refused_at_that_object() {
    // (case, the definition, the field, the field it is a near miss of, the location)
    let rows: Vec<(&str, Value, &str, Option<&str>, &str)> = vec![
        (
            "the state machine object",
            json!({
                "Commentt": "x",
                "StartAt": "A",
                "States": { "A": { "Type": "Pass", "End": true } },
            }),
            "Commentt",
            Some("Comment"),
            "/",
        ),
        (
            "a state",
            one_state("A", json!({ "Type": "Pass", "Resul": {}, "End": true })),
            "Resul",
            Some("Result"),
            "/States/A",
        ),
        (
            "a retrier",
            one_state(
                "A",
                task_state(json!({
                    "Retry": [{ "ErrorEquals": ["States.ALL"], "MaxAttempt": 2 }],
                })),
            ),
            "MaxAttempt",
            Some("MaxAttempts"),
            "/States/A/Retry[0]",
        ),
        (
            "a catcher",
            one_state(
                "A",
                task_state(json!({
                    "Catch": [{ "ErrorEquals": ["States.ALL"], "Next": "A", "ResultPat": "$.e" }],
                })),
            ),
            "ResultPat",
            Some("ResultPath"),
            "/States/A/Catch[0]",
        ),
        (
            "a Choice rule",
            json!({
                "StartAt": "A",
                "States": {
                    "A": {
                        "Type": "Choice",
                        "Choices": [{ "Variabl": "$.x", "StringEquals": "y", "Next": "B" }],
                        "Default": "B",
                    },
                    "B": { "Type": "Pass", "End": true },
                },
            }),
            "Variabl",
            Some("Variable"),
            "/States/A/Choices[0]",
        ),
        (
            "a rule nested in a Choice rule's Not",
            json!({
                "StartAt": "A",
                "States": {
                    "A": {
                        "Type": "Choice",
                        "Choices": [{
                            "Not": { "Variabl": "$.x", "StringEquals": "y" },
                            "Next": "B",
                        }],
                        "Default": "B",
                    },
                    "B": { "Type": "Pass", "End": true },
                },
            }),
            "Variabl",
            Some("Variable"),
            "/States/A/Choices[0]/Not",
        ),
        (
            "a Map state's processor, which is a state machine object of its own",
            one_state(
                "A",
                json!({
                    "Type": "Map",
                    "ItemProcessor": {
                        "StartsAt": "P",
                        "StartAt": "P",
                        "States": { "P": { "Type": "Pass", "End": true } },
                    },
                    "End": true,
                }),
            ),
            "StartsAt",
            Some("StartAt"),
            "/States/A/ItemProcessor",
        ),
        (
            "a Parallel branch, which is one too",
            one_state(
                "A",
                json!({
                    "Type": "Parallel",
                    "Branches": [{
                        "Commentt": "x",
                        "StartAt": "P",
                        "States": { "P": { "Type": "Pass", "End": true } },
                    }],
                    "End": true,
                }),
            ),
            "Commentt",
            Some("Comment"),
            "/States/A/Branches[0]",
        ),
        (
            "a processor configuration",
            one_state(
                "A",
                json!({
                    "Type": "Map",
                    "ItemProcessor": {
                        "ProcessorConfig": { "Mod": "INLINE" },
                        "StartAt": "P",
                        "States": { "P": { "Type": "Pass", "End": true } },
                    },
                    "End": true,
                }),
            ),
            "Mod",
            Some("Mode"),
            "/States/A/ItemProcessor/ProcessorConfig",
        ),
        (
            "an item reader",
            one_state(
                "A",
                map_state(json!({
                    "ItemReader": { "Resourc": "arn:aws:states:::s3:getObject" },
                })),
            ),
            "Resourc",
            Some("Resource"),
            "/States/A/ItemReader",
        ),
        (
            "a reader configuration",
            one_state(
                "A",
                map_state(json!({
                    "ItemReader": {
                        "Resource": "arn:aws:states:::s3:getObject",
                        "ReaderConfig": { "InputTyp": "CSV" },
                    },
                })),
            ),
            "InputTyp",
            Some("InputType"),
            "/States/A/ItemReader/ReaderConfig",
        ),
        (
            "an item batcher",
            one_state(
                "A",
                map_state(json!({ "ItemBatcher": { "BatchInpu": {} } })),
            ),
            "BatchInpu",
            Some("BatchInput"),
            "/States/A/ItemBatcher",
        ),
        (
            "a writer configuration",
            one_state(
                "A",
                map_state(json!({
                    "ResultWriter": {
                        "Resource": "arn:aws:states:::s3:putObject",
                        "WriterConfig": { "OutputTyp": "JSON" },
                    },
                })),
            ),
            "OutputTyp",
            Some("OutputType"),
            "/States/A/ResultWriter/WriterConfig",
        ),
        (
            // The position this suite was written for: a `Task` state's `Credentials`
            // declares the one field the language gives it.
            "the credentials a Task state's call is made under",
            one_state(
                "A",
                task_state(json!({
                    "Credentials": { "RoleArnn": "arn:aws:iam::000000000000:role/R" },
                })),
            ),
            "RoleArnn",
            Some("RoleArn"),
            "/States/A/Credentials",
        ),
        (
            // The same object, with a name that is a near miss of nothing it declares:
            // the field is named alone, with no suggestion after it.
            "the same credentials, with a name that is a near miss of none of its fields",
            one_state(
                "A",
                task_state(json!({
                    "Credentials": { "Principal": "arn:aws:iam::000000000000:role/R" },
                })),
            ),
            "Principal",
            None,
            "/States/A/Credentials",
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition, field, meant, location)) in rows.iter().enumerate() {
        let subject = unsupported(field, *meant, location);
        let expected = refusal(&subject);
        let (status, code, message) =
            created(&emulator, &format!("closed-object-{index}"), definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with {message:?}"
        );
        assert!(
            message.contains(&subject),
            "case {case:?}: expected a refusal stating {expected:?}, got {message:?}"
        );
        // The validation of the same definition reports the same problem at the same
        // place, because one walk answers both.
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
        assert!(
            members(&validated, "location").contains(&(*location).to_string()),
            "case {case:?}: expected a diagnostic at {location:?}, got {validated:?}"
        );
    }
}

/// The control for the rule above: the fields each of those objects DOES declare are
/// accepted, so the rule is about the set and not about refusing objects.
///
/// The rows differ along the object and along the form its field is written in — a
/// `Credentials` naming the role itself, and the reference field that reads it from the
/// state's data, which is how the JSONPath half of the language writes the same thing.
#[tokio::test]
async fn the_fields_a_declared_object_does_declare_are_accepted() {
    // (case, the definition)
    let rows: Vec<(&str, Value)> = vec![
        (
            "credentials naming the role itself",
            one_state(
                "A",
                task_state(json!({
                    "Credentials": { "RoleArn": "arn:aws:iam::000000000000:role/R" },
                })),
            ),
        ),
        (
            "credentials reading the role from the state's data",
            one_state(
                "A",
                task_state(json!({ "Credentials": { "RoleArn.$": "$.roleArn" } })),
            ),
        ),
        (
            "a processor configuration naming its mode",
            one_state(
                "A",
                json!({
                    "Type": "Map",
                    "ItemProcessor": {
                        "ProcessorConfig": { "Mode": "INLINE" },
                        "StartAt": "P",
                        "States": { "P": { "Type": "Pass", "End": true } },
                    },
                    "End": true,
                }),
            ),
        ),
        (
            "an item reader with its own configuration",
            one_state(
                "A",
                map_state(json!({
                    "ItemReader": {
                        "Resource": "arn:aws:states:::s3:getObject",
                        "ReaderConfig": { "InputType": "CSV", "CSVHeaderLocation": "FIRST_ROW" },
                        "Parameters": { "Bucket": "b", "Key": "k" },
                    },
                })),
            ),
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition)) in rows.iter().enumerate() {
        let (status, code, message) =
            created(&emulator, &format!("declared-object-{index}"), definition).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the definition to be served, got {status} {code} with \
             {message:?}"
        );
        let validated = validate(
            &emulator,
            case,
            &json!({ "definition": definition.to_string() }),
        )
        .await;
        assert_eq!(
            result_of(&validated),
            VALIDATION_RESULT_OK,
            "case {case:?}: expected {VALIDATION_RESULT_OK}, got {validated:?}"
        );
    }
}
