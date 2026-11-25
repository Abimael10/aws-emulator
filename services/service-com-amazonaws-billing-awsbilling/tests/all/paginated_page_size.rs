// SPDX-License-Identifier: CC0-1.0

//! The page size every paginated read of this service holds its answer to.
//!
//! Every assertion here is about ONE rule: the size a paginated read walks with
//! is held inside the bounds that read's modelled `maxResults` range declares —
//! a request that names no size gets the size the member documents, a size
//! outside the range is held at the bound it passed, and the size is therefore
//! never zero. Two things follow, and both are asserted below: no answer carries
//! more entries than the surface states, and no size leaves a stored entry
//! unreachable, because an empty page is how a read says nothing is stored.
//!
//! The first table spans every paginated read of this crate (each read's own
//! bounds and documented default) and, for each, the sizes the rule generalises
//! over: none named, each published bound, one past each bound, far past the
//! upper bound, and a value that is no size at all. The second walks
//! `GetBillingPreferences` over the wire at sizes spanning its range against
//! stored counts under, at and over the page.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::page_limit;
use service_com_amazonaws_billing_awsbilling::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// Every `BillingFeature` the model declares (`BILLING_FEATURE_VALUES`), each
/// activated for the caller so these reads are about paging rather than about
/// the per-feature access gate (pinned in `tests/all/billing_feature_access.rs`).
const ALL_FEATURES: &[&str] = &[
    "BILLING_ALERTS",
    "CREDIT_LEVEL_SHARING",
    "CREDIT_PREFERENCE_OPTIONS",
    "CREDIT_SHARING",
    "CREDIT_SHARING_HISTORY",
    "RI_SHARING",
    "RI_SHARING_HISTORY",
];

/// A ceiling on the pages one walk below may take, so a walk that fails to make
/// progress fails the test instead of running forever.
const MAX_PAGES: usize = 128;

/// One paginated read of this service: the bounds of its modelled `maxResults`
/// range and the page size it serves a request that names none.
struct PaginatedRead {
    operation: &'static str,
    /// The size the `maxResults` member documents for a request that names none,
    /// or — where it documents none — the largest page the read serves.
    default_page: usize,
    /// The `range` minimum of that member.
    minimum: usize,
    /// The `range` maximum of that member.
    maximum: usize,
}

/// Every paginated read this service serves, with the bounds its `maxResults`
/// member is modelled with.
///
/// Source: `smithy_query(query="operation", operation="<each one below>")` —
/// `maxResults` carries `range {min, max}` for each, and its doc states the
/// default where there is one (`GetBillingPreferences`: "Range: 1 to 50.
/// Default: 50"; `ListBillingViews`: "Default is 100").
const PAGINATED_READS: &[PaginatedRead] = &[
    PaginatedRead {
        operation: "GetBillingPreferences",
        default_page: 50,
        minimum: 1,
        maximum: 50,
    },
    PaginatedRead {
        operation: "ListBillingViews",
        default_page: 100,
        minimum: 1,
        maximum: 100,
    },
    PaginatedRead {
        operation: "ListBillingViewSegments",
        default_page: 100,
        minimum: 1,
        maximum: 100,
    },
    PaginatedRead {
        operation: "ListSourceViewsForBillingView",
        default_page: 100,
        minimum: 1,
        maximum: 100,
    },
    PaginatedRead {
        operation: "GetCreditAllocationHistory",
        default_page: 100,
        minimum: 1,
        maximum: 1000,
    },
    PaginatedRead {
        operation: "ListBusinessSupportAccountCharges",
        default_page: 100,
        minimum: 1,
        maximum: 100,
    },
    PaginatedRead {
        operation: "ListBusinessSupportSubscriptionHistory",
        default_page: 100,
        minimum: 1,
        maximum: 100,
    },
    PaginatedRead {
        operation: "ListEnterpriseSupportLinkedAccountCharges",
        default_page: 100,
        minimum: 1,
        maximum: 100,
    },
];

