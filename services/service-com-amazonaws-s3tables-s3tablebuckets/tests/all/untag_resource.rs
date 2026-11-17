// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of removing tags from an S3 Tables resource.
//!
//! The rules under test:
//!
//! * `UntagResource` removes from the table bucket or the table its `resourceArn`
//!   resolves to EXACTLY the keys the request names and nothing else, and leaves
//!   every other resource alone. It is idempotent, as the model declares it: a key
//!   the resource does not carry is already gone, so the call is served (HTTP 204)
//!   and changes nothing, and a second identical call changes nothing more.
//! * A `resourceArn` is resolved against the store, as it is for a tag write: the
//!   ARN of a table bucket or a table the store does not hold — a deleted one
//!   included, since its ARN stays well formed — is the modelled
//!   `NotFoundException` (HTTP 404), and a value that addresses no taggable
//!   resource at all is a `BadRequestException` (HTTP 400). Neither removes
//!   anything.
//! * The reserved namespace belongs to the service, so a key a caller may not
//!   write is one it may not take away either: a caller naming an `aws:` key is
//!   refused with the sentence `TagResource` refuses such a key with, and the
//!   request removes nothing at all — while the in-process path this service's own
//!   control plane sets such tags on does remove them.
//!
//! The table spans what the rule generalises over rather than one recorded
//! request: which resource form the ARN names, how many keys the request names,
//! whether a named key is resident, named twice, differs from a resident one only
//! in case (tag keys are case sensitive), carries the punctuation or the space a
//! tag key may hold, is the longest key `TagKey` models, and how many tags the resource holds
//! — up to the bound `TagResource` publishes. The key length bound comes from
//! `TagKey` (`@length(1, 128)`) and the punctuation set from the sentence this
//! service refuses an unacceptable key with
//! (`smithy_query(query="shape", shape="TagKey")`).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::harness::{expected_bucket_arn, Emulator, Reply};

const REGION: &str = "us-east-1";

/// The sentences this service states each refusal class with. No variable part:
/// that is exactly what is asserted below.
const NO_BUCKET: &str = "The specified bucket does not exist.";
const NO_TABLE: &str = "The specified table does not exist.";
const INVALID_RESOURCE_ARN: &str = "The specified resourceArn is not valid.";
const RESERVED_KEY: &str = "Tags beginning with 'aws:' are reserved for system use.";

/// The bound `TagResource` publishes: "You can add up to 50 tags for each S3
/// resource."
const MAX_TAGS_PER_RESOURCE: usize = 50;

/// One removal case: what the resource carries before the call, which keys the
/// request names, and which keys the resource must carry after it.
struct Case {
    label: String,
    resident: Vec<String>,
    remove: Vec<String>,
    expected: Vec<String>,
}

fn case(label: &str, resident: &[&str], remove: &[&str], expected: &[&str]) -> Case {
    Case {
        label: label.to_string(),
        resident: resident.iter().map(|key| (*key).to_string()).collect(),
        remove: remove.iter().map(|key| (*key).to_string()).collect(),
        expected: expected.iter().map(|key| (*key).to_string()).collect(),
    }
}

