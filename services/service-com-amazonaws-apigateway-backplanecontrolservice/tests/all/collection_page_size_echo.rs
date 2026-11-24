// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PAGE SIZE a paginated collection of this service echoes
//! beside the cursor it carries on at.
//!
//! The rule, asserted over a SET of inputs rather than one recorded call: a page reports
//! the size it was CUT to — the size the request asked for, held inside the range the
//! `limit` member publishes — so a request that asked for a size outside that range is
//! told the size it was served, and a page the collection holds nothing after (and a
//! request that asked for no size at all) is told nothing.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the requested SIZE: below the published minimum, at it, inside the range, at the
//!   published maximum, one past it and far past it, and absent;
//! * the COLLECTION: a resource tree (`GetResources`), the account's APIs
//!   (`GetRestApis`) and the account's API keys (`GetApiKeys`) — three listings of the one
//!   shared envelope, each cursored by a minted id;
//! * whether the collection carries on after the page or ends with it.
//!
//! Source of the range: every paginated `limit` member of this service — "The maximum
//! number of returned results per page. The default value is 25 and the maximum value is
//! 500" (`smithy_query(query="operation", operation="GetResources,GetRestApis,\
//! GetApiKeys", model_path="services/service-com-amazonaws-apigateway-\
//! backplanecontrolservice/smithy-model/com-amazonaws-apigateway-\
//! backplanecontrolservice.json")`).
//!
//! [`Gateway`] and friends are this crate's one wire-test harness, reused from
//! `rest_api_lifecycle`.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT,
};

use crate::rest_api_lifecycle::{create_api, create_resource, page_items, string_member, Gateway};

/// How many elements each collection of the first table is stood up with: enough that a
/// page cut to the smallest sizes leaves elements after it, and few enough that a page cut
/// to a larger size holds the whole collection.
const COUNT: usize = 3;

/// One collection of the first table: the operation that lists it, and the listing's URI.
struct Listing {
    collection: &'static str,
    uri: String,
}

/// The page size a request asking for `requested` is served, which is the size the answer
/// states: the requested size held inside the published range.
fn applied(requested: i64) -> i64 {
    requested.clamp(1, i64::from(MAX_PAGE_LIMIT))
}

