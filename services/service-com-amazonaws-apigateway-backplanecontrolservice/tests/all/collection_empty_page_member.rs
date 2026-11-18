// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the member that carries the PAGE of a `position`/`limit`
//! collection of this service when that page holds no element.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], `create_api` and `string_member` are
//! this crate's one wire-test harness, reused from `rest_api_lifecycle`.
//!
//! The rule, asserted over a SET of inputs rather than one call: a listing reports "the
//! current page of elements from this collection" under the envelope its OWN recording
//! shows — for every collection of this service but one, a page that holds nothing is a
//! success carrying that member as the EMPTY list, so a client counts the page it was served
//! rather than branching on a member that is not there. The base path mappings of a domain
//! name are the exception, and their own recorded empty answer carries no member at all
//! (`crate::emptied_collection_page_member::empty_page_body`, and the
//! `base_path_mapping_collection` suite that owns that listing).
//!
//! The rows span the collections of this service a caller fills and empties again,
//! crossed with the two ways a page comes to hold nothing while the listing still
//! resolves: the collection was EMPTIED (its last element deleted) and the request
//! NARROWED it to nothing (a filter that selects no element of a collection that holds
//! one). The sibling `emptied_collection_page_member` suite drives the emptied answer of
//! the account-level and domain-name collections, so the emptied rows here are the
//! collections it does not reach — the keys a usage plan meters, an API's request
//! validators and an API's documentation parts — while the narrowed rows are every
//! listing of this suite's table that publishes a filter. `collection_page_boundaries`
//! covers the third way, a collection that never held an element, and
//! `collection_page_envelope` the fourth, the window past the last element of a
//! collection that does hold elements. The last test here reads EVERY paginated listing of
//! this service once, so no collection is left out of the rule.

use serde_json::{json, Value};

use crate::emptied_collection_page_member::{deleted, empty_page_body};
use crate::rest_api_lifecycle::{create_api, string_member, Gateway};

/// The collections this suite drives, named by the operation that lists each: an
/// account-level collection keyed by a minted id, two whose elements hang off another
/// record, an API's children, and the one whose elements are addressed by a composite
/// location rather than an id.
const COLLECTIONS: &[&str] = &[
    "GetRestApis",
    "GetApiKeys",
    "GetUsagePlans",
    "GetUsagePlanKeys",
    "GetRequestValidators",
    "GetDocumentationParts",
    "GetClientCertificates",
];

/// The collections whose EMPTIED answer this suite asserts: the ones the sibling
/// `emptied_collection_page_member` table does not reach, so no row is driven twice.
const EMPTIED: &[&str] = &[
    "GetUsagePlanKeys",
    "GetRequestValidators",
    "GetDocumentationParts",
];

/// One collection stood up holding exactly ONE element.
struct Row {
    /// The URI the collection is listed through.
    listing: String,
    /// The URI that deletes the one element the collection holds.
    element: String,
    /// A query that narrows the listing to no element, for the collections whose listing
    /// publishes a filter; `None` for the ones that publish none.
    narrowing: Option<String>,
}

/// The published minimum length of a caller-supplied API key value is 20 characters, so a
/// key is created with none and takes the value the service mints.
///
/// A name, though, is what the `nameQuery` filters below select on — "the name of the
/// to-be-returned usage plan keys" / API keys — so every created key carries one.
const KEY_NAME: &str = "empty-page-key";

/// A name no element of any row carries, for the rows that narrow a listing by name.
///
/// `nameQuery` travels as the query parameter `name` — its `httpQuery` binding on both
/// `GetApiKeysRequest$nameQuery` and `GetUsagePlanKeysRequest$nameQuery`
/// (`smithy_query(query="operation", operation="GetApiKeys,GetUsagePlanKeys")`) — which
/// is the spelling a row narrows with.
const NO_SUCH_NAME: &str = "zzz-no-such-element";

