// SPDX-License-Identifier: CC0-1.0

//! The TAG plane of the service that owns a resource Cloud Control API provisioned.
//!
//! The rule every case here holds the service to: **a resource operation Cloud Control API
//! reports terminal `SUCCESS` for has been applied in the store of the service that owns the
//! `TypeName` — "SUCCESS: The resource operation has successfully completed"
//! (`OperationStatus`) — so that service's OWN tag read resolves the identifier Cloud
//! Control API reported for the resource and reports exactly the tag set the applied
//! resource model declares, and a read of the model back through Cloud Control API agrees
//! with it.** Only a resource the owning service does not hold is an id its tag read refuses.
//!
//! The rows differ along what the rule generalises over: how many tags the model declares
//! (none, one, several), the order the keys were written in against the order they are
//! reported in, the form a tag's value takes, the shape the type's schema publishes its tag
//! set in (`AWS::SSM::Parameter` as a MAP of tag key to tag value, `AWS::SQS::Queue` as a
//! list of `{ Key, Value }` pairs), which tag mutation a patch asks for (dropping one key of
//! several, changing a value, adding a key, dropping the whole property), and which service
//! owns the resource — so what the suite asserts is the write-through to the owning tag
//! plane rather than anything about one owning service.
//!
//! `ssm_parameter_read_through` and `sqs_queue_read_through` hold the wider read-through rule
//! for those two types; this suite is the tag plane's half of it, read through the owning
//! service's own tagging API.

use serde_json::{json, Map, Value};

use super::emulator::{code, text, Emulator};
use super::sqs_queue_read_through::{native_tags, tag, QUEUE};

/// The type whose resources live in the Systems Manager service's own store, named by the
/// `TypeName` its registry schema declares.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// The `ResourceTypeForTagging` value a Systems Manager tag request names a parameter by
/// (from that service's modelled enum, whose values `ListTagsForResource` lists: it includes
/// `Parameter`, whose id is "the name of the resource").
const TAGGING_TYPE_PARAMETER: &str = "Parameter";

/// The error code Systems Manager answers a tag read with for an id that names no resource
/// it holds: "The resource ID isn't valid. Verify that you entered the correct ID and try
/// again." The shape carries no message member, so the code is the whole refusal.
const INVALID_RESOURCE_ID: &str = "InvalidResourceId";

/// A tag set as a case spells it, in the order the case wrote it.
type Tags<'a> = &'a [(&'a str, &'a str)];

/// One tag set in key order, which is the order every read below is compared in.
fn ordered(tags: Tags<'_>) -> Vec<(String, String)> {
    let mut ordered: Vec<(String, String)> = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    ordered.sort();
    ordered
}

/// A desired state for a parameter: its name, the two properties the type's schema declares
/// required, and the tag set as THIS type publishes it — a map of tag key to tag value,
/// carried only where the case declares one, so a row that declares none names no `Tags`
/// property at all.
fn parameter_state(name: &str, value: &str, tags: Tags<'_>) -> Value {
    let mut model = Map::new();
    model.insert("Name".to_string(), json!(name));
    model.insert("Type".to_string(), json!("String"));
    model.insert("Value".to_string(), json!(value));
    if !tags.is_empty() {
        model.insert("Tags".to_string(), tag_object(tags));
    }
    Value::Object(model)
}

