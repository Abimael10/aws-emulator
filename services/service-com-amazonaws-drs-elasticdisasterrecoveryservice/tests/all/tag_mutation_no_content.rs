// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the answer a SERVED member-less success of this service
//! gives — the mutations of the tagging route (`/tags/{resourceArn}`) and, beside them, the
//! deletes that answer the same way.
//!
//! Every test drives the service the way the router does — one buffered `http::Request`
//! per call through [`MultiServiceRouter::dispatch`] — so what is asserted is the wire
//! behaviour (status, error code, body). The harness and the per-operation requests are
//! the ones `resource_tags` builds; what this suite is about is the STATUS and the document
//! of a served mutation, which `resource_tags` asserts through one shared constant.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! 1. A served tag write (`POST`) and a served tag removal (`DELETE`) answer the
//!    no-content status with the empty JSON object and no error code: neither carries a
//!    member for a caller to read. The rows cross every resource type the route addresses
//!    and holds with the shapes a mutation can take.
//! 2. The mutation really happened: the route's own read reports the set the rule says the
//!    resource carries after it, so the answer is a served write and not a refusal in
//!    disguise.
//! 3. Every member-less success of this service answers ALIKE — the two tag mutations and
//!    the deletes, whose status the model states — while the route's READ keeps its own
//!    class: `ListTagsForResource` answers 200 with the tag set in its body, because its
//!    output carries a member.

use serde_json::{json, Map, Value};
use service_com_amazonaws_drs_elasticdisasterrecoveryservice::state::{self, SourceServerRecord};
use smithy_server_runtime::MultiServiceRouter;

use crate::replication_configuration_template_lifecycle::{
    call_in, drs_with_engine, Answer, REGION,
};
use crate::resource_tags::{
    list_tags, plan_with_tags, reported_tags, tag_resource, template_with_tags, untag_resource,
    TAG_MUTATION_SUCCESS_STATUS,
};

/// What a resource the route addresses looks like to this suite: how the case is named and
/// the ARN the mutations are addressed to.
struct Addressed {
    case: &'static str,
    arn: String,
}

/// One resource of every type the route addresses AND this service holds, each created
/// carrying the same tag set, so a mutation can be shown to be served for each of them.
async fn addressed_resources(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    tags: &Value,
) -> Vec<Addressed> {
    let plan = plan_with_tags(router, REGION, Some(tags)).await;
    let template = template_with_tags(router, tags).await;

    // A source server is registered by the replication agent, which this emulator does not
    // model, so the one way to hold one is to write the store directly.
    let server_id = "s-00000000000000011";
    let server = engine::emulator_arn("drs", REGION, &format!("source-server/{server_id}"));
    {
        let store = state::service_state(&**engine).expect("the DRS store");
        let mut store = store.lock();
        store.source_servers.insert(
            server_id.to_string(),
            SourceServerRecord {
                source_server_id: server_id.to_string(),
                arn: server.clone(),
                tags: as_tag_map(tags),
                ..SourceServerRecord::default()
            },
        );
    }

    vec![
        Addressed {
            case: "a recovery plan",
            arn: plan,
        },
        Addressed {
            case: "a replication configuration template",
            arn: template,
        },
        Addressed {
            case: "a source server",
            arn: server,
        },
    ]
}

/// A JSON tag set as the store holds one.
fn as_tag_map(tags: &Value) -> std::collections::HashMap<String, String> {
    tags.as_object()
        .expect("a JSON object of tags")
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value.as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The document a member-less success of this service carries: a response structure with
/// no member, which `restJson1` renders as the empty JSON object — written out as what a
/// client reads rather than taken from the implementation.
fn empty_document() -> Value {
    json!({})
}

/// Assert that `answer` is a served member-less success of this service: the no-content
/// status, no error code, and the empty JSON object for a body.
fn assert_no_content(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, TAG_MUTATION_SUCCESS_STATUS,
        "case {case:?}: expected {TAG_MUTATION_SUCCESS_STATUS}, got {answer:?}"
    );
    assert!(
        answer.error_code.is_none(),
        "case {case:?}: a served mutation carries no error code, got {answer:?}"
    );
    assert_eq!(
        answer.body,
        empty_document(),
        "case {case:?}: expected the empty document, got {answer:?}"
    );
}

/// `stored` with every entry of `written` overlaid — the set the resource carries after a
/// write, computed from the rule rather than listed per row.
fn overlaid(stored: &Value, written: &Value) -> Value {
    let mut expected = stored
        .as_object()
        .expect("the stored set is a JSON object")
        .clone();
    for (key, value) in written.as_object().expect("the written set is a JSON object") {
        expected.insert(key.clone(), value.clone());
    }
    Value::Object(expected)
}

/// `stored` without the entries `removed` names.
fn without(stored: &Value, removed: &[&str]) -> Value {
    let mut expected = stored
        .as_object()
        .expect("the stored set is a JSON object")
        .clone();
    for key in removed {
        expected.remove(*key);
    }
    Value::Object(expected)
}

/// A one-entry tag map built from values a test holds, which `json!` cannot take as a key.
fn one_tag(key: &str, value: &str) -> Value {
    let mut map = Map::new();
    map.insert(key.to_string(), Value::String(value.to_string()));
    Value::Object(map)
}

