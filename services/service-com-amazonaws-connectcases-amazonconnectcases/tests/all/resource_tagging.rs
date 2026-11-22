// SPDX-License-Identifier: CC0-1.0

//! End-to-end restJson1 tests for TAGGING a Cases resource, driven through the
//! real `MultiServiceRouter` so every assertion goes over the wire the SDK
//! speaks.
//!
//! The rule under test: `TagResource` merges the request's `tags` into the tag
//! set of the one resource its `arn` addresses — whatever kind of resource that
//! ARN names — answering HTTP 200 with an empty body, `UntagResource` drops
//! exactly the keys its `tagKeys` names from that same set and leaves every
//! other pair, and `ListTagsForResource` reports exactly the set the stored
//! record then carries, as does a read of the resource itself; an ARN that
//! addresses no record a call can go through is refused with
//! `ResourceNotFoundException` (404) and nothing is written — which
//! for a SOFT-deleted record means the tag set stays readable, exactly as the
//! record's own read does, while a write through the tombstone is refused.
//!
//! The tables below are the class, not one recorded call: their rows differ
//! along the KIND of resource the ARN addresses (every kind this service mints
//! an ARN for), along the keys and values a tag map carries (count, length,
//! punctuation, an empty value, a key already present), along the keys one
//! untag names (a key the resource carries, one it does not, several at once,
//! the whole set, the same keys again), along how an ARN fails
//! to resolve (an unknown id of each kind, another region, another account,
//! another service, a kind this service does not address, a resource that was
//! deleted, a string that is no ARN at all), and — for the ceiling on one
//! resource's tag set — at that bound and one past it.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::state::MAX_TAGS_PER_RESOURCE;

use crate::harness::{
    domain, field_of_type, fields_of_every_type, label, string_at, tags_path, template, Emulator,
};

/// The service's generic not-found sentence, written out here rather than taken
/// from the crate so the expectations are independent of the code under test.
const GENERIC_SENTENCE: &str = "Requested resource not found.";

/// The `resourceType` a refusal reports for a value that addresses no resource
/// kind of this service at all — no ARN, another vendor's ARN, or a resource
/// path naming no kind this service addresses. The member is required, so a
/// refusal reports this generic word rather than nothing.
const UNNAMED_KIND: &str = "resource";

/// The region and account every ARN this emulator mints is scoped to, as the
/// other suites spell them: the harness signs for `us-east-1`.
const HOME_REGION: &str = "us-east-1";
const ACCOUNT: &str = "000000000000";

/// Longest value the model's tag vocabulary admits: `TagValueString` is
/// `@length(max: 256)` (`smithy_query(query="shape",shape="MutableTags")`), the
/// value type of the tag map a create of a case accepts.
const LONGEST_TAG_VALUE: usize = 256;

/// Longest key that vocabulary admits: `MutableTagKey` is `@length(1..128)`
/// (`smithy_query(query="shape",shape="MutableTags")`), and `TagKey` — the
/// element type of the key list one untag names — carries the same bound
/// (`smithy_query(query="shape",shape="TagKeyList")`).
const LONGEST_TAG_KEY: usize = 128;

/// Most keys ONE untag may name: `TagKeyList` is `@length(0..50)`
/// (`smithy_query(query="shape",shape="TagKeyList")`).
const MOST_KEYS_PER_UNTAG: usize = 50;

/// One key-value pair of a tag map, as the tables below spell it.
type Pair<'a> = (&'a str, &'a str);

/// One step of an untag walk: the case, the keys that untag names, and the
/// pairs the resource carries once it has been answered.
type UntagStep<'a> = (&'a str, Vec<&'a str>, Vec<Pair<'a>>);

/// One row of the key-form table: the case, the pairs written first, the keys
/// the untag names and the pairs left behind.
type UntagForm<'a> = (&'a str, Vec<Pair<'a>>, Vec<&'a str>, Vec<Pair<'a>>);

/// A tag map as a JSON object, which is how a `Tags` map travels on the wire.
fn tag_body(tags: &[(&str, &str)]) -> Value {
    json!({
        "tags": Value::Object(
            tags.iter()
                .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
                .collect::<Map<String, Value>>(),
        )
    })
}

/// The tag map a response member spells, in an order two maps compare in. The
/// member is present on every read of a resource, carrying the empty map for a
/// resource that carries no tags.
fn tag_set(response: &Value, member: &str, case: &str) -> BTreeMap<String, String> {
    match response.get(member) {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, value)| {
                let value = value.as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: tag {key:?} is not a string in {response:?}")
                });
                (key.clone(), value.to_string())
            })
            .collect(),
        Some(other) => panic!("case {case:?}: {member} is not a tag map: {other:?}"),
    }
}

