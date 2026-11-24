// SPDX-License-Identifier: CC0-1.0

//! The ENVELOPE `SearchAssets` answers in: the count of matching assets it reports beside the
//! page of hits, and the media type the response carries.
//!
//! RULE: the asset search answers 200 with a `TotalCount` — how many assets the request
//! MATCHED, the whole result set the reported page was cut from rather than the page's own
//! size, present on every answer and 0 when nothing matched — and it is served in the media
//! type of the REST route the model binds it to (`@http POST /search-assets`,
//! `smithy_query(query="operation",operation="SearchAssets")`), `application/json`, rather
//! than in this service's awsJson1_1 media type, which every operation the model binds to no
//! route keeps.
//!
//! The rows differ along what the rule generalises over: how many assets are stored, how many
//! of them the request selects (none, one, some, all), what selects them (free text or a
//! filter clause), and how a page size divides the hits — the page the answer carries and the
//! total it reports come apart exactly there.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_search::{given_asset_type, put_asset, Staged, ASSET_TYPE};
use crate::wire::{call_reported, emulator, REGION};

/// The media type the asset search's answer carries. RECORDED: the rest of this service
/// answers in its protocol's own ([`PROTOCOL_MEDIA_TYPE`]).
const SEARCH_MEDIA_TYPE: &str = "application/json";

/// The media type the awsJson1_1 protocol of this service answers in, which every operation
/// the model binds to no REST route carries.
const PROTOCOL_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The assets every suite here searches over: three of them, each carrying a different term
/// in its name, so a request can select none, one, or all three.
fn corpus() -> Vec<Staged<'static>> {
    vec![
        Staged {
            id: "orders-raw",
            name: "Orders Raw",
            description: Some("Raw order events"),
            content: Some(r#"{"owner":"finance"}"#),
        },
        Staged {
            id: "orders-curated",
            name: "Orders Curated",
            description: Some("Curated orders"),
            content: Some(r#"{"owner":"finance"}"#),
        },
        Staged {
            id: "customers",
            name: "Customers",
            description: Some("Customer master records"),
            content: Some(r#"{"owner":"crm"}"#),
        },
    ]
}

/// An emulator holding [`corpus`].
async fn emulator_with_assets() -> MultiServiceRouter {
    let router = emulator();
    let form_type = given_asset_type(&router).await;
    for asset in corpus() {
        put_asset(&router, &form_type, &asset).await;
    }
    router
}

/// One answer to a search: its status, the media type it was served in, and its body.
async fn search(router: &MultiServiceRouter, request: &Value) -> (u16, String, Value) {
    let (status, headers, body) = call_reported(router, REGION, "SearchAssets", request).await;
    let media_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<no content-type>")
        .to_string();
    (status, media_type, body)
}

/// How many hits an answer's page carries.
fn page_size(case: &str, body: &Value) -> usize {
    body.get("Items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: every answer must report Items, got {body}"))
        .len()
}

/// The `TotalCount` an answer reports, which must be a number on every answer.
fn total_count(case: &str, body: &Value) -> i64 {
    body.get("TotalCount")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: every answer must report a numeric TotalCount, got {body}")
        })
}

/// A search reports how many assets it matched, whatever the request selected and whatever
/// the page it was answered with holds.
#[tokio::test]
async fn a_search_reports_the_number_of_assets_it_matched() {
    let router = emulator_with_assets().await;

    // (case, the search request, the hits it must report in total)
    let rows: Vec<(&str, Value, i64)> = vec![
        (
            "a term no asset carries",
            json!({ "SearchText": "warehouses" }),
            0,
        ),
        (
            "a term one asset carries",
            json!({ "SearchText": "customers" }),
            1,
        ),
        (
            "a term two assets carry",
            json!({ "SearchText": "orders" }),
            2,
        ),
        (
            "a filter every asset satisfies",
            json!({
                "FilterClause": {
                    "AttributeFilter": {
                        "Attribute": "AssetTypeId",
                        "Operator": "equals",
                        "Value": { "StringValue": ASSET_TYPE },
                    },
                },
            }),
            3,
        ),
        (
            "a filter no asset satisfies",
            json!({
                "FilterClause": {
                    "AttributeFilter": {
                        "Attribute": "Name",
                        "Operator": "equals",
                        "Value": { "StringValue": "Nothing Stored" },
                    },
                },
            }),
            0,
        ),
    ];

    for (case, request, expected) in &rows {
        let (status, media_type, body) = search(&router, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the search must answer 200, got {status} with {body}"
        );
        assert_eq!(
            total_count(case, &body),
            *expected,
            "case {case:?}: expected TotalCount {expected}, got {body}"
        );
        assert_eq!(
            page_size(case, &body) as i64,
            *expected,
            "case {case:?}: with no page size asked for, every hit is on the page, got {body}"
        );
        assert_eq!(
            media_type, SEARCH_MEDIA_TYPE,
            "case {case:?}: expected the search to be served as {SEARCH_MEDIA_TYPE}, got \
             {media_type:?}"
        );
    }
}

