// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ApiGatewayV2 `GetTags`.
//!
//! Every test drives the real `restJson1` request path — `MultiServiceRouter::dispatch`
//! over a signed `http::Request` — so what is asserted is the wire behaviour a client
//! sees, not a handler called directly.
//!
//! The rule these cases assert, one level above the recorded case that filed the issue:
//!
//! > `GetTags` (`GET /v2/tags/{ResourceArn}`, modelled HTTP 200) REPORTS the tag collection
//! > stored on the resource its ARN names — the very collection that resource's own reader
//! > reports — as a `tags` member that is present even when the resource carries no tag, and
//! > it changes nothing, so a later caller sees exactly what this one saw. A well-formed ARN
//! > of a taggable kind that the account holds no record for, including a resource a delete
//! > has taken away, is 404 `NotFoundException` with the one sentence a tag operation answers
//! > an unresolved resource ARN with.
//!
//! The rows vary what the rule generalises over: WHICH kind of resource the ARN names (all
//! six whose modelled shape carries a `Tags` member, which is also both ARN forms this
//! service mints — the account-less management form and the account-qualified one), how many
//! tags the record carries (none, one, several), and the form of a key and of a value (ASCII,
//! the separators a tag key may contain, a non-ASCII key and value, a key at the published
//! 128-character maximum, a value at the modelled upper bound). The recorded unit that filed
//! the issue — a tag read of an api that had been deleted — is one row of the last table.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_apigatewayv2_apigatewayv2::ComAmazonawsApigatewayv2Apigatewayv2State;
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, emulator_with_engine, error_code, REGION};
use crate::path_label::addressed;
use crate::resource_tag_removal::{taggable_resources, Target, TAGS};

/// The sentence this API answers a tag operation whose `ResourceArn` resolves to no resource
/// the account holds with. Spelled out here rather than taken from the crate's constant: a
/// test that asserts a handler's own constant back at it cannot catch the wording drifting
/// away from the service's.
const RESOURCE_NOT_FOUND: &str = "Resource was not found";

/// How long a case waits for a removal the clock owes to fall due before giving up —
/// generous, because what is waited for is the CONDITION and not a span.
const REMOVAL_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The tag collection `GetTags` reports for `resource_arn`, asserted to be the modelled 200
/// carrying a `tags` member — which is reported for every resource that has a collection, so
/// a resource carrying no tag reports an empty one rather than no member at all.
async fn get_tags(
    emulator: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
) -> BTreeMap<String, String> {
    let uri = format!("{TAGS}/{}", addressed(resource_arn));
    let (status, body) = call(emulator, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetTags at {uri}, got {status} {body}"
    );
    reported_collection(case, &body)
}

/// The collection the `tags` member of ONE answer already in hand carries — the single
/// reading these cases take of a reported tag collection, so an answer is never read by
/// asking for it a second time (which would be a second, independent call).
fn reported_collection(case: &str, body: &Value) -> BTreeMap<String, String> {
    let Some(reported) = body.get("tags") else {
        panic!("case {case:?}: expected a tags member to be reported, got {body}");
    };
    serde_json::from_value(reported.clone()).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a collection of tags, got {reported} ({error})")
    })
}

/// The tag collection a resource's OWN reader reports, as a map. A reader that reports no
/// `tags` member at all reports no tags.
async fn tags_through_the_resources_own_reader(
    emulator: &MultiServiceRouter,
    case: &str,
    read_uri: &str,
) -> BTreeMap<String, String> {
    let body = read_back(emulator, case, read_uri).await;
    let reported = body.get("tags").cloned().unwrap_or_else(|| json!({}));
    serde_json::from_value(reported.clone()).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a collection of tags, got {reported} ({error})")
    })
}

/// `tags` as the map a reader reports it as.
fn as_map(tags: &Value) -> BTreeMap<String, String> {
    serde_json::from_value(tags.clone()).expect("a row's tags are a collection of strings")
}

/// A tag collection of one pair, for a row whose key or value is built rather than written
/// out — `json!` takes a literal key.
fn one_tag(key: &str, value: &str) -> Value {
    let mut tags = serde_json::Map::new();
    tags.insert(key.to_string(), Value::String(value.to_string()));
    Value::Object(tags)
}

/// The whole record a resource's own reader reports, so a member a read moved is caught and
/// not only the tags.
async fn read_back(emulator: &MultiServiceRouter, case: &str, read_uri: &str) -> Value {
    let (status, body) = call(emulator, "GET", read_uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 reading {read_uri} back, got {status} {body}"
    );
    body
}