/// A tag set as `AWS::SSM::Parameter` publishes it: an object of tag key to tag value.
fn tag_object(tags: Tags<'_>) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// The tags the OWNING service reports for the parameter `name` through its own tagging API,
/// required to be answered: a resource that service holds resolves, whichever tags it
/// carries.
async fn native_parameter_tags(
    emulator: &Emulator,
    case: &str,
    name: &str,
) -> Vec<(String, String)> {
    let (status, body) = emulator
        .ssm(
            "ListTagsForResource",
            json!({ "ResourceType": TAGGING_TYPE_PARAMETER, "ResourceId": name }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ssm:ListTagsForResource expected HTTP 200, got {status} with body \
         {body:?}"
    );
    let mut held: Vec<(String, String)> = body["TagList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| (text(entry, "Key"), text(entry, "Value")))
        .collect();
    held.sort();
    held
}

/// The tag set Cloud Control API's own read of the parameter's model reports, read as the MAP
/// this type publishes — and the empty set for a model carrying no `Tags` property, which is
/// what an untagged resource's model carries.
async fn model_parameter_tags(emulator: &Emulator, name: &str) -> Vec<(String, String)> {
    let model = emulator.properties(PARAMETER, name).await;
    let mut carried: Vec<(String, String)> = model
        .get("Tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| {
                    // A tag value is text, so anything else is carried into the comparison
                    // as the JSON it is rather than read as the empty string: a wrongly
                    // typed value must fail the case rather than pass as an empty tag.
                    let carried = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_string);
                    (key.clone(), carried)
                })
                .collect()
        })
        .unwrap_or_default();
    carried.sort();
    carried
}

/// Hold both reads of one parameter's tag set to `expected`: the OWNING service's own tag
/// read, and Cloud Control API's read of the resource model.
async fn assert_both_planes(
    emulator: &Emulator,
    case: &str,
    name: &str,
    expected: &[(String, String)],
) {
    let held = native_parameter_tags(emulator, case, name).await;
    assert_eq!(
        held, expected,
        "case {case:?}: expected the owning service's tag read to report {expected:?}, got \
         {held:?}"
    );
    let carried = model_parameter_tags(emulator, name).await;
    assert_eq!(
        carried, expected,
        "case {case:?}: expected the model Cloud Control API reports to carry {expected:?}, \
         got {carried:?}"
    );
}

#[tokio::test]
async fn the_owning_services_tag_read_reports_the_tags_a_create_declared() {
    let emulator = Emulator::with_ssm();
    // The rows differ along how many tags the desired state declares, the order its keys were
    // written in, the form a tag's value takes, and the form of the parameter's own name.
    let rows: [(&str, &str, Tags); 5] = [
        ("a model declaring no tags at all", "tagless", &[]),
        (
            "a model declaring one tag",
            "/fleet/one-tag",
            &[("owner", "platform")],
        ),
        (
            "a model declaring two tags",
            "two-tags",
            &[("owner", "platform"), ("tier", "gold")],
        ),
        (
            "a model whose keys were written out of key order",
            "/fleet/unordered",
            &[("zone", "c"), ("alpha", "1"), ("mid", "x")],
        ),
        (
            "a model declaring a tag whose value is empty",
            "/fleet/empty-value",
            &[("retired", "")],
        ),
    ];

    for (case, name, tags) in rows {
        let identifier = emulator
            .provision(PARAMETER, &parameter_state(name, "created", tags))
            .await;
        assert_eq!(
            identifier, name,
            "case {case:?}: expected the parameter's own name as the identifier, got \
             {identifier:?}"
        );
        // The id Cloud Control API reported resolves at the owning service, and the tags the
        // desired state declared are the tags that service holds.
        assert_both_planes(&emulator, case, &identifier, &ordered(tags)).await;
    }
}