/// Stand up `COUNT` elements of each collection the first table spans, on one emulator.
async fn listings(gateway: &Gateway) -> Vec<Listing> {
    // A resource tree: the API's root resource plus the children that complete the count.
    let (api, root) = create_api(gateway, "page-size-echo").await;
    for index in 1..COUNT {
        create_resource(gateway, &api, &root, &format!("part{index}")).await;
    }
    // The account's APIs: the one above plus the rest of the count.
    for index in 1..COUNT {
        create_api(gateway, &format!("page-size-echo-{index}")).await;
    }
    // The account's API keys.
    for index in 0..COUNT {
        let case = format!("CreateApiKey(#{index})");
        let answer = gateway
            .call(
                "POST",
                "/apikeys",
                Some(json!({ "name": format!("page-size-echo-{index}") })),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
    }
    vec![
        Listing {
            collection: "GetResources",
            uri: format!("/restapis/{api}/resources"),
        },
        Listing {
            collection: "GetRestApis",
            uri: "/restapis".to_string(),
        },
        Listing {
            collection: "GetApiKeys",
            uri: "/apikeys".to_string(),
        },
    ]
}

/// One page of `listing`, asserting the 200 every page of a resolvable cursor answers.
async fn page(gateway: &Gateway, uri: &str, case: &str) -> Value {
    let answer = gateway.call("GET", uri, None).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

#[tokio::test]
async fn a_page_echoes_the_size_it_was_cut_to_held_inside_the_published_range() {
    let gateway = Gateway::new();
    for listing in listings(&gateway).await {
        // Sizes a collection of COUNT elements is carried on after, so the page reports a
        // cursor and states the size beside it: one below the published minimum, the
        // minimum itself, and a size inside the range that still leaves elements over.
        for requested in [-1_i64, 0, 1, 2] {
            let case = format!("{}(limit={requested})", listing.collection);
            let body = page(&gateway, &format!("{}?limit={requested}", listing.uri), &case).await;
            let served = applied(requested);
            assert_eq!(
                i64::try_from(page_items(&body, &case).len()).unwrap_or(-1),
                served,
                "case {case}: the page holds the {served} element(s) it was cut to, got {body:?}"
            );
            assert!(
                body["position"].is_string(),
                "case {case}: a collection of {COUNT} carries on after a page of {served}, \
                 got {:?}",
                body["position"]
            );
            assert_eq!(
                body["limit"].as_i64(),
                Some(served),
                "case {case}: expected the served page size {served}, got {:?}",
                body["limit"]
            );
        }

        // Sizes the whole collection fits inside, including the published maximum, one
        // past it and far past it: the page ends the collection, so it reports neither the
        // cursor nor the size.
        for requested in [
            i64::try_from(COUNT).unwrap_or_default(),
            i64::from(DEFAULT_PAGE_LIMIT),
            i64::from(MAX_PAGE_LIMIT),
            i64::from(MAX_PAGE_LIMIT) + 1,
            10_000,
        ] {
            let case = format!("{}(limit={requested}, the whole collection)", listing.collection);
            let body = page(&gateway, &format!("{}?limit={requested}", listing.uri), &case).await;
            assert_eq!(
                page_items(&body, &case).len(),
                COUNT,
                "case {case}: the page holds the whole collection, got {body:?}"
            );
            assert!(
                body["position"].is_null(),
                "case {case}: the page that ends the collection reports no cursor, got {:?}",
                body["position"]
            );
            assert!(
                body["limit"].is_null(),
                "case {case}: the page that ends the collection states no page size, got {:?}",
                body["limit"]
            );
        }

        // A request that asked for no page size is told none, whether or not the
        // collection carries on after the page it is served.
        let case = format!("{}(no limit)", listing.collection);
        let body = page(&gateway, &listing.uri, &case).await;
        assert!(
            body["limit"].is_null(),
            "case {case}: a request that asked for no page size is told none, got {:?}",
            body["limit"]
        );
    }
}

#[tokio::test]
async fn a_size_past_the_published_maximum_is_stated_as_the_maximum_that_was_served() {
    // The upper bound of the range needs a collection the served page does NOT hold all
    // of, so this row stands up one element more than the published maximum — the
    // account's API keys, the one collection of this service whose quota is far above that
    // maximum.
    let gateway = Gateway::new();
    let count = usize::try_from(MAX_PAGE_LIMIT).unwrap_or_default() + 1;
    let mut keys = Vec::with_capacity(count);
    for index in 0..count {
        let case = format!("CreateApiKey(#{index})");
        let answer = gateway
            .call(
                "POST",
                "/apikeys",
                Some(json!({ "name": format!("bound-{index:04}") })),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        keys.push(string_member(&answer.body, "id", &case));
    }

    for requested in [
        i64::from(MAX_PAGE_LIMIT),
        i64::from(MAX_PAGE_LIMIT) + 1,
        10_000,
    ] {
        let case = format!("GetApiKeys(limit={requested})");
        let body = page(&gateway, &format!("/apikeys?limit={requested}"), &case).await;
        let held = page_items(&body, &case).len();
        assert_eq!(
            held,
            usize::try_from(MAX_PAGE_LIMIT).unwrap_or_default(),
            "case {case}: a page holds at most the published maximum, got {held} element(s)"
        );
        let cursor = body["position"].as_str().unwrap_or_else(|| {
            panic!(
                "case {case}: a collection of {count} carries on after a page of \
                 {MAX_PAGE_LIMIT}, got {:?}",
                body["position"]
            )
        });
        assert!(
            keys.iter().all(|id| !cursor.contains(id.as_str())),
            "case {case}: the cursor is this service's own token and spells no key's id, \
             got {cursor:?}"
        );
        let resumed = page(
            &gateway,
            &format!("/apikeys?position={cursor}&limit={requested}"),
            &case,
        )
        .await;
        let rest: Vec<String> = page_items(&resumed, &case)
            .iter()
            .map(|item| string_member(item, "id", &case))
            .collect();
        assert_eq!(
            rest,
            keys[count - 1..].to_vec(),
            "case {case}: resuming at the cursor reports the element the page did not, \
             got {rest:?}"
        );
        assert_eq!(
            body["limit"].as_i64(),
            Some(i64::from(MAX_PAGE_LIMIT)),
            "case {case}: expected the served page size {MAX_PAGE_LIMIT}, got {:?}",
            body["limit"]
        );
    }
}
