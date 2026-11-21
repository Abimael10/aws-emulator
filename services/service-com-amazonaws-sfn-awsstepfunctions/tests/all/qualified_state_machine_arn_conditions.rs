// SPDX-License-Identifier: CC0-1.0

//! What a QUALIFIED state machine ARN — one carrying a version number or an alias
//! name — answers when the store holds no record for the reference it names, driven
//! over this service's own wire protocol through the production `MultiServiceRouter`.
//!
//! The rules under test:
//!
//! 1. the refusal names the resource the ARN REFERS to, whatever the operation then
//!    does with it: an ARN qualified with a VERSION number the store holds no version
//!    under states the version's condition — for a read of it, for a start of an
//!    execution through it and for a listing scoped to it — while an ALIAS qualifier
//!    keeps the state machine's condition, an alias being a route to a version rather
//!    than a resource of its own. The ARN is stated exactly as submitted;
//! 2. `ListExecutions` reports such a reference as a refusal — never as an empty page,
//!    which would make a version that never existed indistinguishable from one that
//!    ran nothing;
//! 3. `PublishStateMachineVersion` publishes "a version from the current revision of a
//!    state machine", which neither a version nor an alias is: such an ARN is a defect
//!    in the request, reported as `InvalidArn` naming the qualifier suffix.
//!
//! The rows differ along the QUALIFIER's kind (a version number never published, one
//! whose version was deleted, an alias name no alias was created under), along the
//! operation reading the ARN, and along the class that operation belongs to.
//!
//! The harness (`Emulator`, `create_machine`, `string`, `error_code`) is the one
//! `state_machine_lifecycle` builds; `tests/all.rs` is this binary's crate root, so it
//! is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    resource_error_message, unexpected_qualifier_message, STATE_MACHINE_DOES_NOT_EXIST,
    STATE_MACHINE_VERSION_DOES_NOT_EXIST,
};

use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, HELLO_DEFINITION,
};

/// A state machine with one published version that is then DELETED, plus the ARNs of
/// the references below: `(state machine, deleted version, number never published,
/// alias never created)`.
async fn machine_with_dangling_references(emulator: &Emulator) -> (String, String, String, String) {
    let machine = create_machine(emulator, "dangling-references", HELLO_DEFINITION).await;
    let published = emulator
        .ok(
            "PublishStateMachineVersion",
            &json!({ "stateMachineArn": &machine }),
        )
        .await;
    let deleted = string(&published, "stateMachineVersionArn");
    emulator
        .ok(
            "DeleteStateMachineVersion",
            &json!({ "stateMachineVersionArn": &deleted }),
        )
        .await;
    let never_published = format!("{machine}:4242");
    let never_created = format!("{machine}:no-such-alias");
    (machine, deleted, never_published, never_created)
}

/// An operation that starts an execution through a dangling reference reports the
/// condition of the resource the ARN refers to: the VERSION's for a version-qualified
/// ARN, the state machine's for an alias-qualified one.
#[tokio::test]
async fn a_start_through_a_dangling_reference_names_the_resource_the_arn_refers_to() {
    let emulator = Emulator::new();
    let (_machine, deleted, never_published, never_created) =
        machine_with_dangling_references(&emulator).await;

    // The rows differ along the qualifier's kind — which decides WHICH resource the ARN
    // referred to — and along which start operation reads it.
    let references = [
        (
            "a version whose version was deleted",
            &deleted,
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
        (
            "a version number never published",
            &never_published,
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
        (
            "an alias name no alias was created under",
            &never_created,
            STATE_MACHINE_DOES_NOT_EXIST,
        ),
    ];
    for (case, arn, condition) in references {
        for (operation, request) in [
            (
                "StartExecution",
                json!({ "stateMachineArn": arn, "input": "{}" }),
            ),
            (
                "StartSyncExecution",
                json!({ "stateMachineArn": arn, "input": "{}" }),
            ),
        ] {
            let (status, body) = emulator.call(operation, &request).await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (400, "StateMachineDoesNotExist"),
                "case {case:?} / {operation}: expected 400 StateMachineDoesNotExist, got {status} \
                 {code} with body {body:?}"
            );
            let expected = resource_error_message(condition, arn);
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?} / {operation}: expected the message {expected:?}, got {body:?}"
            );
        }
    }
}