#[test]
fn the_documented_default_page_of_every_read_is_inside_its_modelled_bounds() {
    for read in PAGINATED_READS {
        let case = format!("{} with no maxResults", read.operation);
        let limit = page_limit(None, read.default_page, read.minimum, read.maximum);
        assert_eq!(
            limit, read.default_page,
            "case {case:?}: expected the documented default page of {}, got {limit}",
            read.default_page
        );
        assert!(
            limit >= read.minimum && limit <= read.maximum,
            "case {case:?}: expected a size within {}..={}, got {limit}",
            read.minimum,
            read.maximum
        );
    }
}

#[test]
fn a_size_inside_the_modelled_bounds_is_the_size_the_read_walks_with() {
    for read in PAGINATED_READS {
        // Every size the member admits, its published bounds included: a size
        // the surface states is served as asked, not narrowed or widened.
        for requested in read.minimum..=read.maximum {
            let case = format!("{} with maxResults {requested}", read.operation);
            let limit = page_limit(
                Some(i32::try_from(requested).expect("a modelled bound is an Integer")),
                read.default_page,
                read.minimum,
                read.maximum,
            );
            assert_eq!(
                limit, requested,
                "case {case:?}: expected the requested size {requested}, got {limit}"
            );
        }
    }
}

#[test]
fn a_size_outside_the_modelled_bounds_is_held_at_the_bound_it_passed() {
    for read in PAGINATED_READS {
        let below = i32::try_from(read.minimum).expect("a modelled bound is an Integer") - 1;
        let above = i32::try_from(read.maximum).expect("a modelled bound is an Integer") + 1;
        // The rows are the sizes no request may name: one past each published
        // bound, far past the upper one, and two that are no page size at all.
        // A size past a bound is held at that bound — never at zero, which
        // would answer an empty page and lose the walk — and a value that names
        // no size at all leaves the documented default in place.
        let rows: &[(&str, i32, usize)] = &[
            ("one below the lower bound", below, read.minimum),
            ("one above the upper bound", above, read.maximum),
            ("far above the upper bound", i32::MAX, read.maximum),
            ("a negative size", -1, read.default_page),
            ("the smallest Integer", i32::MIN, read.default_page),
        ];
        for &(row, requested, expected) in rows {
            let case = format!("{} with {row} ({requested})", read.operation);
            let limit = page_limit(
                Some(requested),
                read.default_page,
                read.minimum,
                read.maximum,
            );
            assert_eq!(
                limit, expected,
                "case {case:?}: expected a page size of {expected}, got {limit}"
            );
            assert!(
                limit >= read.minimum && limit <= read.maximum,
                "case {case:?}: expected a size within {}..={}, got {limit}",
                read.minimum,
                read.maximum
            );
        }
    }
}

