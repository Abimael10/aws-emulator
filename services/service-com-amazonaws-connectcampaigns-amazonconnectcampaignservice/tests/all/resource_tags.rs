// SPDX-License-Identifier: CC0-1.0

//! The tag plane of `/tags/{arn}`, end to end over HTTP.
//!
//! One rule, asserted over tables whose rows differ along the tag map asked for and the
//! ARN it is asked about rather than along the one request that exposed the gap:
//! `TagResource` MERGES the pairs it carries into the tag map of the resource its ARN
//! names — a key already there is re-valued, a key it does not name is left alone — and
//! `ListTagsForResource` REPORTS that stored map and writes nothing, so every reader of
//! the campaign (the tag listing and `DescribeCampaign`) reports one map. An ARN whose
//! resource path is not a campaign of this service is refused before any lookup, and a
//! campaign ARN the store holds nothing under is refused as a miss; both refusals store
//! nothing. Every write is scoped to the one resource its ARN names, so two campaigns —
//! siblings under one Connect instance or campaigns of different instances — hold
//! separate maps even when they carry the same key.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use service_com_amazonaws_connectcampaigns_amazonconnectcampaignservice::state::{
    tags_over_cap_message, MAX_TAGS_PER_RESOURCE, TAG_ARN_BAD_REQUEST_MESSAGE,
    TAG_ARN_NOT_FOUND_MESSAGE, TAG_BAD_REQUEST_CODE, TAG_NOT_FOUND_CODE,
};

use super::common::{call, router, Answer, Harness};

/// The region every case here signs for, and the region the ARNs under test are rooted
/// in.
const REGION: &str = "us-east-1";

/// The Connect instance every campaign below is created for.
const INSTANCE: &str = "tag-instance";

/// `TagValue`'s published length bounds (`@length(min 0, max 256)`, from
/// `smithy_query(query="shape", shape="TagValue")`): the empty value and a 256-character
/// one are both values a tag may carry.
const VALUE_MAX: usize = 256;

/// `TagKey`'s published length maximum (`@length(min 1, max 128)`, from
/// `smithy_query(query="shape", shape="TagKey")`).
const KEY_MAX: usize = 128;

/// A `CreateCampaign` body for `instance`, carrying `tags` when the case wants a
/// campaign born with some.
fn create_body(instance: &str, name: &str, tags: Option<Value>) -> Value {
    let mut body = json!({
        "name": name,
        "connectInstanceId": instance,
        "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.25}},
        "outboundCallConfig": {"connectContactFlowId": "flow-tag-1"},
    });
    if let Some(tags) = tags {
        body["tags"] = tags;
    }
    body
}

/// Register `instance` with the account and create one campaign under it, handing back
/// the ARN the create minted for it — the one fixture step every call of this suite
/// addresses a resource through.
async fn a_stored_campaign_in(
    router: &Harness,
    instance: &str,
    name: &str,
    tags: Option<Value>,
) -> String {
    router.register_connect_instance(REGION, instance);
    let created = call(
        router,
        REGION,
        "PUT",
        "/campaigns",
        &create_body(instance, name, tags),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case \"create {name} under {instance}\": expected HTTP 200, got {} {}",
        created.status, created.body
    );
    created.body["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"create {name}\": no arn in {}", created.body))
        .to_string()
}

/// [`a_stored_campaign_in`] under the instance most of this suite's cases use.
async fn a_stored_campaign(router: &Harness, name: &str, tags: Option<Value>) -> String {
    a_stored_campaign_in(router, INSTANCE, name, tags).await
}

/// The campaign id an ARN this service minted ends with.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/')
        .next()
        .unwrap_or_else(|| panic!("the minted arn {arn:?} carries a resource id"))
}

/// The `{arn}` path label of `/tags/{arn}`, percent-encoded as a single segment.
fn encoded(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// A JSON tag map from `(key, value)` pairs.
fn tag_map<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
    Value::Object(map)
}

