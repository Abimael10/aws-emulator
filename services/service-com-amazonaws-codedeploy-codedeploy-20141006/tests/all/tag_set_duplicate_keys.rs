// SPDX-License-Identifier: CC0-1.0

//! A tag set a request names one key twice in, driven over the wire.
//!
//! RULE under test: the tags a call APPLIES to a CodeDeploy resource are keyed by
//! their key — a key already carried has its value replaced, never duplicated —
//! so a `Tags` list naming one key twice determines no tag set and is refused
//! with HTTP 400 `InvalidTagsToAddException` carrying `Duplicate Tag key: ` and
//! the repeated key, before anything is written: nothing is tagged, nothing is
//! created, and tags the resource already carried are left alone. Sending that
//! key once succeeds.
//!
//! The rule covers every operation that applies a tag set to a resource and
//! models that error — `TagResource`, `CreateApplication` and
//! `CreateDeploymentGroup` — so each is driven here.
//!
//! Every case goes through the same front door a client uses — the shared
//! [`Emulator`] harness of `codedeploy_lifecycle` — so the boundary validator,
//! this service's refusal wording and the awsJson1_1 envelope are exercised.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{error_code, seeded_group, Emulator};

use service_com_amazonaws_codedeploy_codedeploy_20141006::state;

/// The region the harness's unsigned calls are served in, which is the one the
/// service builds an ARN with when a request carries no credential scope.
const REGION: &str = "us-east-1";

/// A `Tags` list entry, as CodeDeploy's `Tag` shape names its members
/// (`smithy_query(query="shape",shape="Tag")`: `Key`, `Value`).
fn tag(key: &str, value: &str) -> Value {
    json!({"Key": key, "Value": value})
}

/// A `Tags` list from `(key, value)` pairs, in the order the request sends them.
fn tags(pairs: &[(&str, &str)]) -> Vec<Value> {
    pairs.iter().map(|(key, value)| tag(key, value)).collect()
}

/// The `$.message` of a failure body: the sentence the refusal carries.
fn message(case: &str, body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a string $.message, got {body}"))
        .to_string()
}

