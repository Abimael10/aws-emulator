// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for which COLLECTION members a reported route carries.
//!
//! The rule these cases assert: **a route's collection members — `RequestModels`,
//! `RequestParameters` and `AuthorizationScopes`, none of which the model declares
//! `@required` or gives a `@default`
//! (`smithy_query(query="shape",shape="Route",model_path="services/service-com-amazonaws-apigatewayv2-apigatewayv2/smithy-model/com-amazonaws-apigatewayv2-apigatewayv2.json")`)
//! — are reported only while the route HOLDS entries under them; a route holding none
//! omits the member entirely instead of reporting an empty map or an empty list.** An
//! absent member and an empty one are different facts to a client that branches on a
//! member being present, so the member is dropped however the route came to hold nothing,
//! and in every answer that reports a route: `CreateRoute`, `GetRoute`, `GetRoutes` and
//! `UpdateRoute`.
//!
//! The rows vary what the rule generalises over: WHICH member is empty (a map of
//! parameter constraints, a map of model names, a list of scopes, and all three at once),
//! HOW it came to be empty (the create named it not at all, the create named it
//! explicitly empty, a patch replaced it with an empty collection, and every entry was
//! deleted through the request parameter's own sub-resource), how MANY entries were
//! removed to get there (one, two, three), and the api's protocol type (`HTTP` and
//! `WEBSOCKET`, which differ in which of these members a route may carry). The recorded
//! case that filed the issue — the last request parameter of a route deleted, and the
//! route read back — is one row.
//!
//! Each row also names the members that must STILL be reported, so the rule is about a
//! collection being empty and not about dropping these members from the representation.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator};

/// The members the rule covers: the route's collection members. Listed once, so a row
/// only has to say which of them it expects reported.
const COLLECTION_MEMBERS: &[&str] = &["requestModels", "requestParameters", "authorizationScopes"];

/// The members every route reports whatever its collections hold. They are asserted
/// present on every row, so a row that expects a collection member gone is distinguishing
/// "this member is absent" from "the answer lost its members".
const ALWAYS_REPORTED: &[&str] = &["routeId", "routeKey", "authorizationType", "apiKeyRequired"];

/// How a row's route comes to hold no entries under the members it expects absent.
enum EmptiedBy {
    /// Nothing further: the create itself left the collections holding nothing.
    TheCreateItself,
    /// One `UpdateRoute` patch, which names each collection as an empty collection.
    APatch(Value),
    /// One `DeleteRouteRequestParameter` per key the route's request-parameter map
    /// holds, which is how a map loses its LAST entry through the sub-resource that
    /// addresses the entries.
    DeletingEveryRequestParameter,
}

