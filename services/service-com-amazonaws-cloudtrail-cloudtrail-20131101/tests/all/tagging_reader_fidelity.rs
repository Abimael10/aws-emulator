// SPDX-License-Identifier: CC0-1.0

//! What the tagging READER reports, driven end to end over the wire through the shared
//! harness (`tests/all/emulator.rs`): the calls a client makes, and the tag sets it then
//! sees.
//!
//! The two rules under test:
//!
//! * **a tag's `Value` is a member every read carries.** `Tag$Value` is optional
//!   (`smithy_query(query="shape",shape="Tag")`), so a request may leave it out — and a tag
//!   stored without one is read back with the member PRESENT and empty, never dropped: a
//!   consumer that indexes `{Key: Value}` finds a present-but-empty value rather than a
//!   missing one. The rule holds wherever a tag enters the registry (`AddTags` and each
//!   create that takes a tag list) and in every reader of it.
//!
//! * **`ListTags` reports one positional entry per ARN its `ResourceIdList` names,
//!   however MANY it names.** The list shape carries no `@length` constraint
//!   (`smithy_query(query="shape",shape="ResourceIdList")`) and the operation models no
//!   error for an over-long list, so the 20 its doc mentions ("The list has a limit of 20
//!   ARNs") bounds nothing the service refuses: every element is answered, duplicates
//!   included, in request order and in one page.

use serde_json::{json, Value};

use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state;

use super::emulator::Emulator;

/// The bucket the trails below deliver to; the harness records it for the call.
const BUCKET: &str = "tag-value-logs";

/// `Destination$Type` for a channel that logs to an event data store.
const EVENT_DATA_STORE: &str = "EVENT_DATA_STORE";

/// The one `Source` a caller-created channel takes.
const CUSTOM_SOURCE: &str = "Custom";

/// The tag set every row writes: one key with a value, one key WITHOUT one, and one whose
/// value is a non-ASCII string — the shapes a `TagsList` takes.
fn requested_tags() -> Value {
    json!([
        {"Key": "team", "Value": "security"},
        {"Key": "cost-centre"},
        {"Key": "notes", "Value": "ingång-日本語-🙂"},
    ])
}

/// The same set as a READ reports it: the tag stated with no value carries the empty one.
fn reported_tags() -> Value {
    json!([
        {"Key": "team", "Value": "security"},
        {"Key": "cost-centre", "Value": ""},
        {"Key": "notes", "Value": "ingång-日本語-🙂"},
    ])
}

/// The tag set `ListTags` reports for the resource `resource_id` names.
async fn listed_tags(emulator: &Emulator, case: &str, resource_id: &str) -> Value {
    let reply = emulator
        .call("ListTags", &json!({"ResourceIdList": [resource_id]}))
        .await;
    let body = reply.expect_ok(case).clone();
    body.get("ResourceTagList")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first().cloned())
        .unwrap_or_else(|| panic!("case {case}: no entry for {resource_id} in {body:?}"))
        .get("TagsList")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

/// Create a trail and hand back its ARN.
async fn trail(emulator: &Emulator, name: &str, tags: Option<&Value>) -> String {
    let mut request = json!({"Name": name, "S3BucketName": BUCKET});
    if let Some(tags) = tags {
        request["TagsList"] = tags.clone();
    }
    member(emulator, "CreateTrail", &request, "TrailARN").await
}

/// Create a dashboard and hand back its ARN.
async fn dashboard(emulator: &Emulator, name: &str, tags: Option<&Value>) -> String {
    let mut request = json!({"Name": name});
    if let Some(tags) = tags {
        request["TagsList"] = tags.clone();
    }
    member(emulator, "CreateDashboard", &request, "DashboardArn").await
}

/// Create an event data store and hand back its ARN.
async fn event_data_store(emulator: &Emulator, name: &str, tags: Option<&Value>) -> String {
    let mut request = json!({"Name": name});
    if let Some(tags) = tags {
        request["TagsList"] = tags.clone();
    }
    member(emulator, "CreateEventDataStore", &request, "EventDataStoreArn").await
}

