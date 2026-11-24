// SPDX-License-Identifier: CC0-1.0

//! What a `RouteKey` may be, the form the service stores it in, what a deployment requires
//! of the routes it freezes, and the two members a request names a thing the api could never
//! evaluate with.
//!
//! Every test drives the real `restJson1` request path — `MultiServiceRouter::dispatch` over
//! a signed `http::Request` — so what is asserted is the wire behaviour a client sees, and
//! every write is read back through the operation that reports it.
//!
//! The rules asserted here, each over a TABLE rather than over one request:
//!
//! * A route key is written in the grammar the api's protocol routes on — an HTTP api's is
//!   the request LINE (`[HTTP METHOD] /[RESOURCE PATH]`) or the catch-all `$default`, a
//!   WEBSOCKET api's is whatever its `RouteSelectionExpression` yields for a message — and a
//!   key outside it is refused `400 BadRequestException`, with nothing persisted.
//! * A GREEDY path variable stands for every segment after it, so it is only ever the LAST
//!   segment of a key written as a request line.
//! * The stored key carries its method half UPPER CASE, which is what makes a key that
//!   differs from a stored one only in that case the DUPLICATE it is (`409`).
//! * Both of a route's writers — `CreateRoute` and `UpdateRoute` — hold a key to those same
//!   rules.
//! * A WEBSOCKET api serves a frame only through the target of the route that matched it, so
//!   `CreateDeployment` refuses to publish one of its routes that names none; an HTTP api is
//!   not held to that.
//! * A collection reader's page size is CLAMPED into the window it serves rather than
//!   refused: `ListRoutingRules` answers a `maxResults` outside the modelled window with the
//!   page it has.
//! * A WEBSOCKET api's `RouteSelectionExpression` is required and has to name something the
//!   api can evaluate; an HTTP api's is defaulted.
//!
//! The rows vary what each rule generalises over: the api's protocol, the shape of the key
//! (method and path, variables, greedy variables, the catch-all, the lifecycle keys, a
//! selector token), the case of the method, which writer the key reaches the store through,
//! how many routes a deployment captures and which of them names a target, and the page size
//! a reader is asked for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewayv2_apigatewayv2::state::{
    duplicate_identity::route_key_in_use_message, route_key_grammar_message,
    undefined_route_target_message, unexpected_selection_variable_message, DEFAULT_ROUTE_KEY,
    GREEDY_VARIABLE_POSITION, HTTP_PROTOCOL_TYPE, INVALID_ROUTE_SELECTION_EXPRESSION,
    MAX_PAGE_SIZE, WEBSOCKET_PROTOCOL_TYPE,
};

use crate::api_lifecycle::{
    as_str, call, call_reading_headers, emulator, error_code, mock_integration, REGION,
};

/// The expression a WEBSOCKET api selects its routes with: "a field of the request body" is
/// what such an api matches a message against (`CreateApi$RouteSelectionExpression`).
const WEBSOCKET_SELECTION: &str = "$request.body.action";

/// The `CreateApi` request for an api of `protocol_type`.
fn api_request(protocol_type: &str, name: &str) -> Value {
    let mut request = json!({ "name": name, "protocolType": protocol_type });
    if protocol_type == WEBSOCKET_PROTOCOL_TYPE {
        request["routeSelectionExpression"] = json!(WEBSOCKET_SELECTION);
    }
    request
}

/// Create one api and answer its `ApiId`.
async fn create_api(
    emulator: &MultiServiceRouter,
    case: &str,
    protocol_type: &str,
    name: &str,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(api_request(protocol_type, name)),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Send one `CreateRoute` and answer `(status, body)`.
async fn create_route(
    emulator: &MultiServiceRouter,
    api_id: &str,
    request: Value,
) -> (u16, Value) {
    call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(request),
    )
    .await
}

/// Send one `UpdateRoute` and answer `(status, body)`.
async fn update_route(
    emulator: &MultiServiceRouter,
    api_id: &str,
    route_id: &str,
    request: Value,
) -> (u16, Value) {
    call(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/routes/{route_id}"),
        Some(request),
    )
    .await
}