/// The removal cases, each differing from the next along what the rule
/// generalises over: how many keys are named, whether each is resident, how it is
/// spelled, and how many tags the resource holds.
fn cases() -> Vec<Case> {
    // The longest key `TagKey` models (`@length(1, 128)`), so a case drives the
    // published bound rather than a convenient short name.
    let longest = "k".repeat(128);
    // A key made of the punctuation a tag key may carry, which also has to
    // survive the query string the keys travel in.
    let punctuated = "cost:center/2+3=4@team_a.b-c";
    // A resource holding the most tags one may hold, so a removal is exercised at
    // the published bound as well as on a handful of keys.
    let at_the_bound: Vec<String> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| format!("key_{index:02}"))
        .collect();
    let bound_keys: Vec<&str> = at_the_bound.iter().map(String::as_str).collect();
    let bound_survivors: Vec<&str> = bound_keys
        .iter()
        .copied()
        .filter(|key| *key != "key_07")
        .collect();

    vec![
        case(
            "one key of three",
            &["alpha", "beta", "gamma"],
            &["beta"],
            &["alpha", "gamma"],
        ),
        case(
            "two keys of three",
            &["alpha", "beta", "gamma"],
            &["gamma", "alpha"],
            &["beta"],
        ),
        case(
            "every key the resource carries",
            &["alpha", "beta", "gamma"],
            &["alpha", "beta", "gamma"],
            &[],
        ),
        case(
            "a key the resource does not carry",
            &["alpha", "beta"],
            &["delta"],
            &["alpha", "beta"],
        ),
        case(
            "a resident key beside an absent one",
            &["alpha", "beta"],
            &["alpha", "delta"],
            &["beta"],
        ),
        case(
            "the same key named twice",
            &["alpha", "beta"],
            &["alpha", "alpha"],
            &["beta"],
        ),
        // Tag keys are case sensitive, so a key differing only in case is
        // another key and the resident one stays.
        case(
            "a key differing from a resident one only in case",
            &["Owner", "owner_team"],
            &["owner", "OWNER_TEAM"],
            &["Owner", "owner_team"],
        ),
        case("a key on a resource carrying none", &[], &["alpha"], &[]),
        case(
            "a key carrying the punctuation a tag key may hold",
            &[punctuated, "alpha"],
            &[punctuated],
            &["alpha"],
        ),
        // A tag key may carry a space as well as that punctuation, so a removal
        // has to name one too.
        case(
            "a key carrying a space",
            &["cost center", "alpha"],
            &["cost center"],
            &["alpha"],
        ),
        case(
            "the longest key TagKey models",
            &[&longest, "alpha"],
            &[&longest],
            &["alpha"],
        ),
        case(
            "one key of a resource holding the published maximum",
            &bound_keys,
            &["key_07"],
            &bound_survivors,
        ),
    ]
}

/// The value a key carries, derived from the key so a surviving tag's value is
/// visibly its own.
fn value_of(key: &str) -> String {
    format!("value-for-{key}")
}

/// `keys` as the tag map a write applies, and as the map a read must report.
fn tag_map(keys: &[String]) -> BTreeMap<String, String> {
    keys.iter()
        .map(|key| (key.clone(), value_of(key)))
        .collect()
}

/// A tag map as the `tags` member of a `TagResource` request.
fn tags_body(tags: &BTreeMap<String, String>) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<Map<String, Value>>(),
    )
}

/// Assert a setup call was served, and hand the reply on: the suites' one copy of that
/// assertion, named for what this suite calls.
fn served(reply: Reply, case: &str) -> Reply {
    crate::harness::served(reply, "the setup call", case)
}

/// A table bucket with one namespace and one table in it: one live resource of
/// each taggable form.
async fn resources(emulator: &Emulator, bucket: &str) -> (String, String) {
    let bucket_arn = served(
        emulator
            .create_table_bucket(REGION, &json!({ "name": bucket }))
            .await,
        bucket,
    )
    .text("arn");
    served(
        emulator
            .create_namespace(REGION, &bucket_arn, &json!({ "namespace": ["ns"] }))
            .await,
        bucket,
    );
    let table_arn = served(
        emulator
            .create_table(
                REGION,
                &bucket_arn,
                "ns",
                &json!({ "name": "subject_table", "format": "ICEBERG" }),
            )
            .await,
        bucket,
    )
    .text("tableARN");
    (bucket_arn, table_arn)
}

/// The tags `ListTagsForResource` reports for `resource_arn`.
async fn listed_tags(
    emulator: &Emulator,
    resource_arn: &str,
    case: &str,
) -> BTreeMap<String, String> {
    let listed = emulator.list_tags_for_resource(REGION, resource_arn).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: the read of {resource_arn} must be served, got {status} {body}",
        status = listed.status,
        body = listed.body
    );
    listed
        .body
        .get("tags")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Apply `tags` to `resource_arn`, asserting the write is served.
async fn apply_tags(
    emulator: &Emulator,
    resource_arn: &str,
    tags: &BTreeMap<String, String>,
    case: &str,
) {
    if tags.is_empty() {
        return;
    }
    served(
        emulator
            .tag_resource(REGION, resource_arn, &tags_body(tags))
            .await,
        case,
    );
}

