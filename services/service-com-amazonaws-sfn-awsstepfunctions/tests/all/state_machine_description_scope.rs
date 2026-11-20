// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH resource a Step Functions description belongs to,
//! driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`, so what is asserted is what a signed AWS SDK request
//! observes.
//!
//! The rule under test: a description given when a VERSION is published — the
//! `versionDescription` of `CreateStateMachine` or `UpdateStateMachine` with
//! `publish`, or the `description` of `PublishStateMachineVersion` — belongs to that
//! immutable version snapshot alone. `DescribeStateMachine` reports it for the
//! version's own qualified ARN and carries no `description` member for the
//! unqualified state machine ARN, which no member of this API gives a description of
//! its own (`smithy_query(query="operation", operation="CreateStateMachine,
//! UpdateStateMachine,DescribeStateMachine")`: every input carries
//! `versionDescription` and none a state-machine `description`, and the output's
//! `description` targets `VersionDescription`).
//!
//! The harness (`Emulator`, `create_machine`, `string`) is the one
//! `state_machine_lifecycle` builds; it is reached through
//! `crate::state_machine_lifecycle` because `tests/all/main.rs` is this binary's
//! crate root.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, string, Emulator, HELLO_DEFINITION, ROLE_ARN,
};

/// A second definition, so a row that publishes through an update names a member
/// that makes the request an update ("you must include at least one of definition or
/// roleArn").
const GOODBYE_DEFINITION: &str =
    r#"{"StartAt":"Bye","States":{"Bye":{"Type":"Pass","Result":{"msg":"bye"},"End":true}}}"#;

/// Which call attached the description to the version.
#[derive(Clone, Copy, Debug)]
enum Published {
    /// `PublishStateMachineVersion(description = …)`.
    Publish,
    /// `CreateStateMachine(publish = true, versionDescription = …)`.
    Create,
    /// `UpdateStateMachine(publish = true, versionDescription = …)`.
    Update,
}

/// `DescribeStateMachine` on `arn`, which must succeed.
async fn describe(emulator: &Emulator, arn: &str) -> Value {
    emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
        .await
}

/// Publish one version of a state machine named `name` the way `how` says, with
/// `description` when the row carries one, and report
/// `(unqualified ARN, version ARN)`.
async fn publish_a_version(
    emulator: &Emulator,
    name: &str,
    how: Published,
    description: Option<&str>,
) -> (String, String) {
    match how {
        Published::Create => {
            let mut request = json!({
                "name": name,
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
                "publish": true,
            });
            if let Some(description) = description {
                request["versionDescription"] = json!(description);
            }
            let created = emulator.ok("CreateStateMachine", &request).await;
            (
                string(&created, "stateMachineArn"),
                string(&created, "stateMachineVersionArn"),
            )
        }
        Published::Update => {
            let arn = create_machine(emulator, name, HELLO_DEFINITION).await;
            let mut request = json!({
                "stateMachineArn": &arn,
                "definition": GOODBYE_DEFINITION,
                "publish": true,
            });
            if let Some(description) = description {
                request["versionDescription"] = json!(description);
            }
            let updated = emulator.ok("UpdateStateMachine", &request).await;
            let version_arn = string(&updated, "stateMachineVersionArn");
            (arn, version_arn)
        }
        Published::Publish => {
            let arn = create_machine(emulator, name, HELLO_DEFINITION).await;
            let mut request = json!({ "stateMachineArn": &arn });
            if let Some(description) = description {
                request["description"] = json!(description);
            }
            let published = emulator
                .ok("PublishStateMachineVersion", &request)
                .await;
            let version_arn = string(&published, "stateMachineVersionArn");
            (arn, version_arn)
        }
    }
}