/// The `RouteKey` the stored route `route_id` reports, read back through `GetRoute`.
async fn stored_route_key(emulator: &MultiServiceRouter, api_id: &str, route_id: &str) -> String {
    let (status, fetched) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{route_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetRoute on {route_id:?}, got {status} {fetched}"
    );
    as_str(&fetched, "routeKey")
}

/// Every `RouteKey` the api holds, read back through `GetRoutes`.
async fn stored_route_keys(emulator: &MultiServiceRouter, api_id: &str) -> Vec<String> {
    let (status, routes) = call(emulator, "GET", &format!("/v2/apis/{api_id}/routes"), None).await;
    assert_eq!(
        status, 200,
        "expected 200 from GetRoutes, got {status} {routes}"
    );
    routes["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|route| as_str(route, "routeKey"))
        .collect()
}

/// One row of the grammar table: the api's protocol, the key the request sends, and the
/// sentence the refusal carries.
struct Refused {
    protocol_type: &'static str,
    route_key: &'static str,
    message: String,
}

fn refused_rows() -> Vec<Refused> {
    let http = |route_key: &'static str, message: String| Refused {
        protocol_type: HTTP_PROTOCOL_TYPE,
        route_key,
        message,
    };
    let websocket = |route_key: &'static str| Refused {
        protocol_type: WEBSOCKET_PROTOCOL_TYPE,
        route_key,
        message: route_key_grammar_message(WEBSOCKET_PROTOCOL_TYPE),
    };
    let http_grammar = route_key_grammar_message(HTTP_PROTOCOL_TYPE);
    vec![
        // An HTTP api's key is a request line: a path with no method, a method with no
        // path, a word that is no method, a relative path, a path carrying further
        // whitespace, a WebSocket lifecycle key, and nothing at all.
        http("/items", http_grammar.clone()),
        http("items", http_grammar.clone()),
        http("FETCH /items", http_grammar.clone()),
        http("GET items", http_grammar.clone()),
        http("GET /items and more", http_grammar.clone()),
        http("$connect", http_grammar.clone()),
        http("", http_grammar.clone()),
        // A greedy variable before the end of the path takes the position's own sentence.
        http("GET /{proxy+}/items", GREEDY_VARIABLE_POSITION.to_string()),
        http("ANY /{proxy+}/{id}", GREEDY_VARIABLE_POSITION.to_string()),
        http("GET /a/{rest+}/b/c", GREEDY_VARIABLE_POSITION.to_string()),
        // A WEBSOCKET api reserves the `$` prefix for the calls it makes, and states one
        // sentence for every key it will not take — a misplaced greedy variable included.
        websocket("$unknown"),
        websocket("$"),
        websocket(""),
        websocket("ANY /{proxy+}/{id}"),
    ]
}

#[tokio::test]
async fn a_route_key_outside_its_api_s_grammar_is_refused_and_persists_nothing() {
    let emulator = emulator();

    for (index, row) in refused_rows().into_iter().enumerate() {
        let case = format!("{} key {:?}", row.protocol_type, row.route_key);
        // One api per row, so a refusal can be told apart from a key another row stored.
        let api_id = create_api(
            &emulator,
            &case,
            row.protocol_type,
            &format!("grammar-host-{index}"),
        )
        .await;
        let (status, headers, body) = call_reading_headers(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({ "routeKey": row.route_key })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateRoute, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            row.message,
            "case {case:?}: expected the message {:?}, got {body}",
            row.message
        );
        // The refusal names its error shape in the header a client reads it from, too.
        assert_eq!(
            headers
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok()),
            Some("BadRequestException"),
            "case {case:?}: expected the error type in the header, got {headers:?}"
        );
        assert!(
            stored_route_keys(&emulator, &api_id).await.is_empty(),
            "case {case:?}: a refused CreateRoute must persist no route"
        );
    }
}

