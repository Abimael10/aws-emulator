// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over the apis API Gateway v2 makes through its OWN API.
//!
//! The rule every case here holds the two services to: **a resource operation on
//! `AWS::ApiGatewayV2::Api` is carried out BY API Gateway v2's own API, and the resource it
//! leaves behind is the api that service holds** — a `CreateResource` really makes an api
//! (through `CreateApi` for a model that states the api's members, and through `ImportApi`
//! for one that carries an OpenAPI definition, which brings the routes that document
//! declares), an `UpdateResource` changes that same api under the identity it was minted
//! with, a `ListResources` enumerates the apis that service holds, and a `DeleteResource`
//! leaves that service holding none. Every assertion is made against that service's own
//! reads (`GetApi`, `GetApis`, `GetRoutes`), so nothing is confirmed from the answer being
//! checked.
//!
//! The rows span what the rule generalises over: whether the desired state states the api's
//! members or carries a definition to import, and so which call the create is; which
//! property an update patches; and how many apis a listing pages over.
//!
//! The sibling suite `cloudformation_apigatewayv2_api_schema_properties` holds the same two
//! services, plus CloudFormation, to the rules about which PROPERTIES a declaration of this
//! type may carry; this one is about the resource lifecycle those properties describe. Both
//! live here rather than in either service's own crate because neither service can answer
//! for the rule alone.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_api_schema_properties::{
    apigatewayv2, build_router, cloud_control, definition, held_routes, API_TYPE,
};

/// How long a wait for a resource operation request to complete runs before the case fails.
///
/// The work a request owes is a call on ANOTHER service, carried out by a later call of this
/// one, so the budget is far above anything that flow takes — including on a host running the
/// whole workspace's tests at once. No case waits for a span: each re-reads until the request
/// reports a terminal status or this deadline passes.
const SETTLED_BUDGET: Duration = Duration::from_secs(120);

/// How often that wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The terminal `OperationStatus` values a resource operation request reports.
const STATUS_SUCCESS: &str = "SUCCESS";
const STATUS_FAILED: &str = "FAILED";
const STATUS_CANCEL_COMPLETE: &str = "CANCEL_COMPLETE";

/// One Cloud Control operation, required to be served.
async fn cc_ok(router: &MultiServiceRouter, case: &str, operation: &str, payload: Value) -> Value {
    let (status, answered) = cloud_control(router, operation, payload).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} {answered}"
    );
    answered
}

/// The `ProgressEvent` a mutating operation's answer carries.
fn progress(answered: &Value) -> Value {
    answered["ProgressEvent"].clone()
}