/// `count` distinct tag keys with values, for a case about how MANY tags a resource
/// holds rather than about what one key looks like.
fn generated_tags(count: usize) -> Value {
    let mut map = Map::new();
    for index in 0..count {
        map.insert(format!("key{index:04}"), Value::String(format!("v{index}")));
    }
    Value::Object(map)
}

/// The `tags` member of a response, as an ordered map so a mismatch prints
/// deterministically; empty when the response carries none.
fn tags_of(body: &Value) -> BTreeMap<String, String> {
    body.get("tags")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or("<not a string>").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `POST /tags/{arn}` with the tag map `tags`.
async fn tag(router: &Harness, arn: &str, tags: &Value) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!("/tags/{}", encoded(arn)),
        &json!({"tags": tags}),
    )
    .await
}

/// `GET /tags/{arn}`.
async fn list_tags(router: &Harness, arn: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/tags/{}", encoded(arn)),
        &Value::Null,
    )
    .await
}

/// `DELETE /tags/{arn}` with one `tagKeys` query parameter per key — the binding
/// `UntagResource$tagKeys` carries (`@httpQuery("tagKeys")`).
async fn untag(router: &Harness, arn: &str, keys: &[&str]) -> Answer {
    let mut path = format!("/tags/{}", encoded(arn));
    for (index, key) in keys.iter().enumerate() {
        let separator = if index == 0 { '?' } else { '&' };
        path.push(separator);
        path.push_str("tagKeys=");
        path.push_str(&encoded_query(key));
    }
    call(router, REGION, "DELETE", &path, &Value::Null).await
}

/// One `tagKeys` value, percent-encoded for a query string: the key shape admits `/`,
/// `:`, `+` and `=`, each of which has its own meaning in a URL.
fn encoded_query(key: &str) -> String {
    key.replace('%', "%25")
        .replace('+', "%2B")
        .replace('=', "%3D")
        .replace('&', "%26")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// The tags `ListTagsForResource` reports for `arn`, asserting it answered and that the
/// answer carries its `tags` member at all — the listing always does, even for a resource
/// holding no tag, so a client can read it unconditionally.
async fn listed_tags(router: &Harness, arn: &str, case: &str) -> BTreeMap<String, String> {
    let answer = list_tags(router, arn).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from the tag listing, got {} {}",
        answer.status, answer.body
    );
    assert!(
        answer.body.get("tags").and_then(Value::as_object).is_some(),
        "case {case:?}: expected the listing to carry a tags member, got {}",
        answer.body
    );
    tags_of(&answer.body)
}

/// The tags `DescribeCampaign` reports for the campaign `arn` names — the second reader
/// of the one stored map.
async fn described_tags(router: &Harness, arn: &str, case: &str) -> BTreeMap<String, String> {
    let answer = call(
        router,
        REGION,
        "GET",
        &format!("/campaigns/{}", id_of(arn)),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from the describe, got {} {}",
        answer.status, answer.body
    );
    tags_of(&answer.body["campaign"])
}

/// One row of the merge table: what the call carries, and the map the resource holds
/// afterwards.
struct MergeRow {
    case: &'static str,
    sent: Value,
    expected: Vec<(String, String)>,
}

/// One row of the untag table: the keys the call names, and the map the resource holds
/// afterwards.
struct UntagRow {
    case: &'static str,
    keys: Vec<&'static str>,
    expected: Vec<(&'static str, &'static str)>,
}