/// One row: the api's protocol, the route to create, how it is emptied, and what the
/// route must report afterwards.
struct Row {
    case: &'static str,
    /// One of `ProtocolType`; the api is created with it.
    protocol: &'static str,
    /// The `CreateRoute` request body.
    create: Value,
    emptied_by: EmptiedBy,
    /// Collection members the emptied route must report, with the value it must report.
    /// Every other member of [`COLLECTION_MEMBERS`] must be absent.
    still_reported: fn() -> Vec<(&'static str, Value)>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an HTTP route whose create named no collection member at all",
            protocol: "HTTP",
            create: json!({ "routeKey": "GET /pets" }),
            emptied_by: EmptiedBy::TheCreateItself,
            still_reported: Vec::new,
        },
        Row {
            case: "a WebSocket route whose create named every collection explicitly empty",
            protocol: "WEBSOCKET",
            create: json!({
                "routeKey": "sendmessage",
                "requestModels": {},
                "requestParameters": {},
                "authorizationScopes": []
            }),
            emptied_by: EmptiedBy::TheCreateItself,
            still_reported: Vec::new,
        },
        Row {
            // The recorded case: a route's one request parameter deleted through the
            // sub-resource that addresses it, and the route read back.
            case: "the only request parameter of a WebSocket connection route deleted",
            protocol: "WEBSOCKET",
            create: json!({
                "routeKey": "$connect",
                "requestParameters": {
                    "route.request.header.Authorization": { "required": true }
                },
                "requestModels": { "$default": "Empty" },
                "authorizationScopes": ["scope/kept"]
            }),
            emptied_by: EmptiedBy::DeletingEveryRequestParameter,
            still_reported: || {
                vec![
                    ("requestModels", json!({ "$default": "Empty" })),
                    ("authorizationScopes", json!(["scope/kept"])),
                ]
            },
        },
        Row {
            case: "all three request parameters of a WebSocket connection route deleted",
            protocol: "WEBSOCKET",
            create: json!({
                "routeKey": "$connect",
                "requestParameters": {
                    "route.request.body.action": { "required": true },
                    "route.request.header.X-Trace": { "required": false },
                    "route.request.querystring.token": {}
                }
            }),
            emptied_by: EmptiedBy::DeletingEveryRequestParameter,
            still_reported: Vec::new,
        },
        Row {
            case: "both request parameters of an HTTP route deleted",
            protocol: "HTTP",
            create: json!({
                "routeKey": "POST /pets",
                "requestParameters": {
                    "route.request.header.x-trace": { "required": false },
                    "route.request.querystring.dryRun": { "required": true }
                }
            }),
            emptied_by: EmptiedBy::DeletingEveryRequestParameter,
            still_reported: Vec::new,
        },
        Row {
            case: "an empty map patched over a stored map of model names",
            protocol: "WEBSOCKET",
            create: json!({
                "routeKey": "$connect",
                "requestModels": { "$default": "Empty", "application/json": "Reply" },
                "requestParameters": { "route.request.querystring.token": { "required": true } },
                "authorizationScopes": ["scope/one"]
            }),
            emptied_by: EmptiedBy::APatch(json!({ "requestModels": {} })),
            still_reported: || {
                vec![
                    (
                        "requestParameters",
                        json!({ "route.request.querystring.token": { "required": true } }),
                    ),
                    ("authorizationScopes", json!(["scope/one"])),
                ]
            },
        },
        Row {
            case: "an empty list patched over a stored list of scopes",
            protocol: "HTTP",
            create: json!({
                "routeKey": "GET /orders",
                "authorizationScopes": ["scope/one", "scope/two"],
                "requestParameters": { "route.request.header.x-trace": { "required": true } }
            }),
            emptied_by: EmptiedBy::APatch(json!({ "authorizationScopes": [] })),
            still_reported: || {
                vec![(
                    "requestParameters",
                    json!({ "route.request.header.x-trace": { "required": true } }),
                )]
            },
        },
        Row {
            case: "every collection emptied by one patch",
            protocol: "WEBSOCKET",
            create: json!({
                "routeKey": "$connect",
                "requestModels": { "$default": "Empty" },
                "requestParameters": { "route.request.header.x-trace": { "required": true } },
                "authorizationScopes": ["scope/one"]
            }),
            emptied_by: EmptiedBy::APatch(json!({
                "requestModels": {},
                "requestParameters": {},
                "authorizationScopes": []
            })),
            still_reported: Vec::new,
        },
    ]
}