/// A string member of an answered document.
fn text(value: &Value, member: &str) -> String {
    value
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read the request `token` names until it reports a TERMINAL status, answering the
/// `ProgressEvent` it reports then.
async fn terminal(router: &MultiServiceRouter, case: &str, token: &str) -> Value {
    let started = Instant::now();
    loop {
        let answered = cc_ok(
            router,
            case,
            "GetResourceRequestStatus",
            json!({ "RequestToken": token }),
        )
        .await;
        let event = progress(&answered);
        let reached = text(&event, "OperationStatus");
        if [STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE].contains(&reached.as_str()) {
            return event;
        }
        assert!(
            started.elapsed() < SETTLED_BUDGET,
            "case {case:?}: expected request {token:?} to reach a terminal status within \
             {SETTLED_BUDGET:?}, it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// [`terminal`], required to have SUCCEEDED.
async fn settled(router: &MultiServiceRouter, case: &str, token: &str) -> Value {
    let event = terminal(router, case, token).await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_SUCCESS,
        "case {case:?}: expected the operation to succeed, got {event}"
    );
    event
}

/// `CreateResource` of one api, waited out — answering the primary identifier the OWNING
/// service assigned, which is the only thing that names the resource afterwards.
async fn provision(router: &MultiServiceRouter, case: &str, desired: &Value) -> String {
    let accepted = cc_ok(
        router,
        case,
        "CreateResource",
        json!({ "TypeName": API_TYPE, "DesiredState": desired.to_string() }),
    )
    .await;
    let event = progress(&accepted);
    // "The resource operation hasn't yet started" / is in progress: the identity is the
    // owning service's to assign, so it is read off the request once that service has.
    let settled = settled(router, case, &text(&event, "RequestToken")).await;
    let identifier = text(&settled, "Identifier");
    assert!(
        !identifier.is_empty(),
        "case {case:?}: expected the identifier API Gateway v2 assigned, got {settled}"
    );
    identifier
}

/// The resource model `GetResource` reports for one api.
async fn model(router: &MultiServiceRouter, case: &str, identifier: &str) -> Value {
    let answered = cc_ok(
        router,
        case,
        "GetResource",
        json!({ "TypeName": API_TYPE, "Identifier": identifier }),
    )
    .await;
    let properties = answered["ResourceDescription"]
        .get("Properties")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no Properties in {answered}"))
        .to_string();
    serde_json::from_str(&properties).expect("a resource model document")
}

/// The api API Gateway v2 really holds under `identifier`, as its own read reports it.
async fn held_api(router: &MultiServiceRouter, case: &str, identifier: &str) -> Value {
    let (status, api) = apigatewayv2(router, "GET", &format!("/v2/apis/{identifier}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetApi for {identifier:?}, got {status} {api}"
    );
    api
}

/// Every identifier `ListResources` reports for this type, walking the pages at
/// `max_results` per page.
async fn walk(router: &MultiServiceRouter, case: &str, max_results: i64) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "TypeName": API_TYPE, "MaxResults": max_results });
        if let Some(token) = &token {
            request["NextToken"] = json!(token);
        }
        let listed = cc_ok(router, case, "ListResources", request).await;
        let page: Vec<String> = listed["ResourceDescriptions"]
            .as_array()
            .map(|descriptions| {
                descriptions
                    .iter()
                    .map(|description| text(description, "Identifier"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            page.len() <= usize::try_from(max_results).unwrap_or(usize::MAX),
            "case {case:?}: expected at most {max_results} resources in one page, got {page:?}"
        );
        identifiers.extend(page);
        match listed["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return identifiers,
        }
        assert!(
            identifiers.len() <= 100,
            "case {case:?}: the listing's pages never ran out, got {identifiers:?}"
        );
    }
}

/// One row of the create table: what the desired state states, and what API Gateway v2 must
/// then hold.
struct Created {
    case: &'static str,
    /// The desired state, built from the name the row's api carries.
    desired: fn(&str) -> Value,
    /// The protocol the api really speaks.
    protocol_type: &'static str,
    /// The route keys the api serves: the ones an imported definition brings, and none for
    /// an api the model states the members of.
    routes: &'static [&'static str],
}

const CREATED: &[Created] = &[
    Created {
        case: "a model stating the api's own members, which CreateApi makes",
        desired: |name| {
            json!({
                "Name": name,
                "ProtocolType": "HTTP",
                "Description": "made through Cloud Control API",
                "RouteSelectionExpression": "$request.method $request.path",
            })
        },
        protocol_type: "HTTP",
        routes: &[],
    },
    Created {
        case: "a model carrying an OpenAPI definition, which ImportApi makes with its routes",
        desired: |name| json!({ "Body": definition(name) }),
        protocol_type: "HTTP",
        routes: &["GET /probe"],
    },
    Created {
        case: "a model stating a WEBSOCKET api, whose protocol the create settles",
        desired: |name| {
            json!({
                "Name": name,
                "ProtocolType": "WEBSOCKET",
                "RouteSelectionExpression": "$request.body.action",
            })
        },
        protocol_type: "WEBSOCKET",
        routes: &[],
    },
];

/// RULE: a `CreateResource` of this type is carried out by API Gateway v2's own API, and what
/// it leaves behind is an api that service holds — read back under the identity that service
/// assigned, with the configuration and the routes the desired state asked for, and reported
/// by this service's own read and listing.
#[tokio::test]
async fn a_created_api_is_one_api_gateway_v2_holds() {
    let router = build_router();
    let mut provisioned: Vec<String> = Vec::new();
    for (index, row) in CREATED.iter().enumerate() {
        let case = row.case;
        let name = format!("cc-apigwv2-{index}");
        let identifier = provision(&router, case, &(row.desired)(&name)).await;
        provisioned.push(identifier.clone());

        // The api is the OWNING service's: its own read answers for it under the identifier
        // the request reported.
        let api = held_api(&router, case, &identifier).await;
        assert_eq!(
            api.get("apiId").and_then(Value::as_str),
            Some(identifier.as_str()),
            "case {case:?}: expected the api to answer to the identifier the request \
             reported, got {api}"
        );
        assert_eq!(
            api.get("name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: expected the api the desired state asked for, got {api}"
        );
        assert_eq!(
            api.get("protocolType").and_then(Value::as_str),
            Some(row.protocol_type),
            "case {case:?}: expected the protocol the desired state asked for, got {api}"
        );
        assert_eq!(
            held_routes(&router, case, &identifier).await,
            row.routes
                .iter()
                .map(|route| (*route).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case:?}: expected the routes the create brings, got {api}"
        );

        // ...and the model this service reports is what that read answers, under the
        // property names the type's schema states — including the two it marks read-only,
        // and none of the write-only ones a create SETS.
        let reported = model(&router, case, &identifier).await;
        for (property, read) in [
            ("ApiId", api.get("apiId")),
            ("Name", api.get("name")),
            ("ProtocolType", api.get("protocolType")),
            ("ApiEndpoint", api.get("apiEndpoint")),
        ] {
            assert_eq!(
                reported.get(property),
                read,
                "case {case:?}: expected the model's {property} to be what the read reports, \
                 got {reported}"
            );
        }
        for write_only in ["Body", "BodyS3Location", "BasePath", "FailOnWarnings"] {
            assert!(
                reported.get(write_only).is_none(),
                "case {case:?}: expected the write-only {write_only} to be out of the model, \
                 got {reported}"
            );
        }
    }

    // Every api the apis above made is one the listing enumerates, and a page carries no
    // more than the window it was asked for.
    let case = "the apis this suite created, paged one at a time";
    let listed = walk(&router, case, 1).await;
    for identifier in &provisioned {
        assert!(
            listed.contains(identifier),
            "case {case:?}: expected {identifier:?} among the listed apis, got {listed:?}"
        );
    }
}

/// RULE: an `UpdateResource` of this type is applied by API Gateway v2's own patch call to
/// the api that already exists — it keeps the identity that service minted and really holds
/// the new value — and a `DeleteResource` leaves that service holding no such api, so a
/// later read of it is a read of a resource that is not there.
#[tokio::test]
async fn an_api_is_patched_and_removed_where_it_lives() {
    let router = build_router();
    let case = "an api whose description and name a patch changes, then deleted";
    let identifier = provision(
        &router,
        case,
        &json!({
            "Name": "cc-apigwv2-patched",
            "ProtocolType": "HTTP",
            "Description": "before",
            "RouteSelectionExpression": "$request.method $request.path",
        }),
    )
    .await;

    // Two properties at once, each a member of the owning service's own patch call.
    let accepted = cc_ok(
        &router,
        case,
        "UpdateResource",
        json!({
            "TypeName": API_TYPE,
            "Identifier": identifier,
            "PatchDocument": json!([
                { "op": "replace", "path": "/Description", "value": "after" },
                { "op": "replace", "path": "/Name", "value": "cc-apigwv2-renamed" },
            ])
            .to_string(),
        }),
    )
    .await;
    let patched = settled(&router, case, &text(&progress(&accepted), "RequestToken")).await;
    assert_eq!(
        text(&patched, "Identifier"),
        identifier,
        "case {case:?}: expected the patch to keep the api's own identity, got {patched}"
    );
    // The api API Gateway v2 holds really carries the patched values, under the same id.
    let api = held_api(&router, case, &identifier).await;
    for (member, expected) in [
        ("description", "after"),
        ("name", "cc-apigwv2-renamed"),
        ("apiId", identifier.as_str()),
    ] {
        assert_eq!(
            api.get(member).and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected the patched {member}, got {api}"
        );
    }

    // ...and the delete is carried out THERE: that service holds no such api afterwards, so
    // neither read answers for one.
    let accepted = cc_ok(
        &router,
        case,
        "DeleteResource",
        json!({ "TypeName": API_TYPE, "Identifier": identifier }),
    )
    .await;
    settled(&router, case, &text(&progress(&accepted), "RequestToken")).await;
    let (status, api) = apigatewayv2(&router, "GET", &format!("/v2/apis/{identifier}")).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected GetApi to report the deleted api as not found, got {status} \
         {api}"
    );
    let (status, answered) = cloud_control(
        &router,
        "GetResource",
        json!({ "TypeName": API_TYPE, "Identifier": identifier }),
    )
    .await;
    // The status a not-found refusal of this service carries is that service's own rule
    // (its `not_found_refusal_status` suite owns it); what this case asserts is WHICH
    // refusal it is — the resource is not there.
    assert_eq!(
        answered.get("__type").and_then(Value::as_str),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected GetResource to refuse a resource that is not there, got \
         {status} {answered}"
    );
    assert!(
        !walk(&router, case, 10).await.contains(&identifier),
        "case {case:?}: expected the deleted api to be out of the listing"
    );
}
