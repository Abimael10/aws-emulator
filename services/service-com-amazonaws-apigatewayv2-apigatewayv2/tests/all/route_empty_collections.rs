// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for which COLLECTION members a reported route carries.
//!
//! The rule these cases assert: **which of a route's collection members — `RequestModels`,
//! `RequestParameters` and `AuthorizationScopes`, none of which the model declares
//! `@required` or gives a `@default`
//! (`smithy_query(query="shape",shape="Route",model_path="services/service-com-amazonaws-apigatewayv2-apigatewayv2/smithy-model/com-amazonaws-apigatewayv2-apigatewayv2.json")`)
//! — a route reports is part of the route's own stored configuration, not something read
//! off the collection's size: a member is reported while the route HOLDS entries under it,
//! and a member a writer MATERIALIZED is reported as the empty collection it is; a route
//! holding nothing under a member nothing materialized omits the member entirely instead of
//! reporting an empty map or an empty list.** An absent member and an empty one are
//! different facts to a client that branches on a member being present, so a route the
//! CALLER configured (`CreateRoute`/`UpdateRoute`) drops the member however it came to hold
//! nothing, and in every answer that reports a route: `CreateRoute`, `GetRoute`, `GetRoutes`
//! and `UpdateRoute`.
//!
//! The other half of the rule — a route written from an OpenAPI definition, where the
//! definition states the configuration of every operation it declares and states it as
//! nothing for an operation that configures nothing — is the last two tests of this suite:
//! such a route reports `authorizationScopes` and `requestParameters` as `[]` and `{}`.
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

use crate::api_import::import;
use crate::api_lifecycle::{as_str, call, emulator};
use crate::api_reimport::reimport;

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

/// What a route written from an OpenAPI definition must report for the two members that
/// definition states the configuration of: the empty collections they are, because an
/// operation that configures nothing still has that configuration stated for it.
fn imported_collections() -> Vec<(&'static str, Value)> {
    vec![
        ("requestParameters", json!({})),
        ("authorizationScopes", json!([])),
    ]
}

/// An `x-amazon-apigateway-integration` extension for an HTTP proxy to `uri`, so an
/// imported operation declares a backend the way a real definition does.
fn http_proxy(uri: &str) -> Value {
    json!({
        "x-amazon-apigateway-integration": {
            "type": "http_proxy",
            "httpMethod": "GET",
            "uri": uri,
            "payloadFormatVersion": "1.0"
        }
    })
}

/// Every route of an api as `(RouteId, Route)`, read back through `GetRoutes`.
async fn listed_routes(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
) -> Vec<(String, Value)> {
    let (status, listed) = call(emulator, "GET", &format!("/v2/apis/{api_id}/routes"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetRoutes, got {status} {listed}"
    );
    listed
        .get("items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: GetRoutes must report items, got {listed}"))
        .iter()
        .map(|route| (as_str(route, "routeId"), route.clone()))
        .collect()
}