/// An api of `protocol`, created through `CreateApi`; its `ApiId`.
async fn create_api(emulator: &MultiServiceRouter, case: &str, name: &str, protocol: &str) -> String {
    let mut request = json!({ "name": name, "protocolType": protocol });
    if protocol == "WEBSOCKET" {
        // The member a WEBSOCKET api is created with: which route serves a frame.
        request["routeSelectionExpression"] = json!("$request.body.action");
    }
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(request)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Assert `answer` reports exactly the collection members `expected` names, and none of
/// the others — a member a route holds no entries under carries no member at all.
fn assert_collections(case: &str, reader: &str, answer: &Value, expected: &[(&str, Value)]) {
    for member in ALWAYS_REPORTED {
        assert!(
            answer.get(member).is_some(),
            "case {case:?}: {reader} must report {member}, got {answer}"
        );
    }
    for member in COLLECTION_MEMBERS {
        match expected.iter().find(|(named, _)| named == member) {
            Some((_, value)) => assert_eq!(
                answer.get(member),
                Some(value),
                "case {case:?}: {reader} must report {member}={value}, got {answer}"
            ),
            None => assert!(
                answer.get(member).is_none(),
                "case {case:?}: the route holds no entries under {member}, so {reader} must \
                 report no {member} member at all, got {:?} in {answer}",
                answer.get(member)
            ),
        }
    }
}

#[tokio::test]
async fn a_route_holding_no_entries_under_a_collection_reports_no_such_member() {
    let emulator = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // One api per row, so a row's route key cannot collide with another row's.
        let api_id = create_api(
            &emulator,
            case,
            &format!("empty-collections-{index}"),
            row.protocol,
        )
        .await;

        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(row.create.clone()),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
        );
        let route_id = as_str(&created, "routeId");
        let expected = (row.still_reported)();

        match &row.emptied_by {
            EmptiedBy::TheCreateItself => {
                // The create's own answer already reports the emptied route.
                assert_collections(case, "CreateRoute", &created, &expected);
            }
            EmptiedBy::APatch(patch) => {
                let (status, patched) = call(
                    &emulator,
                    "PATCH",
                    &format!("/v2/apis/{api_id}/routes/{route_id}"),
                    Some(patch.clone()),
                )
                .await;
                assert_eq!(
                    status, 201,
                    "case {case:?}: expected 201 from UpdateRoute, got {status} {patched}"
                );
                assert_collections(case, "UpdateRoute", &patched, &expected);
            }
            EmptiedBy::DeletingEveryRequestParameter => {
                let keys: Vec<String> = row
                    .create
                    .get("requestParameters")
                    .and_then(Value::as_object)
                    .expect("this row's create declares request parameters")
                    .keys()
                    .cloned()
                    .collect();
                for key in &keys {
                    let (status, body) = call(
                        &emulator,
                        "DELETE",
                        &format!("/v2/apis/{api_id}/routes/{route_id}/requestparameters/{key}"),
                        None,
                    )
                    .await;
                    assert_eq!(
                        status, 204,
                        "case {case:?}: expected 204 deleting {key}, got {status} {body}"
                    );
                }
            }
        }

        // The STORE is what answers: the single reader of one route, and the listing.
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/routes/{route_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetRoute, got {status} {fetched}"
        );
        assert_collections(case, "GetRoute", &fetched, &expected);

        let (status, listed) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/routes"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetRoutes, got {status} {listed}"
        );
        let item = listed
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .unwrap_or_else(|| panic!("case {case:?}: GetRoutes must list the route, got {listed}"));
        assert_collections(case, "GetRoutes", item, &expected);
    }
}

/// The contrast: a route that DOES hold entries reports every collection member, so the
/// rule above drops a member for being empty and not for being a collection.
#[tokio::test]
async fn a_route_holding_entries_reports_every_collection_member() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "a populated route", "populated", "WEBSOCKET").await;

    let create = json!({
        "routeKey": "$connect",
        "requestModels": { "$default": "Empty" },
        "requestParameters": { "route.request.header.x-trace": { "required": true } },
        "authorizationScopes": ["scope/one", "scope/two"]
    });
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(create.clone()),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"a populated route\": expected 201 from CreateRoute, got {status} {created}"
    );
    let route_id = as_str(&created, "routeId");

    let expected: Vec<(&str, Value)> = COLLECTION_MEMBERS
        .iter()
        .map(|member| {
            (
                *member,
                create
                    .get(member)
                    .expect("the create names every collection member")
                    .clone(),
            )
        })
        .collect();
    assert_collections("a populated route", "CreateRoute", &created, &expected);

    let (status, fetched) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{route_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a populated route\": expected 200 from GetRoute, got {status} {fetched}"
    );
    assert_collections("a populated route", "GetRoute", &fetched, &expected);
}
