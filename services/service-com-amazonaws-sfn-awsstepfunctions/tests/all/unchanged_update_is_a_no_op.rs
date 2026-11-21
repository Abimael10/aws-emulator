// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateStateMachine` that asks for NOTHING THE STATE MACHINE DOES NOT
//! ALREADY HOLD changes, driven over this service's own wire protocol through the
//! production `MultiServiceRouter`.
//!
//! The rule under test: an update applies the members it names, and a member whose
//! value the state machine already holds changes nothing — so such an update writes no
//! new configuration: the state machine keeps the revision it is on and the
//! `updateDate` it already reports, which for a state machine nothing has ever changed
//! is its creation. "It doesn't create a duplicate state machine version if it already
//! exists for the current revision", so a publish asked for in the same call answers
//! the version that revision was already published as.
//!
//! Why it matters: a client that detects a real change by comparing `updateDate`
//! against `creationDate` — or by comparing revision ids — must not be told about a
//! change that did not happen, and a deploy that re-applies the same template must not
//! mint a version per re-apply.
//!
//! The rows differ along WHICH member the update names at the value the state machine
//! already holds (the definition, the role, each configuration structure, the version
//! description, every one of them at once), and the changing rows along which member
//! is moved — the two halves of the same rule.
//!
//! The harness (`Emulator`, `create_machine`, `string`) is the one
//! `state_machine_lifecycle` builds; `tests/all.rs` is this binary's crate root, so it
//! is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, string, Emulator, HELLO_DEFINITION, ROLE_ARN,
};

/// A second definition, for the rows that really change the state machine.
const OTHER_DEFINITION: &str =
    r#"{"StartAt":"Other","States":{"Other":{"Type":"Pass","Result":{"msg":"other"},"End":true}}}"#;

/// `DescribeStateMachine` for `arn`.
async fn describe(emulator: &Emulator, arn: &str) -> Value {
    emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
        .await
}

/// An update that names only values the state machine already holds reports the
/// `updateDate` it already had — which for a state machine nothing has changed is its
/// `creationDate` — and no new revision.
#[tokio::test]
async fn an_update_that_asks_for_nothing_new_moves_nothing() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "unchanged", HELLO_DEFINITION).await;
    let created = describe(&emulator, &machine).await;
    let creation_date = created
        .get("creationDate")
        .cloned()
        .unwrap_or_else(|| panic!("expected a creationDate in {created:?}"));

    // Each row names one member — or several at once — at the value the record holds.
    let rows: Vec<(&str, Value)> = vec![
        ("the definition it already has", json!({ "definition": HELLO_DEFINITION })),
        ("the role it already has", json!({ "roleArn": ROLE_ARN })),
        (
            "the logging configuration it is effectively on",
            json!({ "loggingConfiguration": { "level": "OFF", "includeExecutionData": false } }),
        ),
        (
            "the tracing configuration it is effectively on",
            json!({ "tracingConfiguration": { "enabled": false } }),
        ),
        (
            // An update that names only an `encryptionConfiguration` names none of the
            // members this call requires at least one of (`refusal_vocabulary` is that
            // rule), so the row names it beside the definition the record already has.
            "the encryption configuration it is effectively on",
            json!({
                "definition": HELLO_DEFINITION,
                "encryptionConfiguration": { "type": "AWS_OWNED_KEY" },
            }),
        ),
        (
            "every one of them at once",
            json!({
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "loggingConfiguration": { "level": "OFF", "includeExecutionData": false },
                "tracingConfiguration": { "enabled": false },
                "encryptionConfiguration": { "type": "AWS_OWNED_KEY" },
            }),
        ),
    ];

    for (case, members) in rows {
        let mut request = json!({ "stateMachineArn": &machine });
        for (member, value) in members.as_object().expect("the row's members") {
            request[member] = value.clone();
        }
        let answered = emulator.ok("UpdateStateMachine", &request).await;
        assert_eq!(
            answered.get("updateDate"),
            Some(&creation_date),
            "case {case:?}: an update that changes nothing reports the updateDate the record \
             already had, got {answered:?}"
        );
        assert!(
            answered.get("revisionId").is_none_or(Value::is_null),
            "case {case:?}: an update that changes nothing mints no revision, got {answered:?}"
        );
        // And the record is unchanged: the describe reports the same thing it did
        // before the update.
        let described = describe(&emulator, &machine).await;
        assert_eq!(
            described, created,
            "case {case:?}: the record must be exactly as it was, got {described:?}"
        );
    }
}

