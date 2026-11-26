// SPDX-License-Identifier: CC0-1.0

//! `ListAPIKeys`' `ApplicationIntegrationURL`, end to end over the wire.
//!
//! "The CAPTCHA application integration URL, for use in your JavaScript
//! implementation" (`ListAPIKeysResponse$ApplicationIntegrationURL`) is where a
//! client loads the CAPTCHA script from. The RULE asserted here: WAF hosts that
//! integration per ACCOUNT, so the listing reports its URL on every call — whatever
//! keys the scope holds, and whatever page of them the call asked for — as an `https`
//! URL at the address this emulator answers on, whose single path segment is the
//! account's integration id. A client reads the URL from this listing BEFORE it has a
//! key to use with it, so an empty listing carries it too.
//!
//! A URL an operation hands a caller to fetch has to lead back to this emulator, so
//! the base is the emulator's own endpoint and not the public AWS host AWS itself
//! serves the integration from; the part of WAF's published form that is kept is the
//! account's integration id as the path segment and the trailing slash.
//!
//! The id itself is the account's CAPTCHA integration id, which WAF publishes as a
//! TWELVE-character lowercase hex value — the width a client that splits it out of
//! the URL, or recovers the id from it, reads — so the width is asserted alongside
//! the form on every row.
//!
//! Each suite asserts that over a TABLE spanning the class: both scopes (and so both
//! endpoint regions), a scope with no keys, with one and with several, and every way
//! a call can ask for a page — no `Limit`, a `Limit` at each published bound, a
//! marker from a previous page, and a marker past the end.

use serde_json::{json, Value};
use service_com_amazonaws_wafv2_awswaf_20190729::state::{SCOPE_CLOUDFRONT, SCOPE_REGIONAL};
use smithy_server_runtime::UNSERVED_URL_BASE;

use crate::web_acl_lifecycle::{Waf, CLOUDFRONT_ENDPOINT_REGION, TEST_REGION};

/// How many lowercase hex characters the integration id WAF publishes the CAPTCHA
/// integration under carries: the recorded `ApplicationIntegrationURL` carries a
/// six-byte id rendered as hex. `OutputUrl` is an unconstrained string
/// (`smithy_query(query="shape",shape="OutputUrl")`), so this width is the recorded
/// one and is spelled out here independently of the service's own constant.
const INTEGRATION_ID_LENGTH: usize = 12;

/// The bounds `PaginationLimit` allows
/// (`smithy_query(query="shape",shape="PaginationLimit")` → `range 1..100`).
const MIN_PAGE_LIMIT: i64 = 1;
/// See [`MIN_PAGE_LIMIT`].
const MAX_PAGE_LIMIT: i64 = 100;

/// The region the endpoint a call for `scope` goes to is in: the CLOUDFRONT scope is
/// served from `us-east-1` ("To work with CloudFront, you must also specify the
/// Region US East (N. Virginia)" — `Scope`'s own doc), every other from the test
/// region. Kept as a row label: the URL is the account's, so a call's own endpoint
/// region does not change it.
fn endpoint_region(scope: &str) -> &'static str {
    if scope == SCOPE_CLOUDFRONT {
        CLOUDFRONT_ENDPOINT_REGION
    } else {
        TEST_REGION
    }
}

/// One `ListAPIKeys` call, as a 200.
async fn list(waf: &Waf, scope: &str, limit: Option<i64>, marker: Option<&str>) -> Value {
    let mut request = json!({ "Scope": scope });
    if let Some(limit) = limit {
        request["Limit"] = json!(limit);
    }
    if let Some(marker) = marker {
        request["NextMarker"] = json!(marker);
    }
    waf.ok("ListAPIKeys", request).await
}

/// The `ApplicationIntegrationURL` a listing reported; it must be there.
fn integration_url(page: &Value, case: &str) -> String {
    page["ApplicationIntegrationURL"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report an ApplicationIntegrationURL, got {page}")
        })
        .to_string()
}