/// The count is of the MATCHING assets and not of the page: a request that asks for a page
/// smaller than its result set is answered with that page and with the whole count, on every
/// page of the walk.
#[tokio::test]
async fn the_count_is_of_the_matching_assets_and_not_of_the_page() {
    let router = emulator_with_assets().await;

    // (case, the page size asked for, the page sizes the walk must report)
    let rows: Vec<(&str, i64, Vec<usize>)> = vec![
        ("one hit per page", 1, vec![1, 1, 1]),
        ("two of the three hits per page", 2, vec![2, 1]),
        ("a page as large as the result set", 3, vec![3]),
        ("a page larger than the result set", 10, vec![3]),
    ];

    for (case, size, expected_pages) in &rows {
        let mut token: Option<String> = None;
        let mut pages: Vec<usize> = Vec::new();
        loop {
            let mut request = json!({
                "FilterClause": {
                    "AttributeFilter": {
                        "Attribute": "AssetTypeId",
                        "Operator": "equals",
                        "Value": { "StringValue": ASSET_TYPE },
                    },
                },
                "MaxResults": size,
            });
            if let Some(cursor) = &token {
                request["NextToken"] = json!(cursor);
            }
            let (status, media_type, body) = search(&router, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the search must answer 200, got {status} with {body}"
            );
            assert_eq!(
                media_type, SEARCH_MEDIA_TYPE,
                "case {case:?}: expected {SEARCH_MEDIA_TYPE}, got {media_type:?}"
            );
            assert_eq!(
                total_count(case, &body),
                3,
                "case {case:?}: every page must report the whole count, got {body}"
            );
            pages.push(page_size(case, &body));
            token = body
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages.len() <= expected_pages.len(),
                "case {case:?}: the walk must terminate, got pages {pages:?}"
            );
        }
        assert_eq!(
            &pages, expected_pages,
            "case {case:?}: expected the pages {expected_pages:?}, got {pages:?}"
        );
    }
}

/// An account holding no asset at all answers the same envelope: a page with nothing on it
/// and a count of nothing, in the search's own media type.
#[tokio::test]
async fn a_search_of_an_empty_account_reports_no_hits_and_a_count_of_zero() {
    let router = emulator();

    let (status, media_type, body) = search(&router, &json!({ "SearchText": "anything" })).await;
    assert_eq!(
        status, 200,
        "an account holding nothing is searched, not refused, got {status} with {body}"
    );
    assert_eq!(
        total_count("no assets at all", &body),
        0,
        "expected a count of zero, got {body}"
    );
    assert_eq!(
        page_size("no assets at all", &body),
        0,
        "expected no hits, got {body}"
    );
    assert_eq!(
        media_type, SEARCH_MEDIA_TYPE,
        "expected {SEARCH_MEDIA_TYPE}, got {media_type:?}"
    );
}

/// The media type belongs to the OPERATION: the operations the model binds to no REST route
/// keep this service's protocol media type, so the search's own is not a service-wide change.
#[tokio::test]
async fn an_operation_the_model_binds_to_no_route_keeps_the_protocol_media_type() {
    let router = emulator_with_assets().await;

    // (case, the operation, the request). Each is answered by the generated routing table,
    // and none of them is bound to a REST route by the model
    // (`smithy_query(query="operation",operation="GetJob")`).
    let rows: Vec<(&str, &str, Value)> = vec![
        ("a job read that misses", "GetJob", json!({ "JobName": "no-such-job" })),
        ("a job listing", "GetJobs", json!({})),
        ("a usage profile listing", "ListUsageProfiles", json!({})),
    ];

    for (case, operation, request) in &rows {
        let (_, headers, body) = call_reported(&router, REGION, operation, request).await;
        let media_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<no content-type>");
        assert_eq!(
            media_type, PROTOCOL_MEDIA_TYPE,
            "case {case:?}: {operation} must keep the protocol media type, got \
             {media_type:?} with {body}"
        );
    }
}
