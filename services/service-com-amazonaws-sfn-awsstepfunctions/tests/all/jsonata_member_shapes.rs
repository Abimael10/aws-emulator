// SPDX-License-Identifier: CC0-1.0

//! The SHAPE the Amazon States Language declares for a JSONata state's members.
//!
//! The rule under test: a member the language gives ONE shape — the document a `Task`
//! builds its request in, the document a `Map` projects each item through, the array a
//! `Map` iterates, the test a `Choice` rule holds by — carries a value of that shape, or
//! one JSONata expression, which the language evaluates to one. A STRING is such an
//! expression only when the whole value is one (between `{%` and `%}`); a string that is
//! not is the literal it is written as, and a literal string is of none of those shapes,
//! so the definition declares work the language cannot do with it and is refused
//! `InvalidDefinition` where it is submitted.
//!
//! The rows span the member (`Arguments`, `ItemSelector`, `Items`, `Condition`), the
//! shape each one declares (a document, an array, a boolean), and WHAT was written
//! instead (a literal string, a number, a value of one of the other shapes) — beside the
//! controls: the declared shape itself and a whole expression, both of which are served.
//!
//! Driven end to end over this service's own wire protocol through the production
//! router. The harness (`Emulator`, `error_code`, `ROLE_ARN`) is the one
//! `state_machine_lifecycle` builds and the validation readers are
//! `definition_validation`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    INVALID_STATE_MACHINE_DEFINITION, VALIDATION_RESULT_FAIL, VALIDATION_RESULT_OK,
};

use crate::definition_validation::{members, result_of, validate};
use crate::state_machine_lifecycle::{error_code, string, Emulator, ROLE_ARN};

/// What `CreateStateMachine` answers for `definition`: the status, the error code and
/// the message.
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

/// A JSONata definition declaring the one state `state` under the name `name`.
fn one_state(name: &str, state: Value) -> Value {
    json!({ "QueryLanguage": "JSONata", "StartAt": name, "States": { name: state } })
}

/// A JSONata `Task` state whose `Arguments` hold `value`.
fn task_with_arguments(value: Value) -> Value {
    one_state(
        "T",
        json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::aws-sdk:sts:getCallerIdentity",
            "Arguments": value,
            "End": true,
        }),
    )
}

/// A JSONata `Map` state whose `member` holds `value`.
fn map_with(member: &str, value: Value) -> Value {
    let mut state = json!({
        "Type": "Map",
        "Items": [1],
        "ItemProcessor": { "StartAt": "P", "States": { "P": { "Type": "Pass", "End": true } } },
        "End": true,
    });
    state[member] = value;
    one_state("M", state)
}

/// A JSONata `Choice` state whose one rule tests `condition`.
fn choice_with_condition(condition: Value) -> Value {
    json!({
        "QueryLanguage": "JSONata",
        "StartAt": "C",
        "States": {
            "C": {
                "Type": "Choice",
                "Choices": [{ "Condition": condition, "Next": "B" }],
                "Default": "B",
            },
            "B": { "Type": "Pass", "End": true },
        },
    })
}

/// What a diagnostic says about a member of the declared shape `shape` the definition
/// wrote a value of another JSON TYPE at: the member, and the two things its value may
/// be.
fn wrong_shape_message(member: &str, shape: &str) -> String {
    format!("'{member}' must be {shape} or a JSONata expression")
}

/// What a diagnostic says about such a member holding a STRING that is no expression:
/// the text, quoted, and the form an embedded expression takes — the one sentence this
/// service states every value that is no expression by.
fn not_an_expression_message(text: &str) -> String {
    format!(
        "'{text}' is not a JSONata expression: an expression is the whole value of the field, \
         opened with '{{%' and closed with '%}}'"
    )
}

/// A sentence as one subject of a refusal, at the member's own location.
fn subject(sentence: &str, location: &str) -> String {
    format!("SCHEMA_VALIDATION_FAILED: {sentence} at {location}")
}

/// One row: what makes it different, the definition, the sentence the diagnostic states
/// and the location it is reported at.
struct Row {
    case: &'static str,
    definition: Value,
    sentence: String,
    location: &'static str,
}

