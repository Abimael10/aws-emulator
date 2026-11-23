// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a listing answers ABOUT THE COLLECTION a refused write would
//! have joined.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], `create_api`, `string_member`,
//! `page_items` and `assert_failure` are this crate's one wire-test harness, reused from
//! `rest_api_lifecycle`; a domain name comes through `domain_name_collection`, a deployed
//! stage through `base_path_mapping` and a VPC link's target through `vpc_links`, so every
//! element a row creates is created through its own API.
//!
//! The rule, asserted over a TABLE of collections rather than one listing: a refused write
//! stores nothing, so the listing of the collection it would have joined is a 200 reporting
//! that collection exactly as that listing reports a page holding no element — the page
//! member (`item` on the wire) PRESENT and empty for every collection of this service, and
//! the bare empty object for the base path mappings of one domain name, the one collection
//! of this service answered that way
//! ([`crate::emptied_collection_page_member::empty_page_body`], this crate's one reading of
//! an empty page). A client therefore reads "the refusal created nothing" off the listing
//! itself rather than off an absent member.
//!
//! This is the way a page comes to hold nothing that the sibling suites do not drive: the
//! collection a REFUSAL left untouched, beside the collection emptied of its last element
//! (`emptied_collection_page_member`), the listing narrowed by a filter
//! (`collection_empty_page_member`), the collection nothing was ever created in
//! (`collection_page_boundaries`) and the window past the last element
//! (`collection_page_envelope`). The rows differ along the class of the refusal as well — a
//! value outside a modelled enum, a list member sent empty, a value under its published
//! minimum length, an identifier naming no stored record, a body that is not the document an
//! import reads, and a patch of a record that does not exist — and along where the collection
//! hangs: the account's own, an API's children, the keys a usage plan meters, and a domain
//! name's mappings.
//!
//! The second test is every row's positive control: the same write, made well formed, is
//! reported by the very same listing as a page of ONE, so the empty answer above is what the
//! store held and not an answer the listing always makes.

use serde_json::{json, Value};

use crate::authorizer_lifecycle::lambda_uri;
use crate::base_path_mapping::deployed_api;
use crate::domain_name_collection::created_domain;
use crate::emptied_collection_page_member::empty_page_body;
use crate::rest_api_lifecycle::{
    assert_failure, create_api, page_items, string_member, Gateway,
};
use crate::vpc_links::target_arn;

/// The collections this suite drives, named by the operation that lists each.
const COLLECTIONS: &[&str] = &[
    "GetAuthorizers",
    "GetRequestValidators",
    "GetDocumentationParts",
    "GetUsagePlanKeys",
    "GetVpcLinks",
    "GetApiKeys",
    "GetUsagePlans",
    "GetDomainNameAccessAssociations",
    "GetBasePathMappings",
];

/// The error code a request the service could not serve as sent earns.
const BAD_REQUEST: &str = "BadRequestException";

/// The error code a request naming no stored record earns.
const NOT_FOUND: &str = "NotFoundException";

/// An identifier of no stored record, in the width this service mints its ids at.
const UNMINTED_ID: &str = "zzzzzzzzzz";

/// The name the API-key rows create and filter by: `nameQuery` travels as the query
/// parameter `name` ("The name of queried API keys",
/// `smithy_query(query="operation", operation="GetApiKeys")`), so the filtered read below is
/// the "was this key created?" read a client makes.
const KEY_NAME: &str = "refused-write-key";

/// The identity source a `TOKEN` authorizer is created with: "the header mapping expression
/// for the bearer token" (`smithy_query(query="doc", shape="CreateAuthorizerRequest",
/// member="identitySource")`).
const TOKEN_IDENTITY_SOURCE: &str = "method.request.header.Authorization";

