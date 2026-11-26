// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ApiGatewayV2 `RouteResponse` lifecycle.
//!
//! The rules these cases assert:
//!
//! * **`CreateRouteResponse` persists a route response under the route that owns it and
//!   answers 201 with the `RouteResponseId` it minted**; `GetRouteResponse` reports that
//!   same record, member for member. Nothing is echoed from the request without going
//!   through the store, and a member the request left out is reported as carrying
//!   nothing rather than as a value invented for it.
//! * A route response's key identifies it WITHIN its route: a second create under the
//!   same key on the same route is a 400, while the same key on another route is a fresh
//!   route response, and an id is only resolvable through the route that owns it.
//! * A route response is a WebSocket-only resource
//!   (`RouteResponse$ModelSelectionExpression`: "Supported only for WebSocket APIs"), so
//!   a create against an api of another protocol type is refused 400 and stores nothing.
//!
//! The rows vary what the rules generalise over: which optional members the request
//! carries, and how many response models and response parameters it names (none, one,
//! several). The recorded WebSocket case that filed the issue — a `$default` route response
//! on a route of a WebSocket api — is one row. Which KEY a route response may carry at all
//! is the rule beside this one (`tests/all/route_response_key_support.rs`).

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, assert_minted_id, call, carries_nothing, emulator, error_code};
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::duplicate_identity::route_response_key_in_use_message;

/// The one `RouteResponseKey` the service serves: the crate's own constant, so the suite
/// tracks it rather than restating a literal.
const SUPPORTED_ROUTE_RESPONSE_KEY: &str =
    service_com_amazonaws_apigatewayv2_apigatewayv2::state::SUPPORTED_ROUTE_RESPONSE_KEY;

/// One row: the `CreateRouteResponse` request, and the members the stored route
/// response must report back.
struct Row {
    case: &'static str,
    request: Value,
    /// Members the answer — and the later `GetRouteResponse` — must carry.
    expected: fn() -> Vec<(&'static str, Value)>,
    /// Members the request named none of, so the stored record carries nothing for
    /// them: reported either absent or as an empty collection, never as a value the
    /// service made up.
    expect_empty: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "only the required member",
            request: json!({ "routeResponseKey": "$default" }),
            expected: || vec![("routeResponseKey", json!("$default"))],
            expect_empty: &[
                "modelSelectionExpression",
                "responseModels",
                "responseParameters",
            ],
        },
        Row {
            case: "a model selection expression",
            request: json!({
                "routeResponseKey": "$default",
                "modelSelectionExpression": "$request.body.action"
            }),
            expected: || {
                vec![
                    ("routeResponseKey", json!("$default")),
                    ("modelSelectionExpression", json!("$request.body.action")),
                ]
            },
            expect_empty: &["responseModels", "responseParameters"],
        },
        Row {
            case: "one response model",
            request: json!({
                "routeResponseKey": "$default",
                "responseModels": { "$default": "Empty" }
            }),
            expected: || {
                vec![
                    ("routeResponseKey", json!("$default")),
                    ("responseModels", json!({ "$default": "Empty" })),
                ]
            },
            expect_empty: &["modelSelectionExpression", "responseParameters"],
        },
        Row {
            case: "several response models and response parameters",
            request: json!({
                "routeResponseKey": "$default",
                "modelSelectionExpression": "$request.body.kind",
                "responseModels": { "$default": "Empty", "application/json": "Reply" },
                "responseParameters": {
                    "route.response.header.x-trace": { "required": true },
                    "route.response.header.x-hint": { "required": false }
                }
            }),
            expected: || {
                vec![
                    ("routeResponseKey", json!("$default")),
                    ("modelSelectionExpression", json!("$request.body.kind")),
                    (
                        "responseModels",
                        json!({ "$default": "Empty", "application/json": "Reply" }),
                    ),
                    (
                        "responseParameters",
                        json!({
                            "route.response.header.x-trace": { "required": true },
                            "route.response.header.x-hint": { "required": false }
                        }),
                    ),
                ]
            },
            expect_empty: &[],
        },
        Row {
            case: "explicitly empty collections, which store nothing",
            request: json!({
                "routeResponseKey": "$default",
                "responseModels": {},
                "responseParameters": {}
            }),
            expected: || vec![("routeResponseKey", json!("$default"))],
            expect_empty: &[
                "modelSelectionExpression",
                "responseModels",
                "responseParameters",
            ],
        },
    ]
}

