// SPDX-License-Identifier: CC0-1.0

//! How a MAP KEY outside the constraints its key shape declares is refused, over the tag map
//! `TagResource` writes.
//!
//! RULE: a modelled constraint broken by a map's KEY is refused with the protocol's
//! `ValidationException` (HTTP 400) stated ONCE at the MAP MEMBER — quoting the whole map the
//! request submitted as the value the clause is about, and listing what a key of that map has
//! to satisfy as the per-member constraint list (`Map keys must satisfy constraint: [ … ]`),
//! each requirement the one the framework words for a member it reports itself. It is the
//! member that failed the constraint, so one clause is stated however many keys of the map
//! broke it.
//!
//! `Tags` targets `IdentityPoolTagsType`, whose key shape `TagKeysType` declares
//! `length 1..128` and no `@pattern`
//! (`smithy_query(query="shape",shape="IdentityPoolTagsType,TagKeysType")`), so the list
//! names the two bounds and nothing else.
//!
//! The rows differ along what the rule generalises over: WHICH bound the key breaks (over the
//! maximum, under the minimum), how far past it, how MANY entries the map carries, how many of
//! them are bad, the characters a key is spelled with, and — the control rows — a key sitting
//! exactly ON each bound, which is not refused at all.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds.

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentity_awscognitoidentityservice::state::identity_pool_arn;

use crate::identity_pool_flow::{error_code, Emulator};

/// The error code the constraint layer ahead of every handler refuses with — the protocol's
/// own, which no operation of this service declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The modelled bounds of a tag key, `TagKeysType`'s own `length 1..128`.
const TAG_KEY_MIN: usize = 1;
const TAG_KEY_MAX: usize = 128;

/// The per-member constraint list a `TagKeysType` key has to satisfy, as the framework words
/// each requirement: the maximum first, then the minimum, and no `@pattern` clause because
/// the key shape declares none.
fn key_requirements() -> String {
    format!(
        "[Member must have length less than or equal to {TAG_KEY_MAX}, Member must have length \
         greater than or equal to {TAG_KEY_MIN}]"
    )
}

/// The whole map as a clause quotes one: `{<key>=<value>, …}` over the entries in the order
/// this service states a map in, through the shared renderings the emulator itself words a
/// clause with — so the assertion pins the VALUE half being the submitted map rather than one
/// hand-written order.
fn quoted_map(entries: &[(String, String)]) -> String {
    let held: Vec<(&str, &str)> = entries
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    service_common::validation::rendered_map(
        &service_common::submitted_collections::hashed_entry_order(&held),
    )
}

/// The `Tags` member one case submits.
fn tags(entries: &[(String, String)]) -> Value {
    Value::Object(
        entries
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

/// `(key, value)` as owned strings.
fn entry(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// The message a refusal states, which every refusal has to carry.
fn message(body: &Value, case: &str) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {body}"))
        .to_string()
}

/// RULE: a tag key outside `TagKeysType`'s bounds is refused once at `tags`, quoting the whole
/// submitted map and listing the key shape's requirements.
#[tokio::test]
async fn a_tag_key_outside_its_bound_is_refused_at_the_map_member() {
    let emulator = Emulator::new();
    let pool_id = emulator.pool("tag-map-key-refusal", true).await;
    let arn = identity_pool_arn(&pool_id);

    let over_bound = "k".repeat(TAG_KEY_MAX + 1);
    let far_over_bound = "k".repeat(TAG_KEY_MAX * 3);
    // A key of 129 CHARACTERS spelled in a script whose characters take more than one byte:
    // a `@length` constraint is measured in characters, so this is one character past the
    // maximum like the ASCII row, and the clause quotes it as submitted.
    let over_bound_unicode = "é".repeat(TAG_KEY_MAX + 1);

    // (case, the entries the request carries)
    let rows: Vec<(&str, Vec<(String, String)>)> = vec![
        (
            "one key one character past the maximum",
            vec![entry(&over_bound, "some-value")],
        ),
        (
            "a bad key beside a good one",
            vec![
                entry(&over_bound, "some-value"),
                entry("valid-key", "some-other-value"),
            ],
        ),
        (
            "a key far past the maximum",
            vec![entry(&far_over_bound, "some-value")],
        ),
        (
            "a non-ASCII key one character past the maximum",
            vec![entry(&over_bound_unicode, "valeur")],
        ),
        (
            "an empty key, under the minimum",
            vec![entry("", "some-value")],
        ),
        (
            "an empty key beside two good ones",
            vec![
                entry("", "some-value"),
                entry("Environment", "test"),
                entry("Owner", "platform"),
            ],
        ),
        (
            "two keys outside the bounds at once",
            vec![entry("", "some-value"), entry(&over_bound, "another-value")],
        ),
    ];

    for (case, entries) in rows {
        let (status, body) = emulator
            .call(
                "TagResource",
                &json!({ "ResourceArn": &arn, "Tags": tags(&entries) }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {body}"
        );
        let expected = format!(
            "1 validation error detected: Value '{}' at 'tags' failed to satisfy constraint: \
             Map keys must satisfy constraint: {}",
            quoted_map(&entries),
            key_requirements()
        );
        assert_eq!(
            message(&body, case),
            expected,
            "case {case:?}: expected {expected:?}, got {body}"
        );
    }

    // Nothing of a refused call was written: the pool still carries no tag at all.
    let reported = emulator
        .ok("ListTagsForResource", &json!({ "ResourceArn": &arn }))
        .await;
    let stored = reported.get("Tags").and_then(Value::as_object).cloned();
    assert!(
        stored.is_none_or(|tags| tags.is_empty()),
        "case \"after every refusal\": a refused call writes nothing, got {reported}"
    );
}

/// RULE, the other side: a key sitting exactly ON either bound satisfies the constraint, so it
/// is stored rather than refused — which is what makes the refusals above refusals of the
/// bound.
#[tokio::test]
async fn a_tag_key_on_either_bound_is_stored() {
    let emulator = Emulator::new();
    let pool_id = emulator.pool("tag-map-key-at-bound", true).await;
    let arn = identity_pool_arn(&pool_id);

    let at_maximum = "k".repeat(TAG_KEY_MAX);
    let at_minimum = "k".repeat(TAG_KEY_MIN);
    let rows = [
        ("a key at the maximum of 128", at_maximum.as_str()),
        ("a key at the minimum of 1", at_minimum.as_str()),
    ];

    for (case, key) in rows {
        let (status, body) = emulator
            .call(
                "TagResource",
                &json!({ "ResourceArn": &arn, "Tags": { key: "stored" } }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let reported = emulator
            .ok("ListTagsForResource", &json!({ "ResourceArn": &arn }))
            .await;
        assert_eq!(
            reported["Tags"][key].as_str(),
            Some("stored"),
            "case {case:?}: expected the tag to be read back, got {reported}"
        );
    }
}