/// The integration id a URL of the published form carries, asserting that form: the
/// address this emulator answers on, then the account's integration id as the single
/// path segment, then the trailing `/`.
fn asserted_integration_id(url: &str, case: &str) -> String {
    let path = url.strip_prefix(UNSERVED_URL_BASE).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the integration URL must be published at the address this \
             emulator answers on ({UNSERVED_URL_BASE}), got {url:?}"
        )
    });
    let id = path
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the path must be the integration id and a trailing slash, \
                 got {path:?} in {url:?}"
            )
        });
    assert!(
        id.chars().all(|digit| matches!(digit, '0'..='9' | 'a'..='f')),
        "case {case:?}: the integration id must be lowercase hex, got {id:?}"
    );
    assert_eq!(
        id.chars().count(),
        INTEGRATION_ID_LENGTH,
        "case {case:?}: the integration id must be {INTEGRATION_ID_LENGTH} characters \
         wide, got {id:?} in {url:?}"
    );
    id.to_string()
}

/// Mint `count` keys in `scope`, each over its own domain.
async fn mint_keys(waf: &Waf, scope: &str, count: usize) {
    for index in 0..count {
        waf.ok(
            "CreateAPIKey",
            json!({ "Scope": scope, "TokenDomains": [format!("host{index}.example.com")] }),
        )
        .await;
    }
}

/// Every listing carries the integration URL, in the documented form, for every
/// scope and whatever the scope holds: no keys, one, or several.
#[tokio::test]
async fn every_listing_reports_the_integration_url() {
    for scope in [SCOPE_REGIONAL, SCOPE_CLOUDFRONT] {
        let region = endpoint_region(scope);
        for keys in [0_usize, 1, 4] {
            let case = format!("{scope} at {region} with {keys} key(s)");
            // A fresh service per row, so the row's key count is the scope's.
            let waf = Waf::new();
            mint_keys(&waf, scope, keys).await;

            let page = list(&waf, scope, None, None).await;
            assert_eq!(
                page["APIKeySummaries"].as_array().map(Vec::len),
                Some(keys),
                "case {case:?}: the listing must report the scope's keys, got {page}"
            );
            let url = integration_url(&page, &case);
            let id = asserted_integration_id(&url, &case);

            // The URL is the account's, not the listing's: the same value on every
            // call, and on the listing of the other scope's own endpoint region.
            let again = integration_url(&list(&waf, scope, None, None).await, &case);
            assert_eq!(
                again, url,
                "case {case:?}: the integration URL must be the same on every call, \
                 got {again:?} after {url:?}"
            );
            let other = if scope == SCOPE_REGIONAL {
                SCOPE_CLOUDFRONT
            } else {
                SCOPE_REGIONAL
            };
            let other_url = integration_url(&list(&waf, other, None, None).await, &case);
            let other_id = asserted_integration_id(&other_url, &case);
            assert_eq!(
                other_id, id,
                "case {case:?}: the integration id is the account's, so both scopes \
                 report it, got {other_id:?} against {id:?}"
            );
        }
    }
}

/// Paging does not take the integration URL away: every page of a listing carries
/// it, at each published `Limit` bound, after a marker, and on the empty page past
/// the end.
#[tokio::test]
async fn every_page_of_a_listing_reports_the_integration_url() {
    let waf = Waf::new();
    let scope = SCOPE_REGIONAL;
    mint_keys(&waf, scope, 3).await;
    let expected = integration_url(&list(&waf, scope, None, None).await, "the whole listing");

    // Walked to exhaustion one key at a time, then at the other published bound, so
    // a first page, a middle page and the terminal page are all rows.
    for limit in [MIN_PAGE_LIMIT, 2, MAX_PAGE_LIMIT] {
        let mut marker: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let case = format!("Limit {limit}, page {pages}");
            let page = list(&waf, scope, Some(limit), marker.as_deref()).await;
            assert_eq!(
                integration_url(&page, &case),
                expected,
                "case {case:?}: every page must carry the account's integration URL"
            );
            asserted_integration_id(&expected, &case);
            pages += 1;
            assert!(
                pages <= 5,
                "case {case:?}: the walk must terminate, got {page}"
            );
            match page["NextMarker"].as_str() {
                Some(next) if !next.is_empty() => marker = Some(next.to_string()),
                _ => break,
            }
        }
    }

    // A marker past every key names a position nothing follows: an empty page, which
    // still reports where the integration lives.
    let case = "a marker past the end";
    let past = list(&waf, scope, None, Some(&"9".repeat(20))).await;
    assert_eq!(
        past["APIKeySummaries"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a marker past the end must answer an empty page, got {past}"
    );
    assert_eq!(
        integration_url(&past, case),
        expected,
        "case {case:?}: an empty page must carry the account's integration URL"
    );
}
