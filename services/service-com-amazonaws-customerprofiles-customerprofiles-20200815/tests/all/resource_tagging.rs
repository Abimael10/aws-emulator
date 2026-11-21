// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `TagResource`, `ListTagsForResource` and `UntagResource`.
//!
//! Two rules under test. The SUCCESS STATUS of the two tagging MUTATIONS: a write through
//! this surface carries no content, so `TagResource` and `UntagResource` answer HTTP 204 with
//! no body at all — never the protocol-default 200 with an empty JSON object — while the
//! report beside them answers 200 with its map.
//!
//! The second rule: the tagging trio names its subject by the ARN of a Customer
//! Profiles resource — `arn:<partition>:profile:<region>:<account>:` followed by the
//! resource's own URI path — so every resource this emulator holds tags for is reachable
//! through it. A tag attaches to the record that resource's own operations wrote, the
//! report answers from that record, and an untag removes a key from it; an ARN whose
//! resource the store does not hold is the tagging surface's own `NotFoundException` (404,
//! "Resource was not found"), and a value that is not one of this service's ARNs is
//! `BadRequestException` (400). The published
//! `@pattern` on that member closes with a whole-value anchor right after the service
//! segment, so enforcing it as written would make a 400 of every ARN above: what the
//! member is held to instead is the expression this model publishes for one of its ARNs.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test
//! harness, in `tests/all/domain_profile_lifecycle.rs`.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
    ARN_SERVICE, MAX_TAGS_PER_RESOURCE,
};

use crate::calculated_attribute_definitions::{define_object_type, definition};
use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};
use crate::profile_objects::put_contact_type;

/// The region these tests sign for, and so the one the ARNs they build name.
const REGION: &str = "us-east-1";

/// Percent-encode one URI path segment the way an SDK does: an ARN carries `:` and `/`,
/// and `/tags/{resourceArn}` is a single (non-greedy) label, so everything outside RFC
/// 3986's unreserved set is encoded and survives the server's decode.
fn encode_label(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => character.to_string(),
            other => other
                .to_string()
                .bytes()
                .map(|byte| format!("%{byte:02X}"))
                .collect(),
        })
        .collect()
}

/// The ARN of one resource of this service: the account every emulator ARN carries, the
/// region the caller signed for, and the resource's own URI path.
fn arn(resource_path: &str) -> String {
    engine::emulator_arn(ARN_SERVICE, REGION, resource_path)
}

/// `TagResource` over the wire: `POST /tags/{resourceArn}`.
async fn tag(emulator: &Emulator, resource_arn: &str, tags: Value) -> (u16, Value) {
    emulator
        .post(
            &format!("/tags/{}", encode_label(resource_arn)),
            json!({ "tags": tags }),
        )
        .await
}

/// `ListTagsForResource` over the wire: `GET /tags/{resourceArn}`.
async fn list_tags(emulator: &Emulator, resource_arn: &str) -> (u16, Value) {
    emulator
        .send("GET", &format!("/tags/{}", encode_label(resource_arn)), None)
        .await
}

/// `UntagResource` over the wire: `DELETE /tags/{resourceArn}?tagKeys=…` — `tagKeys` is an
/// `@httpQuery` list.
async fn untag(emulator: &Emulator, resource_arn: &str, tag_keys: &[&str]) -> (u16, Value) {
    let query: Vec<String> = tag_keys
        .iter()
        .map(|key| format!("tagKeys={}", encode_label(key)))
        .collect();
    emulator
        .send(
            "DELETE",
            &format!(
                "/tags/{}?{}",
                encode_label(resource_arn),
                query.join("&")
            ),
            None,
        )
        .await
}

/// The status a tagging WRITE answers: no content, so there is nothing to parse.
const NO_CONTENT: u16 = 204;

/// Assert one tagging write answered its no-content success: the status that says a response
/// carries nothing, and a body that carries nothing — the harness reads an empty body as
/// `Value::Null`, so an empty JSON object would be seen here as the object it is.
fn assert_no_content(case: &str, operation: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, body),
        (NO_CONTENT, &Value::Null),
        "case {case:?} {operation}: expected {NO_CONTENT} and no body, got {status} {body:?}"
    );
}