/// The set every resource below is created carrying, so a removal always has a key to take
/// off and a write always has a key to overwrite.
fn created_with() -> Value {
    json!({"env": "prod", "team": "dr"})
}

/// A served WRITE answers the no-content status with the empty document for every resource
/// type the route addresses and every shape of written set, and the set the rule says the
/// resource then carries is what the route's read reports.
#[tokio::test]
async fn a_served_tag_write_answers_no_content_for_every_addressed_resource() {
    let longest_key = "k".repeat(256);
    let written_sets: Vec<(&str, Value)> = vec![
        ("no key at all", json!({})),
        ("one new key", json!({"tier": "1"})),
        ("several new keys", json!({"tier": "1", "owner": "ops"})),
        ("a key the resource already carries", json!({"env": "dr"})),
        (
            "a stored key and a new one",
            json!({"env": "dr", "tier": "1"}),
        ),
        ("an empty value", json!({"tier": ""})),
        (
            "a key and a value at the greatest modelled length",
            one_tag(&longest_key, &"v".repeat(256)),
        ),
    ];

    let (router, engine) = drs_with_engine();
    for (shape, written) in &written_sets {
        let stored = created_with();
        for resource in addressed_resources(&router, &engine, &stored).await {
            let case = format!("{} / {shape}", resource.case);
            let answer = tag_resource(&router, REGION, &resource.arn, written).await;
            assert_no_content(&case, &answer);

            let expected = overlaid(&stored, written);
            let read = list_tags(&router, REGION, &resource.arn).await;
            assert_eq!(
                reported_tags(&read),
                Some(&expected),
                "case {case:?}: expected {expected} after the write, got {read:?}"
            );
            // Rule 3: the route's read answers in its own class, with a body.
            assert_eq!(
                read.status, 200,
                "case {case:?}: the route's read answers 200, got {read:?}"
            );
        }
    }
}

/// A served REMOVAL answers the same way for every resource type the route addresses and
/// every shape of key list, including one that removes nothing.
#[tokio::test]
async fn a_served_tag_removal_answers_no_content_for_every_addressed_resource() {
    let removals: Vec<(&str, Vec<&str>)> = vec![
        ("one of the stored keys", vec!["env"]),
        ("every stored key", vec!["env", "team"]),
        ("a key the resource does not carry", vec!["absent"]),
        (
            "a stored key and one the resource does not carry",
            vec!["team", "absent"],
        ),
    ];

    let (router, engine) = drs_with_engine();
    for (shape, keys) in &removals {
        let stored = created_with();
        for resource in addressed_resources(&router, &engine, &stored).await {
            let case = format!("{} / {shape}", resource.case);
            let answer = untag_resource(&router, REGION, &resource.arn, keys).await;
            assert_no_content(&case, &answer);

            let expected = without(&stored, keys);
            let read = list_tags(&router, REGION, &resource.arn).await;
            assert_eq!(
                reported_tags(&read),
                Some(&expected),
                "case {case:?}: expected {expected} after the removal, got {read:?}"
            );

            // Repeating a removal is served the same way: nothing left to remove is not a
            // refusal.
            let again = untag_resource(&router, REGION, &resource.arn, keys).await;
            assert_no_content(&format!("{case} repeated"), &again);
        }
    }
}

/// EVERY member-less success of this service answers alike: the two mutations of the
/// tagging route and the deletes, whose status the model itself states, are one answer —
/// so a client that reads a delete's answer reads a tag mutation's answer the same way.
/// The rows are the operations of this service whose output declares no member.
#[tokio::test]
async fn every_member_less_success_of_this_service_answers_alike() {
    let (router, engine) = drs_with_engine();
    let stored = created_with();
    let addressed = addressed_resources(&router, &engine, &stored).await;
    let plan = &addressed
        .first()
        .expect("the addressed resources start with the recovery plan")
        .arn;

    // The two resources the deletes below give up, each minted for this test.
    let launch_template = call_in(
        &router,
        REGION,
        "CreateLaunchConfigurationTemplate",
        &json!({}),
    )
    .await;
    assert_eq!(
        launch_template.status, 201,
        "setup: creating a launch configuration template, got {launch_template:?}"
    );
    let launch_template_id = launch_template
        .member("launchConfigurationTemplate")
        .and_then(|template| template.get("launchConfigurationTemplateID"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the created template's id, got {launch_template:?}"))
        .to_string();

    // `InitializeService` is the account-level no-op whose success carries no member
    // either, so it belongs to the same rule.
    let answers = [
        (
            "TagResource",
            tag_resource(&router, REGION, plan, &json!({"tier": "1"})).await,
        ),
        (
            "UntagResource",
            untag_resource(&router, REGION, plan, &["tier"]).await,
        ),
        (
            "InitializeService",
            call_in(&router, REGION, "InitializeService", &json!({})).await,
        ),
        (
            "DeleteLaunchConfigurationTemplate",
            call_in(
                &router,
                REGION,
                "DeleteLaunchConfigurationTemplate",
                &json!({"launchConfigurationTemplateID": launch_template_id}),
            )
            .await,
        ),
    ];

    for (operation, answer) in &answers {
        assert_no_content(operation, answer);
    }
}