#[tokio::test]
async fn a_route_key_its_api_s_grammar_admits_is_stored_and_read_back() {
    let emulator = emulator();

    // (the api's protocol, the key the request sends, the key the store holds)
    let rows = [
        (HTTP_PROTOCOL_TYPE, "GET /items", "GET /items"),
        (HTTP_PROTOCOL_TYPE, "DELETE /items/{id}", "DELETE /items/{id}"),
        (HTTP_PROTOCOL_TYPE, "ANY /{proxy+}", "ANY /{proxy+}"),
        (HTTP_PROTOCOL_TYPE, "GET /a/{id}/{rest+}", "GET /a/{id}/{rest+}"),
        (HTTP_PROTOCOL_TYPE, "POST /", "POST /"),
        (HTTP_PROTOCOL_TYPE, DEFAULT_ROUTE_KEY, DEFAULT_ROUTE_KEY),
        // A WEBSOCKET api's lifecycle keys, its catch-all, and the selector values a
        // message can carry — a value with a space among them, since a selector yields
        // whatever the message holds.
        (WEBSOCKET_PROTOCOL_TYPE, "$connect", "$connect"),
        (WEBSOCKET_PROTOCOL_TYPE, "$disconnect", "$disconnect"),
        (WEBSOCKET_PROTOCOL_TYPE, DEFAULT_ROUTE_KEY, DEFAULT_ROUTE_KEY),
        (WEBSOCKET_PROTOCOL_TYPE, "sendMessage", "sendMessage"),
        (WEBSOCKET_PROTOCOL_TYPE, "send.message-1_x", "send.message-1_x"),
        (WEBSOCKET_PROTOCOL_TYPE, "send a message", "send a message"),
        (WEBSOCKET_PROTOCOL_TYPE, "GET /items", "GET /items"),
    ];

    for (index, (protocol_type, requested, stored)) in rows.into_iter().enumerate() {
        let case = format!("{protocol_type} key {requested:?}");
        let api_id = create_api(
            &emulator,
            &case,
            protocol_type,
            &format!("admitted-host-{index}"),
        )
        .await;
        let (status, created) =
            create_route(&emulator, &api_id, json!({ "routeKey": requested })).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
        );
        assert_eq!(
            as_str(&created, "routeKey"),
            stored,
            "case {case:?}: expected the stored key {stored:?}, got {created}"
        );
        let route_id = as_str(&created, "routeId");
        let read_back = stored_route_key(&emulator, &api_id, &route_id).await;
        assert_eq!(
            read_back, stored,
            "case {case:?}: GetRoute must report the stored key {stored:?}, got {read_back:?}"
        );
    }
}

#[tokio::test]
async fn the_stored_route_key_carries_its_method_upper_cased_and_a_recased_key_is_a_duplicate() {
    let emulator = emulator();

    // (the key the request sends, the key the store holds, a second spelling of the same
    // key). The method half moves case; the path half, which is case-sensitive, does not.
    let rows = [
        ("get /x", "GET /x", "GET /x"),
        ("Get /x", "GET /x", "get /x"),
        ("GET /x", "GET /x", "gEt /x"),
        ("patch /Items/{Id}", "PATCH /Items/{Id}", "PATCH /Items/{Id}"),
        ("any /{proxy+}", "ANY /{proxy+}", "AnY /{proxy+}"),
    ];

    for (index, (requested, stored, again)) in rows.into_iter().enumerate() {
        let case = format!("key {requested:?}");
        let api_id = create_api(
            &emulator,
            &case,
            HTTP_PROTOCOL_TYPE,
            &format!("normalised-host-{index}"),
        )
        .await;
        let (status, created) =
            create_route(&emulator, &api_id, json!({ "routeKey": requested })).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
        );
        assert_eq!(
            as_str(&created, "routeKey"),
            stored,
            "case {case:?}: expected the method upper-cased in {stored:?}, got {created}"
        );
        let route_id = as_str(&created, "routeId");
        let read_back = stored_route_key(&emulator, &api_id, &route_id).await;
        assert_eq!(
            read_back, stored,
            "case {case:?}: GetRoute must report {stored:?}, got {read_back:?}"
        );

        // The same key in another case names the same route, so a second create of it is
        // the conflict it is — and the sentence names the key as the store holds it.
        let (status, conflict) =
            create_route(&emulator, &api_id, json!({ "routeKey": again })).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected 409 for the recased key {again:?}, got {status} {conflict}"
        );
        assert_eq!(
            error_code(&conflict),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {conflict}"
        );
        assert_eq!(
            as_str(&conflict, "message"),
            route_key_in_use_message(stored),
            "case {case:?}: expected the duplicate-key sentence, got {conflict}"
        );
        assert_eq!(
            stored_route_keys(&emulator, &api_id).await,
            vec![stored.to_string()],
            "case {case:?}: the refused create must leave the api with one route"
        );
    }
}

