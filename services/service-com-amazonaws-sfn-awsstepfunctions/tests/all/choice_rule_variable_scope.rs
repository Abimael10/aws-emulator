// SPDX-License-Identifier: CC0-1.0

//! The variables a `Choice` rule's own members read, judged where the definition is
//! submitted.
//!
//! The rule under test: a field the Amazon States Language RESOLVES A PATH at names the
//! variable it reads as the first term of that path, so a path naming a variable the
//! scope reaching that field may not hold has nothing to resolve against and is an
//! ERROR (`UNDEFINED_VARIABLE`) — wherever such a field is written. A `Choice` rule
//! names the value its comparator tests through `Variable` and reads a second operand
//! through a `*Path` comparator, so a rule is judged member by member exactly as a state
//! is: a variable named there is a path the language resolves, and not the literal a
//! payload template's own keys hold.
//!
//! The rows differ along the member that reads it (`Variable`, each family's `*Path`
//! comparator), along where the rule sits (a state's own `Choices`, a rule nested in
//! `And`, `Or` and `Not`), and along the scope — a variable nothing assigns, one a state
//! before the `Choice` assigns (resolved, so the definition is served) and one only one
//! branch of the graph assigns.
//!
//! Driven end to end over this service's own wire protocol through the production
//! router. The harness (`Emulator`, `error_code`, `ROLE_ARN`) is the one
//! `state_machine_lifecycle` builds and the validation readers are
//! `definition_validation`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::UNDEFINED_VARIABLE;
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    VALIDATION_RESULT_FAIL, VALIDATION_RESULT_OK,
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

/// A definition whose `Choice` state tests `rules`, routing every outcome to one `Pass`.
fn choosing(rules: Value) -> Value {
    json!({
        "StartAt": "Route",
        "States": {
            "Route": { "Type": "Choice", "Choices": rules, "Default": "Done" },
            "Done": { "Type": "Pass", "End": true },
        },
    })
}

/// The same definition with a state before the `Choice` that assigns `variable`.
fn assigning_then_choosing(variable: &str, rules: Value) -> Value {
    json!({
        "StartAt": "Set",
        "States": {
            "Set": { "Type": "Pass", "Assign": { variable: "x" }, "Next": "Route" },
            "Route": { "Type": "Choice", "Choices": rules, "Default": "Done" },
            "Done": { "Type": "Pass", "End": true },
        },
    })
}

/// What a diagnostic says about a variable such a field names and the scope may not
/// hold.
fn not_defined(name: &str) -> String {
    format!("The variable '${name}' is not defined.")
}

/// A `Choice` rule naming a variable nothing in scope assigns is refused where the
/// definition is submitted, at the rule member that names it.
#[tokio::test]
async fn a_choice_rule_naming_an_unassigned_variable_is_refused_at_that_member() {
    // (case, the definition, the location the diagnostic is reported at)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "the value a rule's comparator tests",
            choosing(json!([{ "Variable": "$missing", "StringEquals": "y", "Next": "Done" }])),
            "/States/Route/Choices[0]/Variable",
        ),
        (
            "a numeric comparator's own operand",
            choosing(json!([{
                "Variable": "$.n",
                "NumericEqualsPath": "$missing",
                "Next": "Done",
            }])),
            "/States/Route/Choices[0]/NumericEqualsPath",
        ),
        (
            "a timestamp comparator's own operand",
            choosing(json!([{
                "Variable": "$.when",
                "TimestampGreaterThanPath": "$missing",
                "Next": "Done",
            }])),
            "/States/Route/Choices[0]/TimestampGreaterThanPath",
        ),
        (
            "the second rule of the same state",
            choosing(json!([
                { "Variable": "$.n", "NumericEquals": 1, "Next": "Done" },
                { "Variable": "$missing", "StringEquals": "y", "Next": "Done" },
            ])),
            "/States/Route/Choices[1]/Variable",
        ),
        (
            "a rule nested in an And",
            choosing(json!([{
                "And": [
                    { "Variable": "$.n", "NumericEquals": 1 },
                    { "Variable": "$missing", "StringEquals": "y" },
                ],
                "Next": "Done",
            }])),
            "/States/Route/Choices[0]/And[1]/Variable",
        ),
        (
            "a rule nested in an Or",
            choosing(json!([{
                "Or": [{ "Variable": "$missing", "StringEquals": "y" }],
                "Next": "Done",
            }])),
            "/States/Route/Choices[0]/Or[0]/Variable",
        ),
        (
            "a rule nested in a Not",
            choosing(json!([{
                "Not": { "Variable": "$missing", "StringEquals": "y" },
                "Next": "Done",
            }])),
            "/States/Route/Choices[0]/Not/Variable",
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition, location)) in rows.iter().enumerate() {
        let sentence = not_defined("missing");
        let (status, code, message) =
            created(&emulator, &format!("rule-scope-{index}"), definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with {message:?}"
        );
        assert!(
            message.contains(&format!("{UNDEFINED_VARIABLE}: ")) && message.contains(location),
            "case {case:?}: expected a refusal stating {UNDEFINED_VARIABLE} at {location:?}, got \
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
            VALIDATION_RESULT_FAIL,
            "case {case:?}: expected {VALIDATION_RESULT_FAIL}, got {validated:?}"
        );
        assert!(
            members(&validated, "code").contains(&UNDEFINED_VARIABLE.to_string()),
            "case {case:?}: expected an {UNDEFINED_VARIABLE} diagnostic, got {validated:?}"
        );
        assert!(
            members(&validated, "location").contains(&(*location).to_string()),
            "case {case:?}: expected a diagnostic at {location:?}, got {validated:?}"
        );
        assert!(
            message.contains(&sentence),
            "case {case:?}: expected the refusal to state {sentence:?}, got {message:?}"
        );
    }
}

/// The control: the same rules over a variable a state before the `Choice` assigns are
/// served, so the rule is about the SCOPE reaching the rule and not about refusing a
/// variable named in one.
#[tokio::test]
async fn a_choice_rule_naming_a_variable_in_scope_is_served() {
    // (case, the definition)
    let rows: Vec<(&str, Value)> = vec![
        (
            "the value a rule's comparator tests",
            assigning_then_choosing(
                "held",
                json!([{ "Variable": "$held", "StringEquals": "y", "Next": "Done" }]),
            ),
        ),
        (
            "a comparator's own operand",
            assigning_then_choosing(
                "held",
                json!([{ "Variable": "$.n", "NumericEqualsPath": "$held", "Next": "Done" }]),
            ),
        ),
        (
            "a rule nested in a Not",
            assigning_then_choosing(
                "held",
                json!([{
                    "Not": { "Variable": "$held", "StringEquals": "y" },
                    "Next": "Done",
                }]),
            ),
        ),
        (
            "a rule testing the state's own input, which is no variable at all",
            choosing(json!([{ "Variable": "$.n", "NumericEquals": 1, "Next": "Done" }])),
        ),
        (
            "a rule testing the context object, which is no variable either",
            choosing(json!([{
                "Variable": "$$.Execution.Name",
                "StringEquals": "y",
                "Next": "Done",
            }])),
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition)) in rows.iter().enumerate() {
        let (status, code, message) =
            created(&emulator, &format!("rule-scope-ok-{index}"), definition).await;
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