/// Stand `collection` up on a fresh emulator holding exactly one element.
async fn stand_up(gateway: &Gateway, collection: &str) -> Row {
    match collection {
        "GetRestApis" => {
            let (api, _root) = create_api(gateway, "empty-page-api").await;
            Row {
                listing: "/restapis".to_string(),
                element: format!("/restapis/{api}"),
                // `GetRestApis` publishes no filter member: its input is `position` and
                // `limit` only.
                narrowing: None,
            }
        }
        "GetApiKeys" => {
            let key = created(
                gateway,
                "/apikeys",
                json!({ "name": KEY_NAME }),
                "CreateApiKey",
            )
            .await;
            let key_id = string_member(&key, "id", "CreateApiKey");
            Row {
                listing: "/apikeys".to_string(),
                element: format!("/apikeys/{key_id}"),
                narrowing: Some(format!("name={NO_SUCH_NAME}")),
            }
        }
        "GetUsagePlans" => {
            let plan = created(
                gateway,
                "/usageplans",
                json!({ "name": "empty-page-plan" }),
                "CreateUsagePlan",
            )
            .await;
            let plan_id = string_member(&plan, "id", "CreateUsagePlan");
            // `keyId` selects the plans that meter that API key, and this one is metered
            // by no plan at all.
            let key = created(
                gateway,
                "/apikeys",
                json!({ "name": KEY_NAME }),
                "CreateApiKey(unmetered)",
            )
            .await;
            let key_id = string_member(&key, "id", "CreateApiKey(unmetered)");
            Row {
                listing: "/usageplans".to_string(),
                element: format!("/usageplans/{plan_id}"),
                narrowing: Some(format!("keyId={key_id}")),
            }
        }
        "GetUsagePlanKeys" => {
            let plan = created(
                gateway,
                "/usageplans",
                json!({ "name": "empty-page-key-plan" }),
                "CreateUsagePlan",
            )
            .await;
            let plan_id = string_member(&plan, "id", "CreateUsagePlan");
            let key = created(
                gateway,
                "/apikeys",
                json!({ "name": KEY_NAME }),
                "CreateApiKey",
            )
            .await;
            let key_id = string_member(&key, "id", "CreateApiKey");
            created(
                gateway,
                &format!("/usageplans/{plan_id}/keys"),
                json!({ "keyId": key_id, "keyType": "API_KEY" }),
                "CreateUsagePlanKey",
            )
            .await;
            Row {
                listing: format!("/usageplans/{plan_id}/keys"),
                element: format!("/usageplans/{plan_id}/keys/{key_id}"),
                narrowing: Some(format!("name={NO_SUCH_NAME}")),
            }
        }
        "GetRequestValidators" => {
            let (api, _root) = create_api(gateway, "empty-page-validators").await;
            let validator = created(
                gateway,
                &format!("/restapis/{api}/requestvalidators"),
                json!({ "name": "empty-page-validator" }),
                "CreateRequestValidator",
            )
            .await;
            let validator_id = string_member(&validator, "id", "CreateRequestValidator");
            Row {
                listing: format!("/restapis/{api}/requestvalidators"),
                element: format!("/restapis/{api}/requestvalidators/{validator_id}"),
                // `GetRequestValidators` publishes no filter member either.
                narrowing: None,
            }
        }
        "GetDocumentationParts" => {
            let (api, _root) = create_api(gateway, "empty-page-parts").await;
            let part = created(
                gateway,
                &format!("/restapis/{api}/documentation/parts"),
                json!({
                    "location": {
                        "type": "QUERY_PARAMETER",
                        "path": "/widgets",
                        "method": "GET",
                        "name": "q",
                    },
                    "properties": json!({ "description": "the query parameter" }).to_string(),
                }),
                "CreateDocumentationPart",
            )
            .await;
            let part_id = string_member(&part, "id", "CreateDocumentationPart");
            Row {
                listing: format!("/restapis/{api}/documentation/parts"),
                element: format!("/restapis/{api}/documentation/parts/{part_id}"),
                // `type` selects the parts documenting one kind of API entity, and this
                // API's one part documents another kind.
                narrowing: Some("type=MODEL".to_string()),
            }
        }
        "GetClientCertificates" => {
            let certificate = created(
                gateway,
                "/clientcertificates",
                json!({ "description": "the empty-page certificate" }),
                "GenerateClientCertificate",
            )
            .await;
            let certificate_id = string_member(&certificate, "clientCertificateId", "Generate");
            Row {
                listing: "/clientcertificates".to_string(),
                element: format!("/clientcertificates/{certificate_id}"),
                narrowing: None,
            }
        }
        other => panic!("case {other}: this suite drives no such collection"),
    }
}