/// A tag map as a comparison reads it, whichever order the response rendered it in.
fn tags_of(body: &Value, member: &str) -> BTreeMap<String, String> {
    body.get(member)
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

/// The tags `ListTagsForResource` reports, asserting the call succeeded.
async fn reported(
    emulator: &Emulator,
    case: &str,
    resource_arn: &str,
) -> BTreeMap<String, String> {
    let (status, body) = list_tags(emulator, resource_arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListTagsForResource 200 for {resource_arn}, got {status} {body:?}"
    );
    assert!(
        body.get("tags").is_some(),
        "case {case:?}: expected tags present (empty when the resource carries none), got {body:?}"
    );
    tags_of(&body, "tags")
}

/// One expected tag map, from pairs.
fn expected(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The rule itself, over a table spanning the KIND of resource an ARN names: for each one,
/// the report answers from the record the resource's own create wrote, a tag is appended, a
/// tag on a key already carried replaces its value, and an untag removes exactly the keys it
/// names.
#[tokio::test]
async fn every_resource_kind_an_arn_names_tags_lists_and_untags() {
    let emulator = Emulator::new();
    // A domain created WITH tags and one created without, so the report is asserted over
    // both halves of the present-and-possibly-empty map.
    let (status, body) = emulator
        .post(
            "/domains/tagdom",
            json!({ "DefaultExpirationDays": 366, "Tags": { "env": "dev" } }),
        )
        .await;
    assert_eq!(status, 200, "CreateDomain tagdom: got {status} {body:?}");
    emulator.create_domain("plaindom", 366).await;
    put_contact_type(&emulator, "tagdom", "Contact_Record", true).await;
    define_object_type(&emulator, "tagdom").await;
    let (status, body) = crate::calculated_attribute_definitions::create(
        &emulator,
        "tagdom",
        "Order_Total",
        definition("SUM"),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateCalculatedAttributeDefinition: got {status} {body:?}"
    );

    // (case, the resource's ARN, the tags its own create stored)
    let rows: Vec<(&str, String, BTreeMap<String, String>)> = vec![
        (
            "a domain created with tags",
            arn("domains/tagdom"),
            expected(&[("env", "dev")]),
        ),
        (
            "a domain created with no tags",
            arn("domains/plaindom"),
            BTreeMap::new(),
        ),
        (
            "a profile object type",
            arn("domains/tagdom/object-types/Contact_Record"),
            BTreeMap::new(),
        ),
        (
            "a calculated attribute definition",
            arn("domains/tagdom/calculated-attributes/Order_Total"),
            BTreeMap::new(),
        ),
    ];

    for (case, resource_arn, at_create) in rows {
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            at_create,
            "case {case:?}: expected the tags its create stored"
        );

        // Appended: "If you specify a new tag key, this tag is appended to the list of tags
        // associated with the resource."
        let (status, body) = tag(
            &emulator,
            &resource_arn,
            json!({ "owner": "crm", "cost-center": "42" }),
        )
        .await;
        assert_no_content(case, "TagResource", status, &body);
        let mut expected_now = at_create.clone();
        expected_now.insert("owner".to_string(), "crm".to_string());
        expected_now.insert("cost-center".to_string(), "42".to_string());
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            expected_now,
            "case {case:?}: expected the appended tags beside the ones set at create"
        );

        // Replaced: "If you specify a tag key that is already associated with the resource,
        // the new tag value that you specify replaces the previous value for that tag."
        let (status, body) = tag(&emulator, &resource_arn, json!({ "owner": "support" })).await;
        assert_no_content(case, "TagResource on a key already carried", status, &body);
        expected_now.insert("owner".to_string(), "support".to_string());
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            expected_now,
            "case {case:?}: expected the value of the key already carried to be replaced"
        );

        // Removed: the keys the request names, and no others. A key the resource does not
        // carry removes nothing and is not an error.
        let (status, body) = untag(&emulator, &resource_arn, &["owner", "never-set"]).await;
        assert_no_content(case, "UntagResource", status, &body);
        expected_now.remove("owner");
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            expected_now,
            "case {case:?}: expected exactly the named keys removed"
        );
    }
}

