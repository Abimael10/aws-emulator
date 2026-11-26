// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for which `RouteResponseKey` a route response may carry, and for
//! how a stored route response is reported back.
//!
//! Two rules:
//!
//! * **The service serves exactly one route response key.** A route response is what makes
//!   a WebSocket route two-way ("To define a two-way route, create a route response"), and
//!   the only key API Gateway reads that from is `$default`, so a `CreateRouteResponse` or
//!   `UpdateRouteResponse` naming any other key is refused `BadRequestException` (400,
//!   `smithy_query(query="operation", operation="CreateRouteResponse")`) with the sentence
//!   that states which key is supported and what creating one under it means — and nothing
//!   is written, so the route's collection never comes to hold a response AWS would not
//!   have created.
//! * **A route response's collection members are reported only while they hold entries.**
//!   `ResponseModels` and `ResponseParameters` are neither `@required` nor defaulted
//!   (`smithy_query(query="shape", shape="RouteResponse")`), so a record holding no entries
//!   under one reports no such member at all rather than an empty map: an absent member and
//!   an empty one are different facts to a client that tests member presence to tell
//!   "unset" from "set empty".
//!
//! The rows span what the rules generalise over: the form of the key a caller can write (a
//! bare word, a route key of the api's own protocol, the other reserved `$` keys, a
//! case-shifted spelling, a prefix of the supported key, the empty string), which writer is
//! driven (the create and the patch), and which collection members a route response holds
//! entries under (neither, one, both, and one cleared by a patch). The recorded cases that
//! filed the issue — a `CreateRouteResponse` with key `custom`, and the `GetRouteResponses`
//! listing that followed it — are two of the rows.

use serde_json::{json, Value};
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::{
    unsupported_route_response_key_message, SUPPORTED_ROUTE_RESPONSE_KEY,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, carries_nothing, emulator, error_code};

/// Create one WebSocket api — the protocol whose routes carry route responses — and answer
/// its `ApiId`.
async fn create_api(emulator: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Create one route under `api_id` and answer its `RouteId`.
async fn create_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({ "routeKey": route_key })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
    );
    as_str(&created, "routeId")
}

/// The route responses a route holds, as `GetRouteResponses` reports them.
async fn list_route_responses(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_id: &str,
) -> Vec<Value> {
    let (status, listed) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetRouteResponses, got {status} {listed}"
    );
    listed["items"].as_array().cloned().unwrap_or_default()
}

/// The keys the service does NOT serve, spanning the forms a caller can write a selection
/// key in: a bare word, two route keys of the api's own protocol, the other reserved `$`
/// keys, a case-shifted spelling of the supported key, a value carrying it as a prefix, and
/// the empty string.
const UNSUPPORTED_KEYS: &[&str] = &[
    "custom",
    "echo",
    "sendMessage",
    "GET /items",
    "$connect",
    "$disconnect",
    "$DEFAULT",
    "$default-elsewhere",
    "",
];

#[tokio::test]
async fn create_route_response_serves_one_key_and_refuses_every_other() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "route-response-key-create").await;
    let route_id = create_route(&emulator, "the owning route", &api_id, "$connect").await;

    for key in UNSUPPORTED_KEYS {
        let (status, body) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            Some(json!({ "routeResponseKey": key })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {key:?}: expected 400 from CreateRouteResponse, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {key:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            unsupported_route_response_key_message(),
            "case {key:?}: expected the unsupported-key refusal, got {body}"
        );
        // Every refusal left the route's collection empty: no response AWS would have
        // refused is in the store for a later reader to enumerate.
        let listed = list_route_responses(&emulator, key, &api_id, &route_id).await;
        assert!(
            listed.is_empty(),
            "case {key:?}: a refused create must store no route response, got {listed:?}"
        );
    }

    // The one key the service serves is accepted on that same route, and the listing then
    // reports exactly it.
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
        Some(json!({ "routeResponseKey": SUPPORTED_ROUTE_RESPONSE_KEY })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 for the supported key, got {status} {created}"
    );
    assert_eq!(
        as_str(&created, "routeResponseKey"),
        SUPPORTED_ROUTE_RESPONSE_KEY,
        "expected the stored key to be the one the service serves, got {created}"
    );
    let listed = list_route_responses(&emulator, "the supported key", &api_id, &route_id).await;
    assert_eq!(
        listed.len(),
        1,
        "expected the route to hold its one route response, got {listed:?}"
    );
    assert_eq!(
        as_str(&listed[0], "routeResponseKey"),
        SUPPORTED_ROUTE_RESPONSE_KEY,
        "expected the listing to report the stored key, got {listed:?}"
    );
    assert_eq!(
        as_str(&listed[0], "routeResponseId"),
        as_str(&created, "routeResponseId"),
        "expected the listing to report the id the create minted, got {listed:?}"
    );
}