/// A published version's description is reported for the VERSION ARN and never for
/// the unqualified state machine ARN, whichever call published the version and
/// whatever text it carried.
#[tokio::test]
async fn a_version_description_never_reaches_the_unqualified_state_machine() {
    let emulator = Emulator::new();

    // Rows vary along the call that published the version and along the description
    // itself, spanning the modelled `VersionDescription` length (0..256, from
    // `smithy_query(query="shape", shape="VersionDescription")`): absent, the empty
    // string at the lower bound, ordinary text, text outside ASCII, and exactly the
    // upper bound.
    let longest = "d".repeat(256);
    let cases: Vec<(&str, Published, Option<&str>)> = vec![
        ("a publish with ordinary text", Published::Publish, Some("v1 snapshot")),
        ("a publish with no description", Published::Publish, None),
        ("a publish with the empty description", Published::Publish, Some("")),
        (
            "a publish with text outside ASCII",
            Published::Publish,
            Some("プロダクション — v1 ✅"),
        ),
        (
            "a publish with the longest description",
            Published::Publish,
            Some(longest.as_str()),
        ),
        ("a create that published", Published::Create, Some("the first cut")),
        ("a create that published with no description", Published::Create, None),
        (
            "a create that published the longest description",
            Published::Create,
            Some(longest.as_str()),
        ),
        ("an update that published", Published::Update, Some("after the update")),
        ("an update that published the empty description", Published::Update, Some("")),
    ];

    for (index, (case, how, description)) in cases.iter().enumerate() {
        let (arn, version_arn) =
            publish_a_version(&emulator, &format!("described-{index}"), *how, *description).await;

        // The version ARN is where the description lives.
        let version = describe(&emulator, &version_arn).await;
        assert_eq!(
            version.get("description").and_then(Value::as_str),
            *description,
            "case {case:?}: expected the version to carry the description {description:?}, \
             got {version:?}"
        );

        // The unqualified ARN carries no `description` MEMBER at all — not an empty
        // one, and not the version's.
        let base = describe(&emulator, &arn).await;
        assert!(
            base.get("description").is_none(),
            "case {case:?}: expected no description member on the unqualified ARN, got {:?} \
             in {base:?}",
            base.get("description")
        );
        assert_eq!(
            base.get("stateMachineArn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected the unqualified ARN to describe itself, got {base:?}"
        );
    }
}

/// Publishing is a side-effect-free snapshot of the state machine's own metadata: a
/// publish changes nothing the unqualified ARN reports, and a later update of the
/// state machine changes nothing the version reports.
#[tokio::test]
async fn publishing_leaves_the_state_machine_metadata_alone() {
    let emulator = Emulator::new();

    // Rows vary along how many versions, each with its own description, were
    // published before the state machine's own metadata is read back.
    let cases: Vec<(&str, Vec<Option<&str>>)> = vec![
        ("one version with a description", vec![Some("v1 snapshot")]),
        (
            "three versions, one of them without a description",
            vec![Some("first"), None, Some("third")],
        ),
    ];

    for (index, (case, descriptions)) in cases.iter().enumerate() {
        let arn = create_machine(&emulator, &format!("frozen-{index}"), HELLO_DEFINITION).await;
        let before = describe(&emulator, &arn).await;

        let mut versions: Vec<(Option<&str>, String)> = Vec::new();
        for (round, description) in descriptions.iter().enumerate() {
            // Each version needs its own revision, since publishing is idempotent per
            // revision: the update that makes one also carries the publish.
            let definition = if round % 2 == 0 {
                GOODBYE_DEFINITION
            } else {
                HELLO_DEFINITION
            };
            let mut request = json!({
                "stateMachineArn": &arn,
                "definition": definition,
                "publish": true,
            });
            if let Some(description) = description {
                request["versionDescription"] = json!(description);
            }
            let updated = emulator.ok("UpdateStateMachine", &request).await;
            versions.push((*description, string(&updated, "stateMachineVersionArn")));
        }

        let after = describe(&emulator, &arn).await;
        assert!(
            after.get("description").is_none(),
            "case {case:?}: expected no description member after {} publishes, got {after:?}",
            descriptions.len()
        );
        for member in ["stateMachineArn", "name", "type", "creationDate"] {
            assert_eq!(
                before.get(member),
                after.get(member),
                "case {case:?}: expected {member} to be untouched by the publishes, \
                 got {:?} where it was {:?}",
                after.get(member),
                before.get(member)
            );
        }

        // Every version still reports the description its own publish gave it, and
        // the definition frozen at that instant.
        for (round, (description, version_arn)) in versions.iter().enumerate() {
            let version = describe(&emulator, version_arn).await;
            assert_eq!(
                version.get("description").and_then(Value::as_str),
                *description,
                "case {case:?}: expected version {} to keep the description {description:?}, \
                 got {version:?}",
                round + 1
            );
            let expected = if round % 2 == 0 {
                GOODBYE_DEFINITION
            } else {
                HELLO_DEFINITION
            };
            assert_eq!(
                version.get("definition").and_then(Value::as_str),
                Some(expected),
                "case {case:?}: expected version {} to keep its published definition, \
                 got {version:?}",
                round + 1
            );
        }
    }
}