/// The `(key, value)` pairs `ListTagsForResource` reports for `resource_arn`, in
/// report order, requiring the read to succeed.
async fn tags_of(emulator: &Emulator, resource_arn: &str) -> Vec<(String, String)> {
    let listed = emulator
        .ok("ListTagsForResource", json!({"ResourceArn": resource_arn}))
        .await;
    listed["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["Key"].as_str().unwrap_or_default().to_string(),
                entry["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// One row of the duplicate table: the case name, the `(key, value)` tags the
/// request sends, and the key the refusal has to name.
type DuplicateCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);

/// The duplicate-key rows every operation under the rule is driven with.
///
/// Rows differ along WHERE the repeat sits in the list, how long the list is,
/// whether the repeated entries carry the same value, how many times the key
/// repeats, and which key of several repeats first.
const DUPLICATES: &[DuplicateCase<'static>] = &[
    (
        "two values for one key",
        &[("owner", "platform"), ("owner", "infra")],
        "owner",
    ),
    (
        "the same value twice",
        &[("owner", "platform"), ("owner", "platform")],
        "owner",
    ),
    (
        "the repeat sits after another key",
        &[("Env", "prod"), ("owner", "platform"), ("owner", "infra")],
        "owner",
    ),
    (
        "the repeat straddles other keys",
        &[
            ("Env", "prod"),
            ("Tier", "api"),
            ("Env", "dev"),
            ("Owner", "ana"),
        ],
        "Env",
    ),
    (
        "three entries for one key",
        &[("Env", "a"), ("Env", "b"), ("Env", "c")],
        "Env",
    ),
    (
        "two repeated keys: the first repeat is named",
        &[
            ("Owner", "ana"),
            ("Env", "prod"),
            ("Owner", "bo"),
            ("Env", "dev"),
        ],
        "Owner",
    ),
    (
        "a key whose value is empty",
        &[("Marker", ""), ("Marker", "set")],
        "Marker",
    ),
];

/// The tag sets the rule ACCEPTS, each a list in which every key is distinct:
/// the case name and its `(key, value)` pairs.
///
/// Rows differ along the length of the list, and include the positive control the
/// duplicate rows are built from (one entry for the repeated key) and two keys
/// that differ only in case, which are two keys and not a repeat.
const DISTINCT: &[(&str, &[(&str, &str)])] = &[
    ("the repeated key, sent once", &[("owner", "platform")]),
    ("two distinct keys", &[("Env", "prod"), ("Owner", "ana")]),
    ("keys differing only in case", &[("Env", "prod"), ("env", "dev")]),
    ("no tags at all", &[]),
];

/// RULE: `TagResource` refuses a `Tags` list that names one key twice, naming
/// that key, and tags nothing.
#[tokio::test]
async fn tag_resource_refuses_a_repeated_key_and_tags_nothing() {
    let emulator = Emulator::new();
    emulator
        .ok("CreateApplication", json!({"applicationName": "dup-app"}))
        .await;
    let group = seeded_group(&emulator, "dup-group-app", "Server").await;

    // The same rows are driven against each taggable resource kind, by the ARN a
    // client names it with.
    let resources = [
        ("application", state::application_arn(REGION, "dup-app")),
        (
            "deployment group",
            state::deployment_group_arn(REGION, "dup-group-app", &group),
        ),
    ];

    for (kind, resource_arn) in &resources {
        for (case, pairs, repeated) in DUPLICATES {
            let case = format!("{kind}/{case}");
            let (status, body) = emulator
                .call(
                    "TagResource",
                    json!({"ResourceArn": resource_arn, "Tags": tags(pairs)}),
                )
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for a repeated tag key, got {status} {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidTagsToAddException",
                "case {case:?}: expected InvalidTagsToAddException, got {body:?}"
            );
            let got = message(&case, &body);
            let expected = format!("Duplicate Tag key: {repeated}");
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?}, got {got:?}"
            );
            // Nothing was tagged: the resource still reports no tag at all.
            let reported = tags_of(&emulator, resource_arn).await;
            assert!(
                reported.is_empty(),
                "case {case:?}: a refused call must tag nothing, got {reported:?}"
            );
        }
    }
}