/// Read every route of `api_id` through BOTH readers and assert each reports exactly
/// `expected` of the collection members, over the route keys `expect_route_keys` names.
async fn assert_every_route_reports(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    expect_route_keys: &[&str],
    expected: &[(&str, Value)],
) {
    let routes = listed_routes(emulator, case, api_id).await;
    let mut keys: Vec<String> = routes
        .iter()
        .map(|(_, route)| as_str(route, "routeKey"))
        .collect();
    keys.sort();
    let want: Vec<String> = expect_route_keys.iter().map(ToString::to_string).collect();
    assert_eq!(
        keys, want,
        "case {case:?}: expected the route keys {want:?}, got {keys:?}"
    );

    for (route_id, listed) in routes {
        assert_collections(case, "GetRoutes", &listed, expected);
        let (status, fetched) = call(
            emulator,
            "GET",
            &format!("/v2/apis/{api_id}/routes/{route_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetRoute, got {status} {fetched}"
        );
        assert_collections(case, "GetRoute", &fetched, expected);
    }
}

/// RULE, the other half: a route an OpenAPI definition wrote reports the two members that
/// definition states a route's configuration with, as the empty collections they are — and
/// reports no `requestModels`, which is WebSocket-only and which an imported HTTP api's
/// definition states nothing of.
///
/// The rows vary what the rule generalises over: how many paths and operations the
/// definition declares, which methods they use (`get`, `post`, `delete`, the `ANY` catch-all
/// extension), whether an operation declares an integration at all, the definition's version
/// (OpenAPI 3 and Swagger 2.0) and the `Basepath` mode the import is asked for. The recorded
/// case that filed the issue — a `basepath=split` import read back through `GetRoutes` — is
/// ONE row. Every row is re-read after the same definition is put over the api again through
/// `ReimportApi`, which writes the api's routes the same way.
#[tokio::test]
async fn a_route_an_openapi_definition_wrote_reports_its_stated_configuration() {
    let emulator = emulator();
    let expected = imported_collections();

    let rows: Vec<(&str, &str, String, Vec<&str>)> = vec![
        (
            "one operation declaring an integration",
            "",
            json!({
                "openapi": "3.0.1",
                "info": { "title": "one-operation", "version": "1.0" },
                "paths": { "/pets": { "get": http_proxy("https://pets.example.com/pets") } }
            })
            .to_string(),
            vec!["GET /pets"],
        ),
        (
            "three operations over two paths, one of them the ANY catch-all",
            "",
            json!({
                "openapi": "3.0.1",
                "info": { "title": "three-operations", "version": "1.0" },
                "paths": {
                    "/pets": {
                        "get": http_proxy("https://pets.example.com/pets"),
                        "post": http_proxy("https://pets.example.com/pets")
                    },
                    "/pets/{petId}": {
                        "x-amazon-apigateway-any-method":
                            http_proxy("https://pets.example.com/pets/{petId}")
                    }
                }
            })
            .to_string(),
            vec!["ANY /pets/{petId}", "GET /pets", "POST /pets"],
        ),
        (
            "an operation that declares no integration at all",
            "",
            json!({
                "openapi": "3.0.1",
                "info": { "title": "no-integration", "version": "1.0" },
                "paths": { "/orders": { "delete": {} } }
            })
            .to_string(),
            vec!["DELETE /orders"],
        ),
        (
            // The recorded case: an import whose base path is split, read through GetRoutes.
            "two operations imported with basepath=split",
            "?basepath=split",
            json!({
                "openapi": "3.0.1",
                "info": { "title": "split-base-path", "version": "1.0" },
                "servers": [{ "url": "https://api.example.com/dev/v1" }],
                "paths": {
                    "/pets": { "get": http_proxy("https://pets.example.com/pets") },
                    "/orders": { "post": http_proxy("https://orders.example.com/orders") }
                }
            })
            .to_string(),
            vec!["GET /v1/pets", "POST /v1/orders"],
        ),
        (
            "a swagger 2.0 definition imported with basepath=prepend",
            "?basepath=prepend",
            json!({
                "swagger": "2.0",
                "info": { "title": "swagger-prepend", "version": "0.9" },
                "basePath": "/dev",
                "paths": { "/items": { "get": http_proxy("https://items.example.com/items") } }
            })
            .to_string(),
            vec!["GET /dev/items"],
        ),
    ];

    for (case, query, definition, route_keys) in rows {
        let (status, imported) = import(&emulator, query, &definition).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ImportApi, got {status} {imported}"
        );
        let api_id = as_str(&imported, "apiId");
        assert_every_route_reports(&emulator, case, &api_id, &route_keys, &expected).await;

        // The same definition put over the api again: the routes `ReimportApi` rewrites
        // carry the configuration it states, so the members are reported from that writer
        // too.
        let (status, reimported) = reimport(&emulator, &api_id, query, &definition).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ReimportApi, got {status} {reimported}"
        );
        assert_every_route_reports(
            &emulator,
            &format!("{case}, reimported"),
            &api_id,
            &route_keys,
            &expected,
        )
        .await;
    }
}

/// RULE, the two halves together: the members an imported route reports are its
/// CONFIGURATION, so what a writer puts in them is what is reported, and removing the last
/// request parameter through the sub-resource that addresses the entries takes the member out
/// of that configuration — exactly as it does for a route the caller created.
#[tokio::test]
async fn configuring_and_emptying_an_imported_route_moves_its_members() {
    let emulator = emulator();
    let case = "an imported route configured through UpdateRoute";
    let definition = json!({
        "openapi": "3.0.1",
        "info": { "title": "configured-after-import", "version": "1.0" },
        "paths": { "/pets": { "get": http_proxy("https://pets.example.com/pets") } }
    })
    .to_string();

    let (status, imported) = import(&emulator, "", &definition).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ImportApi, got {status} {imported}"
    );
    let api_id = as_str(&imported, "apiId");
    let routes = listed_routes(&emulator, case, &api_id).await;
    let (route_id, _) = routes
        .first()
        .cloned()
        .expect("the import persisted the definition's one route");

    let scopes = json!(["scope/read", "scope/write"]);
    let parameters = json!({ "route.request.header.x-trace": { "required": true } });
    let (status, patched) = call(
        &emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/routes/{route_id}"),
        Some(json!({
            "authorizationScopes": scopes.clone(),
            "requestParameters": parameters.clone()
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from UpdateRoute, got {status} {patched}"
    );
    let configured = vec![
        ("requestParameters", parameters),
        ("authorizationScopes", scopes.clone()),
    ];
    assert_collections(case, "UpdateRoute", &patched, &configured);
    assert_every_route_reports(&emulator, case, &api_id, &["GET /pets"], &configured).await;

    // The LAST request parameter removed through its own sub-resource: the member leaves the
    // route's configuration, so no member is reported for it — while the scopes the route
    // still carries are reported unchanged.
    let deleted_case = "the last request parameter of an imported route deleted";
    let (status, deleted) = call(
        &emulator,
        "DELETE",
        &format!(
            "/v2/apis/{api_id}/routes/{route_id}/requestparameters/route.request.header.x-trace"
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {deleted_case:?}: expected 204 from DeleteRouteRequestParameter, got {status} \
         {deleted}"
    );
    assert_every_route_reports(
        &emulator,
        deleted_case,
        &api_id,
        &["GET /pets"],
        &[("authorizationScopes", scopes)],
    )
    .await;
}