#[tokio::test]
async fn a_patched_route_key_is_held_to_the_same_grammar_and_stored_the_same_way() {
    let emulator = emulator();
    let case = "a patched route key";
    let api_id = create_api(&emulator, case, HTTP_PROTOCOL_TYPE, "patched-host").await;
    let (status, created) =
        create_route(&emulator, &api_id, json!({ "routeKey": "GET /items" })).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
    );
    let route_id = as_str(&created, "routeId");

    // A patch the grammar admits is stored in the same form a create would be.
    let (status, patched) =
        update_route(&emulator, &api_id, &route_id, json!({ "routeKey": "post /moved" })).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from UpdateRoute, got {status} {patched}"
    );
    assert_eq!(
        as_str(&patched, "routeKey"),
        "POST /moved",
        "case {case:?}: expected the patched key stored with its method upper-cased, got \
         {patched}"
    );

    // And a patch outside it is refused, leaving the record exactly as it was.
    for route_key in ["/moved", "GET /{proxy+}/items", "$connect"] {
        let (status, refused) =
            update_route(&emulator, &api_id, &route_id, json!({ "routeKey": route_key })).await;
        assert_eq!(
            status, 400,
            "case {route_key:?}: expected 400 from UpdateRoute, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {route_key:?}: expected BadRequestException, got {refused}"
        );
        let read_back = stored_route_key(&emulator, &api_id, &route_id).await;
        assert_eq!(
            read_back, "POST /moved",
            "case {route_key:?}: a refused patch must leave the stored key alone, got \
             {read_back:?}"
        );
    }
}