/// A WebSocket api, the protocol whose routes carry route responses.
fn websocket_api(name: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": "$request.body.action"
    })
}

/// Create one api and answer its `ApiId`.
async fn create_api(emulator: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(request.clone())).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
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

#[tokio::test]
async fn create_route_response_persists_the_record_get_route_response_reports_back() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the owning api", &websocket_api("route-response-host")).await;

    let mut minted = BTreeSet::new();
    for (index, row) in rows().into_iter().enumerate() {
        // One route per row: a route carries at most one route response, because the one
        // key the service serves can only be taken once on it.
        let route_id =
            create_route(&emulator, row.case, &api_id, &format!("sendMessage{index}")).await;
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            Some(row.request.clone()),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: expected 201 from CreateRouteResponse, got {status} {created}",
            row.case
        );

        let route_response_id = as_str(&created, "routeResponseId");
        assert_minted_id(row.case, "RouteResponseId", &route_response_id);
        assert!(
            minted.insert(route_response_id.clone()),
            "case {:?}: RouteResponseId {route_response_id:?} was minted twice",
            row.case
        );

        for (member, value) in (row.expected)() {
            assert_eq!(
                created.get(member),
                Some(&value),
                "case {:?}: expected CreateRouteResponse to report {member}={value}, got {created}",
                row.case
            );
        }
        for member in row.expect_empty {
            assert!(
                carries_nothing(&created, member),
                "case {:?}: the request named no {member}, so it must carry nothing, got {created}",
                row.case
            );
        }

        // The same record, read back through the route that owns it.
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!(
                "/v2/apis/{api_id}/routes/{route_id}/routeresponses/{route_response_id}"
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetRouteResponse, got {status} {fetched}",
            row.case
        );
        assert_eq!(
            fetched, created,
            "case {:?}: GetRouteResponse must report the record CreateRouteResponse stored",
            row.case
        );
    }
}