/// One router over ONE shared engine — what `core/router` builds in production,
/// so every call in a test observes the same state.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    {
        let state = engine
            .state::<service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState>(
            );
        let mut state = state.lock();
        for feature in ALL_FEATURES {
            state.grant_feature_access(feature);
        }
    }
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one awsJson1_0 call and return its HTTP status and decoded body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSBilling.{operation}"))
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/billing/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// The `__type` an awsJson error body carries, without its namespace prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The keys a `GetBillingPreferences` body reports, in the order it reported them.
fn reported_keys(body: &Value) -> Vec<String> {
    body.get("billingPreferences")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    entry
                        .get("key")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Store `count` preference keys under `feature` and answer them in the order a
/// read reports them (the store is key-ordered).
async fn store_keys(router: &MultiServiceRouter, feature: &str, count: usize) -> Vec<String> {
    let keys: Vec<String> = (0..count).map(|index| format!("key/{index:03}")).collect();
    if keys.is_empty() {
        return keys;
    }
    let pairs: Vec<Value> = keys
        .iter()
        .map(|key| json!({ "key": key, "value": "ENABLED" }))
        .collect();
    let (status, body) = call(
        router,
        "UpdateBillingPreferences",
        &json!({ "feature": feature, "billingPreferencesPerKey": pairs }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: expected {count} keys of {feature:?} to be stored, got {status} {body:?}"
    );
    keys
}

#[tokio::test]
async fn a_walk_at_any_size_the_surface_admits_reports_every_stored_preference_once() {
    // The rows differ in the page size (the published bounds of the modelled
    // `maxResults` range and sizes inside it) and in how the stored count sits
    // against that size: under it, exactly it, one over and several pages of it.
    // Every page a walk takes carries at least one entry while entries remain
    // and at most the size asked for, so nothing stored is unreachable.
    let rows: &[(usize, usize)] = &[
        (1, 1),
        (1, 4),
        (2, 1),
        (2, 2),
        (2, 5),
        (3, 7),
        (7, 3),
        (49, 51),
        (50, 50),
        (50, 51),
    ];
    for &(page_size, stored_count) in rows {
        let case = format!("maxResults {page_size} over {stored_count} stored keys");
        let router = emulator();
        let expected = store_keys(&router, "BILLING_ALERTS", stored_count).await;

        let mut walked: Vec<String> = Vec::new();
        let mut request = json!({ "features": ["BILLING_ALERTS"], "maxResults": page_size });
        for taken in 0..MAX_PAGES {
            let (status, body) = call(&router, "GetBillingPreferences", &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected page {taken} to succeed, got {status} {body:?}"
            );
            let page = reported_keys(&body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: page {taken} must carry at most {page_size} entries, got {}",
                page.len()
            );
            let remaining = expected.len() - walked.len();
            assert_eq!(
                page.len(),
                remaining.min(page_size),
                "case {case:?}: page {taken} must carry every remaining entry up to {page_size}, got {page:?}"
            );
            walked.extend(page);
            let token = body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                token.is_some(),
                walked.len() < expected.len(),
                "case {case:?}: a nextToken is carried exactly when entries remain past page {taken}, got {token:?}"
            );
            match token {
                Some(token) => request["nextToken"] = json!(token),
                None => break,
            }
            assert!(
                taken + 1 < MAX_PAGES,
                "case {case:?}: the walk must terminate, got {MAX_PAGES} pages of {walked:?}"
            );
        }
        assert_eq!(
            walked, expected,
            "case {case:?}: expected every stored key exactly once in key order, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn only_the_sizes_the_modelled_range_admits_reach_the_walk() {
    let router = emulator();
    let stored = store_keys(&router, "CREDIT_SHARING", 3).await;

    // `maxResults` of `GetBillingPreferences` is modelled `@range(min: 1,
    // max: 50)`: both bounds are served, and one past each — the sizes the
    // handler would otherwise have to hold at a bound — is refused as a bad
    // request, so the two layers agree on what a page size may be.
    let rows: &[(&str, i64, u16)] = &[
        ("the lower bound", 1, 200),
        ("inside the range", 2, 200),
        ("the upper bound", 50, 200),
        ("one below the lower bound", 0, 400),
        ("one above the upper bound", 51, 400),
        ("far above the upper bound", i64::from(i32::MAX), 400),
        ("a negative size", -1, 400),
    ];
    for &(case, max_results, expected_status) in rows {
        let (status, body) = call(
            &router,
            "GetBillingPreferences",
            &json!({ "features": ["CREDIT_SHARING"], "maxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for maxResults {max_results}, got {status} {body:?}"
        );
        if expected_status == 200 {
            let page = reported_keys(&body);
            let expected_len = stored.len().min(usize::try_from(max_results).unwrap_or(0));
            assert_eq!(
                page.len(),
                expected_len,
                "case {case:?}: expected a page of {expected_len} of {} stored keys, got {page:?}",
                stored.len()
            );
            assert!(
                !page.is_empty(),
                "case {case:?}: entries are stored, so the page carries at least one, got {page:?}"
            );
        } else {
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {body:?}"
            );
        }
    }
}