/// The map `pairs` spells, for comparison against what the service reports.
fn expected_set(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// `TagResource` against `arn`, which must answer HTTP 200 with an EMPTY body:
/// the operation's output shape is `Unit`, so there is nothing for a caller to
/// parse (`smithy_query(query="operation",operation="TagResource")`).
async fn tag_resource(emulator: &Emulator, case: &str, arn: &str, tags: &[(&str, &str)]) {
    let (status, body) = emulator
        .send("POST", &tags_path(arn), Some(tag_body(tags)))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from TagResource, got {status} with body {body:?}"
    );
    assert_eq!(
        body,
        Value::Null,
        "case {case:?}: TagResource answers an empty body, got {body:?}"
    );
}

/// The `UntagResource` path of one resource: its `tagKeys` member is
/// `@httpQuery("tagKeys")`
/// (`smithy_query(query="operation",operation="UntagResource")`), so the keys
/// travel in the query string, one `tagKeys=` parameter per key, rather than in
/// a body — the call is a `DELETE`.
fn untag_path(arn: &str, tag_keys: &[&str]) -> String {
    let mut path = tags_path(arn);
    for (index, key) in tag_keys.iter().enumerate() {
        path.push(if index == 0 { '?' } else { '&' });
        path.push_str("tagKeys=");
        path.push_str(&label(key));
    }
    path
}

/// `UntagResource` against `arn` for `tag_keys`, which must answer HTTP 200 with
/// an EMPTY body: the operation's output shape is `Unit`, so there is nothing
/// for a caller to parse
/// (`smithy_query(query="operation",operation="UntagResource")`).
async fn untag_resource(emulator: &Emulator, case: &str, arn: &str, tag_keys: &[&str]) {
    let (status, body) = emulator
        .send("DELETE", &untag_path(arn, tag_keys), None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UntagResource, got {status} with body {body:?}"
    );
    assert_eq!(
        body,
        Value::Null,
        "case {case:?}: UntagResource answers an empty body, got {body:?}"
    );
}

/// The tag set `ListTagsForResource` reports for `arn`.
async fn list_tags(emulator: &Emulator, case: &str, arn: &str) -> BTreeMap<String, String> {
    let listed = emulator
        .ok(case, "GET", &tags_path(arn), Value::Null)
        .await;
    assert!(
        listed.get("tags").is_some(),
        "case {case:?}: ListTagsForResource reports the tag set it resolved, got {listed:?}"
    );
    tag_set(&listed, "tags", case)
}

/// The kinds of resource this service mints an ARN for, which are the kinds a
/// tagging call addresses: the ARN forms are the ones every other suite asserts
/// a create answers (`domain/{id}`, and `field`, `template`, `layout` and `case`
/// under a domain, with a related item under its case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Domain,
    Field,
    Template,
    Layout,
    Case,
    RelatedItem,
}

/// Every kind, so a table row per kind covers the whole class.
const EVERY_KIND: [Kind; 6] = [
    Kind::Domain,
    Kind::Field,
    Kind::Template,
    Kind::Layout,
    Kind::Case,
    Kind::RelatedItem,
];

/// One resource of every kind, as [`one_resource_of_every_kind`] created them.
struct Resources {
    domain_id: String,
    field_id: String,
    template_id: String,
    layout_id: String,
    case_id: String,
    related_item_id: String,
}

impl Kind {
    /// The resource path of an ARN of this kind, under `resources`.
    fn resource_path(self, resources: &Resources) -> String {
        let Resources {
            domain_id,
            field_id,
            template_id,
            layout_id,
            case_id,
            related_item_id,
        } = resources;
        match self {
            Self::Domain => format!("domain/{domain_id}"),
            Self::Field => format!("domain/{domain_id}/field/{field_id}"),
            Self::Template => format!("domain/{domain_id}/template/{template_id}"),
            Self::Layout => format!("domain/{domain_id}/layout/{layout_id}"),
            Self::Case => format!("domain/{domain_id}/case/{case_id}"),
            Self::RelatedItem => {
                format!("domain/{domain_id}/case/{case_id}/related-item/{related_item_id}")
            }
        }
    }

    /// The ARN addressing this kind of resource under `resources`.
    fn arn(self, resources: &Resources) -> String {
        arn_in(HOME_REGION, ACCOUNT, &self.resource_path(resources))
    }

    /// The service's lowercase token for this kind, which a refusal about it
    /// reports in `resourceType`.
    fn resource_type(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::Field => "field",
            Self::Template => "template",
            Self::Layout => "layout",
            Self::Case => "case",
            Self::RelatedItem => "related-item",
        }
    }
}

/// A Cases ARN in `region` for `account`, addressing `resource_path`.
fn arn_in(region: &str, account: &str, resource_path: &str) -> String {
    format!("arn:aws:cases:{region}:{account}:{resource_path}")
}

