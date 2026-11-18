// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the page-size bound every paginated listing of this service
//! shares.
//!
//! The RULE asserted here: **the largest `Limit` a listing accepts is the modelled
//! maximum of `LimitType`, the shape every `Limit` member of this service targets
//! (`@range(min: 1, max: 1000)`, `smithy_query(query="operation",operation="ListGrants")`
//! -> `Limit`); a value inside that range is SERVED, and one outside it is refused 400
//! `ValidationException` in the counted envelope a constraint refusal carries, naming the
//! member and the modelled bound** — no listing states a tighter bound of its own, and no
//! refusal of one is worded by hand.
//!
//! The rows are the four listings whose `Limit` targets that shape, each at both
//! published bounds of the range, inside it, and one past each bound. The listings differ
//! in what they page over (aliases, grants, retirable grants, resource tags) and in
//! whether they name a key, which is what the rule generalises over.

use serde_json::{json, Value};

use super::harness::{create_key, ok, one_violation, refused, router, text, REGION};

/// The modelled bounds of `LimitType`, which every `Limit` member of this service
/// targets.
const MIN_LIMIT: i64 = 1;
const MAX_LIMIT: i64 = 1000;

/// The clause a `Limit` past the modelled maximum is refused with.
fn past_the_maximum(limit: i64) -> String {
    format!(
        "Value '{limit}' at 'limit' failed to satisfy constraint: Member must have value \
         less than or equal to {MAX_LIMIT}"
    )
}

/// The clause a `Limit` below the modelled minimum is refused with.
fn below_the_minimum(limit: i64) -> String {
    format!(
        "Value '{limit}' at 'limit' failed to satisfy constraint: Member must have value \
         greater than or equal to {MIN_LIMIT}"
    )
}

/// Every listing whose `Limit` targets `LimitType`, with the request members it needs
/// besides the limit and the member its page arrives in.
struct Listing {
    operation: &'static str,
    /// Whether the request has to name the key the listing is scoped to.
    names_a_key: bool,
    /// The member the served page is reported in.
    page_member: &'static str,
}

const LISTINGS: &[Listing] = &[
    Listing {
        operation: "ListAliases",
        names_a_key: false,
        page_member: "Aliases",
    },
    Listing {
        operation: "ListGrants",
        names_a_key: true,
        page_member: "Grants",
    },
    Listing {
        operation: "ListResourceTags",
        names_a_key: true,
        page_member: "Tags",
    },
    Listing {
        operation: "ListRetirableGrants",
        names_a_key: false,
        page_member: "Grants",
    },
];

/// The request body a listing takes for `limit`, scoped to `key_id` when it names a key
/// and to the retiring principal when it selects by one.
fn request(listing: &Listing, key_id: &str, limit: Option<i64>) -> Value {
    let mut body = json!({});
    if listing.names_a_key {
        body["KeyId"] = json!(key_id);
    }
    if listing.operation == "ListRetirableGrants" {
        // This listing selects by the retiring principal rather than by a key, and a
        // request naming neither selects nothing.
        body["RetiringPrincipal"] = json!(super::harness::principal("root"));
    }
    if let Some(limit) = limit {
        body["Limit"] = json!(limit);
    }
    body
}

/// A `Limit` anywhere inside the modelled range is served, including the maximum itself.
#[tokio::test]
async fn every_listing_serves_a_limit_inside_the_modelled_range() {
    let router = router();
    let metadata = create_key(&router, &json!({"Description": "paged"})).await;
    let key_id = text("the listed key", &metadata, "KeyId");
    ok(
        &router,
        "TagResource",
        &json!({"KeyId": &key_id, "Tags": [{"TagKey": "k", "TagValue": "v"}]}),
    )
    .await;

    for listing in LISTINGS {
        // The rows: the modelled minimum, the bound the API reference names for several
        // of these listings, one past that, and the modelled maximum.
        for limit in [MIN_LIMIT, 50, 100, 101, 999, MAX_LIMIT] {
            let case = format!("{} with Limit {limit}", listing.operation);
            let answer = ok(
                &router,
                listing.operation,
                &request(listing, &key_id, Some(limit)),
            )
            .await;
            let page = answer[listing.page_member]
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: expected a {} list, got {answer}",
                        listing.page_member
                    )
                })
                .clone();
            assert!(
                i64::try_from(page.len()).unwrap_or(i64::MAX) <= limit,
                "case {case}: a page holds at most {limit} entries, got {}",
                page.len()
            );
            assert_eq!(
                answer["Truncated"],
                json!(false),
                "case {case}: a page holding everything is not truncated, got {answer}"
            );
        }
    }
}

/// A `Limit` outside the modelled range is refused with the counted envelope, naming the
/// member and the modelled bound — one past each published bound, and a value far past
/// the maximum.
#[tokio::test]
async fn every_listing_refuses_a_limit_outside_the_modelled_range() {
    let router = router();
    let metadata = create_key(&router, &json!({"Description": "paged"})).await;
    let key_id = text("the listed key", &metadata, "KeyId");

    for listing in LISTINGS {
        let rows: [(i64, String); 4] = [
            (0, below_the_minimum(0)),
            (-1, below_the_minimum(-1)),
            (MAX_LIMIT + 1, past_the_maximum(MAX_LIMIT + 1)),
            (2000, past_the_maximum(2000)),
        ];
        for (limit, clause) in rows {
            let case = format!("{} with Limit {limit}", listing.operation);
            let (status, code, message) = refused(
                &router,
                REGION,
                listing.operation,
                &request(listing, &key_id, Some(limit)),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationException"),
                "case {case}: expected HTTP 400 ValidationException, got {status} {code}"
            );
            assert_eq!(
                message,
                one_violation(&clause),
                "case {case}: expected {:?}, got {message:?}",
                one_violation(&clause)
            );
        }
    }
}

/// A page the maximum `Limit` asks for really carries what it was given: with more
/// entries held than a smaller limit admits, the listing at the maximum reports them all
/// in one page while the smaller one truncates.
#[tokio::test]
async fn the_modelled_maximum_serves_a_whole_page() {
    let router = router();
    let metadata = create_key(&router, &json!({"Description": "tagged"})).await;
    let key_id = text("the tagged key", &metadata, "KeyId");
    let held = 7;
    let tags: Vec<Value> = (0..held)
        .map(|n| json!({"TagKey": format!("tag-{n}"), "TagValue": format!("value-{n}")}))
        .collect();
    ok(
        &router,
        "TagResource",
        &json!({"KeyId": &key_id, "Tags": tags}),
    )
    .await;

    let whole = ok(
        &router,
        "ListResourceTags",
        &json!({"KeyId": &key_id, "Limit": MAX_LIMIT}),
    )
    .await;
    assert_eq!(
        whole["Tags"].as_array().map(Vec::len).unwrap_or_default(),
        held,
        "case Limit at the modelled maximum: expected all {held} tags in one page, got \
         {whole}"
    );
    assert_eq!(
        whole["Truncated"],
        json!(false),
        "case Limit at the modelled maximum: a complete page is not truncated, got {whole}"
    );

    let short = ok(
        &router,
        "ListResourceTags",
        &json!({"KeyId": &key_id, "Limit": 2}),
    )
    .await;
    assert_eq!(
        short["Tags"].as_array().map(Vec::len).unwrap_or_default(),
        2,
        "case Limit 2: expected a page of two, got {short}"
    );
    assert_eq!(
        short["Truncated"],
        json!(true),
        "case Limit 2: entries remain after a short page, got {short}"
    );
}
