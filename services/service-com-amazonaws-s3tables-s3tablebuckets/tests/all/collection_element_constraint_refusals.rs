// SPDX-License-Identifier: CC0-1.0

//! How S3 Tables refuses a value a constraint declared on a COLLECTION's
//! ELEMENT shape rules out.
//!
//! The rule under test: a modelled constraint the model declares on a
//! collection member's element shape is reported AT that collection member —
//! `Value at '<collection>' failed to satisfy constraint: Member must satisfy
//! constraint: [<the element shape's whole declared set>]`, inside the
//! parameter validator's envelope (`1 validation error detected: …`, HTTP 400
//! under its own `ValidationException` code) — and never at the element that
//! broke it: the constraint belongs to the member the model declares it on, so
//! the clause names no position, lists the element shape's WHOLE set rather than
//! the one bound a value missed, and echoes no value. The refusal is stated
//! before the operation resolves anything, so the request has no effect at all.
//!
//! The only such member of this service's operation inputs is
//! `UntagResource.tagKeys`, a `TagKeyList` whose member targets `TagKey`
//! (`@length(1, 128)`, by `smithy_query(query="shape",
//! shape="TagKeyList,TagKey")`) — so both bounds below are the model's, and the
//! bracketed set lists the maximum before the minimum, which is the order such a
//! clause states them in.
//!
//! The table spans what the rule generalises over rather than one recorded
//! request: WHICH bound the element broke (over the maximum, by one character
//! and by far; under the minimum), WHERE the offending element sat (alone,
//! first, in the middle, last), HOW MANY elements of the one list offended (one,
//! two, and two breaking DIFFERENT bounds — one validation error either way,
//! because the clause is about the member), and whether the resource the call
//! addresses exists at all (the clause precedes every lookup).
//!
//! The accepted rows keep the refusal honest: a key AT each published bound is
//! served, so what is refused is the keys outside the bound and not the member —
//! and the tags a refused call named survive it, while the same call with a
//! well-formed key removes exactly that key.

use std::collections::BTreeMap;

use serde_json::json;

use crate::harness::{expected_bucket_arn, listed_tags, tag_map, tags_body, Emulator, Reply};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The code the parameter validator answers with: not a shape of this service's
/// model, which is what tells a client the request was refused for the SHAPE of
/// a parameter rather than by a rule of the service.
const PARAMETER_VALIDATION: &str = "ValidationException";

/// `TagKey`'s length bounds, as `TagKeyList`'s member declares them
/// (`smithy_query(query="shape", shape="TagKeyList,TagKey")`).
const TAG_KEY_MIN: usize = 1;
const TAG_KEY_MAX: usize = 128;

/// The whole sentence a refusal of a `tagKeys` element must carry: the element
/// shape's set at the list member, with no element position and no value.
fn expected_message() -> String {
    format!(
        "1 validation error detected: Value at 'tagKeys' failed to satisfy constraint: Member \
         must satisfy constraint: [Member must have length less than or equal to {TAG_KEY_MAX}, \
         Member must have length greater than or equal to {TAG_KEY_MIN}]"
    )
}

/// A tag key of `length` characters, built so a case states the length it drives
/// rather than a literal.
fn key_of(length: usize) -> String {
    "k".repeat(length)
}

/// One refused removal: the keys the request names, and the values it carried
/// that may appear nowhere in the refusal.
struct Case {
    label: String,
    keys: Vec<String>,
}

fn case(label: &str, keys: &[String]) -> Case {
    Case {
        label: label.to_string(),
        keys: keys.to_vec(),
    }
}

/// The refused cases, each differing from the next along what the rule
/// generalises over: which bound the element broke, where it sat in the list,
/// and how many elements of the list broke one.
fn cases() -> Vec<Case> {
    let resident = "owner".to_string();
    let other = "cost_center".to_string();
    let one_over = key_of(TAG_KEY_MAX + 1);
    let far_over = key_of(TAG_KEY_MAX * 4);
    let under = String::new();

    vec![
        case(
            "one character over the maximum, alone",
            std::slice::from_ref(&one_over),
        ),
        case(
            "far over the maximum, alone",
            std::slice::from_ref(&far_over),
        ),
        case(
            "one character over the maximum, first of three",
            &[one_over.clone(), resident.clone(), other.clone()],
        ),
        case(
            "one character over the maximum, in the middle",
            &[resident.clone(), one_over.clone(), other.clone()],
        ),
        case(
            "one character over the maximum, last",
            &[resident.clone(), other, one_over.clone()],
        ),
        case("under the minimum, alone", std::slice::from_ref(&under)),
        case(
            "under the minimum beside a well-formed key",
            &[resident, under.clone()],
        ),
        case(
            "two elements over the maximum",
            &[one_over.clone(), far_over],
        ),
        case(
            "one element over the maximum and one under the minimum",
            &[one_over, under],
        ),
    ]
}