/// Create one resource of every kind, and hand back their identifiers: a domain
/// with a `Text` field, a template requiring that field, a layout, a case
/// written against the template and a related item associated with that case.
async fn one_resource_of_every_kind(emulator: &Emulator, name: &str) -> Resources {
    let domain_id = domain(emulator, name).await;
    let fields = fields_of_every_type(emulator, &domain_id).await;
    let field_id = field_of_type(&fields, "Text");
    let template_id = template(
        emulator,
        &domain_id,
        &format!("{name}-template"),
        std::slice::from_ref(&field_id),
    )
    .await;
    let layout = emulator
        .ok(
            name,
            "POST",
            &format!("/domains/{domain_id}/layouts"),
            json!({ "name": format!("{name}-layout"), "content": { "basic": {} } }),
        )
        .await;
    let layout_id = string_at(&layout, "layoutId", name);
    let created_case = emulator
        .ok(
            name,
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [{ "id": field_id, "value": { "stringValue": "a customer note" } }],
            }),
        )
        .await;
    let case_id = string_at(&created_case, "caseId", name);
    let item = emulator
        .ok(
            name,
            "POST",
            &format!("/domains/{domain_id}/cases/{case_id}/related-items/"),
            json!({
                "type": "Comment",
                "content": { "comment": { "body": "called back", "contentType": "Text/Plain" } },
            }),
        )
        .await;
    let related_item_id = string_at(&item, "relatedItemId", name);

    Resources {
        domain_id,
        field_id,
        template_id,
        layout_id,
        case_id,
        related_item_id,
    }
}

/// The tag set a read of the RESOURCE ITSELF reports, through that kind's own
/// implemented reader — the second place the stored tag set surfaces, so a tag
/// write that reached some other record than the one the ARN addressed would
/// show here.
async fn tags_through_the_resource(
    emulator: &Emulator,
    case: &str,
    kind: Kind,
    resources: &Resources,
) -> BTreeMap<String, String> {
    let Resources {
        domain_id,
        field_id,
        template_id,
        layout_id,
        case_id,
        related_item_id,
    } = resources;
    match kind {
        Kind::Domain => {
            let read = emulator
                .ok(case, "POST", &format!("/domains/{domain_id}"), json!({}))
                .await;
            tag_set(&read, "tags", case)
        }
        Kind::Field => {
            let read = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/fields-batch"),
                    json!({ "fields": [{ "id": field_id }] }),
                )
                .await;
            let fields = read
                .get("fields")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("case {case:?}: BatchGetField reported no fields: {read:?}"));
            tag_set(&fields[0], "tags", case)
        }
        Kind::Template => {
            let read = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/templates/{template_id}"),
                    json!({}),
                )
                .await;
            tag_set(&read, "tags", case)
        }
        Kind::Layout => {
            let read = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/layouts/{layout_id}"),
                    json!({}),
                )
                .await;
            tag_set(&read, "tags", case)
        }
        Kind::Case => {
            let read = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/cases/{case_id}"),
                    json!({ "fields": [{ "id": field_id }] }),
                )
                .await;
            tag_set(&read, "tags", case)
        }
        Kind::RelatedItem => {
            let read = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/related-items-search"),
                    json!({}),
                )
                .await;
            let items = read
                .get("relatedItems")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("case {case:?}: the search reported no items: {read:?}"));
            let found = items
                .iter()
                .find(|item| {
                    item.get("relatedItemId").and_then(Value::as_str) == Some(related_item_id)
                })
                .unwrap_or_else(|| panic!("case {case:?}: {related_item_id} missing from {read:?}"));
            tag_set(found, "tags", case)
        }
    }
}