/// The body of the refused write: a JSON structure, or a payload sent VERBATIM for the
/// import whose body is a document rather than a modelled structure.
enum Payload {
    Json(Value),
    Raw(&'static str),
}

/// One collection, the refused write into it, and the listing read afterwards.
struct Row {
    /// What the refused write is, for the case label.
    refused: &'static str,
    /// The refused request.
    method: &'static str,
    uri: String,
    payload: Payload,
    /// The modelled refusal it earns: HTTP status and `x-amzn-errortype` code.
    refusal: (u16, &'static str),
    /// Every spelling of the listing read afterwards: the bare collection first, then the
    /// published filters that also select nothing.
    listings: Vec<String>,
    /// The same write made WELL FORMED: the URI it is POSTed to, its body and the modelled
    /// success status — `None` for a collection whose element this suite cannot create
    /// without a second service mounted.
    accepted: Option<(String, Value, u16)>,
}

/// Stand `collection` up on a fresh emulator, holding nothing of its own.
async fn stand_up(gateway: &Gateway, collection: &str) -> Row {
    match collection {
        "GetAuthorizers" => {
            let (api, _root) = create_api(gateway, "refused-authorizer-api").await;
            let uri = format!("/restapis/{api}/authorizers");
            Row {
                refused: "CreateAuthorizer(a type outside the modelled enum)",
                method: "POST",
                uri: uri.clone(),
                payload: Payload::Json(json!({
                    "name": "refused-authorizer",
                    "type": "NOT_A_REAL_AUTHORIZER_TYPE",
                    "identitySource": TOKEN_IDENTITY_SOURCE,
                })),
                refusal: (400, BAD_REQUEST),
                listings: vec![uri.clone()],
                accepted: Some((
                    uri,
                    json!({
                        "name": "accepted-authorizer",
                        "type": "TOKEN",
                        "identitySource": TOKEN_IDENTITY_SOURCE,
                        "authorizerUri": lambda_uri("authorize"),
                    }),
                    201,
                )),
            }
        }
        "GetRequestValidators" => {
            let (api, _root) = create_api(gateway, "refused-validator-api").await;
            Row {
                refused: "UpdateRequestValidator(an id naming no stored validator)",
                method: "PATCH",
                uri: format!("/restapis/{api}/requestvalidators/{UNMINTED_ID}"),
                payload: Payload::Json(json!({
                    "patchOperations": [
                        { "op": "replace", "path": "/name", "value": "renamed" },
                    ],
                })),
                refusal: (404, NOT_FOUND),
                listings: vec![format!("/restapis/{api}/requestvalidators")],
                accepted: Some((
                    format!("/restapis/{api}/requestvalidators"),
                    json!({ "name": "accepted-validator" }),
                    201,
                )),
            }
        }
        "GetDocumentationParts" => {
            let (api, _root) = create_api(gateway, "refused-parts-api").await;
            let uri = format!("/restapis/{api}/documentation/parts");
            Row {
                refused: "ImportDocumentationParts(a body that is no documentation set)",
                method: "PUT",
                uri: uri.clone(),
                payload: Payload::Raw("not a documentation set"),
                refusal: (400, BAD_REQUEST),
                listings: vec![uri.clone()],
                accepted: Some((
                    uri,
                    json!({
                        "location": { "type": "METHOD", "path": "/pets", "method": "GET" },
                        "properties": json!({ "description": "list pets" }).to_string(),
                    }),
                    201,
                )),
            }
        }
        "GetUsagePlanKeys" => {
            let plan = created(
                gateway,
                "/usageplans",
                json!({ "name": "refused-key-plan" }),
                201,
                collection,
            )
            .await;
            let plan_id = string_member(&plan, "id", collection);
            let key = created(
                gateway,
                "/apikeys",
                json!({ "name": KEY_NAME }),
                201,
                collection,
            )
            .await;
            let key_id = string_member(&key, "id", collection);
            let uri = format!("/usageplans/{plan_id}/keys");
            Row {
                refused: "CreateUsagePlanKey(a keyId naming no stored api key)",
                method: "POST",
                uri: uri.clone(),
                payload: Payload::Json(json!({
                    "keyId": UNMINTED_ID,
                    "keyType": "API_KEY",
                })),
                refusal: (404, NOT_FOUND),
                listings: vec![uri.clone()],
                accepted: Some((
                    uri,
                    json!({ "keyId": key_id, "keyType": "API_KEY" }),
                    201,
                )),
            }
        }
        "GetVpcLinks" => Row {
            refused: "CreateVpcLink(targetArns naming no target)",
            method: "POST",
            uri: "/vpclinks".to_string(),
            payload: Payload::Json(json!({ "name": "refused-link", "targetArns": [] })),
            refusal: (400, BAD_REQUEST),
            listings: vec!["/vpclinks".to_string()],
            accepted: Some((
                "/vpclinks".to_string(),
                json!({ "name": "accepted-link", "targetArns": [target_arn(0)] }),
                // A VPC link is provisioned rather than ready on the create, which the
                // modelled 202 states.
                202,
            )),
        },
        "GetApiKeys" => Row {
            refused: "CreateApiKey(a value under its published minimum length)",
            method: "POST",
            uri: "/apikeys".to_string(),
            payload: Payload::Json(json!({ "name": KEY_NAME, "value": "too-short" })),
            refusal: (400, BAD_REQUEST),
            // The bare collection, and the filtered read a client makes to ask whether
            // THAT key was created.
            listings: vec![
                "/apikeys".to_string(),
                format!("/apikeys?name={KEY_NAME}"),
            ],
            accepted: Some((
                "/apikeys".to_string(),
                json!({ "name": KEY_NAME }),
                201,
            )),
        },
        "GetUsagePlans" => Row {
            refused: "CreateUsagePlan(an apiStages entry naming no stored api)",
            method: "POST",
            uri: "/usageplans".to_string(),
            payload: Payload::Json(json!({
                "name": "refused-plan",
                "apiStages": [{ "apiId": UNMINTED_ID, "stage": "prod" }],
            })),
            refusal: (404, NOT_FOUND),
            listings: vec!["/usageplans".to_string()],
            accepted: Some((
                "/usageplans".to_string(),
                json!({ "name": "accepted-plan" }),
                201,
            )),
        },
        "GetDomainNameAccessAssociations" => Row {
            refused: "CreateDomainNameAccessAssociation(a domainNameArn that is no ARN)",
            method: "POST",
            uri: "/domainnameaccessassociations".to_string(),
            payload: Payload::Json(json!({
                "domainNameArn": "not-an-arn",
                "accessAssociationSourceType": "VPCE",
                "accessAssociationSource": "vpce-0123456789abcdef0",
            })),
            refusal: (400, BAD_REQUEST),
            // Every value the modelled `ResourceOwner` enum declares
            // (`smithy_query(query="shape", shape="ResourceOwner")`: `SELF`,
            // `OTHER_ACCOUNTS`) beside the request that sets none, because the member is
            // optional and a client reads the collection either way.
            listings: vec![
                "/domainnameaccessassociations".to_string(),
                "/domainnameaccessassociations?resourceOwner=SELF".to_string(),
                "/domainnameaccessassociations?resourceOwner=OTHER_ACCOUNTS".to_string(),
            ],
            // An association's source must be a VPC endpoint another service holds, so the
            // accepted create of one is the `domain_name_access_associations` suite's,
            // which mounts that service.
            accepted: None,
        },
        "GetBasePathMappings" => {
            let (api, stage) = deployed_api(gateway, "refused-mapping-api", "prod").await;
            let domain_name = "refused-mapping.example.com";
            created_domain(gateway, domain_name, json!({})).await;
            let uri = format!("/domainnames/{domain_name}/basepathmappings");
            Row {
                refused: "CreateBasePathMapping(a restApiId naming no stored api)",
                method: "POST",
                uri: uri.clone(),
                payload: Payload::Json(json!({
                    "basePath": "v1",
                    "restApiId": UNMINTED_ID,
                    "stage": stage,
                })),
                refusal: (400, BAD_REQUEST),
                listings: vec![uri.clone()],
                accepted: Some((
                    uri,
                    json!({ "basePath": "v1", "restApiId": api, "stage": stage }),
                    201,
                )),
            }
        }
        other => panic!("case {other}: this suite drives no such collection"),
    }
}

/// A create request, asserting `expected` and answering the created record: this crate's one
/// spelling of a create of an element a listing then reports
/// ([`crate::emptied_collection_page_member::created`]).
async fn created(
    gateway: &Gateway,
    uri: &str,
    request: Value,
    expected: u16,
    case: &str,
) -> Value {
    crate::emptied_collection_page_member::created(gateway, uri, request, expected, case).await
}

/// Send the row's refused write and assert the modelled refusal it earns.
async fn refuse(gateway: &Gateway, row: &Row, case: &str) {
    let answer = match &row.payload {
        Payload::Json(body) => gateway.call(row.method, &row.uri, Some(body.clone())).await,
        Payload::Raw(body) => gateway.call_raw(row.method, &row.uri, body).await,
    };
    assert_failure(&answer, row.refusal.0, row.refusal.1, case);
}

/// Read `uri`, asserting the 200 every readable listing answers, and answer its body.
async fn listed(gateway: &Gateway, uri: &str, case: &str) -> Value {
    let answer = gateway.call("GET", uri, None).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

#[tokio::test]
async fn a_refused_write_leaves_the_listing_reporting_its_empty_page() {
    for collection in COLLECTIONS {
        let gateway = Gateway::new();
        let row = stand_up(&gateway, collection).await;
        refuse(&gateway, &row, &format!("{collection} / {}", row.refused)).await;

        // The refusal stored nothing, so the listing answers the whole body it answers for
        // a page holding no element — and nothing beside the page member, because a page of
        // nothing carries on at no cursor and echoes no page size.
        let expected = empty_page_body(collection);
        for listing in &row.listings {
            let case = format!("{collection}({listing}) after {}", row.refused);
            let body = listed(&gateway, listing, &case).await;
            assert_eq!(
                body, expected,
                "case {case}: expected {expected:?}, got {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_same_write_made_well_formed_is_reported_as_a_page_of_one() {
    for collection in COLLECTIONS {
        let gateway = Gateway::new();
        let row = stand_up(&gateway, collection).await;
        let Some((uri, body, expected_status)) = row.accepted.clone() else {
            continue;
        };
        refuse(&gateway, &row, &format!("{collection} / {}", row.refused)).await;

        let case = format!("{collection}(the well-formed write)");
        created(&gateway, &uri, body, expected_status, &case).await;

        // The same read that answered a page of nothing above now reports the one element
        // the accepted write stored, so the empty page was the STORE's answer.
        let listing = &row.listings[0];
        let read = listed(&gateway, listing, &case).await;
        let items = page_items(&read, &case);
        assert_eq!(
            items.len(),
            1,
            "case {case}: expected a page of 1 element from {listing}, got {items:?}"
        );
    }
}
