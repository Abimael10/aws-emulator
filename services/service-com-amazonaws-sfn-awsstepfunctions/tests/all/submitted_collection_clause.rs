// SPDX-License-Identifier: CC0-1.0

//! What a constraint refusal QUOTES when the clause is about a COLLECTION member.
//!
//! The rule under test: a value outside a constraint the model declares is refused over
//! one clause per violation, each naming the value that was rejected — and where that
//! member is a list, the value is the list the request submitted, rendered as its
//! elements (`[<element>, …]`, a structure element as `<ShapeName>(<member>=<value>,
//! …)`). The `@length` of a list is checked on the MEMBER, so the validating walk has
//! only a summary of its size there; the clause states the collection itself.
//!
//! The rows differ along the number of elements the request carried (one past the
//! declared maximum, and further past it), along the operation that carries the member
//! and along the values its elements hold — beside the controls: the EMPTY list, whose
//! own clause states `[]`, and a clause about an element's own value, which names that
//! value and not the list.
//!
//! Driven end to end over this service's own wire protocol through the production
//! router, so what is asserted is what a signed SDK request observes. The harness
//! (`Emulator`, `error_code`, `ROLE_ARN`) is the one `state_machine_lifecycle` builds;
//! `tests/all/main.rs` picks this file up, so it is reached through `crate::<suite>`.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{error_code, string, Emulator, ROLE_ARN};

/// The modelled maximum of `routingConfiguration`
/// (`smithy_query(query="shape", shape="RoutingConfigurationList")`: `@length` 1..2).
const MAX_ROUTING_ENTRIES: usize = 2;

/// A state machine with `versions` published versions, and its ARN.
async fn machine_with_versions(emulator: &Emulator, name: &str, versions: usize) -> String {
    let definition = json!({
        "StartAt": "A",
        "States": { "A": { "Type": "Pass", "End": true } },
    })
    .to_string();
    let created = emulator
        .ok(
            "CreateStateMachine",
            &json!({ "name": name, "definition": definition, "roleArn": ROLE_ARN, "publish": true }),
        )
        .await;
    let machine = string(&created, "stateMachineArn");
    for _ in 1..versions {
        emulator
            .ok(
                "PublishStateMachineVersion",
                &json!({ "stateMachineArn": &machine }),
            )
            .await;
    }
    machine
}

/// One routing entry: the version it names and the weight it carries.
fn route(version: &str, weight: i64) -> Value {
    json!({ "stateMachineVersionArn": version, "weight": weight })
}

/// That entry as the clause renders it — the form the AWS SDKs print a modelled
/// structure in, over the members the request set.
fn rendered(version: &str, weight: i64) -> String {
    format!("RoutingConfigurationListItem(stateMachineVersionArn={version}, weight={weight})")
}

/// The whole validation message one `@length` violation of `routingConfiguration`
/// states, with `elements` as the clause renders the list.
fn length_refusal(elements: &[String], requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at 'routingConfiguration' failed to satisfy \
         constraint: {requirement}",
        elements.join(", ")
    )
}

/// What the refusal of `operation` states: the status, the code and the message.
async fn refused(emulator: &Emulator, operation: &str, body: &Value) -> (u16, String, String) {
    let (status, answered) = emulator.call(operation, body).await;
    let message = match status {
        200 => String::new(),
        _ => string(&answered, "message"),
    };
    (status, error_code(&answered), message)
}

/// A clause about a list member quotes the list the request carried, element by element.
///
/// The rows differ along the element count (one past the declared maximum and three
/// past it), along the weights the elements hold, and along the operation the member is
/// carried on — `CreateStateMachineAlias` and `UpdateStateMachineAlias`, which declare
/// the same member.
#[tokio::test]
async fn a_length_clause_about_a_list_quotes_the_list_the_request_carried() {
    let emulator = Emulator::new();
    let machine = machine_with_versions(&emulator, "quoted-list", 2).await;
    let first = format!("{machine}:1");
    let second = format!("{machine}:2");
    let alias = emulator
        .ok(
            "CreateStateMachineAlias",
            &json!({ "name": "PROD", "routingConfiguration": [route(&first, 100)] }),
        )
        .await;
    let alias = string(&alias, "stateMachineAliasArn");
    let requirement = format!("Member must have length less than or equal to {MAX_ROUTING_ENTRIES}");

    // (case, the weights the entries carry)
    let rows: Vec<(&str, Vec<i64>)> = vec![
        ("one entry past the maximum", vec![34, 33, 33]),
        ("three entries past it", vec![20, 20, 20, 20, 20]),
        ("one entry past it, with a weight of nothing", vec![100, 0, 0]),
    ];

    for (case, weights) in &rows {
        let versions: Vec<&String> = weights
            .iter()
            .enumerate()
            .map(|(index, _)| match index % 2 {
                0 => &first,
                _ => &second,
            })
            .collect();
        let routing: Vec<Value> = versions
            .iter()
            .zip(weights)
            .map(|(version, weight)| route(version, *weight))
            .collect();
        let elements: Vec<String> = versions
            .iter()
            .zip(weights)
            .map(|(version, weight)| rendered(version, *weight))
            .collect();
        let expected = length_refusal(&elements, &requirement);

        for (operation, body) in [
            (
                "CreateStateMachineAlias",
                json!({ "name": "CANARY", "routingConfiguration": &routing }),
            ),
            (
                "UpdateStateMachineAlias",
                json!({ "stateMachineAliasArn": &alias, "routingConfiguration": &routing }),
            ),
        ] {
            let case = format!("{operation} with {case}");
            let (status, code, message) = refused(&emulator, operation, &body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {code} with \
                 {message:?}"
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }
}

/// The clauses this rule does not change: the EMPTY list, which the summary of its size
/// determines completely, and a clause about the value one ELEMENT held, which names
/// that value at the element's own path.
#[tokio::test]
async fn the_other_clauses_about_the_same_member_are_unchanged() {
    let emulator = Emulator::new();
    let machine = machine_with_versions(&emulator, "quoted-list-controls", 1).await;
    let version = format!("{machine}:1");

    // (case, the routing configuration, the whole message the refusal states)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a list under the declared minimum",
            json!([]),
            length_refusal(&[], "Member must have length greater than or equal to 1"),
        ),
        (
            "an element whose weight is past its own maximum",
            json!([route(&version, 101)]),
            "1 validation error detected: Value '101' at 'routingConfiguration.1.member.weight' \
             failed to satisfy constraint: Member must have value less than or equal to 100"
                .to_string(),
        ),
        (
            "an element whose weight is under its own minimum",
            json!([route(&version, -1)]),
            "1 validation error detected: Value '-1' at 'routingConfiguration.1.member.weight' \
             failed to satisfy constraint: Member must have value greater than or equal to 0"
                .to_string(),
        ),
    ];

    for (index, (case, routing, expected)) in rows.iter().enumerate() {
        let (status, code, message) = refused(
            &emulator,
            "CreateStateMachineAlias",
            &json!({ "name": format!("CTRL{index}"), "routingConfiguration": routing }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code} with {message:?}"
        );
        assert_eq!(
            &message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The control for the rule as a whole: a configuration inside the bound is applied.
    let created = emulator
        .ok(
            "CreateStateMachineAlias",
            &json!({ "name": "LIVE", "routingConfiguration": [route(&version, 100)] }),
        )
        .await;
    assert_eq!(
        string(&created, "stateMachineAliasArn"),
        format!("{machine}:LIVE"),
        "a configuration inside the declared bound is applied, got {created:?}"
    );
}
