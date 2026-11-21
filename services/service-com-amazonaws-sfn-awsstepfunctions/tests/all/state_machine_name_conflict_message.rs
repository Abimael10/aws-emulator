// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TEXT of the refusal a `CreateStateMachine` earns when the
//! name it asks for is already held by a state machine the request does not describe,
//! driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test is the service's general one for a refusal ABOUT a resource the
//! request named: the message is the condition in spaced Title Case, then `: ` and the
//! ARN of the resource the condition is about, in single quotes — the same shape
//! `State Machine Does Not Exist: '<arn>'` and `Execution Already Exists: '<arn>'`
//! carry. So the conflict reads `State Machine Already Exists: '<arn>'`, where the ARN
//! is the one the store holds for the name, not a sentence summarising the rule and not
//! a value echoed back from the request.
//!
//! The sentence is spelled LITERALLY here rather than read from the service's constants,
//! so a reworded constant fails as well as a varying one.
//!
//! The harness (`Emulator`, `create_machine`, `string`) is the one
//! `state_machine_lifecycle` already builds, and `failure` the one `error_messages`
//! already builds; `tests/all/main.rs` is this binary's crate root, so both are reached
//! through `crate::<sibling>`.

use serde_json::{json, Value};

use crate::error_messages::failure;
use crate::state_machine_lifecycle::{
    create_machine, string, Emulator, ACCOUNT, HELLO_DEFINITION, REGION, ROLE_ARN,
};

/// A definition different from [`HELLO_DEFINITION`], so a repeat create carrying it is
/// the conflict rather than the documented idempotent hit.
const OTHER_DEFINITION: &str =
    r#"{"StartAt":"Bye","States":{"Bye":{"Type":"Pass","Result":{"msg":"bye"},"End":true}}}"#;

/// The conflict quotes the ARN of the state machine already holding the name, whatever
/// the request varies: the name asked for, and WHICH member of the configuration the
/// repeat differs in.
///
/// Each row creates its own state machine and then asks for the same name again under a
/// configuration the store does not hold, so the expected ARN is the one the create
/// answered rather than a string this test assembles.
#[tokio::test]
async fn the_name_conflict_quotes_the_state_machines_arn() {
    let emulator = Emulator::new();

    // Rows differ along the NAME (length and the punctuation `Name`'s pattern allows)
    // and along which member of the configuration the repeat changes — the definition,
    // the workflow type, a tracing configuration, a logging configuration, or a
    // published version — because the rule is about the refusal's wording rather than
    // about any one member of the create.
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "a different definition",
            "conflict-definition",
            json!({ "definition": OTHER_DEFINITION, "roleArn": ROLE_ARN }),
        ),
        (
            "a different workflow type",
            "c",
            json!({ "definition": HELLO_DEFINITION, "roleArn": ROLE_ARN, "type": "EXPRESS" }),
        ),
        (
            "an added tracing configuration",
            "conflict_with_underscores.and.dots",
            json!({
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "tracingConfiguration": { "enabled": true },
            }),
        ),
        (
            "an added logging configuration",
            "conflict-on-a-rather-long-state-machine-name-that-is-still-inside-the-bound",
            json!({
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "loggingConfiguration": {
                    "level": "ALL",
                    "includeExecutionData": false,
                    "destinations": [{ "cloudWatchLogsLogGroup": { "logGroupArn":
                        format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:/sfn:*") } }],
                },
            }),
        ),
        (
            "a published version the stored machine never had",
            "conflict-publish",
            json!({ "definition": HELLO_DEFINITION, "roleArn": ROLE_ARN, "publish": true }),
        ),
    ];

    for (case, name, mut repeat) in cases {
        let arn = create_machine(&emulator, name, HELLO_DEFINITION).await;
        repeat["name"] = json!(name);
        let (status, code, message) = failure(&emulator, case, "CreateStateMachine", &repeat).await;
        let expected = format!("State Machine Already Exists: '{arn}'");
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "StateMachineAlreadyExists", expected.as_str()),
            "case {case:?}: expected 400 StateMachineAlreadyExists {expected:?}, got {status} \
             {code} {message:?}"
        );
    }
}

/// The quoted ARN is read off the state machine the store holds, so it carries the
/// region the request signs for and names the state machine itself — never a version of
/// it, and never the region of some other client of the same store.
#[tokio::test]
async fn the_quoted_arn_is_the_stored_state_machines_own() {
    // One store, two clients signing for different regions: a name is in use only
    // within the region it was created in, so each client's conflict quotes its OWN ARN.
    let west = Emulator::new();
    let frankfurt = Emulator::in_region_on(&west, "eu-central-1");
    let name = "conflict-per-region";

    for (case, emulator, region) in [
        ("the region the store was created in", &west, REGION),
        ("another region over the same store", &frankfurt, "eu-central-1"),
    ] {
        // Created WITH a published version, so the store holds a version ARN too: the
        // refusal must still quote the state machine's unqualified ARN.
        let created = emulator
            .ok(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HELLO_DEFINITION,
                    "roleArn": ROLE_ARN,
                    "publish": true,
                }),
            )
            .await;
        let arn = string(&created, "stateMachineArn");
        let version = string(&created, "stateMachineVersionArn");
        assert_eq!(
            arn,
            format!("arn:aws:states:{region}:{ACCOUNT}:stateMachine:{name}"),
            "case {case:?}: expected the create to answer the region's own ARN, got {arn:?}"
        );

        let (status, code, message) = failure(
            emulator,
            case,
            "CreateStateMachine",
            &json!({ "name": name, "definition": OTHER_DEFINITION, "roleArn": ROLE_ARN }),
        )
        .await;
        let expected = format!("State Machine Already Exists: '{arn}'");
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "StateMachineAlreadyExists", expected.as_str()),
            "case {case:?}: expected 400 StateMachineAlreadyExists {expected:?}, got {status} \
             {code} {message:?}"
        );
        assert!(
            !message.contains(version.as_str()),
            "case {case:?}: expected the state machine's ARN, not its version's, got {message:?}"
        );
    }
}

/// The repeat that IS the same request is still served, so the conflict is raised by the
/// configuration differing rather than by the name being in use at all.
#[tokio::test]
async fn an_identical_repeat_is_still_idempotent() {
    let emulator = Emulator::new();
    let request = json!({
        "name": "conflict-idempotent",
        "definition": HELLO_DEFINITION,
        "roleArn": ROLE_ARN,
    });
    let created = emulator.ok("CreateStateMachine", &request).await;
    let again = emulator.ok("CreateStateMachine", &request).await;
    assert_eq!(
        again, created,
        "case \"an identical repeat\": expected the first response again, got {again:?}"
    );

    // And the record the refusal would quote is the one a read answers for.
    let arn = string(&created, "stateMachineArn");
    let described = emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": &arn }))
        .await;
    assert_eq!(
        described.get("stateMachineArn").and_then(Value::as_str),
        Some(arn.as_str()),
        "case \"a read of the conflicting record\": expected {arn:?}, got {described:?}"
    );
}
