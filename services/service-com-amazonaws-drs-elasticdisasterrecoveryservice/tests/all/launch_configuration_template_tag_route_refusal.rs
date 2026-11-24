// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what the DRS tagging route (`/tags/{resourceArn}`, by
//! method) answers an ARN naming a Launch Configuration Template, and for the tag set
//! such a template keeps while the route refuses it.
//!
//! Every test drives the service the way the router does — one buffered `http::Request`
//! per call through [`MultiServiceRouter::dispatch`] — so what is asserted is the wire
//! behaviour (status, error code, JSON body). The harness and the per-operation requests
//! are the ones `resource_tags` builds.
//!
//! The rule under test, asserted over a TABLE of stored tag sets and of attempted
//! changes to them rather than over one recorded request:
//!
//! 1. A Launch Configuration Template's tag set is established by the create that mints
//!    the template (`CreateLaunchConfigurationTemplate` takes `tags`,
//!    `UpdateLaunchConfigurationTemplate` does not) and is readable through that
//!    resource's own reader, `DescribeLaunchConfigurationTemplates`. The shared tagging
//!    route does not address that resource type, so EVERY operation of the route — the
//!    read (`GET`), the write (`POST`) and the removal (`DELETE`) — refuses such an ARN as
//!    a fault in the value the request supplied: HTTP 400, the error code
//!    `BadRequestException` and the message `Invalid input resource arn`.
//! 2. The refusal is decided from the ARN alone, before anything is looked up, so it does
//!    not depend on the ARN resolving to a template this service holds, nor on the
//!    partition, Region or account the ARN carries.
//! 3. Nothing a refused call named is written, whatever the change it attempted: adding a
//!    key the template does not carry, overwriting one it does, removing one it carries,
//!    removing one it does not, or removing every key it carries all leave the template
//!    reporting exactly the set its create supplied.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::replication_configuration_template_lifecycle::{
    call, drs_with_engine, items, Answer, REGION,
};
use crate::resource_tags::{
    launch_template_with_tags, list_tags, reported_tags, tag_resource, text, untag_resource,
};

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The error code and the whole message the route answers a `resourceArn` it does not
/// address with — written out as the literals a client reads rather than taken from the
/// implementation's constants.
const BAD_REQUEST_CODE: &str = "BadRequestException";
const INVALID_RESOURCE_ARN: &str = "Invalid input resource arn";

/// A tag map built from pairs a test holds, which `json!` cannot take as keys.
fn tag_map(pairs: &[(&str, &str)]) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), Value::String((*value).to_string()));
    }
    Value::Object(map)
}

/// One attempt to reach a tag set through the shared tagging route — one per operation
/// the route binds.
#[derive(Debug, Clone)]
enum Attempt {
    /// `GET /tags/{resourceArn}`: `ListTagsForResource`.
    Read,
    /// `POST /tags/{resourceArn}`: `TagResource` with the tags it names.
    Write(Value),
    /// `DELETE /tags/{resourceArn}?tagKeys=…`: `UntagResource` for the keys it names.
    Remove(Vec<String>),
}

impl Attempt {
    /// Send this attempt against `arn` and answer what the route replied.
    async fn perform(&self, router: &MultiServiceRouter, arn: &str) -> Answer {
        match self {
            Self::Read => list_tags(router, REGION, arn).await,
            Self::Write(tags) => tag_resource(router, REGION, arn, tags).await,
            Self::Remove(keys) => {
                let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                untag_resource(router, REGION, arn, &keys).await
            }
        }
    }
}

/// Assert that `answer` is the route's refusal of an ARN it does not address, and that it
/// reports no tag set.
fn assert_not_addressed(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected 400, got {answer:?}"
    );
    assert_eq!(
        answer.error_code.as_deref(),
        Some(BAD_REQUEST_CODE),
        "case {case:?}: expected {BAD_REQUEST_CODE}, got {answer:?}"
    );
    assert_eq!(
        text(answer, "message"),
        INVALID_RESOURCE_ARN,
        "case {case:?}: expected the message {INVALID_RESOURCE_ARN:?}, got {answer:?}"
    );
    assert!(
        reported_tags(answer).is_none(),
        "case {case:?}: a refusal must report no tags, got {answer:?}"
    );
}

/// Every operation of the route, each attempting a change of a different relation to the
/// set `stored` holds: the dimension the rule generalises over, derived from the stored
/// set rather than listed per row.
fn attempts_against(stored: &Value) -> Vec<(String, Attempt)> {
    let held = stored.as_object().expect("a tag map");
    let mut attempts = vec![
        ("a read".to_string(), Attempt::Read),
        (
            "a write of a key the template does not carry".to_string(),
            Attempt::Write(tag_map(&[("added", "v")])),
        ),
        (
            "a removal of a key the template does not carry".to_string(),
            Attempt::Remove(vec!["absent".to_string()]),
        ),
    ];
    if let Some(carried) = held.keys().next() {
        let mut mixed = Map::new();
        mixed.insert(carried.clone(), Value::String("overwritten".to_string()));
        mixed.insert("added".to_string(), Value::String("v".to_string()));
        attempts.extend([
            (
                "a write overwriting a key the template carries".to_string(),
                Attempt::Write(tag_map(&[(carried, "overwritten")])),
            ),
            (
                "a write mixing a carried key and a new one".to_string(),
                Attempt::Write(Value::Object(mixed)),
            ),
            (
                "a removal of a key the template carries".to_string(),
                Attempt::Remove(vec![carried.clone()]),
            ),
            (
                "a removal of every key the template carries".to_string(),
                Attempt::Remove(held.keys().cloned().collect()),
            ),
        ]);
    }
    attempts
}