#[tokio::test]
async fn a_quick_create_route_key_is_held_to_the_same_grammar_and_stored_the_same_way() {
    let emulator = emulator();

    // `CreateApi`'s `RouteKey` names the route QUICK CREATE mints, so it is a route key like
    // any other. (the key the request sends, the key the quick-create route holds, or `None`
    // where the create is refused with the HTTP grammar's sentence).
    let rows: [(&str, Option<&str>); 6] = [
        ("GET /items", Some("GET /items")),
        ("get /items", Some("GET /items")),
        ("ANY /{proxy+}", Some("ANY /{proxy+}")),
        (DEFAULT_ROUTE_KEY, Some(DEFAULT_ROUTE_KEY)),
        ("/items", None),
        ("FETCH /items", None),
    ];

    for (index, (requested, stored)) in rows.into_iter().enumerate() {
        let case = format!("quick-create routeKey {requested:?}");
        let (status, body) = call(
            &emulator,
            "POST",
            "/v2/apis",
            Some(json!({
                "name": format!("quick-create-host-{index}"),
                "protocolType": HTTP_PROTOCOL_TYPE,
                "routeKey": requested,
                "target": "https://example.com/upstream"
            })),
        )
        .await;
        match stored {
            Some(stored) => {
                assert_eq!(
                    status, 201,
                    "case {case:?}: expected 201 from CreateApi, got {status} {body}"
                );
                // The key the quick-create route HOLDS is what a later reader reports.
                let api_id = as_str(&body, "apiId");
                assert_eq!(
                    stored_route_keys(&emulator, &api_id).await,
                    vec![stored.to_string()],
                    "case {case:?}: expected the quick-create route to hold {stored:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400 from CreateApi, got {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "BadRequestException",
                    "case {case:?}: expected BadRequestException, got {body}"
                );
                assert_eq!(
                    as_str(&body, "message"),
                    route_key_grammar_message(HTTP_PROTOCOL_TYPE),
                    "case {case:?}: expected the grammar refusal, got {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_websocket_deployment_requires_every_route_it_captures_to_name_a_target() {
    let emulator = emulator();
    let case = "a WEBSOCKET api with one untargeted route";
    let api_id = create_api(&emulator, case, WEBSOCKET_PROTOCOL_TYPE, "deployable-host").await;
    let target = format!(
        "integrations/{}",
        mock_integration(&emulator, case, &api_id).await
    );

    // One route that names a target and one that does not: the deployment is refused, and
    // the sentence names the route that could not be served.
    let (status, targeted) = create_route(
        &emulator,
        &api_id,
        json!({ "routeKey": "$connect", "target": &target }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {targeted}"
    );
    let (status, orphan) =
        create_route(&emulator, &api_id, json!({ "routeKey": "sendMessage" })).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {orphan}"
    );
    let orphan_id = as_str(&orphan, "routeId");

    let (status, refused) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 from CreateDeployment, got {status} {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "BadRequestException",
        "case {case:?}: expected BadRequestException, got {refused}"
    );
    assert_eq!(
        as_str(&refused, "message"),
        undefined_route_target_message(&orphan_id, "sendMessage"),
        "case {case:?}: expected the sentence to name the untargeted route, got {refused}"
    );
    let (status, deployments) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/deployments"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetDeployments, got {status} {deployments}"
    );
    assert_eq!(
        deployments["items"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a refused deployment must persist nothing, got {deployments}"
    );

    // Wire that route to a target and the same call is accepted, and the deployment reads
    // back under the id it minted.
    let (status, patched) = update_route(
        &emulator,
        &api_id,
        &orphan_id,
        json!({ "target": &target }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from UpdateRoute, got {status} {patched}"
    );
    let (status, deployed) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateDeployment once every route has a target, \
         got {status} {deployed}"
    );
    let deployment_id = as_str(&deployed, "deploymentId");
    let (status, fetched) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/deployments/{deployment_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetDeployment, got {status} {fetched}"
    );
    assert_eq!(
        fetched.get("deploymentStatus"),
        Some(&json!("DEPLOYED")),
        "case {case:?}: expected the released deployment, got {fetched}"
    );
}

#[tokio::test]
async fn a_deployment_an_untargeted_route_does_not_block() {
    let emulator = emulator();

    // The two apis the rule does NOT refuse: an HTTP api, whose route answers the request
    // that matches it at the data plane with no integration attached, and a WEBSOCKET api
    // whose every route names a target.
    let rows = [
        (
            "an HTTP api whose route names no target",
            HTTP_PROTOCOL_TYPE,
            "GET /items",
        ),
        (
            "a WEBSOCKET api whose only route names a target",
            WEBSOCKET_PROTOCOL_TYPE,
            "$connect",
        ),
    ];

    for (index, (case, protocol_type, route_key)) in rows.into_iter().enumerate() {
        let api_id = create_api(
            &emulator,
            case,
            protocol_type,
            &format!("unblocked-host-{index}"),
        )
        .await;
        // A WEBSOCKET api serves a frame only through the target of the route that matched
        // it, so its route is wired to one; an HTTP api's route is deployed without.
        let mut request = json!({ "routeKey": route_key });
        if protocol_type == WEBSOCKET_PROTOCOL_TYPE {
            let target = format!(
                "integrations/{}",
                mock_integration(&emulator, case, &api_id).await
            );
            request["target"] = json!(target);
        }
        let (status, created) = create_route(&emulator, &api_id, request).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
        );
        let (status, deployed) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateDeployment, got {status} {deployed}"
        );
    }
}

/// Create one custom domain name, and answer the name it was created under.
async fn create_domain_name(emulator: &MultiServiceRouter, case: &str, domain: &str) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/domainnames",
        Some(json!({
            "domainName": domain,
            "domainNameConfigurations": [{
                "certificateArn": engine::emulator_arn(
                    "acm",
                    REGION,
                    &format!("certificate/{domain}"),
                ),
            }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateDomainName, got {status} {created}"
    );
    as_str(&created, "domainName")
}

#[tokio::test]
async fn a_page_size_outside_the_modelled_window_is_served_rather_than_refused() {
    let emulator = emulator();
    let case = "the routing rules of one domain name";
    let domain = create_domain_name(&emulator, case, "paged.example.com").await;
    let api_id = create_api(&emulator, case, HTTP_PROTOCOL_TYPE, "paged-api").await;
    let (status, stage) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": "prod" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateStage, got {status} {stage}"
    );

    // The window the model declares for this member is 1..100; the rows span both of its
    // bounds, the values below and above them, and a request naming no page size at all.
    // Every one is SERVED: the window is where the page size is clamped, not a refusal.
    let page_sizes = ["0", "1", "2", "100", "101", "1000", "-1"];

    // First on a domain name holding no rule at all, where the answer is the empty
    // collection rather than an error.
    for page_size in page_sizes {
        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/domainnames/{domain}/routingrules?maxResults={page_size}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case maxResults={page_size:?}: expected 200 from ListRoutingRules on a domain \
             name with no rules, got {status} {body}"
        );
        assert_eq!(
            body.get("routingRules"),
            Some(&json!([])),
            "case maxResults={page_size:?}: expected the empty collection, got {body}"
        );
    }

    // Then with rules stored, where a page carries at least one and at most the window's
    // ceiling.
    let stored = 3usize;
    for priority in 1..=stored {
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/domainnames/{domain}/routingrules"),
            Some(json!({
                "priority": priority,
                "actions": [{ "invokeApi": { "apiId": api_id, "stage": "prod" } }],
                "conditions": [{ "matchBasePaths": { "anyOf": [format!("orders{priority}")] } }]
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoutingRule, got {status} {created}"
        );
    }
    for page_size in page_sizes {
        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/domainnames/{domain}/routingrules?maxResults={page_size}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case maxResults={page_size:?}: expected 200 from ListRoutingRules, got {status} \
             {body}"
        );
        let page = body["routingRules"].as_array().cloned().unwrap_or_default();
        let asked: usize = page_size.parse().unwrap_or(1);
        let expected = asked.clamp(1, MAX_PAGE_SIZE).min(stored);
        assert_eq!(
            page.len(),
            expected,
            "case maxResults={page_size:?}: expected a page of {expected} of the {stored} \
             stored rules, got {body}"
        );
    }
}

#[tokio::test]
async fn a_route_selection_expression_an_api_cannot_evaluate_is_refused() {
    let emulator = emulator();

    // (the expression the request carries, the sentence a WEBSOCKET api refuses it with, or
    // `None` where the api is created). A WEBSOCKET api selects its routes with this
    // expression, so one it was not given — or one with nothing in it — is no selector; one
    // that IS a selector but reads a variable the service does not evaluate, or a part of
    // the request a frame does not have, names the reference back.
    let rows: Vec<(Option<&str>, Option<String>)> = vec![
        (None, Some(INVALID_ROUTE_SELECTION_EXPRESSION.to_string())),
        (Some(""), Some(INVALID_ROUTE_SELECTION_EXPRESSION.to_string())),
        (Some("   "), Some(INVALID_ROUTE_SELECTION_EXPRESSION.to_string())),
        (
            Some("$nonsense.action"),
            Some(unexpected_selection_variable_message("$nonsense")),
        ),
        (
            Some("$request.method"),
            Some(unexpected_selection_variable_message("$request.method")),
        ),
        (Some(WEBSOCKET_SELECTION), None),
        (Some("${request.body.route}"), None),
    ];

    for (index, (expression, refusal)) in rows.into_iter().enumerate() {
        let case = format!("WEBSOCKET routeSelectionExpression {expression:?}");
        let mut request = json!({
            "name": format!("selector-host-{index}"),
            "protocolType": WEBSOCKET_PROTOCOL_TYPE
        });
        if let Some(expression) = expression {
            request["routeSelectionExpression"] = json!(expression);
        }
        let (status, body) = call(&emulator, "POST", "/v2/apis", Some(request)).await;
        match refusal {
            Some(message) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400 from CreateApi, got {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "BadRequestException",
                    "case {case:?}: expected BadRequestException, got {body}"
                );
                assert_eq!(
                    as_str(&body, "message"),
                    message,
                    "case {case:?}: expected the message {message:?}, got {body}"
                );
            }
            None => {
                assert_eq!(
                    status, 201,
                    "case {case:?}: expected 201 from CreateApi, got {status} {body}"
                );
                assert_eq!(
                    as_str(&body, "routeSelectionExpression"),
                    expression.unwrap_or_default(),
                    "case {case:?}: expected the stored expression reported back, got {body}"
                );
            }
        }
    }

    // An HTTP api is not held to it: the member is defaulted when the request names none
    // ("If not provided, this will be the default for HTTP APIs").
    let (status, created) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "default-selector-host", "protocolType": HTTP_PROTOCOL_TYPE })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateApi for an HTTP api naming no expression, got {status} \
         {created}"
    );
    assert_eq!(
        as_str(&created, "routeSelectionExpression"),
        "$request.method $request.path",
        "expected the documented default for an HTTP api, got {created}"
    );
}