/// RULE: a list whose keys are all distinct is applied — including the very key a
/// duplicate row repeated, sent once — and a later refused call leaves the tags
/// an accepted one applied exactly as they were.
#[tokio::test]
async fn a_distinct_tag_list_is_applied_and_a_refusal_leaves_it_alone() {
    let emulator = Emulator::new();

    for (index, (case, pairs)) in DISTINCT.iter().enumerate() {
        // One application per row, so each row's reported tags are its own.
        let application = format!("distinct-app-{index}");
        emulator
            .ok(
                "CreateApplication",
                json!({"applicationName": application.clone()}),
            )
            .await;
        let resource_arn = state::application_arn(REGION, &application);
        let (status, body) = emulator
            .call(
                "TagResource",
                json!({"ResourceArn": resource_arn, "Tags": tags(pairs)}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a list of distinct keys must be applied, got {status} {body:?}"
        );
        let expected: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let reported = tags_of(&emulator, &resource_arn).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: the resource reports the tags it was given, got {reported:?}"
        );

        // A refused duplicate call afterwards changes none of them.
        let (status, body) = emulator
            .call(
                "TagResource",
                json!({
                    "ResourceArn": resource_arn,
                    "Tags": tags(&[("owner", "platform"), ("owner", "infra")]),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidTagsToAddException"),
            "case {case:?}: expected the duplicate to be refused, got {status} {body:?}"
        );
        let reported = tags_of(&emulator, &resource_arn).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: a refused call must leave the applied tags alone, got {reported:?}"
        );
    }
}

/// RULE: a create that applies a tag set follows the same rule — a `tags` list
/// naming one key twice is refused with `InvalidTagsToAddException`, and the
/// resource is not created at all.
#[tokio::test]
async fn create_application_refuses_a_repeated_key_and_creates_nothing() {
    let emulator = Emulator::new();

    for (index, (case, pairs, repeated)) in DUPLICATES.iter().enumerate() {
        let application = format!("dup-created-app-{index}");
        let (status, body) = emulator
            .call(
                "CreateApplication",
                json!({
                    "applicationName": application.clone(),
                    "computePlatform": "Server",
                    "tags": tags(pairs),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a repeated tag key, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidTagsToAddException",
            "case {case:?}: expected InvalidTagsToAddException, got {body:?}"
        );
        let got = message(case, &body);
        let expected = format!("Duplicate Tag key: {repeated}");
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?}"
        );
        // Nothing was created: the name is still free, which a read reports and a
        // create under the same name with ONE entry for that key proves.
        let (status, body) = emulator
            .call(
                "GetApplication",
                json!({"applicationName": application.clone()}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ApplicationDoesNotExistException"),
            "case {case:?}: a refused create must store nothing, got {status} {body:?}"
        );
    }

    // The positive control: the same key sent once creates the application and
    // the tag is on it.
    emulator
        .ok(
            "CreateApplication",
            json!({
                "applicationName": "created-with-one-owner",
                "computePlatform": "Server",
                "tags": tags(&[("owner", "platform")]),
            }),
        )
        .await;
    let reported = tags_of(
        &emulator,
        &state::application_arn(REGION, "created-with-one-owner"),
    )
    .await;
    assert_eq!(
        reported,
        vec![("owner".to_string(), "platform".to_string())],
        "case \"the key sent once\": expected the created application to carry it, got \
         {reported:?}"
    );
}

/// RULE: the same holds for a deployment group's own tag set — the group is not
/// created, so the application still holds no group under that name.
#[tokio::test]
async fn create_deployment_group_refuses_a_repeated_key_and_creates_nothing() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": "dup-group-owner", "computePlatform": "Server"}),
        )
        .await;

    for (index, (case, pairs, repeated)) in DUPLICATES.iter().enumerate() {
        let group = format!("dup-group-{index}");
        let (status, body) = emulator
            .call(
                "CreateDeploymentGroup",
                json!({
                    "applicationName": "dup-group-owner",
                    "deploymentGroupName": group.clone(),
                    "serviceRoleArn": "arn:aws:iam::123456789012:role/CodeDeployRole",
                    "tags": tags(pairs),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a repeated tag key, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidTagsToAddException",
            "case {case:?}: expected InvalidTagsToAddException, got {body:?}"
        );
        let got = message(case, &body);
        let expected = format!("Duplicate Tag key: {repeated}");
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?}"
        );
        let (status, body) = emulator
            .call(
                "GetDeploymentGroup",
                json!({
                    "applicationName": "dup-group-owner",
                    "deploymentGroupName": group.clone(),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "DeploymentGroupDoesNotExistException"),
            "case {case:?}: a refused create must store nothing, got {status} {body:?}"
        );
    }

    // The positive control: the same key sent once creates the group and the tag
    // reads back off it.
    emulator
        .ok(
            "CreateDeploymentGroup",
            json!({
                "applicationName": "dup-group-owner",
                "deploymentGroupName": "group-with-one-owner",
                "serviceRoleArn": "arn:aws:iam::123456789012:role/CodeDeployRole",
                "tags": tags(&[("owner", "platform")]),
            }),
        )
        .await;
    let reported = tags_of(
        &emulator,
        &state::deployment_group_arn(REGION, "dup-group-owner", "group-with-one-owner"),
    )
    .await;
    assert_eq!(
        reported,
        vec![("owner".to_string(), "platform".to_string())],
        "case \"the key sent once\": expected the created group to carry it, got {reported:?}"
    );
}