/// A member of a declared shape holding neither that shape nor an expression is refused
/// where the definition is submitted, stating at the member itself what is wrong with
/// the value it holds: a STRING is only ever the expression it is written as, so it is
/// stated by the sentence every value that is no expression is stated by, and a value of
/// another JSON type by the shape the language declares.
#[tokio::test]
async fn a_member_of_a_declared_shape_holds_that_shape_or_an_expression() {
    let rows: Vec<Row> = vec![
        Row {
            case: "Arguments as a literal string",
            definition: task_with_arguments(json!("plain")),
            sentence: not_an_expression_message("plain"),
            location: "/States/T/Arguments",
        },
        Row {
            case: "Arguments as a string that opens no expression at all",
            definition: task_with_arguments(json!("$states.input")),
            sentence: not_an_expression_message("$states.input"),
            location: "/States/T/Arguments",
        },
        Row {
            case: "Arguments as a number",
            definition: task_with_arguments(json!(42)),
            sentence: wrong_shape_message("Arguments", "a JSON object"),
            location: "/States/T/Arguments",
        },
        Row {
            case: "Arguments as an array",
            definition: task_with_arguments(json!([1, 2])),
            sentence: wrong_shape_message("Arguments", "a JSON object"),
            location: "/States/T/Arguments",
        },
        Row {
            case: "ItemSelector as a literal string",
            definition: map_with("ItemSelector", json!("plain")),
            sentence: not_an_expression_message("plain"),
            location: "/States/M/ItemSelector",
        },
        Row {
            case: "Items as a literal string",
            definition: map_with("Items", json!("plain")),
            sentence: not_an_expression_message("plain"),
            location: "/States/M/Items",
        },
        Row {
            case: "Items as an object",
            definition: map_with("Items", json!({ "first": 1 })),
            sentence: wrong_shape_message("Items", "a JSON array"),
            location: "/States/M/Items",
        },
        Row {
            case: "Condition as a literal string",
            definition: choice_with_condition(json!("plain")),
            sentence: not_an_expression_message("plain"),
            location: "/States/C/Choices[0]/Condition",
        },
        Row {
            case: "Condition as a number",
            definition: choice_with_condition(json!(1)),
            sentence: wrong_shape_message("Condition", "a boolean"),
            location: "/States/C/Choices[0]/Condition",
        },
    ];

    let emulator = Emulator::new();
    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            definition,
            sentence,
            location,
        } = row;
        let stated = subject(sentence, location);
        let expected = format!("{INVALID_STATE_MACHINE_DEFINITION}: '{stated}'");
        let (status, code, message) =
            created(&emulator, &format!("shape-{index}"), definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with {message:?}"
        );
        assert!(
            message.contains(stated.as_str()),
            "case {case:?}: expected a refusal stating {expected:?}, got {message:?}"
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
        assert!(
            members(&validated, "message").contains(sentence),
            "case {case:?}: expected the validation to state {sentence:?}, got {validated:?}"
        );
        assert!(
            members(&validated, "location").contains(&(*location).to_string()),
            "case {case:?}: expected a diagnostic at {location:?}, got {validated:?}"
        );
    }
}

/// A string at such a member that CARRIES the marks an expression is delimited by
/// without being one whole expression is reported ONCE — as the malformed expression it
/// is written as, and not a second time as a value of the wrong shape.
///
/// The rows differ along the member and along how the text fails to be one expression
/// (opened and never closed, closed without being opened, text outside the marks).
#[tokio::test]
async fn a_malformed_expression_at_such_a_member_is_reported_once() {
    // (case, the definition, the location, the text the diagnostic quotes)
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "Arguments opened and never closed",
            task_with_arguments(json!("{% $states.input")),
            "/States/T/Arguments",
            "{% $states.input",
        ),
        (
            "Items closed without being opened",
            map_with("Items", json!("$states.input %}")),
            "/States/M/Items",
            "$states.input %}",
        ),
        (
            "Condition with text outside the marks",
            choice_with_condition(json!("value: {% true %}")),
            "/States/C/Choices[0]/Condition",
            "value: {% true %}",
        ),
    ];

    let emulator = Emulator::new();
    for (case, definition, location, text) in rows {
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
        let at_member: Vec<String> = members(&validated, "location")
            .into_iter()
            .filter(|reported| reported == location)
            .collect();
        assert_eq!(
            at_member.len(),
            1,
            "case {case:?}: expected exactly one diagnostic at {location:?}, got {validated:?}"
        );
        assert!(
            members(&validated, "message").contains(&not_an_expression_message(text)),
            "case {case:?}: expected the diagnostic to state {:?}, got {validated:?}",
            not_an_expression_message(text)
        );
    }
}

/// The control: each of those members holding the shape the language declares — or one
/// whole JSONata expression, whose own value no walk of the definition can know — is
/// served.
#[tokio::test]
async fn the_declared_shape_and_a_whole_expression_are_both_served() {
    // (case, the definition)
    let rows: Vec<(&str, Value)> = vec![
        (
            "Arguments as the document it builds",
            task_with_arguments(json!({ "Name": "{% $states.input.name %}" })),
        ),
        (
            "Arguments as one whole expression",
            task_with_arguments(json!("{% { 'Name': $states.input.name } %}")),
        ),
        (
            "ItemSelector as the document it projects",
            map_with("ItemSelector", json!({ "v": "{% $states.context.Map.Item.Value %}" })),
        ),
        (
            "ItemSelector as one whole expression",
            map_with("ItemSelector", json!("{% { 'v': 1 } %}")),
        ),
        ("Items as the array it holds", map_with("Items", json!([1, 2, 3]))),
        (
            "Items as one whole expression",
            map_with("Items", json!("{% [1, 2, 3] %}")),
        ),
        (
            "Condition as one whole expression",
            choice_with_condition(json!("{% $states.input.n > 1 %}")),
        ),
        ("Condition as the boolean it tests", choice_with_condition(json!(true))),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition)) in rows.iter().enumerate() {
        let (status, code, message) =
            created(&emulator, &format!("shape-ok-{index}"), definition).await;
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
