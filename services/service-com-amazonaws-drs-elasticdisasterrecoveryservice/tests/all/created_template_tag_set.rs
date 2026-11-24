// SPDX-License-Identifier: CC0-1.0

//! The tag set a created replication configuration template carries, as the create's own
//! 201 reports it.
//!
//! The rule: a create associates exactly the tag set its `tags` member names, and the 201
//! reports that set — every key the request named with the value it named, and no key the
//! request did not name, so a create that named no tags reports the empty set. The same
//! set is what every later reader of that template reports, because a resource carries ONE
//! tag set.
//!
//! The rows span what the rule generalises over: whether the member is sent at all, the
//! size of the map (none, one, several, the documented maximum per resource), and the form
//! of a key and of a value — a camelCase name, separators, non-ASCII, the empty string and
//! the published maximum length. `TagsMap` is
//! `map<TagKey length 0..256, TagValue length 0..256>`
//! (`smithy_query(query="shape", shape="TagsMap")`), so both published bounds are rows, and
//! a value one code point past the maximum is refused by the service's body-schema check
//! with nothing stored.

use serde_json::{json, Map, Value};

use crate::replication_configuration_template_lifecycle::{
    call, create_body, drs, initialize, items, REGION,
};
use crate::resource_tags::{list_tags, TAGS_PER_RESOURCE_LIMIT};

/// The subnet every row names for its staging area. No EC2 is mounted beside this service
/// in this suite, so the id is served as `staging_area_subnet.rs` pins.
const SUBNET: &str = "subnet-0123456789abcdef0";

/// The published maximum length of a tag key and of a tag value: `TagKey` and `TagValue`
/// are both `length 0..256` (`smithy_query(query="shape", shape="TagsMap")`).
const TAG_LENGTH_MAX: usize = 256;

/// A `tags` map as a client sends one.
fn tag_map(entries: &[(String, String)]) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}

/// A row's tag entries, so a key or a value can be built rather than written out.
fn owned(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The tag sets a create associates, one per row: the case name and what the request's
/// `tags` member carries (`None` for a request that omits the member).
fn rows() -> Vec<(&'static str, Option<Value>)> {
    // The documented maximum one resource carries, built rather than written out.
    let full_set: Vec<(String, String)> = (0..TAGS_PER_RESOURCE_LIMIT)
        .map(|index| (format!("key{index:02}"), format!("value{index:02}")))
        .collect();

    vec![
        ("the member is absent", None),
        ("an empty map", Some(tag_map(&[]))),
        (
            "one key in the camelCase form of an API member name",
            Some(tag_map(&owned(&[(
                "ownerContactArn",
                "arn:aws:iam::123456789012:user/disaster-recovery",
            )]))),
        ),
        (
            "several keys of different forms",
            Some(tag_map(&owned(&[
                ("Name", "nightly-dr"),
                ("cost-center", "4812"),
                ("owner.email", "dr@example.test"),
                ("équipe", "réprise"),
                ("", "a key of the minimum length is still a key"),
            ]))),
        ),
        (
            "a value of the minimum length",
            Some(tag_map(&owned(&[("emptyValued", "")]))),
        ),
        (
            "a key and a value at the published maximum length",
            Some(tag_map(&[(
                "k".repeat(TAG_LENGTH_MAX),
                "v".repeat(TAG_LENGTH_MAX),
            )])),
        ),
        (
            "the documented maximum number of tags per resource",
            Some(tag_map(&full_set)),
        ),
    ]
}

/// A create body carrying `tags` exactly when the row sends the member.
fn body_with(tags: Option<&Value>) -> Value {
    let mut extra = Map::new();
    if let Some(tags) = tags {
        extra.insert("tags".to_string(), tags.clone());
    }
    create_body(SUBNET, Value::Object(extra))
}

/// The create's 201 reports exactly the tag set the request associated — no entry of it
/// dropped and no entry the request never named — and so does every later reader of the
/// template: the id-keyed `DescribeReplicationConfigurationTemplates` and the tagging
/// route's `ListTagsForResource`.
#[tokio::test]
async fn create_reports_exactly_the_tag_set_the_request_associated() {
    let router = drs();
    initialize(&router).await;

    for (case, sent) in rows() {
        let answer = call(
            &router,
            "CreateReplicationConfigurationTemplate",
            &body_with(sent.as_ref()),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {answer:?}"
        );

        // A template carries a tag set whether or not the request named one, so a
        // request that named none is reported as the empty set.
        let expected = sent.clone().unwrap_or_else(|| json!({}));
        assert_eq!(
            answer.member("tags"),
            Some(&expected),
            "case {case}: expected the 201 to report {expected}, got {:?}",
            answer.member("tags")
        );

        let id = answer
            .member("replicationConfigurationTemplateID")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: no template id in {answer:?}"))
            .to_string();
        let arn = answer
            .member("arn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: no ARN in {answer:?}"))
            .to_string();

        let described = call(
            &router,
            "DescribeReplicationConfigurationTemplates",
            &json!({"replicationConfigurationTemplateIDs": [id]}),
        )
        .await;
        assert_eq!(
            described.status, 200,
            "case {case}: describe expected 200, got {described:?}"
        );
        let reported = items(&described);
        assert_eq!(
            reported.len(),
            1,
            "case {case}: expected the created template to be described once, got {described:?}"
        );
        assert_eq!(
            reported[0].get("tags"),
            Some(&expected),
            "case {case}: expected a later read to report {expected}, got {:?}",
            reported[0].get("tags")
        );

        let listed = list_tags(&router, REGION, &arn).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: list tags expected 200, got {listed:?}"
        );
        assert_eq!(
            listed.member("tags"),
            Some(&expected),
            "case {case}: expected the tagging route to report {expected}, got {:?}",
            listed.member("tags")
        );
    }
}