#[tokio::test]
async fn a_tag_write_merges_into_the_tag_set_of_the_resource_its_arn_addresses() {
    let emulator = Emulator::new();
    let resources = one_resource_of_every_kind(&emulator, "tag-every-kind").await;

    for kind in EVERY_KIND {
        let case = format!("{kind:?}");
        let arn = kind.arn(&resources);

        // A resource that was never tagged carries no tags: the reporting
        // operation answers the empty set rather than refusing.
        let listed = list_tags(&emulator, &case, &arn).await;
        assert_eq!(
            listed,
            BTreeMap::new(),
            "case {case:?}: an untagged resource carries no tags, got {listed:?}"
        );

        // The first write stores both pairs.
        let first = [("team", "support"), ("kind", kind.resource_type())];
        tag_resource(&emulator, &case, &arn, &first).await;
        for (reader, reported) in [
            ("ListTagsForResource", list_tags(&emulator, &case, &arn).await),
            (
                "the resource's own reader",
                tags_through_the_resource(&emulator, &case, kind, &resources).await,
            ),
        ] {
            assert_eq!(
                reported,
                expected_set(&first),
                "case {case:?}: {reader} must report the tags just written, got {reported:?}"
            );
        }

        // A second write MERGES: a key already carried takes the new value, a
        // new key is added, and a key the request does not name is kept.
        let second = [("kind", "overwritten"), ("owner", "cases-team")];
        tag_resource(&emulator, &case, &arn, &second).await;
        let merged = expected_set(&[
            ("team", "support"),
            ("kind", "overwritten"),
            ("owner", "cases-team"),
        ]);
        for (reader, reported) in [
            ("ListTagsForResource", list_tags(&emulator, &case, &arn).await),
            (
                "the resource's own reader",
                tags_through_the_resource(&emulator, &case, kind, &resources).await,
            ),
        ] {
            assert_eq!(
                reported, merged,
                "case {case:?}: {reader} must report the merged tag set, got {reported:?}"
            );
        }
    }

    // Each ARN addresses ONE record: every kind still carries only the tags
    // written through its own ARN, so no write leaked into a neighbouring
    // resource of another kind.
    for kind in EVERY_KIND {
        let case = format!("{kind:?} after every other kind was tagged");
        let reported = list_tags(&emulator, &case, &kind.arn(&resources)).await;
        assert_eq!(
            reported.get("kind").map(String::as_str),
            Some("overwritten"),
            "case {case:?}: the tag set must be this resource's own, got {reported:?}"
        );
        assert_eq!(
            reported.len(),
            3,
            "case {case:?}: exactly the three tags written here, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_tag_write_stores_keys_and_values_of_every_form_the_vocabulary_admits() {
    let emulator = Emulator::new();
    let resources = one_resource_of_every_kind(&emulator, "tag-forms").await;
    let longest_key = "k".repeat(LONGEST_TAG_KEY);
    let longest_value = "v".repeat(LONGEST_TAG_VALUE);

    // Rows differ along the keys and values a tag map carries. `Tags` targets a
    // plain `String` key and value with no pattern of its own
    // (`smithy_query(query="shape",shape="Tags")`), so the forms below are the
    // characters and lengths the service's tag vocabulary admits
    // (`MutableTags`), each written through a tagging call.
    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("no tags at all", vec![]),
        ("one pair", vec![("env", "prod")]),
        (
            "the punctuation the vocabulary admits",
            vec![("cost.center:team/a=b+c-d_e@f", "a Z9 _.:/=+-@")],
        ),
        ("an empty value", vec![("retired", "")]),
        (
            "the longest key and value",
            vec![(longest_key.as_str(), longest_value.as_str())],
        ),
        (
            "several pairs at once",
            vec![("a", "1"), ("b", "2"), ("c", "3")],
        ),
    ];

    for (case, pairs) in &rows {
        // Each row is written to a FRESH domain, so what a row leaves behind is
        // exactly what it wrote.
        let fresh = domain(&emulator, &format!("tag-forms-{case}")).await;
        let arn = arn_in(HOME_REGION, ACCOUNT, &format!("domain/{fresh}"));
        tag_resource(&emulator, case, &arn, pairs).await;
        let reported = list_tags(&emulator, case, &arn).await;
        assert_eq!(
            reported,
            expected_set(pairs),
            "case {case:?}: the stored tag set must be what was written, got {reported:?}"
        );
    }

    // Writing an empty map over a tagged resource changes nothing: there is no
    // key to merge, and a merge never removes one.
    let arn = Kind::Domain.arn(&resources);
    tag_resource(&emulator, "keep", &arn, &[("team", "support")]).await;
    tag_resource(&emulator, "empty merge", &arn, &[]).await;
    let reported = list_tags(&emulator, "empty merge", &arn).await;
    assert_eq!(
        reported,
        expected_set(&[("team", "support")]),
        "case \"empty merge\": an empty tags map removes nothing, got {reported:?}"
    );
}

#[tokio::test]
async fn one_resource_carries_at_most_the_ceiling_of_tags_and_a_merge_past_it_writes_nothing() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "tag-ceiling").await;
    let arn = arn_in(HOME_REGION, ACCOUNT, &format!("domain/{domain_id}"));

    // At the bound: a resource carries the whole ceiling of tags.
    let keys: Vec<String> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| format!("key-{index}"))
        .collect();
    let at_the_bound: Vec<(&str, &str)> = keys
        .iter()
        .map(|key| (key.as_str(), "stored"))
        .collect();
    tag_resource(&emulator, "at the ceiling", &arn, &at_the_bound).await;
    let full = list_tags(&emulator, "at the ceiling", &arn).await;
    assert_eq!(
        full.len(),
        MAX_TAGS_PER_RESOURCE,
        "case \"at the ceiling\": every tag at the bound must be stored, got {} tags",
        full.len()
    );

    // One past it: the merge is refused, and the tag set is left exactly as it
    // was — a refused write stores nothing, not even the keys that would fit.
    let (status, body) = emulator
        .send(
            "POST",
            &tags_path(&arn),
            Some(tag_body(&[("one-too-many", "refused")])),
        )
        .await;
    let requested = MAX_TAGS_PER_RESOURCE + 1;
    assert_eq!(
        (status, body.get("__type").and_then(Value::as_str)),
        (400, Some("ValidationException")),
        "case \"past the ceiling\": expected 400 ValidationException, got {status} {body:?}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(
            format!(
                "tags would leave {arn} carrying {requested} tags; at most \
                 {MAX_TAGS_PER_RESOURCE} are accepted per resource"
            )
            .as_str()
        ),
        "case \"past the ceiling\": the refusal must name the member, the count and the \
         ceiling, got {body:?}"
    );
    let unchanged = list_tags(&emulator, "past the ceiling", &arn).await;
    assert_eq!(
        unchanged, full,
        "case \"past the ceiling\": a refused merge leaves the tag set untouched, got \
         {unchanged:?}"
    );

    // A merge that only overwrites keys the resource already carries is
    // accepted at the ceiling: it adds no key, so it crosses nothing.
    tag_resource(&emulator, "overwrite at the ceiling", &arn, &[("key-0", "rewritten")]).await;
    let rewritten = list_tags(&emulator, "overwrite at the ceiling", &arn).await;
    assert_eq!(
        rewritten.len(),
        MAX_TAGS_PER_RESOURCE,
        "case \"overwrite at the ceiling\": an overwrite adds no key, got {} tags",
        rewritten.len()
    );
    assert_eq!(
        rewritten.get("key-0").map(String::as_str),
        Some("rewritten"),
        "case \"overwrite at the ceiling\": the overwritten key takes the new value, got \
         {rewritten:?}"
    );
}