/// The tags `DescribeLaunchConfigurationTemplates` reports for the template `id` — the
/// template's OWN reader, the one place its tags are readable.
async fn described_tags(router: &MultiServiceRouter, id: &str) -> Value {
    let answer: Answer = call(
        router,
        "DescribeLaunchConfigurationTemplates",
        &json!({"launchConfigurationTemplateIDs": [id]}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "describing {id}: expected 200, got {answer:?}"
    );
    items(&answer)
        .first()
        .and_then(|item| item.get("tags"))
        .cloned()
        .unwrap_or_else(|| panic!("expected {id} to be reported with its tags, got {answer:?}"))
}

/// The id a Launch Configuration Template's ARN carries in its resource part.
fn id_in(arn: &str) -> String {
    arn.rsplit_once('/')
        .expect("a launch-configuration-template ARN")
        .1
        .to_string()
}

/// Whatever the set a template was created with, and whatever change the route is asked
/// to make to it, the route refuses the template's ARN and the template keeps reporting
/// exactly the create-time set through its own reader.
#[tokio::test]
async fn no_attempt_through_the_tagging_route_reaches_a_launch_configuration_template() {
    let (router, _engine) = drs_with_engine();

    // The rows differ along the shape of the set the create established: `TagsMap` admits
    // the empty set, one entry, several, an empty value, and a key and a value at the
    // greatest length it allows (`smithy_query(query="shape",shape="TagsMap")` — key
    // `TagKey` len 0..256, value `TagValue` len 0..256).
    let longest = "k".repeat(256);
    let rows: Vec<(&str, Value)> = vec![
        ("a template created with no tags", json!({})),
        ("a template created with one tag", tag_map(&[("owner", "x")])),
        (
            "a template created with several tags",
            tag_map(&[("owner", "x"), ("env", "prod"), ("tier", "1")]),
        ),
        (
            "a template created with an empty value",
            tag_map(&[("owner", "")]),
        ),
        (
            "a template created with the longest key and value TagsMap admits",
            tag_map(&[(longest.as_str(), "v".repeat(256).as_str())]),
        ),
    ];

    for (row, stored) in &rows {
        let arn = launch_template_with_tags(&router, stored).await;
        let id = id_in(&arn);
        for (attempt_name, attempt) in attempts_against(stored) {
            let case = format!("{attempt_name} on {row}");
            let answer = attempt.perform(&router, &arn).await;
            assert_not_addressed(&case, &answer);
            // Nothing was written: the template's own reader still reports the set its
            // create supplied, checked after EACH attempt so no attempt can hide behind
            // a later one.
            assert_eq!(
                &described_tags(&router, &id).await,
                stored,
                "case {case:?}: a refused call must leave {stored} in place"
            );
        }
    }
}

/// The refusal is read off the ARN itself, so it is the same for an ARN that resolves to
/// a template this service holds and for one that resolves to nothing at all — and the
/// held template is untouched by either.
#[tokio::test]
async fn the_refusal_does_not_depend_on_the_arn_resolving_to_a_held_template() {
    let (router, _engine) = drs_with_engine();

    let stored = tag_map(&[("owner", "x"), ("env", "prod")]);
    let held = launch_template_with_tags(&router, &stored).await;
    let held_id = id_in(&held);

    // Every ARN below satisfies the modelled `ARN` (length 20..2048, pattern
    // `^arn:.{16,2044}$`), so each reaches the handler rather than the boundary validator.
    let rows: Vec<(&str, String)> = vec![
        ("a template this service holds", held.clone()),
        (
            "a template id nothing minted",
            format!(
                "arn:aws:drs:{REGION}:{ACCOUNT}:launch-configuration-template/\
                 lct-00000000000000000"
            ),
        ),
        (
            "a held template's id under another Region",
            format!("arn:aws:drs:us-east-2:{ACCOUNT}:launch-configuration-template/{held_id}"),
        ),
        (
            "a held template's id under another account",
            format!("arn:aws:drs:{REGION}:123456789012:launch-configuration-template/{held_id}"),
        ),
        (
            "a held template's id under another partition",
            format!("arn:aws-us-gov:drs:{REGION}:{ACCOUNT}:launch-configuration-template/{held_id}"),
        ),
        (
            "an id that is not of the modelled form",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:launch-configuration-template/not-an-lct-id"),
        ),
    ];

    for (row, arn) in &rows {
        for (attempt_name, attempt) in attempts_against(&stored) {
            let case = format!("{attempt_name} on {row}");
            let answer = attempt.perform(&router, arn).await;
            assert_not_addressed(&case, &answer);
        }
    }

    assert_eq!(
        described_tags(&router, &held_id).await,
        stored,
        "the held template must still carry {stored}"
    );
}