/// One row: the members a route response is created (and then patched) with, and the
/// collection members the record must report back afterwards.
struct CollectionRow {
    case: &'static str,
    /// Members sent beside the required key on the create.
    create: Value,
    /// A patch applied afterwards, where the row drives one.
    patch: Option<Value>,
    /// Collection members the record must report, with their entries.
    reported: fn() -> Vec<(&'static str, Value)>,
    /// Collection members the record must NOT carry at all, because it holds no entries
    /// under them.
    absent: &'static [&'static str],
}

fn collection_rows() -> Vec<CollectionRow> {
    vec![
        CollectionRow {
            case: "a route response the request named no collection entries for",
            create: json!({}),
            patch: None,
            reported: Vec::new,
            absent: &["responseModels", "responseParameters"],
        },
        CollectionRow {
            case: "collection members the request sent EMPTY",
            create: json!({ "responseModels": {}, "responseParameters": {} }),
            patch: None,
            reported: Vec::new,
            absent: &["responseModels", "responseParameters"],
        },
        CollectionRow {
            case: "one member holding entries, the other holding none",
            create: json!({ "responseModels": { "$default": "Empty" } }),
            patch: None,
            reported: || vec![("responseModels", json!({ "$default": "Empty" }))],
            absent: &["responseParameters"],
        },
        CollectionRow {
            case: "both members holding entries",
            create: json!({
                "responseModels": { "$default": "Empty", "application/json": "Reply" },
                "responseParameters": {
                    "route.response.header.x-trace": { "required": true }
                }
            }),
            patch: None,
            reported: || {
                vec![
                    (
                        "responseModels",
                        json!({ "$default": "Empty", "application/json": "Reply" }),
                    ),
                    (
                        "responseParameters",
                        json!({ "route.response.header.x-trace": { "required": true } }),
                    ),
                ]
            },
            absent: &[],
        },
        CollectionRow {
            case: "a member whose entries a patch cleared",
            create: json!({
                "responseModels": { "$default": "Empty" },
                "responseParameters": {
                    "route.response.header.x-trace": { "required": true }
                }
            }),
            patch: Some(json!({ "responseModels": {} })),
            reported: || {
                vec![(
                    "responseParameters",
                    json!({ "route.response.header.x-trace": { "required": true } }),
                )]
            },
            absent: &["responseModels"],
        },
    ]
}

#[tokio::test]
async fn a_route_response_reports_a_collection_member_only_while_it_holds_entries() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "route-response-collections").await;

    for (index, row) in collection_rows().into_iter().enumerate() {
        // One route per row: a route carries at most one route response, since the one key
        // the service serves can only be taken once on it.
        let route_id = create_route(&emulator, row.case, &api_id, &format!("row{index}")).await;
        let mut request = row.create.clone();
        request["routeResponseKey"] = json!(SUPPORTED_ROUTE_RESPONSE_KEY);
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            Some(request),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: expected 201 from CreateRouteResponse, got {status} {created}",
            row.case
        );
        let route_response_id = as_str(&created, "routeResponseId");
        let uri =
            format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses/{route_response_id}");

        let answered = match &row.patch {
            Some(patch) => {
                let (status, patched) = call(&emulator, "PATCH", &uri, Some(patch.clone())).await;
                assert_eq!(
                    status, 201,
                    "case {:?}: expected 201 from UpdateRouteResponse, got {status} {patched}",
                    row.case
                );
                patched
            }
            None => created,
        };

        // Every reader of the record projects it the same way: the write's own answer, the
        // single read, and the listing.
        let (status, fetched) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetRouteResponse, got {status} {fetched}",
            row.case
        );
        let listed = list_route_responses(&emulator, row.case, &api_id, &route_id).await;
        assert_eq!(
            listed.len(),
            1,
            "case {:?}: expected the route to hold one route response, got {listed:?}",
            row.case
        );
        for (reader, record) in [
            ("the write's answer", &answered),
            ("GetRouteResponse", &fetched),
            ("GetRouteResponses", &listed[0]),
        ] {
            for (member, entries) in (row.reported)() {
                assert_eq!(
                    record.get(member),
                    Some(&entries),
                    "case {:?}: expected {reader} to report {member}={entries}, got {record}",
                    row.case
                );
            }
            for member in row.absent {
                assert!(
                    record.get(*member).is_none(),
                    "case {:?}: the record holds no entries under {member}, so {reader} must \
                     not carry the member at all, got {record}",
                    row.case
                );
                // …and the one reading every suite asserts "carries nothing" through
                // agrees, so an absent member is not read as a value either.
                assert!(
                    carries_nothing(record, member),
                    "case {:?}: expected {reader} to carry nothing for {member}, got {record}",
                    row.case
                );
            }
        }
    }
}