/// A domain's tags are ONE set, whichever surface reads or writes them: what `CreateDomain`
/// stored is what `GetDomain` and `ListTagsForResource` both report, and what `TagResource`
/// adds reaches both of them.
#[tokio::test]
async fn domain_tags_are_one_set_across_get_domain_and_the_tagging_surface() {
    let emulator = Emulator::new();
    let (status, created) = emulator
        .post(
            "/domains/roundtrip",
            json!({ "DefaultExpirationDays": 366, "Tags": { "env": "dev", "team": "crm" } }),
        )
        .await;
    assert_eq!(status, 200, "CreateDomain: got {status} {created:?}");
    let resource_arn = arn("domains/roundtrip");

    let at_create = expected(&[("env", "dev"), ("team", "crm")]);
    assert_eq!(
        tags_of(&created, "Tags"),
        at_create,
        "expected CreateDomain to echo the tags it stored, got {:?}",
        created.get("Tags")
    );
    let (status, fetched) = emulator.send("GET", "/domains/roundtrip", None).await;
    assert_eq!(status, 200, "GetDomain: got {status} {fetched:?}");
    assert_eq!(
        tags_of(&fetched, "Tags"),
        at_create,
        "expected GetDomain to report the tags set at create, got {:?}",
        fetched.get("Tags")
    );
    assert_eq!(
        reported(&emulator, "at create", &resource_arn).await,
        at_create,
        "expected ListTagsForResource to report the tags set at create"
    );

    let (status, body) = tag(&emulator, &resource_arn, json!({ "stage": "beta" })).await;
    assert_no_content("a domain created with tags", "TagResource", status, &body);
    let mut after = at_create.clone();
    after.insert("stage".to_string(), "beta".to_string());
    assert_eq!(
        reported(&emulator, "after TagResource", &resource_arn).await,
        after,
        "expected the added tag beside the ones set at create"
    );
    let (_, fetched) = emulator.send("GET", "/domains/roundtrip", None).await;
    assert_eq!(
        tags_of(&fetched, "Tags"),
        after,
        "expected GetDomain to report the added tag too, got {:?}",
        fetched.get("Tags")
    );

    // A read changes nothing: the next report is identical.
    assert_eq!(
        reported(&emulator, "read twice", &resource_arn).await,
        after,
        "expected two reports of an unchanged resource to be identical"
    );
}

/// A well-formed ARN naming a resource the store does not hold is the tagging surface's own
/// not-found on each of the three operations — over a table spanning WHICH part of the
/// resource path names nothing, so a missing domain and a missing resource nested under a
/// domain the store does hold are both reached.
#[tokio::test]
async fn an_arn_naming_nothing_the_store_holds_is_not_found() {
    let emulator = Emulator::new();
    emulator.create_domain("heldom", 366).await;
    // A domain that WAS created and then deleted: its ARN is as well-formed as ever, and
    // names nothing.
    emulator.create_domain("gonedom", 366).await;
    let (status, body) = emulator.send("DELETE", "/domains/gonedom", None).await;
    assert_eq!(status, 200, "DeleteDomain: got {status} {body:?}");

    let rows = vec![
        ("a domain never created", arn("domains/nosuchdom")),
        ("a domain that was deleted", arn("domains/gonedom")),
        (
            "an object type the domain does not hold",
            arn("domains/heldom/object-types/Absent_Type"),
        ),
        (
            "a calculated attribute the domain does not hold",
            arn("domains/heldom/calculated-attributes/Absent"),
        ),
        (
            "a recommender schema the domain does not hold",
            arn("domains/heldom/recommender-schemas/Absent"),
        ),
        (
            "a resource under a domain that was never created",
            arn("domains/nosuchdom/object-types/Contact_Record"),
        ),
    ];

    for (case, resource_arn) in rows {
        for (operation, (status, body)) in [
            (
                "TagResource",
                tag(&emulator, &resource_arn, json!({ "env": "dev" })).await,
            ),
            ("ListTagsForResource", list_tags(&emulator, &resource_arn).await),
            ("UntagResource", untag(&emulator, &resource_arn, &["env"]).await),
        ] {
            // The tagging surface answers its OWN not-found: the shape `NotFoundException`,
            // whose sentence interpolates nothing — not the ARN the request carried.
            assert_eq!(
                (status, error_code(&body)),
                (404, "NotFoundException"),
                "case {case:?} {operation}: expected 404 NotFoundException, got {status} {body:?}"
            );
            assert_eq!(
                error_message(&body),
                "Resource was not found",
                "case {case:?} {operation}: expected the service's wording, got {:?}",
                error_message(&body)
            );
        }
    }
}