/// An update that names a value the state machine does NOT hold changes it: the
/// `updateDate` moves off the creation and a fresh revision is minted, which is what
/// makes the rule above about asking for nothing new.
#[tokio::test]
async fn an_update_that_asks_for_something_new_moves_the_record() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "changed", HELLO_DEFINITION).await;
    let created = describe(&emulator, &machine).await;

    // The rows differ along WHICH member the update moves.
    let rows: Vec<(&str, Value)> = vec![
        ("another definition", json!({ "definition": OTHER_DEFINITION })),
        (
            "another role",
            json!({ "roleArn": format!("{ROLE_ARN}-other") }),
        ),
        (
            "execution data its logging configuration did not carry",
            json!({ "loggingConfiguration": { "level": "OFF", "includeExecutionData": true } }),
        ),
        (
            "tracing it did not enable",
            json!({ "tracingConfiguration": { "enabled": true } }),
        ),
    ];

    let mut revisions: Vec<String> = Vec::new();
    for (case, members) in rows {
        let mut request = json!({ "stateMachineArn": &machine });
        for (member, value) in members.as_object().expect("the row's members") {
            request[member] = value.clone();
        }
        let answered = emulator.ok("UpdateStateMachine", &request).await;
        let revision = string(&answered, "revisionId");
        assert!(
            !revisions.contains(&revision),
            "case {case:?}: every real change mints a revision of its own, got {revision} again"
        );
        revisions.push(revision.clone());
        // What the change wrote is readable back, under the revision the update
        // answered: the record moved, which is what distinguishes it from the rows
        // above. The timestamps are not compared here — two calls can fall in the same
        // instant — the REVISION is what a client reads a change by.
        let described = describe(&emulator, &machine).await;
        assert_eq!(
            described.get("revisionId").and_then(Value::as_str),
            Some(revision.as_str()),
            "case {case:?}: the record must report the revision the update minted, got \
             {described:?}"
        );
        assert_ne!(
            described.get("revisionId"),
            created.get("revisionId"),
            "case {case:?}: a real change moves the record off the one it was created \
             with, got {described:?}"
        );
    }
}

/// With `publish`, an update that asks for nothing new answers the version its current
/// revision was already published as, and one that asks for something new publishes the
/// next version.
#[tokio::test]
async fn publishing_an_unchanged_revision_answers_the_version_it_already_has() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "republished", HELLO_DEFINITION).await;

    let first = emulator
        .ok(
            "UpdateStateMachine",
            &json!({
                "stateMachineArn": &machine,
                "definition": OTHER_DEFINITION,
                "publish": true,
            }),
        )
        .await;
    assert_eq!(
        string(&first, "stateMachineVersionArn"),
        format!("{machine}:1"),
        "the first publish takes version 1, got {first:?}"
    );

    // The rows differ along what the repeat carries beside the unchanged definition:
    // nothing, and the same version description — each of them asks for a version the
    // state machine already holds.
    for (case, extra) in [
        ("the same definition again", json!({})),
        (
            "the same definition and no description again",
            json!({ "definition": OTHER_DEFINITION }),
        ),
    ] {
        let mut request = json!({
            "stateMachineArn": &machine,
            "definition": OTHER_DEFINITION,
            "publish": true,
        });
        for (member, value) in extra.as_object().expect("the row's members") {
            request[member] = value.clone();
        }
        let answered = emulator.ok("UpdateStateMachine", &request).await;
        assert_eq!(
            string(&answered, "stateMachineVersionArn"),
            format!("{machine}:1"),
            "case {case:?}: a publish of the unchanged revision answers the version it already \
             has, got {answered:?}"
        );
    }
    let listed = emulator
        .ok(
            "ListStateMachineVersions",
            &json!({ "stateMachineArn": &machine }),
        )
        .await;
    assert_eq!(
        listed
            .get("stateMachineVersions")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "no repeat may mint a second snapshot of one revision, got {listed:?}"
    );

    // And a publish that asks for something new takes the next number. The rows differ
    // along WHAT is new: the version's own description — RECORDED, an update that names
    // a `versionDescription` the state machine does not already carry, with its
    // definition and role unchanged, moves it off the revision it was on — and then the
    // definition itself.
    let next = emulator
        .ok(
            "UpdateStateMachine",
            &json!({
                "stateMachineArn": &machine,
                "definition": OTHER_DEFINITION,
                "versionDescription": "the second",
                "publish": true,
            }),
        )
        .await;
    assert_eq!(
        string(&next, "stateMachineVersionArn"),
        format!("{machine}:2"),
        "a publish that asks for a version the machine does not carry takes the next number, got \
         {next:?}"
    );
    let described = emulator
        .ok(
            "DescribeStateMachine",
            &json!({ "stateMachineArn": format!("{machine}:2") }),
        )
        .await;
    assert_eq!(
        described.get("description").and_then(Value::as_str),
        Some("the second"),
        "the version carries the description its publish gave it, got {described:?}"
    );
    let third = emulator
        .ok(
            "UpdateStateMachine",
            &json!({
                "stateMachineArn": &machine,
                "definition": HELLO_DEFINITION,
                "versionDescription": "the second",
                "publish": true,
            }),
        )
        .await;
    assert_eq!(
        string(&third, "stateMachineVersionArn"),
        format!("{machine}:3"),
        "a publish of another definition takes the next number, got {third:?}"
    );
}