#[tokio::test]
async fn an_arn_that_addresses_no_stored_resource_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let resources = one_resource_of_every_kind(&emulator, "tag-refusals").await;
    let Resources {
        domain_id,
        field_id,
        template_id,
        case_id,
        ..
    } = &resources;

    // A live resource of every kind, tagged first, so a refused call that wrote
    // to the wrong record would be visible afterwards.
    for kind in EVERY_KIND {
        tag_resource(
            &emulator,
            &format!("{kind:?} baseline"),
            &kind.arn(&resources),
            &[("team", "support")],
        )
        .await;
    }

    // A second domain, deleted: a domain's delete takes the whole subtree with
    // it, so nothing under a domain that no longer exists resolves either.
    let gone_domain = domain(&emulator, "tag-refusals-deleted").await;
    emulator
        .ok(
            "DeleteDomain",
            "DELETE",
            &format!("/domains/{gone_domain}"),
            Value::Null,
        )
        .await;
    let deleted_template = template(
        &emulator,
        domain_id,
        "tag-refusals-deleted-template",
        std::slice::from_ref(field_id),
    )
    .await;
    emulator
        .ok(
            "DeleteTemplate",
            "DELETE",
            &format!("/domains/{domain_id}/templates/{deleted_template}"),
            Value::Null,
        )
        .await;

    // (case, the ARN, the `resourceType` token the refusal must report)
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "an unknown domain",
            arn_in(HOME_REGION, ACCOUNT, "domain/no-such-domain"),
            "domain",
        ),
        (
            "an unknown field of a live domain",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/field/no-such-field"),
            ),
            "field",
        ),
        (
            "an unknown template of a live domain",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/template/no-such-template"),
            ),
            "template",
        ),
        (
            "an unknown layout of a live domain",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/layout/no-such-layout"),
            ),
            "layout",
        ),
        (
            "an unknown case of a live domain",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/case/no-such-case"),
            ),
            "case",
        ),
        (
            "an unknown related item of a live case",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/case/{case_id}/related-item/no-such-item"),
            ),
            "related-item",
        ),
        (
            "a field of a domain that does not exist",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/no-such-domain/field/{field_id}"),
            ),
            "field",
        ),
        (
            "a domain that was deleted",
            arn_in(HOME_REGION, ACCOUNT, &format!("domain/{gone_domain}")),
            "domain",
        ),
        (
            "a field that was deleted with its domain",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{gone_domain}/field/{field_id}"),
            ),
            "field",
        ),
        (
            "a live resource addressed through another region",
            arn_in(
                "us-west-2",
                ACCOUNT,
                &format!("domain/{domain_id}/template/{template_id}"),
            ),
            "template",
        ),
        (
            "a live resource addressed through another account",
            arn_in(
                HOME_REGION,
                "000000000001",
                &format!("domain/{domain_id}"),
            ),
            "domain",
        ),
        (
            "a resource kind this service does not address",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/case-rule/a-rule"),
            ),
            UNNAMED_KIND,
        ),
        (
            "another service's ARN",
            "arn:aws:connect:us-east-1:000000000000:instance/i-1".to_string(),
            UNNAMED_KIND,
        ),
        (
            "a string that is no ARN at all",
            "not-an-arn".to_string(),
            UNNAMED_KIND,
        ),
    ];

    for (case, arn, resource_type) in &rows {
        // The write, the REMOVAL and the read of a tag set all resolve the same
        // way: whichever way an ARN fails to address a record this store holds,
        // every tagging call on it is refused alike.
        for (operation, method, path, body) in [
            (
                "TagResource",
                "POST",
                tags_path(arn),
                Some(tag_body(&[("team", "leaked")])),
            ),
            ("ListTagsForResource", "GET", tags_path(arn), None),
            (
                "UntagResource",
                "DELETE",
                untag_path(arn, &["team"]),
                None,
            ),
        ] {
            let case = format!("{operation} on {case}");
            let (status, response) = emulator.send(method, &path, body).await;
            assert_eq!(
                (status, response.get("__type").and_then(Value::as_str)),
                (404, Some("ResourceNotFoundException")),
                "case {case:?}: expected 404 ResourceNotFoundException, got {status} {response:?}"
            );
            assert_eq!(
                string_at(&response, "resourceId", &case),
                *arn,
                "case {case:?}: the refusal reports the ARN the request named, got {response:?}"
            );
            assert_eq!(
                string_at(&response, "resourceType", &case),
                *resource_type,
                "case {case:?}: expected the resourceType token {resource_type:?}, got \
                 {response:?}"
            );
            assert_eq!(
                response.get("message").and_then(Value::as_str),
                Some(GENERIC_SENTENCE),
                "case {case:?}: expected the generic sentence, got {response:?}"
            );
        }
    }

    // Nothing the refused calls carried reached any stored record, and nothing
    // the refused removals named was taken off one either.
    for kind in EVERY_KIND {
        let case = format!("{kind:?} after every refusal");
        let reported = list_tags(&emulator, &case, &kind.arn(&resources)).await;
        assert_eq!(
            reported,
            expected_set(&[("team", "support")]),
            "case {case:?}: a refused tag write stores and removes nothing, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn the_tags_a_create_carried_are_the_set_a_tag_write_merges_into() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "create-tags-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let field_id = field_of_type(&fields, "Text");
    let template_id = template(
        &emulator,
        &domain_id,
        "create-tags-template",
        std::slice::from_ref(&field_id),
    )
    .await;
    let created = emulator
        .ok(
            "CreateCase",
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [{ "id": field_id, "value": { "stringValue": "a customer note" } }],
                "tags": { "team": "support" },
            }),
        )
        .await;
    let case_id = string_at(&created, "caseId", "CreateCase");
    let arn = string_at(&created, "caseArn", "CreateCase");

    // One tag set per resource: the tags a create carried are reported by the
    // tag reader, and a later tag write merges into that same set.
    let reported = list_tags(&emulator, "CreateCase tags", &arn).await;
    assert_eq!(
        reported,
        expected_set(&[("team", "support")]),
        "case \"CreateCase tags\": the tags the create carried must be the stored set, got \
         {reported:?}"
    );

    tag_resource(
        &emulator,
        "TagResource over CreateCase tags",
        &arn,
        &[("env", "prod")],
    )
    .await;
    let merged = list_tags(&emulator, "TagResource over CreateCase tags", &arn).await;
    assert_eq!(
        merged,
        expected_set(&[("team", "support"), ("env", "prod")]),
        "case \"TagResource over CreateCase tags\": the write must merge into the create's \
         tags, got {merged:?}"
    );

    // The case itself reports the same set, so there is one tag set and not two.
    let read = emulator
        .ok(
            "GetCase",
            "POST",
            &format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": field_id }] }),
        )
        .await;
    let through_the_case = tag_set(&read, "tags", "GetCase");
    assert_eq!(
        through_the_case, merged,
        "case \"GetCase\": the case must report the tag set the tag reader reports, got \
         {through_the_case:?}"
    );
}