/// A create request, asserting the modelled 201 and answering the created record.
///
/// The wire call itself is the sibling suite's one spelling of a create
/// ([`crate::emptied_collection_page_member::created`]) — every create this suite makes
/// answers the modelled 201 — so the two suites cannot drift on what a create must answer.
async fn created(gateway: &Gateway, uri: &str, request: Value, case: &str) -> Value {
    crate::emptied_collection_page_member::created(gateway, uri, request, 201, case).await
}

/// A listing, asserting the 200 every readable collection answers.
async fn listed(gateway: &Gateway, uri: &str, case: &str) -> Value {
    let answer = gateway.call("GET", uri, None).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// Assert `body` reports a page of `count` element(s): the member is always there, and
/// carries exactly that many elements — the empty list for a page that holds nothing.
fn assert_page_of(body: &Value, count: usize, case: &str) {
    let items = body
        .get("item")
        .unwrap_or_else(|| panic!("case {case}: expected the page member, got {body:?}"));
    let items = items
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected the page to be a list, got {body:?}"));
    assert_eq!(
        items.len(),
        count,
        "case {case}: expected a page of {count} element(s), got {items:?}"
    );
}

#[tokio::test]
async fn a_collection_emptied_of_its_last_element_reports_the_empty_page_member() {
    for collection in EMPTIED {
        let gateway = Gateway::new();
        let row = stand_up(&gateway, collection).await;

        // The one element it holds is on the page, which the emptied listing reports as
        // the empty list.
        let case = format!("{collection}(one element)");
        let held = listed(&gateway, &row.listing, &case).await;
        assert_page_of(&held, 1, &case);

        deleted(&gateway, &row.element, &format!("Delete({collection})")).await;

        let case = format!("{collection}(emptied)");
        let emptied = listed(&gateway, &row.listing, &case).await;
        assert_page_of(&emptied, 0, &case);
        assert!(
            emptied["position"].is_null(),
            "case {case}: a page the collection holds nothing after reports no cursor, \
             got {:?}",
            emptied["position"]
        );
        assert!(
            emptied["limit"].is_null(),
            "case {case}: that page echoes no page size either, got {:?}",
            emptied["limit"]
        );
    }
}

#[tokio::test]
async fn a_listing_narrowed_to_no_element_reports_the_empty_page_member() {
    for collection in COLLECTIONS {
        let gateway = Gateway::new();
        let row = stand_up(&gateway, collection).await;
        let Some(narrowing) = row.narrowing else {
            continue;
        };

        let case = format!("{collection}({narrowing})");
        let narrowed = listed(
            &gateway,
            &format!("{}?{narrowing}", row.listing),
            &case,
        )
        .await;
        assert_page_of(&narrowed, 0, &case);
        assert!(
            narrowed["position"].is_null(),
            "case {case}: a narrowed page with nothing after it reports no cursor, got \
             {:?}",
            narrowed["position"]
        );

        // The element is still there: the narrowing emptied the PAGE, not the collection.
        let case = format!("{collection}(unnarrowed after {narrowing})");
        let held = listed(&gateway, &row.listing, &case).await;
        assert_page_of(&held, 1, &case);
    }
}

#[tokio::test]
async fn every_paginated_listing_of_this_service_reports_its_page_as_a_list() {
    // The rule holds for EVERY listing of this service, so the rows below are all of them,
    // read on one emulator holding one API, one usage plan and one domain name and nothing
    // else — which is how the rows differ along what the rule generalises over: a
    // collection nothing has been created in yet (most of them), one an API is created
    // already holding (its root resource, its two default models) and one holding the
    // element this emulator was stood up with (the API, the plan, the domain name).
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "page-member-api").await;
    let plan = created(
        &gateway,
        "/usageplans",
        json!({ "name": "page-member-plan" }),
        "CreateUsagePlan",
    )
    .await;
    let plan_id = string_member(&plan, "id", "CreateUsagePlan");
    let domain_name = "page-member.example.com";
    crate::domain_name_collection::created_domain(&gateway, domain_name, json!({})).await;

    // The third column is how many elements the row's collection holds on that emulator:
    // `Some(n)` where the count is exact — nothing was created in it (0), the API was
    // created holding it (its root resource, its two default models) or this emulator was
    // stood up with the one element (the API, the plan, the domain name) — and `None` for
    // the two catalogs the service publishes rather than a caller filling, which are
    // asserted non-empty because their contents are their own suites' rule.
    let rows: Vec<(&str, String, Option<usize>)> = vec![
        ("GetRestApis", "/restapis".to_string(), Some(1)),
        ("GetApiKeys", "/apikeys".to_string(), Some(0)),
        ("GetUsagePlans", "/usageplans".to_string(), Some(1)),
        (
            "GetUsagePlanKeys",
            format!("/usageplans/{plan_id}/keys"),
            Some(0),
        ),
        ("GetVpcLinks", "/vpclinks".to_string(), Some(0)),
        (
            "GetClientCertificates",
            "/clientcertificates".to_string(),
            Some(0),
        ),
        ("GetDomainNames", "/domainnames".to_string(), Some(1)),
        (
            "GetDomainNameAccessAssociations",
            "/domainnameaccessassociations".to_string(),
            Some(0),
        ),
        (
            "GetBasePathMappings",
            format!("/domainnames/{domain_name}/basepathmappings"),
            Some(0),
        ),
        (
            "GetAuthorizers",
            format!("/restapis/{api}/authorizers"),
            Some(0),
        ),
        (
            "GetRequestValidators",
            format!("/restapis/{api}/requestvalidators"),
            Some(0),
        ),
        (
            "GetDocumentationParts",
            format!("/restapis/{api}/documentation/parts"),
            Some(0),
        ),
        (
            "GetDocumentationVersions",
            format!("/restapis/{api}/documentation/versions"),
            Some(0),
        ),
        (
            "GetDeployments",
            format!("/restapis/{api}/deployments"),
            Some(0),
        ),
        ("GetStages", format!("/restapis/{api}/stages"), Some(0)),
        (
            "GetResources",
            format!("/restapis/{api}/resources"),
            Some(1),
        ),
        ("GetModels", format!("/restapis/{api}/models"), Some(2)),
        (
            "GetGatewayResponses",
            format!("/restapis/{api}/gatewayresponses"),
            None,
        ),
        ("GetSdkTypes", "/sdktypes".to_string(), None),
    ];

    for (case, uri, held) in &rows {
        let body = listed(&gateway, uri, case).await;
        // The one collection whose recorded empty answer carries NO member is asserted as
        // that whole envelope instead: a page of nothing is the bare success object there.
        if empty_page_body(case) == json!({}) && *held == Some(0) {
            assert_eq!(
                body,
                json!({}),
                "case {case}: expected the bare success envelope for an empty page, got \
                 {body:?}"
            );
            continue;
        }
        let reported = body
            .get("item")
            .unwrap_or_else(|| panic!("case {case}: expected the page member, got {body:?}"));
        let elements = reported
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: the page member must be a list, got {reported:?}"));
        match held {
            Some(count) => assert_eq!(
                elements.len(),
                *count,
                "case {case}: expected a page of {count} element(s), got {elements:?}"
            ),
            None => assert!(
                !elements.is_empty(),
                "case {case}: the catalog this service publishes is never empty, got \
                 {elements:?}"
            ),
        }
    }
}