/// A value that is not one of this service's ARNs is the modelled 400 — over a table
/// spanning WHICH part of the member's contract the value breaks.
#[tokio::test]
async fn a_value_that_is_not_a_service_arn_is_a_bad_request() {
    let emulator = Emulator::new();
    emulator.create_domain("refusedom", 366).await;

    let rows = vec![
        ("not an ARN at all", "domains/refusedom".to_string()),
        (
            "another service's ARN",
            format!("arn:aws:connect:{REGION}:000000000000:instance/abc"),
        ),
        (
            "an ARN with no resource part",
            format!("arn:aws:profile:{REGION}:000000000000:"),
        ),
        (
            "the service segment alone, which the published pattern admits",
            "arn:aws:profile".to_string(),
        ),
        (
            "past the modelled length bound of 256",
            arn(&format!("domains/{}", "d".repeat(256))),
        ),
    ];

    for (case, resource_arn) in rows {
        for (operation, (status, body)) in [
            (
                "TagResource",
                tag(&emulator, &resource_arn, json!({ "env": "dev" })).await,
            ),
            ("ListTagsForResource", list_tags(&emulator, &resource_arn).await),
            ("UntagResource", untag(&emulator, &resource_arn, &["env"]).await),
        ] {
            assert_eq!(
                (status, error_code(&body)),
                (400, "BadRequestException"),
                "case {case:?} {operation}: expected 400 BadRequestException, got {status} {body:?}"
            );
            assert!(
                error_message(&body).contains("at 'resourceArn' failed to satisfy constraint"),
                "case {case:?} {operation}: expected the refusal to name the member, got {:?}",
                error_message(&body)
            );
        }
    }
}

/// A no-op untag writes nothing: a request naming keys the resource does not carry leaves
/// the stored record exactly as it was, so another operation's read of that record cannot
/// tell the untag happened — in particular a resource that reported NO tag map does not
/// start reporting an empty one.
#[tokio::test]
async fn an_untag_that_removes_nothing_leaves_the_record_as_it_was() {
    let emulator = Emulator::new();
    emulator.create_domain("nooptag", 366).await;
    put_contact_type(&emulator, "nooptag", "Contact_Record", true).await;

    // The object type was created with no `Tags`, and `GetProfileObjectType` omits the
    // member for one that carries none.
    let (status, before) = emulator
        .send("GET", "/domains/nooptag/object-types/Contact_Record", None)
        .await;
    assert_eq!(status, 200, "GetProfileObjectType: got {status} {before:?}");
    assert_eq!(
        before.get("Tags"),
        None,
        "expected an untagged object type to omit Tags, got {:?}",
        before.get("Tags")
    );

    let resource_arn = arn("domains/nooptag/object-types/Contact_Record");
    for (case, keys) in [
        ("one key it never carried", vec!["never-set"]),
        ("several keys it never carried", vec!["a", "b", "c"]),
    ] {
        let (status, body) = untag(&emulator, &resource_arn, &keys).await;
        assert_no_content(case, "UntagResource", status, &body);
        let (status, after) = emulator
            .send("GET", "/domains/nooptag/object-types/Contact_Record", None)
            .await;
        assert_eq!(
            (status, after.get("Tags")),
            (200, None),
            "case {case:?}: expected the record to be untouched, got {status} {after:?}"
        );
        assert_eq!(
            after, before,
            "case {case:?}: expected the whole projection to be untouched"
        );
    }

    // The same request once the resource DOES carry tags removes only what it names.
    let (status, body) = tag(&emulator, &resource_arn, json!({ "env": "dev" })).await;
    assert_no_content("an object type now carrying a tag", "TagResource", status, &body);
    let (status, body) = untag(&emulator, &resource_arn, &["never-set"]).await;
    assert_no_content("an object type now carrying a tag", "UntagResource", status, &body);
    assert_eq!(
        reported(&emulator, "a key it never carried", &resource_arn).await,
        expected(&[("env", "dev")]),
        "expected the tag it does carry to survive a removal of one it does not"
    );
}

/// The store's bound on a resource's tags: at the modelled maximum a request is served, and
/// a request that would leave the resource carrying more is refused with nothing written.
#[tokio::test]
async fn a_resource_carries_at_most_the_modelled_number_of_tags() {
    let emulator = Emulator::new();
    emulator.create_domain("capdom", 366).await;
    let resource_arn = arn("domains/capdom");

    // Exactly the maximum, in one request: served.
    let full: Value = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("k{index}"), Value::from(format!("v{index}"))))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let (status, body) = tag(&emulator, &resource_arn, full).await;
    assert_no_content("at the maximum", "TagResource", status, &body);
    assert_eq!(
        reported(&emulator, "at the maximum", &resource_arn).await.len(),
        MAX_TAGS_PER_RESOURCE,
        "case at the maximum: expected the resource to carry every tag"
    );

    // A key it already carries is not growth, so it is still served at the maximum.
    let (status, body) = tag(&emulator, &resource_arn, json!({ "k0": "replaced" })).await;
    assert_no_content("a replacement at the maximum", "TagResource", status, &body);

    // One more key is one too many: refused, and nothing written.
    let (status, body) = tag(&emulator, &resource_arn, json!({ "one-too-many": "x" })).await;
    assert_eq!(
        (status, error_code(&body)),
        (400, "BadRequestException"),
        "case one past the maximum: expected 400 BadRequestException, got {status} {body:?}"
    );
    let held = reported(&emulator, "one past the maximum", &resource_arn).await;
    assert_eq!(
        held.len(),
        MAX_TAGS_PER_RESOURCE,
        "case one past the maximum: expected nothing written, got {held:?}"
    );
    assert_eq!(
        held.get("k0").map(String::as_str),
        Some("replaced"),
        "case one past the maximum: expected the replacement to have stood, got {held:?}"
    );
}