/// One code point past a published bound is the other side of the same rule: the body
/// fails the service's schema check, so the create is refused as that check's own
/// `BadRequestException` and nothing is stored.
#[tokio::test]
async fn a_tag_value_past_the_published_maximum_is_refused_and_stores_nothing() {
    /// The code this service answers a body that does not satisfy its model's own rules
    /// with (the class `request_body_schema_refusal` pins).
    const BAD_REQUEST_CODE: &str = "BadRequestException";
    /// The whole message that refusal carries.
    const INVALID_REQUEST_BODY: &str = "Invalid request body";

    let cases = vec![
        (
            "a value one code point past the maximum",
            tag_map(&[("owner".to_string(), "x".repeat(TAG_LENGTH_MAX + 1))]),
        ),
        (
            "one of several values past the maximum",
            tag_map(&[
                ("Name".to_string(), "nightly-dr".to_string()),
                ("owner".to_string(), "x".repeat(TAG_LENGTH_MAX + 9)),
            ]),
        ),
    ];

    let router = drs();
    initialize(&router).await;

    for (case, tags) in cases {
        let answer = call(
            &router,
            "CreateReplicationConfigurationTemplate",
            &body_with(Some(&tags)),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected 400, got {answer:?}"
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some(BAD_REQUEST_CODE),
            "case {case}: expected {BAD_REQUEST_CODE}, got {answer:?}"
        );
        assert_eq!(
            answer.member("message").and_then(Value::as_str),
            Some(INVALID_REQUEST_BODY),
            "case {case}: expected the message {INVALID_REQUEST_BODY:?}, got {answer:?}"
        );

        let described = call(
            &router,
            "DescribeReplicationConfigurationTemplates",
            &json!({}),
        )
        .await;
        assert_eq!(
            described.status, 200,
            "case {case}: describe expected 200, got {described:?}"
        );
        assert!(
            items(&described).is_empty(),
            "case {case}: a refused create must store nothing, got {described:?}"
        );
    }
}