#[tokio::test]
async fn a_patch_of_the_tag_set_is_a_write_to_the_owning_services_tag_plane() {
    let emulator = Emulator::with_ssm();
    let name = "/fleet/patched";
    let identifier = emulator
        .provision(
            PARAMETER,
            &parameter_state(
                name,
                "before",
                &[("owner", "platform"), ("tier", "gold"), ("zone", "c")],
            ),
        )
        .await;
    assert_both_planes(
        &emulator,
        "the create the patches start from",
        &identifier,
        &ordered(&[("owner", "platform"), ("tier", "gold"), ("zone", "c")]),
    )
    .await;

    // The rows differ along the tag mutation the patch asks for: dropping one key of several,
    // changing a surviving key's value, adding a key, dropping the whole property, and
    // putting a tag set back on a resource that carries none.
    let rows: [(&str, Value, Tags); 5] = [
        (
            "dropping one key of several",
            json!([{ "op": "remove", "path": "/Tags/zone" }]),
            &[("owner", "platform"), ("tier", "gold")],
        ),
        (
            "changing a surviving key's value",
            json!([{ "op": "replace", "path": "/Tags/tier", "value": "silver" }]),
            &[("owner", "platform"), ("tier", "silver")],
        ),
        (
            "adding a key",
            json!([{ "op": "add", "path": "/Tags/wave", "value": "second" }]),
            &[("owner", "platform"), ("tier", "silver"), ("wave", "second")],
        ),
        (
            "dropping the whole tag set",
            json!([{ "op": "remove", "path": "/Tags" }]),
            &[],
        ),
        (
            "putting a tag set back on an untagged resource",
            json!([{ "op": "add", "path": "/Tags", "value": { "owner": "platform" } }]),
            &[("owner", "platform")],
        ),
    ];

    for (case, patch, expected) in rows {
        emulator
            .update_and_settle(PARAMETER, &identifier, &patch)
            .await;
        assert_both_planes(&emulator, case, &identifier, &ordered(expected)).await;
    }
}

#[tokio::test]
async fn only_a_resource_the_owning_service_does_not_hold_is_refused_by_its_tag_read() {
    let emulator = Emulator::with_ssm();
    // The rows differ along why the owning service holds no such parameter: one Cloud Control
    // API removed, and one nothing ever created.
    let removed = "/fleet/removed";
    emulator
        .provision(
            PARAMETER,
            &parameter_state(removed, "doomed", &[("owner", "platform")]),
        )
        .await;
    assert_both_planes(
        &emulator,
        "the parameter before it is removed",
        removed,
        &ordered(&[("owner", "platform")]),
    )
    .await;
    emulator.delete_and_settle(PARAMETER, removed).await;

    for (case, name) in [
        ("a parameter Cloud Control API removed", removed),
        ("a parameter nothing ever created", "/fleet/never-created"),
    ] {
        let (status, body) = emulator
            .ssm(
                "ListTagsForResource",
                json!({ "ResourceType": TAGGING_TYPE_PARAMETER, "ResourceId": name }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: ssm:ListTagsForResource expected HTTP 400, got {status} with body \
             {body:?}"
        );
        assert!(
            code(&body).contains(INVALID_RESOURCE_ID),
            "case {case:?}: expected the {INVALID_RESOURCE_ID} refusal, got {:?}",
            code(&body)
        );
    }
}

#[tokio::test]
async fn the_same_rule_holds_for_a_type_whose_tag_set_is_a_list() {
    let emulator = Emulator::with_sqs();
    // The same rule over a resource ANOTHER service owns, whose schema publishes its tag set
    // as a list of `{ Key, Value }` pairs rather than as a map — so a patch addresses one tag
    // by its position in that list.
    let tags = json!([tag("owner", "platform"), tag("tier", "gold")]);
    let identifier = emulator
        .provision(
            QUEUE,
            &json!({ "QueueName": "tag-plane-queue", "Tags": tags }),
        )
        .await;

    let expected = tags.as_array().cloned().unwrap_or_default();
    let held = native_tags(&emulator, &identifier).await;
    assert_eq!(
        held, expected,
        "case \"the create\": expected the owning service's tag read to report {expected:?}, \
         got {held:?}"
    );

    // Dropping the first entry of the list leaves the owning service holding the survivor,
    // and the model Cloud Control API reports carries exactly that one.
    emulator
        .update_and_settle(
            QUEUE,
            &identifier,
            &json!([{ "op": "remove", "path": "/Tags/0" }]),
        )
        .await;
    let expected = vec![tag("tier", "gold")];
    let held = native_tags(&emulator, &identifier).await;
    assert_eq!(
        held, expected,
        "case \"dropping one entry of the list\": expected the owning service's tag read to \
         report {expected:?}, got {held:?}"
    );
    let model = emulator.properties(QUEUE, &identifier).await;
    assert_eq!(
        model["Tags"],
        Value::Array(expected.clone()),
        "case \"dropping one entry of the list\": expected the model to carry {expected:?}, \
         got {model:?}"
    );
}