/// A tagging MUTATION answers its success as no content — HTTP 204 with no body — over a
/// table spanning what the rule generalises over: which of the two writes it is, how many
/// entries the request carries (one, several, every one the resource holds, the modelled
/// maximum), and whether the write changes the stored set at all (a new key, a replacement of
/// a key already carried, a removal of a key never carried). None of that is the status: a
/// write of this surface has nothing to report, so every row answers alike.
///
/// The REPORT beside them is the contrast: it carries a map, so it answers 200 with a body —
/// the no-content projection belongs to the writes, not to the surface.
#[tokio::test]
async fn every_tagging_mutation_answers_no_content_with_no_body() {
    let emulator = Emulator::new();
    emulator.create_domain("statusdom", 366).await;
    put_contact_type(&emulator, "statusdom", "Contact_Record", true).await;
    let domain_arn = arn("domains/statusdom");
    let object_type_arn = arn("domains/statusdom/object-types/Contact_Record");

    // Every entry of the maximum this store holds per resource, in one request: the largest
    // write the surface serves is still a write with nothing to report.
    let full: Value = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("k{index}"), Value::from(format!("v{index}"))))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let every_key: Vec<String> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| format!("k{index}"))
        .collect();

    // (case, the ARN, the write to make)
    for (case, resource_arn, write) in [
        ("one new key", &domain_arn, json!({ "stage": "probe" })),
        (
            "several new keys",
            &domain_arn,
            json!({ "owner": "crm", "cost-center": "42" }),
        ),
        (
            "a key already carried, whose value is replaced",
            &domain_arn,
            json!({ "stage": "live" }),
        ),
        (
            "the same value written again, which changes nothing",
            &domain_arn,
            json!({ "stage": "live" }),
        ),
        (
            "a resource whose own create stored no tags at all",
            &object_type_arn,
            json!({ "stage": "probe" }),
        ),
    ] {
        let (status, body) = tag(&emulator, resource_arn, write).await;
        assert_no_content(case, "TagResource", status, &body);
    }

    for (case, resource_arn, keys) in [
        ("one key the resource carries", &domain_arn, vec!["stage"]),
        (
            "several keys the resource carries",
            &domain_arn,
            vec!["owner", "cost-center"],
        ),
        (
            "a key the resource never carried, which removes nothing",
            &domain_arn,
            vec!["never-set"],
        ),
        (
            "a resource carrying no tag map at all",
            &arn("domains/statusdom/object-types/Contact_Record"),
            vec!["stage"],
        ),
    ] {
        let (status, body) = untag(&emulator, resource_arn, &keys).await;
        assert_no_content(case, "UntagResource", status, &body);
    }

    // The widest pair: the maximum written in one request, then every one of those keys
    // removed in one request.
    let (status, body) = tag(&emulator, &domain_arn, full).await;
    assert_no_content("the modelled maximum in one request", "TagResource", status, &body);
    let keys: Vec<&str> = every_key.iter().map(String::as_str).collect();
    let (status, body) = untag(&emulator, &domain_arn, &keys).await;
    assert_no_content("every key the resource carries", "UntagResource", status, &body);
    assert_eq!(
        reported(&emulator, "after removing every key", &domain_arn).await,
        BTreeMap::new(),
        "expected the removals to have been applied behind the no-content answers"
    );

    // The report is not a write: it carries a map, so it answers with content.
    let (status, body) = list_tags(&emulator, &domain_arn).await;
    assert_eq!(
        status, 200,
        "case \"the report beside the writes\": expected 200, got {status} {body:?}"
    );
    assert!(
        body.get("tags").is_some(),
        "case \"the report beside the writes\": expected a body carrying the map, got {body:?}"
    );
}