/// Assert `reply` is the refusal `expected_message` states, carrying `status` and
/// `code`.
fn assert_refusal(reply: &Reply, status: u16, code: &str, expected_message: &str, case: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected status {status}, got {actual} {body}",
        actual = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(code),
        "case {case:?}: expected the modelled {code}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        expected_message,
        "case {case:?}: expected the service's sentence, got {actual:?}",
        actual = reply.message()
    );
}

#[tokio::test]
async fn untag_removes_only_the_named_keys_and_repeating_it_changes_nothing() {
    let emulator = Emulator::new();

    for (index, test_case) in cases().into_iter().enumerate() {
        // Both taggable forms, each in its own table bucket, so one case's
        // removals cannot be read in another's resource.
        for (form, slug) in [("a table bucket", "bucket"), ("a table", "table")] {
            let label = format!("{} on {form}", test_case.label);
            let (bucket_arn, table_arn) =
                resources(&emulator, &format!("untag-case-{index}-{slug}")).await;
            let (subject, bystander) = if slug == "bucket" {
                (bucket_arn, table_arn)
            } else {
                (table_arn, bucket_arn)
            };

            // Both resources start out carrying the case's tag set, so the call
            // is shown to touch only the resource it addresses.
            let resident = tag_map(&test_case.resident);
            apply_tags(&emulator, &subject, &resident, &label).await;
            apply_tags(&emulator, &bystander, &resident, &label).await;

            let keys: Vec<&str> = test_case.remove.iter().map(String::as_str).collect();
            let removed = emulator.untag_resource(REGION, &subject, &keys).await;
            assert_eq!(
                removed.status, 204,
                "case {label:?}: the removal must be served, got {status} {body}",
                status = removed.status,
                body = removed.body
            );

            let expected = tag_map(&test_case.expected);
            let after = listed_tags(&emulator, &subject, &label).await;
            assert_eq!(
                after, expected,
                "case {label:?}: only the named keys may go, got {after:?}"
            );

            let untouched = listed_tags(&emulator, &bystander, &label).await;
            assert_eq!(
                untouched, resident,
                "case {label:?}: another resource's tags must be left alone, got {untouched:?}"
            );

            // Idempotent: the same removal again is served and changes nothing,
            // which is also what a request naming a key nothing carries asks for.
            let again = emulator.untag_resource(REGION, &subject, &keys).await;
            assert_eq!(
                again.status, 204,
                "case {label:?}: a repeated removal must be served, got {status} {body}",
                status = again.status,
                body = again.body
            );
            let after_again = listed_tags(&emulator, &subject, &label).await;
            assert_eq!(
                after_again, after,
                "case {label:?}: a repeated removal must change nothing, got {after_again:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_untag_whose_arn_reaches_no_resource_is_refused_and_removes_nothing() {
    let emulator = Emulator::new();
    let (bucket_arn, table_arn) = resources(&emulator, "untag-refusal-bucket").await;

    // The live resources carry tags no refused call may touch.
    let resident = tag_map(&["owner".to_string(), "cost_center".to_string()]);
    apply_tags(&emulator, &bucket_arn, &resident, "the live resources").await;
    apply_tags(&emulator, &table_arn, &resident, "the live resources").await;

    // A table whose ARN stays well formed after it is deleted.
    let doomed_table = served(
        emulator
            .create_table(
                REGION,
                &bucket_arn,
                "ns",
                &json!({ "name": "doomed_table", "format": "ICEBERG" }),
            )
            .await,
        "a deleted table",
    )
    .text("tableARN");
    assert_eq!(
        emulator
            .delete_table(REGION, &bucket_arn, "ns", "doomed_table", &[])
            .await
            .status,
        204,
        "the table must be deleted"
    );

    // An empty table bucket, deleted the same way.
    let doomed_bucket = served(
        emulator
            .create_table_bucket(REGION, &json!({ "name": "untag-refusal-doomed" }))
            .await,
        "a deleted table bucket",
    )
    .text("arn");
    assert_eq!(
        emulator.delete_table_bucket(REGION, &doomed_bucket).await.status,
        204,
        "the empty bucket must be deleted"
    );

    let refusals = [
        (
            "a table bucket the store never held".to_string(),
            expected_bucket_arn(REGION, "untag-refusal-absent"),
            404,
            "NotFoundException",
            NO_BUCKET,
        ),
        (
            "a deleted table bucket".to_string(),
            doomed_bucket,
            404,
            "NotFoundException",
            NO_BUCKET,
        ),
        (
            "a table the store never held".to_string(),
            format!("{bucket_arn}/table/absent-table-id"),
            404,
            "NotFoundException",
            NO_TABLE,
        ),
        (
            "a deleted table".to_string(),
            doomed_table,
            404,
            "NotFoundException",
            NO_TABLE,
        ),
        (
            "an ARN carrying a segment past a table's".to_string(),
            format!("{table_arn}/extra"),
            400,
            "BadRequestException",
            INVALID_RESOURCE_ARN,
        ),
        (
            "an ARN naming a resource class this service does not tag".to_string(),
            format!("{bucket_arn}/namespace/ns"),
            400,
            "BadRequestException",
            INVALID_RESOURCE_ARN,
        ),
    ];

    for (label, resource_arn, status, code, message) in &refusals {
        let refused = emulator
            .untag_resource(REGION, resource_arn, &["owner"])
            .await;
        assert_refusal(&refused, *status, code, message, label);
    }

    // Every live resource still carries the tags the refused calls named.
    for resource in [&bucket_arn, &table_arn] {
        let after = listed_tags(&emulator, resource, "the live resources").await;
        assert_eq!(
            after, resident,
            "a refused removal must leave a live resource's tags as they were, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_caller_may_not_remove_a_tag_in_the_reserved_namespace_but_the_service_may() {
    let emulator = Emulator::new();
    let (bucket_arn, table_arn) = resources(&emulator, "untag-reserved-bucket").await;

    // The prefix is the service's namespace however a request spells it, so each
    // spelling is driven.
    for reserved in ["aws:managed_by", "AWS:managed_by", "Aws:Managed_By"] {
        for resource in [&bucket_arn, &table_arn] {
            let label = format!("{reserved:?} on {resource}");

            // The tag in the reserved namespace is set the only way it can be:
            // on the in-process path this service's own control plane arrives on.
            served(
                emulator
                    .tag_resource_in_process(REGION, resource, &json!({ reserved: "the-service" }))
                    .await,
                &label,
            );
            // An ordinary key of the caller's own, so a refused removal can be
            // shown to take nothing at all.
            served(
                emulator
                    .tag_resource(REGION, resource, &json!({ "owner": "caller" }))
                    .await,
                &label,
            );

            // A caller may not take the service's key away.
            let refused = emulator.untag_resource(REGION, resource, &[reserved]).await;
            assert_refusal(&refused, 400, "BadRequestException", RESERVED_KEY, &label);

            // Nor may it take it away beside a key it does own: the refusal is
            // stated before anything is removed.
            let together = emulator
                .untag_resource(REGION, resource, &[reserved, "owner"])
                .await;
            assert_refusal(&together, 400, "BadRequestException", RESERVED_KEY, &label);
            let after = listed_tags(&emulator, resource, &label).await;
            assert_eq!(
                after.get(reserved).map(String::as_str),
                Some("the-service"),
                "case {label:?}: the service's own tag must survive a caller's removal, got \
                 {after:?}"
            );
            assert_eq!(
                after.get("owner").map(String::as_str),
                Some("caller"),
                "case {label:?}: a refused removal must take no key at all, got {after:?}"
            );

            // The path that set the tag may remove it, and takes only that key.
            let removed = emulator
                .untag_resource_in_process(REGION, resource, &[reserved])
                .await;
            assert_eq!(
                removed.status, 204,
                "case {label:?}: the in-process removal must be served, got {status} {body}",
                status = removed.status,
                body = removed.body
            );
            let after = listed_tags(&emulator, resource, &label).await;
            assert!(
                !after.contains_key(reserved),
                "case {label:?}: the in-process removal must take the service's key, got {after:?}"
            );
            assert_eq!(
                after.get("owner").map(String::as_str),
                Some("caller"),
                "case {label:?}: the caller's own key must stay, got {after:?}"
            );
        }
    }
}