/// Assert `reply` is the parameter validator's refusal of the `tagKeys` member,
/// and that nothing the request carried came back in it.
fn assert_element_refusal(reply: &Reply, case: &str, sent: &[String]) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(PARAMETER_VALIDATION),
        "case {case:?}: expected the parameter validator's code, got {actual:?}",
        actual = reply.error_code
    );
    let message = reply.message();
    assert_eq!(
        message,
        expected_message(),
        "case {case:?}: expected the element set stated at the list member, got {message:?}"
    );
    for value in sent.iter().filter(|value| value.len() > TAG_KEY_MAX) {
        assert!(
            !message.contains(value.as_str()),
            "case {case:?}: the refusal must echo no value, got {message:?}"
        );
    }
}

/// A table bucket of `name` carrying the tags `keys`, and the map a read of it
/// must report.
async fn tagged_bucket(
    emulator: &Emulator,
    name: &str,
    keys: &[&str],
) -> (String, BTreeMap<String, String>) {
    let arn = crate::harness::served(
        emulator
            .create_table_bucket(REGION, &json!({ "name": name }))
            .await,
        "CreateTableBucket",
        name,
    )
    .text("arn");
    let tags = tag_map(keys);
    crate::harness::served(
        emulator.tag_resource(REGION, &arn, &tags_body(&tags)).await,
        "TagResource",
        name,
    );
    (arn, tags)
}

#[tokio::test]
async fn a_tag_key_outside_its_modelled_length_is_refused_at_the_list_member() {
    let emulator = Emulator::new();
    let (arn, resident) = tagged_bucket(
        &emulator,
        "element-constraint-bucket",
        &["owner", "cost_center"],
    )
    .await;
    // The clause precedes every lookup, so an ARN nothing was created under is
    // refused with exactly the same sentence as a resident one.
    let absent = expected_bucket_arn(REGION, "element-constraint-absent");

    for test_case in cases() {
        for (form, resource) in [("a resident bucket", &arn), ("no bucket", &absent)] {
            let label = format!("{} on {form}", test_case.label);
            let keys: Vec<&str> = test_case.keys.iter().map(String::as_str).collect();
            let refused = emulator.untag_resource(REGION, resource, &keys).await;
            assert_element_refusal(&refused, &label, &test_case.keys);
        }

        // The refused call removed nothing — not even the well-formed keys that
        // travelled with the refused element.
        let after = listed_tags(&emulator, REGION, &arn, &test_case.label).await;
        assert_eq!(
            after, resident,
            "case {label:?}: a refused removal must leave every tag as it was, got {after:?}",
            label = test_case.label
        );
    }
}

#[tokio::test]
async fn a_tag_key_at_each_published_bound_is_served() {
    let emulator = Emulator::new();

    // The shortest and the longest key `TagKey` admits, so what the rows above
    // refuse is the keys outside the bound and not the member itself.
    for (index, length) in [TAG_KEY_MIN, TAG_KEY_MAX].into_iter().enumerate() {
        let label = format!("a key of {length} characters");
        let key = key_of(length);
        let (arn, _) = tagged_bucket(
            &emulator,
            &format!("element-bound-bucket-{index}"),
            &["owner"],
        )
        .await;
        let tags = tag_map(std::slice::from_ref(&key));
        crate::harness::served(
            emulator.tag_resource(REGION, &arn, &tags_body(&tags)).await,
            "TagResource",
            &label,
        );

        let removed = emulator.untag_resource(REGION, &arn, &[&key]).await;
        assert_eq!(
            removed.status, 204,
            "case {label:?}: a key at the published bound must be served, got {status} {body}",
            status = removed.status,
            body = removed.body
        );
        let after = listed_tags(&emulator, REGION, &arn, &label).await;
        assert!(
            !after.contains_key(&key),
            "case {label:?}: the named key must be gone, got {after:?}"
        );
        assert_eq!(
            after,
            tag_map(&["owner"]),
            "case {label:?}: every other tag must stay, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_removal_leaves_the_well_formed_removal_to_work() {
    let emulator = Emulator::new();
    let (arn, resident) =
        tagged_bucket(&emulator, "element-constraint-sequence", &["env", "owner"]).await;

    let too_long = key_of(TAG_KEY_MAX + 72);
    let refused = emulator
        .untag_resource(REGION, &arn, &[&too_long, "env"])
        .await;
    assert_element_refusal(&refused, "a refused removal", &[too_long]);

    let after_refusal = listed_tags(&emulator, REGION, &arn, "a refused removal").await;
    assert_eq!(
        after_refusal, resident,
        "case \"a refused removal\": the resident tags must survive it, got {after_refusal:?}"
    );

    let served = emulator.untag_resource(REGION, &arn, &["env"]).await;
    assert_eq!(
        served.status, 204,
        "case \"the well-formed removal\": expected 204, got {status} {body}",
        status = served.status,
        body = served.body
    );
    let after = listed_tags(&emulator, REGION, &arn, "the well-formed removal").await;
    assert_eq!(
        after,
        tag_map(&["owner"]),
        "case \"the well-formed removal\": exactly the named key must go, got {after:?}"
    );
}
