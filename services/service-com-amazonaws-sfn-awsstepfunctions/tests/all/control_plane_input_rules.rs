// SPDX-License-Identifier: CC0-1.0

//! How the CONTROL PLANE words a request it cannot serve: the preconditions a create
//! and an update must satisfy, the ARN a state machine runs as, the routing
//! configuration an alias points through, and the clauses the modelled constraints are
//! stated by.
//!
//! Driven end to end over this service's own wire protocol through the production
//! router, so what is asserted is the `(status, code, message)` a signed SDK request
//! observes.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over:
//!
//! 1. a version description is a description OF a version, so naming one without
//!    asking for a version to be published is refused in one fixed sentence by every
//!    operation that takes the pair;
//! 2. a `roleArn` that is not an ARN of an IAM role is refused `InvalidArn`, stating
//!    the value as the request submitted it and nothing else, by every operation that
//!    takes one;
//! 3. a routing configuration states the rule it broke and lists the references the
//!    request submitted, classified by the reason the modelled `ValidationException`
//!    names for a configuration that cannot be applied;
//! 4. a modelled constraint is stated by the standard validation clause: an enumerated
//!    type's value set in the order this service advertises it, a list member's own
//!    value, and a member breaking several constraints with its `@pattern` requirement
//!    first and that expression quoted as the one a whole value must match.
//!
//! The harness (`Emulator`, `create_machine`, `ROLE_ARN`) is the one
//! `state_machine_lifecycle` builds, `failure` is `error_messages`', the version and
//! alias builders are `state_machine_versions`' and `state_machine_aliases`';
//! `tests/all/main.rs` picks this file up, so each is reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    invalid_role_arn_message, routing_duplicate_version_message, routing_not_a_version_message,
    routing_weight_sum_message, VALIDATION_REASON_INVALID_ROUTING_CONFIGURATION,
    VERSION_DESCRIPTION_WITHOUT_PUBLISH,
};

use crate::error_messages::failure;
use crate::state_machine_aliases::route;
use crate::state_machine_lifecycle::{
    create_machine, Emulator, HELLO_DEFINITION, REGION, ROLE_ARN,
};
use crate::state_machine_versions::machine_with_versions;

// ---------------------------------------------------------------------------
// Rule 1: a version description without a version to describe
// ---------------------------------------------------------------------------