/// One reporting row: the tag collection the row's resources are created with.
struct Row {
    case: &'static str,
    /// The label the row's resources are named from — `label` of [`taggable_resources`].
    label: &'static str,
    tags: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no tag at all",
            label: "untagged",
            tags: json!({}),
        },
        Row {
            case: "one tag",
            label: "one-tag",
            tags: json!({ "team": "payments" }),
        },
        Row {
            case: "several tags",
            label: "several-tags",
            tags: json!({ "team": "payments", "tier": "gold", "env": "test" }),
        },
        Row {
            // "A tag key can have a maximum of 128 characters, and can contain only
            // Unicode letters, digits, white space, or these symbols: _ . : / = + - @"
            // — so the separators and any Unicode letter are part of a key, and a key or a
            // value carrying them is reported back exactly as it was stored.
            case: "a key carrying the separators a tag key may contain, and a non-ASCII pair",
            label: "key-forms",
            tags: json!({ "cost:center/eu": "42", "équipe": "paiements" }),
        },
    ]
}

/// The rows whose key or value sits at a published bound, which cannot be `const` data.
fn bound_rows() -> Vec<Row> {
    vec![
        Row {
            // The 128-character maximum AWS publishes for a tag key.
            case: "a key at the 128-character maximum a tag key may carry",
            label: "key-at-bound",
            tags: one_tag(&"k".repeat(128), "at-the-key-bound"),
        },
        Row {
            // `Tags` is a map of `__string` to `StringWithLengthBetween1And1600`
            // (`smithy_query(query="shape", shape="Tags")`), so 1600 characters is the
            // modelled upper bound of a tag VALUE.
            case: "a value at the modelled 1600-character upper bound",
            label: "value-at-bound",
            tags: one_tag("notes", &"v".repeat(1600)),
        },
    ]
}

#[tokio::test]
async fn get_tags_reports_the_collection_the_resource_its_arn_names_holds() {
    let emulator = emulator();

    for row in rows().into_iter().chain(bound_rows()) {
        let expected = as_map(&row.tags);
        for target in taggable_resources(&emulator, row.label, &row.tags).await {
            let case = format!("{} on {}", row.case, target.kind);

            // The collection the ARN's record holds, reported for the ARN — read twice, so
            // a reporting operation that changed what it reported would be caught.
            for round in 0..2 {
                let reported = get_tags(&emulator, &case, &target.arn).await;
                assert_eq!(
                    reported, expected,
                    "case {case:?} (round {round}): expected GetTags to report the tags the \
                     resource was created with"
                );
            }

            // And it is ONE collection: the resource's own reader reports the same one.
            assert_eq!(
                tags_through_the_resources_own_reader(&emulator, &case, &target.read_uri).await,
                expected,
                "case {case:?}: expected GetTags and the resource's own reader to report one \
                 collection"
            );
        }
    }
}

#[tokio::test]
async fn a_resource_created_without_a_tags_member_reports_an_empty_collection() {
    let emulator = emulator();

    // Two creates that name no `tags` member at all — one resource of each ARN form this
    // service mints: the account-less management ARN of an api, and the ARN a portal product
    // reports for itself.
    let (status, api) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "no-tags-member", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"an api created with no tags member\": expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");

    let (status, product) = call(
        &emulator,
        "POST",
        "/v2/portalproducts",
        Some(json!({ "displayName": "no tags member" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"a portal product created with no tags member\": expected 201 from \
         CreatePortalProduct, got {status} {product}"
    );

    for (case, arn) in [
        (
            "an api created with no tags member",
            format!("arn:aws:apigateway:{REGION}::/apis/{api_id}"),
        ),
        (
            "a portal product created with no tags member",
            as_str(&product, "portalProductArn"),
        ),
    ] {
        assert_eq!(
            get_tags(&emulator, case, &arn).await,
            BTreeMap::new(),
            "case {case:?}: expected GetTags to report an empty collection"
        );
    }
}

#[tokio::test]
async fn reading_the_tags_of_a_resource_changes_nothing_a_later_caller_sees() {
    let (emulator, engine) = emulator_with_engine();
    let tags = json!({ "team": "payments", "tier": "gold" });

    let (status, api) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "read-only", "protocolType": "HTTP", "tags": tags })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"the api the reads are addressed at\": expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");
    let read_uri = format!("/v2/apis/{api_id}");
    let arn = format!("arn:aws:apigateway:{REGION}::/apis/{api_id}");

    let before = read_back(&emulator, "before the tag reads", &read_uri).await;
    let stored_before = engine
        .state::<ComAmazonawsApigatewayv2Apigatewayv2State>()
        .lock()
        .apis
        .clone();

    // A well-formed ARN of a taggable kind the account holds no record for, read beside the
    // held one: a refused read is a read too, so it may not mint the record it missed.
    let absent = format!("arn:aws:apigateway:{REGION}::/apis/nosuchapi00");

    for round in 0..3 {
        assert_eq!(
            get_tags(&emulator, "the held api", &arn).await,
            as_map(&tags),
            "case \"the held api\" (round {round}): expected the stored collection"
        );

        let uri = format!("{TAGS}/{}", addressed(&absent));
        let (status, body) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 404,
            "case \"an ARN the account holds no record for\" (round {round}): expected 404, got \
             {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case \"an ARN the account holds no record for\" (round {round}): expected \
             NotFoundException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            RESOURCE_NOT_FOUND,
            "case \"an ARN the account holds no record for\" (round {round}): expected the \
             sentence this API answers an unresolved resource ARN with, got {body}"
        );
    }

    // Nothing the reads touched moved: the record reads back member for member, and the
    // store itself holds exactly what it held — one api, with the tags it was created with.
    assert_eq!(
        read_back(&emulator, "after the tag reads", &read_uri).await,
        before,
        "case \"the held api\": expected the record to read back unchanged after the tag reads"
    );
    let stored_after = engine
        .state::<ComAmazonawsApigatewayv2Apigatewayv2State>()
        .lock()
        .apis
        .clone();
    assert_eq!(
        stored_after.keys().collect::<Vec<_>>(),
        stored_before.keys().collect::<Vec<_>>(),
        "case \"the held api\": expected the tag reads to mint and remove no record"
    );
    let stored_tags = stored_after
        .get(&api_id)
        .expect("the api the create persisted")
        .tags
        .clone();
    assert_eq!(
        stored_tags,
        as_map(&tags),
        "case \"the held api\": expected the store itself to hold the created tags, got \
         {stored_tags:?}"
    );
}