/// Create a channel logging to a new event data store, and hand back its ARN. A Region
/// holds at most one `Custom` channel, so one call per emulator.
async fn channel(emulator: &Emulator, name: &str, tags: Option<&Value>) -> String {
    let destination = event_data_store(emulator, &format!("{name}-store"), None).await;
    let mut request = json!({
        "Name": name,
        "Source": CUSTOM_SOURCE,
        "Destinations": [{"Type": EVENT_DATA_STORE, "Location": destination}],
    });
    if let Some(tags) = tags {
        request["Tags"] = tags.clone();
    }
    member(emulator, "CreateChannel", &request, "ChannelArn").await
}

/// Make `operation` and hand back the string member `member` of its reply.
async fn member(emulator: &Emulator, operation: &str, request: &Value, member: &str) -> String {
    let case = format!("{operation} {request}");
    let reply = emulator.call(operation, request).await;
    reply
        .expect_ok(&case)
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: no {member} in the reply"))
        .to_string()
}

/// RULE: a tag stored with no `Value` is read back with the member present and empty.
///
/// The rows differ in the KIND of resource the tag set is on and in WHICH call stored it —
/// the create that carried the tag list, and `AddTags` afterwards — which is what the rule
/// generalises over; a tag whose value is non-ASCII is in the same set, so the empty case
/// cannot be confused with a value the reader mangles.
#[tokio::test]
async fn a_tag_stored_with_no_value_is_read_back_as_an_empty_value() {
    let emulator = Emulator::new();

    // Stored by the CREATE that carried the tag list.
    let created: Vec<(&str, String)> = vec![
        (
            "a trail created with the tag set",
            trail(&emulator, "tagged-trail", Some(&requested_tags())).await,
        ),
        (
            "a dashboard created with the tag set",
            dashboard(&emulator, "tagged_dashboard", Some(&requested_tags())).await,
        ),
        (
            "an event data store created with the tag set",
            event_data_store(&emulator, "tagged-store", Some(&requested_tags())).await,
        ),
        (
            "a channel created with the tag set",
            channel(&emulator, "tagged-channel", Some(&requested_tags())).await,
        ),
    ];

    // Stored by `AddTags` on a resource created without tags.
    let added: Vec<(&str, String)> = vec![
        (
            "a trail tagged by AddTags",
            trail(&emulator, "untagged-trail", None).await,
        ),
        (
            "a dashboard tagged by AddTags",
            dashboard(&emulator, "untagged_dashboard", None).await,
        ),
        (
            "an event data store tagged by AddTags",
            event_data_store(&emulator, "untagged-store", None).await,
        ),
    ];
    for (case, resource_id) in &added {
        emulator
            .call(
                "AddTags",
                &json!({"ResourceId": resource_id, "TagsList": requested_tags()}),
            )
            .await
            .expect_ok(case);
    }

    for (case, resource_id) in created.iter().chain(added.iter()) {
        let listed = listed_tags(&emulator, case, resource_id).await;
        assert_eq!(
            listed,
            reported_tags(),
            "case {case:?}: expected {:?}, got {listed:?}",
            reported_tags()
        );
    }

    // Overwriting the empty value with one, and back to none, keeps the member present
    // and reports what the last write stated.
    let (case, resource_id) = &added[0];
    for (step, written, expected) in [
        ("a value written over the empty one", json!("rnd"), "rnd"),
        ("the value left out again", Value::Null, ""),
    ] {
        let tag = match written {
            Value::Null => json!({"Key": "cost-centre"}),
            value => json!({"Key": "cost-centre", "Value": value}),
        };
        emulator
            .call(
                "AddTags",
                &json!({"ResourceId": resource_id, "TagsList": [tag]}),
            )
            .await
            .expect_ok(&format!("{case}: {step}"));
        let listed = listed_tags(&emulator, case, resource_id).await;
        let reported = listed
            .as_array()
            .and_then(|tags| {
                tags.iter()
                    .find(|tag| tag.get("Key") == Some(&json!("cost-centre")))
            })
            .and_then(|tag| tag.get("Value"))
            .and_then(Value::as_str);
        assert_eq!(
            reported,
            Some(expected),
            "case {case:?} ({step}): expected Value {expected:?}, got {listed:?}"
        );
    }
}