/// A READ of the resource the ARN refers to reports THAT resource's condition: a
/// version-qualified ARN is about the version.
#[tokio::test]
async fn a_read_of_a_dangling_version_names_the_version() {
    let emulator = Emulator::new();
    let (_machine, deleted, never_published, never_created) =
        machine_with_dangling_references(&emulator).await;

    // The rows differ along the qualifier's kind, which is what decides WHICH resource
    // the read was about: a version number names a version, while an alias is a route
    // to one rather than a resource the read asked for.
    let cases = [
        (
            "a version whose version was deleted",
            &deleted,
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
        (
            "a version number never published",
            &never_published,
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
        (
            "an alias name no alias was created under",
            &never_created,
            STATE_MACHINE_DOES_NOT_EXIST,
        ),
    ];
    for (case, arn, condition) in cases {
        let (status, body) = emulator
            .call("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
            .await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "StateMachineDoesNotExist"),
            "case {case:?}: expected 400 StateMachineDoesNotExist, got {status} {code} with body \
             {body:?}"
        );
        let expected = resource_error_message(condition, arn);
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}

/// `ListExecutions` is about the state machine whose executions it lists, so a
/// qualifier it holds no version or alias under is reported as that state machine: an
/// empty page would make a version that never existed indistinguishable from one that
/// ran nothing.
#[tokio::test]
async fn listing_a_dangling_reference_is_refused_too() {
    let emulator = Emulator::new();
    let (machine, deleted, never_published, never_created) =
        machine_with_dangling_references(&emulator).await;
    // One execution of the state machine itself, so the refusals below are the
    // REFERENCE resolving to nothing rather than the state machine having nothing.
    emulator
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": &machine, "name": "listed", "input": "{}" }),
        )
        .await;
    let listed = emulator
        .ok("ListExecutions", &json!({ "stateMachineArn": &machine }))
        .await;
    assert_eq!(
        listed
            .get("executions")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "the state machine's own listing must report its execution, got {listed:?}"
    );

    // The rows differ along the qualifier's kind — which decides which resource the
    // reference named — and along whether the state machine behind the ARN is one the
    // store holds at all.
    let absent = format!("{machine}-never-created");
    for (case, arn, condition) in [
        (
            "a version whose version was deleted",
            deleted,
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
        (
            "a version number never published",
            never_published,
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
        (
            "an alias name no alias was created under",
            never_created,
            STATE_MACHINE_DOES_NOT_EXIST,
        ),
        (
            "an unqualified ARN that names nothing",
            absent.clone(),
            STATE_MACHINE_DOES_NOT_EXIST,
        ),
        (
            "a version of a state machine that names nothing",
            format!("{absent}:1"),
            STATE_MACHINE_VERSION_DOES_NOT_EXIST,
        ),
    ] {
        let (status, body) = emulator
            .call("ListExecutions", &json!({ "stateMachineArn": &arn }))
            .await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "StateMachineDoesNotExist"),
            "case {case:?}: expected 400 StateMachineDoesNotExist, got {status} {code} with body \
             {body:?}"
        );
        let expected = resource_error_message(condition, &arn);
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}

/// `PublishStateMachineVersion` takes the state machine itself: a version- or
/// alias-qualified ARN is `InvalidArn`, naming the qualifier suffix it cannot take.
#[tokio::test]
async fn publishing_a_qualified_arn_is_an_arn_defect() {
    let emulator = Emulator::new();
    let (machine, deleted, _never_published, never_created) =
        machine_with_dangling_references(&emulator).await;

    // The rows differ along the qualifier's kind AND along whether the reference
    // resolves: the ARN is judged before anything is resolved from it, so a version
    // that exists is refused exactly as one that never did.
    let republished = emulator
        .ok(
            "PublishStateMachineVersion",
            &json!({ "stateMachineArn": &machine }),
        )
        .await;
    let existing = string(&republished, "stateMachineVersionArn");
    for (case, arn) in [
        ("a version that exists", &existing),
        ("a version whose version was deleted", &deleted),
        ("an alias name no alias was created under", &never_created),
    ] {
        let (status, body) = emulator
            .call(
                "PublishStateMachineVersion",
                &json!({ "stateMachineArn": arn }),
            )
            .await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidArn"),
            "case {case:?}: expected 400 InvalidArn, got {status} {code} with body {body:?}"
        );
        let expected = unexpected_qualifier_message(arn);
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}