/// Poll `GetTags` for `resource_arn` until the account no longer holds what it names.
///
/// The removal of a resource is owed by the CLOCK for some of the kinds this service tags (a
/// VPC link is deprovisioned over a span), so the answer is waited FOR rather than timed:
/// until it comes, the resource is still held and the read reports exactly the collection it
/// carries — never another refusal.
async fn await_not_found(
    emulator: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
    held: &BTreeMap<String, String>,
) {
    let uri = format!("{TAGS}/{}", addressed(resource_arn));
    let deadline = Instant::now() + REMOVAL_DEADLINE;
    loop {
        let (status, body) = call(emulator, "GET", &uri, None).await;
        if status == 404 {
            assert_eq!(
                error_code(&body),
                "NotFoundException",
                "case {case:?}: expected NotFoundException from GetTags, got {body}"
            );
            assert_eq!(
                as_str(&body, "message"),
                RESOURCE_NOT_FOUND,
                "case {case:?}: expected GetTags to answer the sentence this API answers a tag \
                 operation's unresolved resource ARN with, got {body}"
            );
            return;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetTags to either report the tags of a resource still held \
             or answer 404, got {status} {body}"
        );
        // Read out of the answer already in hand: a second call could land past the instant
        // the removal falls due and answer the 404 this loop is waiting for.
        let reported = reported_collection(case, &body);
        assert_eq!(
            &reported, held,
            "case {case:?}: expected a resource whose removal is still outstanding to report the \
             tags it carries"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected GetTags to answer 404 once the account no longer holds the \
             resource"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_tags_of_a_resource_the_account_no_longer_holds_are_a_not_found() {
    let emulator = emulator();
    let tags = json!({ "team": "payments", "tier": "gold" });
    let held = as_map(&tags);

    let taken_away = taggable_resources(&emulator, "taken-away", &tags).await;
    let survivors = taggable_resources(&emulator, "surviving", &tags).await;

    // A stage hangs off its api, so it is taken away BEFORE the api it belongs to: the
    // order the deletes run in is the containment order, not the order of the table.
    let mut order: Vec<&Target> = taken_away.iter().collect();
    order.sort_by_key(|target| usize::from(target.kind != "a stage of an api"));

    for target in order {
        let case = format!("{} that was deleted", target.kind);

        // While it is held, its tags are reported.
        assert_eq!(
            get_tags(&emulator, &case, &target.arn).await,
            held,
            "case {case:?}: expected the created tags before the delete"
        );

        // Every one of these resources is deleted at the URI its own reader serves.
        let (status, body) = call(&emulator, "DELETE", &target.read_uri, None).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: expected the delete of {} to be accepted, got {status} {body}",
            target.read_uri
        );

        await_not_found(&emulator, &case, &target.arn, &held).await;
    }

    // Taking one resource away touched no other: every sibling still reports its tags.
    for target in &survivors {
        let case = format!("the surviving {}", target.kind);
        assert_eq!(
            get_tags(&emulator, &case, &target.arn).await,
            held,
            "case {case:?}: expected a sibling's tags to be untouched by the deletes"
        );
    }
}