/// RULE: `ListTags` answers one positional entry per ARN its `ResourceIdList` names,
/// however many it names — the count bounds nothing — in request order, duplicates
/// included, in one page.
///
/// The rows differ in the COUNT, spanning the one the doc mentions and well past it, and
/// the list mixes the kinds and repeats an ARN, which is what the rule generalises over.
#[tokio::test]
async fn list_tags_answers_every_arn_however_many_the_request_names() {
    let emulator = Emulator::new();
    let trail_arn = trail(&emulator, "batch-trail", Some(&requested_tags())).await;
    let dashboard_arn = dashboard(&emulator, "batch_dashboard", None).await;

    let documented = state::LIST_TAGS_RESOURCE_LIMIT;
    for count in [
        1,
        2,
        documented,
        documented + 1,
        documented + 5,
        documented + 6,
        documented * 2 + 10,
        documented * 2 + 11,
    ] {
        // The ARNs alternate, so the answer has to be POSITIONAL rather than a set: entry
        // `n` carries the tag set of the ARN the request named at `n`.
        let named: Vec<&String> = (0..count)
            .map(|nth| {
                if nth % 2 == 0 {
                    &trail_arn
                } else {
                    &dashboard_arn
                }
            })
            .collect();
        let case = format!("a ResourceIdList of {count} ARNs");
        let reply = emulator
            .call("ListTags", &json!({"ResourceIdList": named}))
            .await;
        let body = reply.expect_ok(&case).clone();
        let entries = body
            .get("ResourceTagList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: no ResourceTagList in {body:?}"));
        assert_eq!(
            entries.len(),
            count,
            "case {case}: expected one entry per named ARN, got {body:?}"
        );
        assert_eq!(
            body.get("NextToken"),
            None,
            "case {case}: one page carries every ARN the request named, got {body:?}"
        );
        for (nth, entry) in entries.iter().enumerate() {
            let expected_id = named[nth];
            let expected_tags = if nth % 2 == 0 {
                reported_tags()
            } else {
                json!([])
            };
            assert_eq!(
                (
                    entry.get("ResourceId"),
                    entry.get("TagsList").cloned().unwrap_or_else(|| json!([]))
                ),
                (Some(&json!(expected_id)), expected_tags.clone()),
                "case {case} (entry {nth}): expected {expected_id} carrying \
                 {expected_tags:?}, got {entry:?}"
            );
        }
    }

    // The emulator's own ceiling on what ONE reply carries: at the ceiling the call is
    // answered, one ARN past it is refused. It is not a rule of the service — every count
    // above is one the service answers — but a bound on the work a single request may
    // choose, so it sits far above any list the service has been seen to answer.
    let ceiling = state::LIST_TAGS_REPLY_RESOURCE_CEILING;
    let at_ceiling: Vec<&String> = (0..ceiling).map(|_| &dashboard_arn).collect();
    let reply = emulator
        .call("ListTags", &json!({"ResourceIdList": at_ceiling}))
        .await;
    let entries = reply
        .expect_ok("a ResourceIdList at the reply ceiling")
        .get("ResourceTagList")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        entries, ceiling,
        "case a ResourceIdList at the reply ceiling: expected one entry per named ARN, got \
         {entries}"
    );

    let past_ceiling: Vec<&String> = (0..=ceiling).map(|_| &dashboard_arn).collect();
    emulator
        .call("ListTags", &json!({"ResourceIdList": past_ceiling}))
        .await
        .expect_error(
            "a ResourceIdList one ARN past the reply ceiling",
            400,
            "ValidationException",
        );
}