#[tokio::test]
async fn a_soft_deleted_resource_keeps_its_tag_set_readable_and_refuses_a_tag_write() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "soft-delete-tags").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let live_field = field_of_type(&fields, "Text");

    // A field, a template and a layout, each tagged and then SOFT-deleted: this
    // service keeps such a record readable (`BatchGetField`, `GetTemplate` and
    // `GetLayout` all answer a deleted record, tags included), while every write
    // through one is refused.
    let doomed_field = string_at(
        &emulator
            .ok(
                "CreateField",
                "POST",
                &format!("/domains/{domain_id}/fields"),
                json!({ "name": "soft-delete-tags-field", "type": "Text" }),
            )
            .await,
        "fieldId",
        "CreateField",
    );
    let doomed_template = template(
        &emulator,
        &domain_id,
        "soft-delete-tags-template",
        std::slice::from_ref(&live_field),
    )
    .await;
    let doomed_layout = string_at(
        &emulator
            .ok(
                "CreateLayout",
                "POST",
                &format!("/domains/{domain_id}/layouts"),
                json!({ "name": "soft-delete-tags-layout", "content": { "basic": {} } }),
            )
            .await,
        "layoutId",
        "CreateLayout",
    );

    // (case, the ARN, the delete that tombstones it)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a soft-deleted field",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/field/{doomed_field}"),
            ),
            format!("/domains/{domain_id}/fields/{doomed_field}"),
        ),
        (
            "a soft-deleted template",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/template/{doomed_template}"),
            ),
            format!("/domains/{domain_id}/templates/{doomed_template}"),
        ),
        (
            "a soft-deleted layout",
            arn_in(
                HOME_REGION,
                ACCOUNT,
                &format!("domain/{domain_id}/layout/{doomed_layout}"),
            ),
            format!("/domains/{domain_id}/layouts/{doomed_layout}"),
        ),
    ];

    for (case, arn, delete_path) in &rows {
        tag_resource(&emulator, case, arn, &[("team", "support")]).await;
        emulator.ok(case, "DELETE", delete_path, Value::Null).await;

        // The read survives the delete, as the resource's own read does.
        let reported = list_tags(&emulator, case, arn).await;
        assert_eq!(
            reported,
            expected_set(&[("team", "support")]),
            "case {case:?}: the tag set must stay readable for as long as the record is, got \
             {reported:?}"
        );

        // The write does not: a tombstone is not a resource a caller can take a
        // new reference to, so the merge is refused and nothing is stored, and
        // the removal of a key it carries is refused the same way.
        for (operation, method, path, body) in [
            (
                "TagResource",
                "POST",
                tags_path(arn),
                Some(tag_body(&[("env", "prod")])),
            ),
            (
                "UntagResource",
                "DELETE",
                untag_path(arn, &["team"]),
                None,
            ),
        ] {
            let case = format!("{operation} on {case}");
            let (status, response) = emulator.send(method, &path, body).await;
            assert_eq!(
                (status, response.get("__type").and_then(Value::as_str)),
                (404, Some("ResourceNotFoundException")),
                "case {case:?}: a tag write through a tombstone is refused, got {status} \
                 {response:?}"
            );
            let unchanged = list_tags(&emulator, &case, arn).await;
            assert_eq!(
                unchanged, reported,
                "case {case:?}: the refused write changes nothing, got {unchanged:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_untag_removes_exactly_the_keys_it_names_from_the_resource_its_arn_addresses() {
    let emulator = Emulator::new();
    let resources = one_resource_of_every_kind(&emulator, "untag-every-kind").await;

    // Every kind starts from the same four-pair set, so what one untag leaves
    // behind is the same question of each kind of resource an ARN addresses.
    let written = [
        ("team", "support"),
        ("env", "prod"),
        ("owner", "cases-team"),
        ("retired", ""),
    ];

    // (case, the keys one untag names, the set the resource then carries) —
    // rows differ along what the named keys are to the stored set: one key it
    // carries, a key it does not, several at once with one of each, a key whose
    // value is empty, the same keys a second time, and the last key it carries.
    let steps: Vec<UntagStep<'_>> = vec![
        (
            "one key the resource carries",
            vec!["env"],
            vec![("team", "support"), ("owner", "cases-team"), ("retired", "")],
        ),
        (
            "a key the resource does not carry",
            vec!["never-written"],
            vec![("team", "support"), ("owner", "cases-team"), ("retired", "")],
        ),
        (
            "several keys at once, one of them absent",
            vec!["owner", "never-written"],
            vec![("team", "support"), ("retired", "")],
        ),
        (
            "a key whose stored value is empty",
            vec!["retired"],
            vec![("team", "support")],
        ),
        (
            "the same keys a second time",
            vec!["owner", "never-written"],
            vec![("team", "support")],
        ),
        ("the last key it carries", vec!["team"], vec![]),
    ];

    for kind in EVERY_KIND {
        let arn = kind.arn(&resources);
        tag_resource(&emulator, &format!("{kind:?} baseline"), &arn, &written).await;

        for (step, keys, remaining) in &steps {
            let case = format!("{kind:?}: untag {step}");
            untag_resource(&emulator, &case, &arn, keys).await;

            // Both readers of the one tag set must agree, so a removal that
            // reached some other record than the one the ARN addressed shows.
            for (reader, reported) in [
                ("ListTagsForResource", list_tags(&emulator, &case, &arn).await),
                (
                    "the resource's own reader",
                    tags_through_the_resource(&emulator, &case, kind, &resources).await,
                ),
            ] {
                assert_eq!(
                    reported,
                    expected_set(remaining),
                    "case {case:?}: {reader} must report the set without the keys the untag \
                     named, got {reported:?}"
                );
            }
        }

        // A tag write after an untag merges into the set the untag left, so a
        // removed key is a key a later call can write again.
        let case = format!("{kind:?}: tagged again after the untag");
        tag_resource(&emulator, &case, &arn, &[("env", "staging")]).await;
        let reported = list_tags(&emulator, &case, &arn).await;
        assert_eq!(
            reported,
            expected_set(&[("env", "staging")]),
            "case {case:?}: a removed key must be writable again, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn an_untag_takes_keys_of_every_form_the_vocabulary_admits_and_leaves_every_other_pair() {
    let emulator = Emulator::new();
    let longest_key = "k".repeat(LONGEST_TAG_KEY);
    let whole_set: Vec<String> = (0..MOST_KEYS_PER_UNTAG)
        .map(|index| format!("key-{index}"))
        .collect();
    let whole_set_written: Vec<(&str, &str)> = whole_set
        .iter()
        .map(|key| (key.as_str(), "stored"))
        .collect();
    let whole_set_named: Vec<&str> = whole_set.iter().map(String::as_str).collect();

    // (case, the pairs written first, the keys the untag names, the pairs left)
    // — rows differ along the FORM of the keys named: the punctuation `TagKey`
    // admits, its longest key, as many keys as one list may carry, and keys the
    // resource carries none of.
    let rows: Vec<UntagForm<'_>> = vec![
        (
            "the punctuation a tag key admits",
            vec![("cost.center:team/a=b+c-d_e", "team-a"), ("keep", "1")],
            vec!["cost.center:team/a=b+c-d_e"],
            vec![("keep", "1")],
        ),
        (
            "the longest key the vocabulary admits",
            vec![(longest_key.as_str(), "stored"), ("keep", "1")],
            vec![longest_key.as_str()],
            vec![("keep", "1")],
        ),
        (
            "as many keys as one untag may name",
            whole_set_written.clone(),
            whole_set_named.clone(),
            vec![],
        ),
        (
            "keys the resource carries none of",
            vec![("keep", "1"), ("also-keep", "2")],
            vec!["absent-one", "absent-two"],
            vec![("keep", "1"), ("also-keep", "2")],
        ),
    ];

    for (case, written, named, remaining) in &rows {
        // Each row runs against a FRESH domain, so what it leaves behind is
        // exactly what it wrote and then removed.
        let fresh = domain(&emulator, &format!("untag-forms-{case}")).await;
        let arn = arn_in(HOME_REGION, ACCOUNT, &format!("domain/{fresh}"));
        tag_resource(&emulator, case, &arn, written).await;
        untag_resource(&emulator, case, &arn, named).await;
        let reported = list_tags(&emulator, case, &arn).await;
        assert_eq!(
            reported,
            expected_set(remaining),
            "case {case:?}: exactly the keys the untag named must be gone, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn an_untag_is_bounded_by_the_key_list_the_model_declares() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "untag-bounds").await;
    let arn = arn_in(HOME_REGION, ACCOUNT, &format!("domain/{domain_id}"));
    let written = [("team", "support"), ("env", "prod")];
    tag_resource(&emulator, "bounds baseline", &arn, &written).await;

    // At the LOW end of what one call may name: a key list with a single key
    // the resource does not carry has nothing to remove, so it succeeds and
    // leaves the stored set exactly as it was.
    untag_resource(&emulator, "a single absent key", &arn, &["absent"]).await;
    let reported = list_tags(&emulator, "a single absent key", &arn).await;
    assert_eq!(
        reported,
        expected_set(&written),
        "case \"a single absent key\": an untag naming no stored key removes nothing, got \
         {reported:?}"
    );

    // One past each published bound: a list longer than the model admits, and a
    // key longer than one `TagKey` admits. Both are refused at the boundary
    // with a 400 `ValidationException`, and the stored set is untouched.
    let too_many: Vec<String> = (0..=MOST_KEYS_PER_UNTAG)
        .map(|index| format!("key-{index}"))
        .collect();
    let too_many_named: Vec<&str> = too_many.iter().map(String::as_str).collect();
    let too_long_key = "k".repeat(LONGEST_TAG_KEY + 1);
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("one key more than a list may name", too_many_named),
        ("a key one character too long", vec![too_long_key.as_str()]),
    ];

    for (case, named) in &rows {
        let (status, response) = emulator
            .send("DELETE", &untag_path(&arn, named), None)
            .await;
        assert_eq!(
            (status, response.get("__type").and_then(Value::as_str)),
            (400, Some("ValidationException")),
            "case {case:?}: expected 400 ValidationException, got {status} {response:?}"
        );
        let unchanged = list_tags(&emulator, case, &arn).await;
        assert_eq!(
            unchanged,
            expected_set(&written),
            "case {case:?}: a refused untag removes nothing, got {unchanged:?}"
        );
    }
}

#[tokio::test]
async fn an_untag_touches_only_the_resource_its_arn_addresses() {
    let emulator = Emulator::new();
    let resources = one_resource_of_every_kind(&emulator, "untag-one-record").await;

    // One tag set per resource, all alike, so a removal that reached a
    // neighbouring record of another kind would be visible as a missing pair.
    for kind in EVERY_KIND {
        tag_resource(
            &emulator,
            &format!("{kind:?} baseline"),
            &kind.arn(&resources),
            &[("team", "support"), ("env", "prod")],
        )
        .await;
    }

    for (untagged_index, untagged) in EVERY_KIND.iter().enumerate() {
        let case = format!("untag {untagged:?}");
        untag_resource(&emulator, &case, &untagged.arn(&resources), &["env"]).await;

        for (index, kind) in EVERY_KIND.iter().enumerate() {
            let case = format!("{kind:?} after {case}");
            let reported = list_tags(&emulator, &case, &kind.arn(&resources)).await;
            // Only the kinds already untagged have lost the key: every other
            // resource still carries both pairs it was given.
            let expected = if index <= untagged_index {
                expected_set(&[("team", "support")])
            } else {
                expected_set(&[("team", "support"), ("env", "prod")])
            };
            assert_eq!(
                reported, expected,
                "case {case:?}: an untag removes the key from one record only, got {reported:?}"
            );
        }
    }
}