/// Naming a `versionDescription` without `publish` is refused `ValidationException` in
/// the one sentence the service states that rule in, by both operations that take the
/// pair.
///
/// The rows differ along the operation, along what the description says (so nothing of
/// it is interpolated into the sentence) and along what `publish` carries (absent, and
/// written `false`), and end with the control: the same description WITH `publish`
/// publishes a version.
#[tokio::test]
async fn a_version_description_without_a_publish_is_refused_in_one_sentence() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "version-description", HELLO_DEFINITION).await;

    // (case, the operation, the request)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a create naming no publish at all",
            "CreateStateMachine",
            json!({
                "name": "described-1",
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "versionDescription": "the first cut",
            }),
        ),
        (
            "a create naming publish false",
            "CreateStateMachine",
            json!({
                "name": "described-2",
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "publish": false,
                "versionDescription": "another description entirely",
            }),
        ),
        (
            "an update naming no publish at all",
            "UpdateStateMachine",
            json!({
                "stateMachineArn": &machine,
                "definition": HELLO_DEFINITION,
                "versionDescription": "the next cut",
            }),
        ),
        (
            "an update naming publish false",
            "UpdateStateMachine",
            json!({
                "stateMachineArn": &machine,
                "definition": HELLO_DEFINITION,
                "publish": false,
                "versionDescription": "",
            }),
        ),
    ];

    for (case, operation, body) in rows {
        let (status, code, message) = failure(&emulator, case, operation, &body).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code} with \
             {message:?}"
        );
        assert_eq!(
            message, VERSION_DESCRIPTION_WITHOUT_PUBLISH,
            "case {case:?}: expected the message {VERSION_DESCRIPTION_WITHOUT_PUBLISH:?}, got \
             {message:?}"
        );
        assert_eq!(
            message, "Version description can only be set when publish is true",
            "case {case:?}: unexpected construction of the refusal"
        );
    }

    // The control: the same pair WITH publish is served, which is what makes the rule
    // about the pair rather than about the description.
    let published = emulator
        .ok(
            "CreateStateMachine",
            &json!({
                "name": "described-and-published",
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "publish": true,
                "versionDescription": "the first cut",
            }),
        )
        .await;
    assert!(
        published
            .get("stateMachineVersionArn")
            .and_then(Value::as_str)
            .is_some_and(|arn| arn.ends_with(":1")),
        "case \"a description with a publish\": expected version 1 to be published, got \
         {published:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 2: what a roleArn that is not a role ARN is refused with
// ---------------------------------------------------------------------------

/// A `roleArn` that is not the ARN of an IAM role is refused `InvalidArn`, stating the
/// value exactly as the request submitted it, by every operation that takes one.
///
/// The rows differ along WHY the value is not a role ARN — text that is not an ARN at
/// all, an ARN stopping short of its resource, an ARN of another service, an IAM ARN
/// naming another resource type, a role ARN with no name — and along the operation
/// that read it.
#[tokio::test]
async fn a_role_arn_that_is_not_one_states_the_value_it_was_handed() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "role-arn-rules", HELLO_DEFINITION).await;
    let state = json!({ "Type": "Pass", "End": true }).to_string();
    let task = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::aws-sdk:sts:getCallerIdentity",
        "End": true,
    })
    .to_string();

    // (case, the value)
    let values: Vec<(&str, &str)> = vec![
        ("text that is not an ARN at all", "invalid"),
        ("a word with the ARN prefix only", "arn:"),
        (
            "an ARN stopping short of its resource",
            "arn:aws:iam::000000000000",
        ),
        (
            "an ARN of another service",
            "arn:aws:lambda:us-west-2:000000000000:function:not-a-role",
        ),
        (
            "an IAM ARN naming another resource type",
            "arn:aws:iam::000000000000:user/someone",
        ),
        ("a role ARN with no name", "arn:aws:iam::000000000000:role/"),
    ];

    for (case, value) in values {
        // Every operation that takes a `roleArn`, and the rest of its own request.
        let calls: Vec<(&str, Value)> = vec![
            (
                "CreateStateMachine",
                json!({
                    "name": "role-arn-create",
                    "definition": HELLO_DEFINITION,
                    "roleArn": value,
                }),
            ),
            (
                "UpdateStateMachine",
                json!({ "stateMachineArn": &machine, "roleArn": value }),
            ),
            (
                "TestState",
                json!({ "definition": &state, "roleArn": value }),
            ),
            (
                "TestState",
                json!({ "definition": &task, "roleArn": value }),
            ),
        ];
        for (operation, body) in calls {
            let (status, code, message) = failure(&emulator, case, operation, &body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidArn"),
                "case {case:?} / {operation}: expected 400 InvalidArn, got {status} {code} with \
                 {message:?}"
            );
            let expected = invalid_role_arn_message(value);
            assert_eq!(
                message, expected,
                "case {case:?} / {operation}: expected the message {expected:?}, got {message:?}"
            );
            assert_eq!(
                message,
                format!("Invalid Role Arn: '{value}'"),
                "case {case:?} / {operation}: unexpected construction of the refusal"
            );
        }
    }

    // The control: the role ARN this account's tests run as is served by all of them.
    for (operation, body) in [
        (
            "CreateStateMachine",
            json!({
                "name": "role-arn-served",
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
            }),
        ),
        (
            "UpdateStateMachine",
            json!({ "stateMachineArn": &machine, "roleArn": ROLE_ARN }),
        ),
        (
            "TestState",
            json!({ "definition": &state, "roleArn": ROLE_ARN }),
        ),
    ] {
        let (status, answered) = emulator.call(operation, &body).await;
        assert_eq!(
            status, 200,
            "case \"the role ARN of this account\" / {operation}: expected 200, got {status} with \
             {answered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: what a routing configuration that cannot be applied is refused with
// ---------------------------------------------------------------------------

/// A routing configuration that cannot be applied is refused `ValidationException`
/// under the reason the model names for one, stating the rule it broke and listing the
/// references the request submitted.
///
/// The rows differ along WHICH rule the configuration broke — a reference that is not a
/// version, one version named twice, weights that do not add up — along the number of
/// entries, and along the operation that read the configuration.
#[tokio::test]
async fn a_routing_configuration_states_the_rule_it_broke_and_what_was_submitted() {
    let emulator = Emulator::new();
    let machine = machine_with_versions(&emulator, "routing-rules", 2).await;
    let first = format!("{machine}:1");
    let second = format!("{machine}:2");
    let alias = {
        let created = emulator
            .ok(
                "CreateStateMachineAlias",
                &json!({
                    "name": "live",
                    "routingConfiguration": [route(&first, 100)],
                }),
            )
            .await;
        created
            .get("stateMachineAliasArn")
            .and_then(Value::as_str)
            .expect("the alias ARN")
            .to_string()
    };

    // (case, the configuration, the message it is refused with)
    let rows: Vec<(&str, Vec<Value>, String)> = vec![
        (
            "an unqualified state machine ARN, which names no version",
            vec![route(&machine, 100)],
            routing_not_a_version_message(&routing(&[(&machine, 100)])),
        ),
        (
            "the alias's own ARN, which routes rather than being routed to",
            vec![route(&alias, 100)],
            routing_not_a_version_message(&routing(&[(&alias, 100)])),
        ),
        (
            "one version named twice",
            vec![route(&first, 60), route(&first, 40)],
            routing_duplicate_version_message(&routing(&[(&first, 60), (&first, 40)])),
        ),
        (
            "weights that do not add up",
            vec![route(&first, 60), route(&second, 30)],
            routing_weight_sum_message(&[60, 30]),
        ),
        (
            "weights that add up to more than the whole",
            vec![route(&first, 100), route(&second, 1)],
            routing_weight_sum_message(&[100, 1]),
        ),
    ];

    for (index, (case, configuration, expected)) in rows.iter().enumerate() {
        // Both operations that read a routing configuration: the create, which resolves
        // the alias's state machine FROM it, and the update of an alias that already
        // belongs to one.
        let calls: Vec<(&str, Value)> = vec![
            (
                "CreateStateMachineAlias",
                json!({
                    "name": format!("routing-{index}"),
                    "routingConfiguration": configuration,
                }),
            ),
            (
                "UpdateStateMachineAlias",
                json!({
                    "stateMachineAliasArn": &alias,
                    "routingConfiguration": configuration,
                }),
            ),
        ];
        for (operation, body) in calls {
            let (status, code, message) = failure(&emulator, case, operation, &body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationException"),
                "case {case:?} / {operation}: expected 400 ValidationException, got {status} \
                 {code} with {message:?}"
            );
            assert_eq!(
                &message, expected,
                "case {case:?} / {operation}: expected the message {expected:?}, got {message:?}"
            );
            // The modelled reason classifies the refusal, so a client branches on it
            // rather than on the sentence.
            let (_, body) = emulator.call(operation, &body).await;
            assert_eq!(
                body.get("reason").and_then(Value::as_str),
                Some(VALIDATION_REASON_INVALID_ROUTING_CONFIGURATION),
                "case {case:?} / {operation}: expected the routing-configuration reason, got \
                 {body:?}"
            );
        }
    }

    // The control: a configuration every rule admits is applied, and the alias it
    // created routes through it.
    let created = emulator
        .ok(
            "CreateStateMachineAlias",
            &json!({
                "name": "split",
                "routingConfiguration": [route(&first, 70), route(&second, 30)],
            }),
        )
        .await;
    assert_eq!(
        created.get("stateMachineAliasArn").and_then(Value::as_str),
        Some(format!("{machine}:split").as_str()),
        "case \"a configuration every rule admits\": expected the alias to be created, got \
         {created:?}"
    );
}

/// The routing configuration entries `entries` names, as the shape the state holds.
fn routing(
    entries: &[(&str, i32)],
) -> Vec<
    service_com_amazonaws_sfn_awsstepfunctions::ops::types::shared_types::RoutingConfigurationListItem,
> {
    entries
        .iter()
        .map(|(arn, weight)| {
            service_com_amazonaws_sfn_awsstepfunctions::ops::types::shared_types::RoutingConfigurationListItem {
                state_machine_version_arn: (*arn).to_string(),
                weight: *weight,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rule 4: the clauses a modelled constraint is stated by
// ---------------------------------------------------------------------------

/// A modelled constraint is stated by the standard validation clause, with the three
/// renderings this service's refusals show: an enumerated type's value set in the order
/// the service advertises it, a LIST member's own value, and a member breaking several
/// constraints stated with its `@pattern` requirement first, that expression being the
/// one a whole value must match.
///
/// The rows differ along the constraint class, along the member and along the operation
/// that carries it.
#[tokio::test]
async fn a_modelled_constraint_is_stated_by_this_services_clause() {
    let emulator = Emulator::new();
    let machine = machine_with_versions(&emulator, "clauses", 1).await;
    let execution = {
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": &machine, "name": "clause-run" }),
            )
            .await;
        started
            .get("executionArn")
            .and_then(Value::as_str)
            .expect("the execution ARN")
            .to_string()
    };

    // (case, the operation, the request, the whole message)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            // The value set is the service's own order, which is not the order the
            // model declares the enum's members in (`STANDARD | EXPRESS`).
            "a workflow type the enum does not admit",
            "CreateStateMachine",
            json!({
                "name": "bogus-type",
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "type": "BOGUS",
            }),
            clause(
                Some("BOGUS"),
                "type",
                "Member must satisfy enum value set: [EXPRESS, STANDARD]",
            ),
        ),
        (
            "the same enum on the validation of a definition",
            "ValidateStateMachineDefinition",
            json!({ "definition": HELLO_DEFINITION, "type": "BOGUS" }),
            clause(
                Some("BOGUS"),
                "type",
                "Member must satisfy enum value set: [EXPRESS, STANDARD]",
            ),
        ),
        (
            // A LIST member states the list it was handed, where the framework's own
            // clause states a summary of its size.
            "a routing configuration with no entry at all",
            "CreateStateMachineAlias",
            json!({ "name": "empty-routing", "routingConfiguration": [] }),
            clause(
                Some("[]"),
                "routingConfiguration",
                "Member must have length greater than or equal to 1",
            ),
        ),
        (
            // Two constraints of ONE member: the expression it must match is stated
            // first, unanchored, and the length bound after it.
            "a client token that is neither long enough nor made of the characters",
            "RedriveExecution",
            json!({ "executionArn": &execution, "clientToken": "" }),
            "2 validation errors detected: Value '' at 'clientToken' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: [!-~]+; Value '' at \
             'clientToken' failed to satisfy constraint: Member must have length greater \
             than or equal to 1"
                .to_string(),
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, code, message) = failure(&emulator, case, operation, &body).await;
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

    // The control: a client token the pattern and the length both admit passes the
    // constraint walk — what the execution's own state then answers (this one is still
    // RUNNING) is a different refusal entirely, and not a constraint clause.
    let redriven = emulator
        .call(
            "RedriveExecution",
            &json!({ "executionArn": &execution, "clientToken": "a-token" }),
        )
        .await;
    let message = redriven
        .1
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        !message.contains("failed to satisfy constraint"),
        "case \"a client token both constraints admit\": expected no constraint clause, got \
         {redriven:?}"
    );
    // And an alias name the pattern admits is created, which is the member whose
    // expression this service publishes exactly as its model declares it.
    let named = emulator
        .call(
            "CreateStateMachineAlias",
            &json!({
                "name": "9",
                "routingConfiguration": [route(&format!("{machine}:1"), 100)],
            }),
        )
        .await;
    let message = named
        .1
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(r"Member must satisfy regular expression pattern: ^(?=.*[a-zA-Z_\-\.])"),
        "case \"an alias name outside its own pattern\": expected the expression this service \
         publishes as declared, got {message:?}"
    );
    // The region is the endpoint's own, which the ARNs above are read in.
    assert!(
        machine.contains(REGION),
        "the machine under test is the endpoint region's: {machine}"
    );
}

/// The standard validation envelope over ONE clause: the value as the clause states it
/// (absent for a member whose value the refusal withholds), the member path and the
/// requirement.
fn clause(value: Option<&str>, member: &str, requirement: &str) -> String {
    let stated = match value {
        Some(value) => format!("Value '{value}' "),
        None => "Value ".to_string(),
    };
    format!("1 validation error detected: {stated}at '{member}' failed to satisfy constraint: {requirement}")
}