#[tokio::test]
async fn a_route_response_belongs_to_the_one_route_that_owns_it() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the owning api", &websocket_api("owner-ws-api")).await;
    let other_api = create_api(&emulator, "another api", &websocket_api("other-ws-api")).await;
    let route_id = create_route(&emulator, "the owning route", &api_id, "$connect").await;
    let other_route = create_route(&emulator, "another route", &api_id, "sendMessage").await;

    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
        Some(json!({ "routeResponseKey": "$default" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateRouteResponse, got {status} {created}"
    );
    let route_response_id = as_str(&created, "routeResponseId");

    // The key identifies the route response within its route, so the same route cannot
    // carry a second response under it.
    let (status, conflict) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
        Some(json!({ "routeResponseKey": "$default" })),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected 400 from a second route response under one key, got {status} {conflict}"
    );
    assert_eq!(
        error_code(&conflict),
        "BadRequestException",
        "expected BadRequestException, got {conflict}"
    );
    assert_eq!(
        as_str(&conflict, "message"),
        route_response_key_in_use_message("$default"),
        "expected the refusal to name the key that collided, got {conflict}"
    );

    // Neither another route of the same api nor another api resolves that id.
    for (case, uri) in [
        (
            "another route of the same api",
            format!("/v2/apis/{api_id}/routes/{other_route}/routeresponses/{route_response_id}"),
        ),
        (
            "another api",
            format!("/v2/apis/{other_api}/routes/{route_id}/routeresponses/{route_response_id}"),
        ),
        (
            "a route response id nothing minted",
            format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses/nosuchresp0"),
        ),
        (
            "a route id nothing minted",
            format!("/v2/apis/{api_id}/routes/nosuchroute/routeresponses/{route_response_id}"),
        ),
        (
            "an api id nothing minted",
            format!("/v2/apis/nosuchapi00/routes/{route_id}/routeresponses/{route_response_id}"),
        ),
    ] {
        let (status, body) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from GetRouteResponse, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
    }

    // The key is free on another route: that create succeeds, and each route reports its
    // OWN response.
    let (status, sibling) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes/{other_route}/routeresponses"),
        Some(json!({ "routeResponseKey": "$default", "modelSelectionExpression": "$default" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 for the same key on another route, got {status} {sibling}"
    );
    let sibling_id = as_str(&sibling, "routeResponseId");
    let (status, fetched) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{other_route}/routeresponses/{sibling_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetRouteResponse on the sibling route, got {status} {fetched}"
    );
    assert_eq!(
        fetched, sibling,
        "the sibling route must report its own stored route response"
    );
    let (status, owner) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses/{route_response_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetRouteResponse on the owning route, got {status} {owner}"
    );
    assert_eq!(
        owner, created,
        "the owning route's route response must be unchanged by the sibling create, got {owner}"
    );

    // Creating under a route or an api that does not exist creates nothing.
    for (case, uri) in [
        (
            "a route id nothing minted",
            format!("/v2/apis/{api_id}/routes/nosuchroute/routeresponses"),
        ),
        (
            "an api id nothing minted",
            format!("/v2/apis/nosuchapi00/routes/{route_id}/routeresponses"),
        ),
    ] {
        let (status, body) = call(
            &emulator,
            "POST",
            &uri,
            Some(json!({ "routeResponseKey": "$default" })),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from CreateRouteResponse, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
    }
}

#[tokio::test]
async fn a_route_response_is_refused_on_an_api_that_is_not_a_websocket_api() {
    let emulator = emulator();
    let api_id = create_api(
        &emulator,
        "an http api",
        &json!({ "name": "http-no-route-responses", "protocolType": "HTTP" }),
    )
    .await;
    let route_id = create_route(&emulator, "a route of an http api", &api_id, "GET /items").await;

    for (case, request) in [
        (
            "the $default key",
            json!({ "routeResponseKey": "$default" }),
        ),
        (
            "a key with response models",
            json!({ "routeResponseKey": "$default", "responseModels": { "$default": "Empty" } }),
        ),
    ] {
        let (status, body) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            Some(request),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateRouteResponse on an HTTP api, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
    }

    // A refused create stored nothing: the route holds no route response to read.
    let (status, body) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses/nosuchresp0"),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "expected the route to hold no route response, got {status} {body}"
    );
}

#[tokio::test]
async fn a_route_holds_at_most_one_route_response() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the owning api", &websocket_api("bounded-ws-api")).await;
    let route_id = create_route(&emulator, "the owning route", &api_id, "$connect").await;
    let uri = format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses");

    // The route's one route response: the service serves one key, so this takes it.
    let (status, created) = call(
        &emulator,
        "POST",
        &uri,
        Some(json!({ "routeResponseKey": SUPPORTED_ROUTE_RESPONSE_KEY })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 for the route's first route response, got {status} {created}"
    );
    let held = as_str(&created, "routeResponseId");

    // Every further create on that route is refused, so no loop of creates grows the
    // store, and the response it already holds is untouched.
    for attempt in 0..3 {
        let (status, body) = call(
            &emulator,
            "POST",
            &uri,
            Some(json!({ "routeResponseKey": SUPPORTED_ROUTE_RESPONSE_KEY })),
        )
        .await;
        assert_eq!(
            status, 400,
            "attempt {attempt}: expected 400 for a second route response on one route, got \
             {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "attempt {attempt}: expected BadRequestException, got {body}"
        );
    }
    let (status, listed) = call(&emulator, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "expected 200 from GetRouteResponses, got {status} {listed}"
    );
    let reported: Vec<String> = listed["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| as_str(item, "routeResponseId"))
        .collect();
    assert_eq!(
        reported,
        vec![held],
        "expected the route to hold its one route response, got {listed}"
    );

    // Another route of the same api is unaffected: the key is scoped to its route.
    let sibling = create_route(&emulator, "another route", &api_id, "sendMessage").await;
    let (status, body) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes/{sibling}/routeresponses"),
        Some(json!({ "routeResponseKey": SUPPORTED_ROUTE_RESPONSE_KEY })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 on a route of its own, got {status} {body}"
    );
}