/// The successive tag calls one resource takes, each row differing along the map it
/// carries: how many pairs, the characters a key uses, the length of a key and of a
/// value at the published bounds (`TagKey` 1..128, `TagValue` 0..256), re-valuing a key
/// the resource already holds, and carrying no pair at all.
fn merge_rows() -> Vec<MergeRow> {
    let long_value = "v".repeat(VALUE_MAX);
    let long_key = "k".repeat(KEY_MAX);
    vec![
        MergeRow {
            case: "one pair onto an untagged resource",
            sent: tag_map([("env", "dev")]),
            expected: vec![("env".to_string(), "dev".to_string())],
        },
        MergeRow {
            case: "two more pairs, leaving the first alone",
            sent: tag_map([("team", "core"), ("tier", "gold")]),
            expected: vec![
                ("env".to_string(), "dev".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
        MergeRow {
            case: "re-valuing a key the resource already holds",
            sent: tag_map([("env", "prod")]),
            expected: vec![
                ("env".to_string(), "prod".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
        MergeRow {
            // The character class of `TagKey`'s pattern spells ASCII letters, `_` and
            // the `+`..`=` range (`+ , - . / 0-9 : ; < =`).
            case: "a key using the punctuation the key shape admits",
            sent: tag_map([("a_b.c:d/e+f=g", "punctuated")]),
            expected: vec![
                ("a_b.c:d/e+f=g".to_string(), "punctuated".to_string()),
                ("env".to_string(), "prod".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
        MergeRow {
            case: "the empty value the value shape's minimum admits",
            sent: tag_map([("blank", "")]),
            expected: vec![
                ("a_b.c:d/e+f=g".to_string(), "punctuated".to_string()),
                ("blank".to_string(), String::new()),
                ("env".to_string(), "prod".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
        MergeRow {
            // `TagValue` carries no pattern, so a value is any text within its bounds.
            case: "a non-ASCII value",
            sent: tag_map([("saison", "été")]),
            expected: vec![
                ("a_b.c:d/e+f=g".to_string(), "punctuated".to_string()),
                ("blank".to_string(), String::new()),
                ("env".to_string(), "prod".to_string()),
                ("saison".to_string(), "été".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
        MergeRow {
            case: "a key and a value at their published length maxima",
            sent: tag_map([(long_key.as_str(), long_value.as_str())]),
            expected: vec![
                ("a_b.c:d/e+f=g".to_string(), "punctuated".to_string()),
                ("blank".to_string(), String::new()),
                ("env".to_string(), "prod".to_string()),
                (long_key.clone(), long_value.clone()),
                ("saison".to_string(), "été".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
        MergeRow {
            case: "a call carrying no pair at all",
            sent: Value::Object(Map::new()),
            expected: vec![
                ("a_b.c:d/e+f=g".to_string(), "punctuated".to_string()),
                ("blank".to_string(), String::new()),
                ("env".to_string(), "prod".to_string()),
                (long_key, long_value),
                ("saison".to_string(), "été".to_string()),
                ("team".to_string(), "core".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
        },
    ]
}

#[tokio::test]
async fn tagging_merges_into_the_one_map_every_later_read_reports() {
    let router = router();
    let arn = a_stored_campaign(&router, "tag-merge", None).await;

    // A resource nobody has tagged holds no tags, so the listing reports none.
    let initial = listed_tags(&router, &arn, "an untagged campaign").await;
    assert!(
        initial.is_empty(),
        "case \"an untagged campaign\": expected no tags, got {initial:?}"
    );

    for row in merge_rows() {
        let tagged = tag(&router, &arn, &row.sent).await;
        assert_eq!(
            tagged.status, 200,
            "case {:?}: expected HTTP 200 from the tag call, got {} {}",
            row.case, tagged.status, tagged.body
        );

        let expected: BTreeMap<String, String> = row.expected.into_iter().collect();
        let listed = listed_tags(&router, &arn, row.case).await;
        assert_eq!(
            listed, expected,
            "case {:?}: expected the merged map from the tag listing, got {listed:?}",
            row.case
        );

        // The second reader of the same stored map agrees with the first.
        let described = described_tags(&router, &arn, row.case).await;
        assert_eq!(
            described, expected,
            "case {:?}: expected the describe to report the same map, got {described:?}",
            row.case
        );

        // And the listing is a READ: looking again reports what the first look did.
        let again = listed_tags(&router, &arn, row.case).await;
        assert_eq!(
            again, expected,
            "case {:?}: expected a second listing to report the same map, got {again:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_tags_a_create_was_given_are_what_the_listing_reports() {
    let router = router();

    // Rows differ along the map the create carried: none at all, one pair, and several.
    struct CreatedRow {
        case: &'static str,
        sent: Option<Value>,
        expected: Vec<(&'static str, &'static str)>,
    }
    let rows: Vec<CreatedRow> = vec![
        CreatedRow {
            case: "a create carrying no tags",
            sent: None,
            expected: Vec::new(),
        },
        CreatedRow {
            case: "a create carrying one pair",
            sent: Some(tag_map([("owner", "tag-suite")])),
            expected: vec![("owner", "tag-suite")],
        },
        CreatedRow {
            case: "a create carrying three pairs",
            sent: Some(tag_map([("a", "1"), ("b", "2"), ("c", "3")])),
            expected: vec![("a", "1"), ("b", "2"), ("c", "3")],
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let arn = a_stored_campaign(&router, &format!("created-{index}"), row.sent).await;
        let expected: BTreeMap<String, String> = row
            .expected
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let listed = listed_tags(&router, &arn, row.case).await;
        assert_eq!(
            listed, expected,
            "case {:?}: expected the listing to report the tags the create stored, \
             got {listed:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_arn_that_names_no_campaign_of_this_service_is_refused_before_any_lookup() {
    let router = router();
    let arn = a_stored_campaign(&router, "untouched", Some(tag_map([("keep", "me")]))).await;

    // Rows span the ways an ARN of the published shape (`Arn`, len 20..500, pattern
    // `^arn:.*$`) can still name no campaign of this service: another vendor's ARN, this
    // service's namespace with another resource type, a resource portion with no id, and
    // a resource portion that is a bare id.
    let rows: Vec<(&str, String)> = vec![
        (
            "another vendor's ARN",
            "arn:aws:connect:us-east-1:000000000000:instance/abcdef".to_string(),
        ),
        (
            "this service's namespace with an unknown resource type",
            arn.replace("campaign/", "queue/"),
        ),
        (
            "a campaign resource portion with no id",
            arn.rsplit_once('/')
                .map(|(prefix, _)| prefix.to_string())
                .expect("the minted arn carries a resource id"),
        ),
        (
            "a resource portion that is a bare id",
            arn.replace("campaign/", ""),
        ),
    ];

    for (case, bad_arn) in rows {
        for (verb, answer) in [
            ("the tag listing", list_tags(&router, &bad_arn).await),
            (
                "the tag call",
                tag(&router, &bad_arn, &tag_map([("env", "dev")])).await,
            ),
            ("the untag call", untag(&router, &bad_arn, &["env"]).await),
        ] {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400 from {verb}, got {} {}",
                answer.status, answer.body
            );
            assert!(
                answer.error_code.ends_with(TAG_BAD_REQUEST_CODE),
                "case {case:?}: expected {TAG_BAD_REQUEST_CODE} from {verb}, got {:?}",
                answer.error_code
            );
            assert_eq!(
                answer.message(),
                TAG_ARN_BAD_REQUEST_MESSAGE,
                "case {case:?}: expected this service's unresolvable-ARN wording from \
                 {verb}, got {}",
                answer.body
            );
        }
    }

    // None of those refusals touched the resource that does exist.
    let kept = listed_tags(&router, &arn, "the stored campaign after every refusal").await;
    assert_eq!(
        kept,
        BTreeMap::from([("keep".to_string(), "me".to_string())]),
        "case \"the stored campaign after every refusal\": expected its tags untouched, \
         got {kept:?}"
    );
}

#[tokio::test]
async fn a_campaign_arn_the_store_holds_nothing_under_is_refused_as_a_miss() {
    let router = router();
    let arn = a_stored_campaign(&router, "present", Some(tag_map([("keep", "me")]))).await;

    // A campaign created and then deleted: its ARN is well formed and names nothing.
    let dropped = a_stored_campaign(&router, "to-be-deleted", None).await;
    let deleted = call(
        &router,
        REGION,
        "DELETE",
        &format!("/campaigns/{}", id_of(&dropped)),
        &Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case \"delete a campaign to address afterwards\": expected HTTP 200, got {} {}",
        deleted.status, deleted.body
    );

    // Rows differ along WHICH part of a well-formed campaign ARN names nothing: the id,
    // the region, the account, and an ARN whose campaign has been deleted. An ARN names
    // a partition, a region and an account as well as a resource, so one differing in
    // any of them names a different resource.
    let rows: Vec<(&str, String)> = vec![
        (
            "a campaign id nothing was ever created under",
            arn.replace(
                id_of(&arn),
                "2f9c7b416d054e389a210b7c5d4e3f20",
            ),
        ),
        ("another region", arn.replace(REGION, "us-west-2")),
        (
            "another account",
            arn.replace(":000000000000:", ":111122223333:"),
        ),
        ("a campaign a delete has dropped", dropped),
    ];

    for (case, missing) in rows {
        for (verb, answer) in [
            ("the tag listing", list_tags(&router, &missing).await),
            (
                "the tag call",
                tag(&router, &missing, &tag_map([("env", "dev")])).await,
            ),
            ("the untag call", untag(&router, &missing, &["env"]).await),
        ] {
            assert_eq!(
                answer.status, 404,
                "case {case:?}: expected HTTP 404 from {verb}, got {} {}",
                answer.status, answer.body
            );
            assert!(
                answer.error_code.ends_with(TAG_NOT_FOUND_CODE),
                "case {case:?}: expected {TAG_NOT_FOUND_CODE} from {verb}, got {:?}",
                answer.error_code
            );
            assert_eq!(
                answer.message(),
                TAG_ARN_NOT_FOUND_MESSAGE,
                "case {case:?}: expected this service's tag-plane not-found wording from \
                 {verb}, got {}",
                answer.body
            );
        }
    }

    let kept = listed_tags(&router, &arn, "the stored campaign after every miss").await;
    assert_eq!(
        kept,
        BTreeMap::from([("keep".to_string(), "me".to_string())]),
        "case \"the stored campaign after every miss\": expected its tags untouched, \
         got {kept:?}"
    );
}

#[tokio::test]
async fn a_merge_past_the_per_resource_tag_ceiling_is_refused_and_writes_nothing() {
    let router = router();
    let arn = a_stored_campaign(&router, "ceiling", None).await;

    // At the ceiling the merge is taken; one pair past it is refused and the resource is
    // left holding exactly what it held.
    let at_ceiling = tag(&router, &arn, &generated_tags(MAX_TAGS_PER_RESOURCE)).await;
    assert_eq!(
        at_ceiling.status, 200,
        "case \"a map at the per-resource ceiling\": expected HTTP 200, got {} {}",
        at_ceiling.status, at_ceiling.body
    );
    let full = listed_tags(&router, &arn, "a map at the per-resource ceiling").await;
    assert_eq!(
        full.len(),
        MAX_TAGS_PER_RESOURCE,
        "case \"a map at the per-resource ceiling\": expected {MAX_TAGS_PER_RESOURCE} \
         tags, got {full:?}"
    );

    // Rows differ along how far past the ceiling the call would take the resource.
    let rows: Vec<(&str, Value, usize)> = vec![
        (
            "one pair past the ceiling",
            tag_map([("one-too-many", "v")]),
            MAX_TAGS_PER_RESOURCE + 1,
        ),
        (
            "ten pairs past the ceiling",
            generated_tags(MAX_TAGS_PER_RESOURCE + 10),
            MAX_TAGS_PER_RESOURCE + 10,
        ),
    ];

    for (case, sent, would_hold) in rows {
        let refused = tag(&router, &arn, &sent).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 past the ceiling, got {} {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_code.ends_with("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            tags_over_cap_message(would_hold),
            "case {case:?}: expected the ceiling stated with the count the resource would \
             have held, got {}",
            refused.body
        );
        let after = listed_tags(&router, &arn, case).await;
        assert_eq!(
            after, full,
            "case {case:?}: a refused merge writes nothing, got {after:?}"
        );
    }

    // Re-valuing keys the resource already holds is within the ceiling, so a full
    // resource still takes a merge that adds no key.
    let revalued = tag(&router, &arn, &generated_tags(MAX_TAGS_PER_RESOURCE)).await;
    assert_eq!(
        revalued.status, 200,
        "case \"re-valuing every key of a full resource\": expected HTTP 200, got {} {}",
        revalued.status, revalued.body
    );
}

#[tokio::test]
async fn untagging_removes_only_the_keys_it_names_and_an_absent_key_is_a_no_op() {
    let router = router();
    let arn = a_stored_campaign(&router, "untag", None).await;
    let tagged = tag(
        &router,
        &arn,
        &tag_map([
            ("env", "dev"),
            ("team", "core"),
            ("tier", "gold"),
            ("a_b.c:d/e+f=g", "punctuated"),
        ]),
    )
    .await;
    assert_eq!(
        tagged.status, 200,
        "case \"tag the resource to untag\": expected HTTP 200, got {} {}",
        tagged.status, tagged.body
    );

    // Rows differ along which keys the call names: one the resource carries, one it never
    // carried, a mix of both, a key whose punctuation the key shape admits, and finally
    // every key that is left. `tagKeys`' published minimum of zero has no row: the member
    // is `@required` and bound to the query string, so a request carrying none of it names
    // no route at all rather than reaching this operation.
    let rows: Vec<UntagRow> = vec![
        UntagRow {
            case: "one key the resource carries",
            keys: vec!["env"],
            expected: vec![
                ("a_b.c:d/e+f=g", "punctuated"),
                ("team", "core"),
                ("tier", "gold"),
            ],
        },
        UntagRow {
            case: "a key the resource never carried",
            keys: vec!["absent"],
            expected: vec![
                ("a_b.c:d/e+f=g", "punctuated"),
                ("team", "core"),
                ("tier", "gold"),
            ],
        },
        UntagRow {
            case: "the same key again, already removed",
            keys: vec!["env"],
            expected: vec![
                ("a_b.c:d/e+f=g", "punctuated"),
                ("team", "core"),
                ("tier", "gold"),
            ],
        },
        UntagRow {
            case: "one present and one absent key together",
            keys: vec!["team", "absent"],
            expected: vec![("a_b.c:d/e+f=g", "punctuated"), ("tier", "gold")],
        },
        UntagRow {
            case: "a key carrying the punctuation the key shape admits",
            keys: vec!["a_b.c:d/e+f=g"],
            expected: vec![("tier", "gold")],
        },
        UntagRow {
            case: "every key that is left",
            keys: vec!["tier"],
            expected: Vec::new(),
        },
    ];

    for row in rows {
        let answer = untag(&router, &arn, &row.keys).await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected HTTP 200 from the untag call, got {} {}",
            row.case, answer.status, answer.body
        );

        let expected: BTreeMap<String, String> = row
            .expected
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let listed = listed_tags(&router, &arn, row.case).await;
        assert_eq!(
            listed, expected,
            "case {:?}: expected only the named keys gone, got {listed:?}",
            row.case
        );
        let described = described_tags(&router, &arn, row.case).await;
        assert_eq!(
            described, expected,
            "case {:?}: expected the describe to report the same map, got {described:?}",
            row.case
        );
    }

    // A resource every tag has been removed from takes tags again, so the removal left a
    // live resource rather than an unusable one.
    let again = tag(&router, &arn, &tag_map([("env", "prod")])).await;
    assert_eq!(
        again.status, 200,
        "case \"tag an untagged resource again\": expected HTTP 200, got {} {}",
        again.status, again.body
    );
    let listed = listed_tags(&router, &arn, "tag an untagged resource again").await;
    assert_eq!(
        listed,
        BTreeMap::from([("env".to_string(), "prod".to_string())]),
        "case \"tag an untagged resource again\": expected the new pair, got {listed:?}"
    );
}

/// A second Connect instance, so a case can hold campaigns under more than one parent
/// and show that a tag write is scoped to the resource rather than to its instance.
const OTHER_INSTANCE: &str = "tag-instance-other";

/// What one row of the scoping table asks of the one resource it addresses.
enum Scoped {
    /// `TagResource` carrying this map.
    Tag(Value),
    /// `UntagResource` naming these keys.
    Untag(Vec<&'static str>),
}

/// One row of the scoping table: which of the resources under test the call addresses,
/// what it asks of it, and the map EVERY resource under test holds afterwards.
struct ScopeRow {
    case: &'static str,
    target: usize,
    asked: Scoped,
    expected: [Vec<(&'static str, &'static str)>; 3],
}

#[tokio::test]
async fn a_tag_write_lands_on_exactly_the_resource_its_arn_names() {
    let router = router();

    // Three resources: two campaigns under one Connect instance and a third under
    // another, so a row can show that neither sharing a parent nor sharing a tag key
    // joins two tag maps.
    let arns = [
        a_stored_campaign_in(&router, INSTANCE, "scope-a", None).await,
        a_stored_campaign_in(&router, INSTANCE, "scope-b", None).await,
        a_stored_campaign_in(&router, OTHER_INSTANCE, "scope-c", None).await,
    ];

    // Rows differ along which resource the call addresses, whether it adds or removes,
    // and whether the key it names is one another resource also carries.
    let rows: Vec<ScopeRow> = vec![
        ScopeRow {
            case: "tagging the first campaign",
            target: 0,
            asked: Scoped::Tag(tag_map([("env", "dev"), ("owner", "a")])),
            expected: [
                vec![("env", "dev"), ("owner", "a")],
                Vec::new(),
                Vec::new(),
            ],
        },
        ScopeRow {
            case: "a sibling under the same instance taking its own value for that key",
            target: 1,
            asked: Scoped::Tag(tag_map([("env", "prod")])),
            expected: [
                vec![("env", "dev"), ("owner", "a")],
                vec![("env", "prod")],
                Vec::new(),
            ],
        },
        ScopeRow {
            case: "a campaign under another instance taking the same keys",
            target: 2,
            asked: Scoped::Tag(tag_map([("env", "test"), ("owner", "c")])),
            expected: [
                vec![("env", "dev"), ("owner", "a")],
                vec![("env", "prod")],
                vec![("env", "test"), ("owner", "c")],
            ],
        },
        ScopeRow {
            case: "the same map again on the first campaign",
            target: 0,
            asked: Scoped::Tag(tag_map([("env", "dev"), ("owner", "a")])),
            expected: [
                vec![("env", "dev"), ("owner", "a")],
                vec![("env", "prod")],
                vec![("env", "test"), ("owner", "c")],
            ],
        },
        ScopeRow {
            case: "removing a key the other two also carry from the second campaign",
            target: 1,
            asked: Scoped::Untag(vec!["env"]),
            expected: [
                vec![("env", "dev"), ("owner", "a")],
                Vec::new(),
                vec![("env", "test"), ("owner", "c")],
            ],
        },
        ScopeRow {
            case: "removing one carried and one absent key from the third campaign",
            target: 2,
            asked: Scoped::Untag(vec!["owner", "absent"]),
            expected: [
                vec![("env", "dev"), ("owner", "a")],
                Vec::new(),
                vec![("env", "test")],
            ],
        },
        ScopeRow {
            case: "re-valuing one key of the first campaign",
            target: 0,
            asked: Scoped::Tag(tag_map([("owner", "a2")])),
            expected: [
                vec![("env", "dev"), ("owner", "a2")],
                Vec::new(),
                vec![("env", "test")],
            ],
        },
    ];

    for row in rows {
        let target = &arns[row.target];
        let answer = match &row.asked {
            Scoped::Tag(sent) => tag(&router, target, sent).await,
            Scoped::Untag(keys) => untag(&router, target, keys).await,
        };
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected HTTP 200 from the addressed resource, got {} {}",
            row.case, answer.status, answer.body
        );

        for (index, arn) in arns.iter().enumerate() {
            let expected: BTreeMap<String, String> = row.expected[index]
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            let listed = listed_tags(&router, arn, row.case).await;
            assert_eq!(
                listed, expected,
                "case {:?}: expected campaign {index} to hold its own map, got {listed:?}",
                row.case
            );
            let described = described_tags(&router, arn, row.case).await;
            assert_eq!(
                described, expected,
                "case {:?}: expected the describe of campaign {index} to report the same \
                 map, got {described:?}",
                row.case
            );
        }
    }
}
