// SPDX-License-Identifier: CC0-1.0

//! What the gateway does to a BACKEND's answer, and to the request it relays, on the
//! `execute-api` data plane of an HTTP api.
//!
//! The rules the tables below are each one slice of:
//!
//! * **the integration's `ResponseParameters` map the status code the backend answered onto
//!   one set of parameters, and that set alone transforms the response before the client
//!   reads it — `overwrite:statuscode` replaces the status, and `append`/`overwrite`/`remove`
//!   of `header.<name>` adds, replaces or drops a response header, each value either static
//!   text or a mapping expression evaluated against the call** ("you use response parameters
//!   to transform the HTTP response from a backend integration before returning the response
//!   to clients. … The selection key must be a valid HTTP status code within the range of
//!   200-599", `CreateIntegration$ResponseParameters`);
//! * **a Lambda proxy response is the structured document its payload format defines, with
//!   every member of the documented type — a function that named one as another type, like a
//!   `body` that is no string, answered no proxy response at all and the call is the
//!   gateway's own `500`; what `2.0` INFERS for a function that returned valid JSON naming no
//!   `statusCode` is "the function's response as the body", so a returned string answers that
//!   string's own text**;
//! * **a gateway hop preserves the client's `X-Forwarded-For` chain and APPENDS the address
//!   it observed the request from, so the backend's last element is the peer the gateway read
//!   the request from, while every other header the client sent — `Via` among them — reaches
//!   the backend unchanged**;
//! * **a `REQUEST` authorizer whose function RAISED states no decision, and which refusal
//!   that earns is the error it raised: the single word `Unauthorized` is how a Lambda
//!   authorizer signals an unauthenticated caller and the call is `401`
//!   `{"message":"Unauthorized"}`, while any other unhandled error is the gateway's `500`.**
//!
//! Nothing here is a value this emulator fills in: every assertion is on something a function
//! RAN and returned, or on what the gateway did to that answer, which is why AWS Lambda is
//! mounted beside this service on one engine.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, given_function, lambda_integration, ECHO_EVENT,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The Region the functions below live in.
const REGION: &str = "us-east-1";

/// The stage every case addresses, and the route it invokes.
const STAGE: &str = "prod";
const ROUTE_KEY: &str = "GET /orders";
const ROUTE_PATH: &str = "/orders";

/// The status and sentence an HTTP api answers for an integration that produced no response.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The status and sentence a call whose authorizer identified no caller is refused with.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The address the emulator's own front door observes a request FROM: a client reaches it
/// over loopback, so that is the peer the gateway appends to the forwarded chain.
const OBSERVED_PEER: &str = "127.0.0.1";

/// The status a call whose backend RAN and did not answer within its own deadline is refused
/// with — the class the rows below wait past rather than assert on ([`answered`]).
const TIMED_OUT: u16 = 503;

/// How long a row waits for the route to be answered by a backend that could RUN, and how
/// often it looks.
///
/// The FIRST invocation of a function starts its runtime, and on a loaded host that start can
/// overrun the deadline the gateway gives the authorizer or the integration — a transient
/// gateway refusal that the next call does not get. So a row whose expectation is not a
/// refusal looks again until the route is answered by the backend, which is a CONDITION and
/// not a wait for a fixed time; the deadline is generous, and nothing here asserts on the
/// clock.
const BACKEND_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// Create an api and answer `(ApiId, the host its `ApiEndpoint` names)`.
async fn given_api(emulator: &MultiServiceRouter, case: &str) -> (String, String) {
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "response-mapping-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = api
        .get("apiId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let host = api
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();
    (api_id, host)
}

/// Put `integration` behind [`ROUTE_KEY`] of `api_id` on a deployed [`STAGE`], optionally
/// gated by `authorizer` (an `AuthorizerId` of the SAME api).
async fn given_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    integration: Value,
    authorizer: Option<&str>,
) {
    let (status, created) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(integration),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration answered {status} {created}"
    );
    let integration_id = created
        .get("integrationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut route = json!({
        "routeKey": ROUTE_KEY,
        "target": format!("integrations/{integration_id}")
    });
    if let Some(authorizer_id) = authorizer {
        // `CUSTOM` is the `AuthorizationType` "for using a Lambda authorizer".
        route["authorizationType"] = json!("CUSTOM");
        route["authorizerId"] = json!(authorizer_id);
    }
    let (status, created) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(route),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute answered {status} {created}"
    );
    let (status, created) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": STAGE, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {created}"
    );
}

/// Create a `REQUEST` authorizer of `api_id` over the `x-token` header of the call, and
/// answer its `AuthorizerId`.
async fn given_authorizer(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    function_arn: &str,
) -> String {
    // The form `AuthorizerUri` documents: "arn:aws:apigateway:{region}:lambda:path/
    // 2015-03-31/functions/{function-arn}/invocations".
    let uri = format!(
        "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/invocations"
    );
    let (status, created) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(json!({
            "name": "route-auth",
            "authorizerType": "REQUEST",
            "authorizerUri": uri,
            "identitySource": ["$request.header.x-token"],
            // No caching, so each call of a case is decided by its own invocation.
            "authorizerResultTtlInSeconds": 0,
            "authorizerPayloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateAuthorizer answered {status} {created}"
    );
    created
        .get("authorizerId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateAuthorizer answered no AuthorizerId"))
        .to_string()
}

/// One invoke of the deployed route, carrying `headers` and `query`.
async fn invoke(
    emulator: &MultiServiceRouter,
    host: &str,
    query: &str,
    headers: &[(&str, &str)],
) -> Answer {
    send(
        emulator,
        host,
        "GET",
        &format!("/{STAGE}{ROUTE_PATH}{query}"),
        headers,
    )
    .await
}

/// The route's answer once a backend that could RUN produced it: [`invoke`], looked at again
/// while the gateway is still refusing the call for a backend that did not answer in time and
/// `expected` is not that refusal itself (see [`BACKEND_DEADLINE`]).
async fn answered(
    emulator: &MultiServiceRouter,
    host: &str,
    query: &str,
    headers: &[(&str, &str)],
    expected: u16,
) -> Answer {
    let deadline = Instant::now() + BACKEND_DEADLINE;
    loop {
        let answer = invoke(emulator, host, query, headers).await;
        let transient = answer.status != expected
            && (answer.status == INTEGRATION_FAULT || answer.status == TIMED_OUT);
        if !transient || Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A handler that answers the structured response with the status the call asked for.
const STATUS_FROM_THE_QUERY: &str = "import json\n\
     def lambda_handler(event, context):\n\
     \x20   wanted = (event.get('queryStringParameters') or {}).get('status', '200')\n\
     \x20   return {'statusCode': int(wanted),\n\
     \x20           'headers': {'x-from-backend': 'yes', 'content-type': 'application/json'},\n\
     \x20           'body': json.dumps({'from': 'backend'})}\n";

#[tokio::test]
async fn a_status_keyed_response_mapping_transforms_the_backends_answer() {
    let case = "an integration whose ResponseParameters are keyed on two of the statuses its \
                backend answers";
    let (emulator, engine) = emulator_with_lambda();
    let function = given_function(
        &emulator,
        &engine,
        case,
        "mapped-function",
        STATUS_FROM_THE_QUERY,
    )
    .await;
    let mut integration = lambda_integration(&function, "2.0");
    integration["responseParameters"] = json!({
        // Keyed on the backend's own failure: the client is told the api's contract instead.
        "500": {
            "overwrite:statuscode": "403",
            "append:header.x-mapped": "by-the-api"
        },
        // Keyed on a success, mapping headers only: the status the backend named stands.
        "201": {
            "overwrite:header.x-from-backend": "$request.header.x-caller",
            // A `remove` operates on the header its key names and reads no value, so its
            // value is the empty STATIC value the mapping language spells quoted: a value
            // carried truly empty names no source, so the write stores no mapping for it
            // (`crate::state::parameter_mapping::stored_parameters`).
            "remove:header.content-type": "''"
        }
    });
    let (api_id, host) = given_api(&emulator, case).await;
    given_route(&emulator, case, &api_id, integration, None).await;

    /// One row: the status the backend answers, the status the client must read, and the
    /// header values the mapping owes it.
    type Row = (&'static str, u16, u16, Option<&'static str>, Option<&'static str>);
    // Rows differ along WHICH selection key the backend's status names: one that overwrites
    // the status, one that maps headers only, and a status the integration maps nothing for.
    let cases: [Row; 3] = [
        (
            "the status the mapping overwrites",
            500,
            403,
            Some("by-the-api"),
            Some("yes"),
        ),
        (
            "a status whose mapping only maps headers",
            201,
            201,
            None,
            Some("the-client"),
        ),
        ("a status the integration maps nothing for", 200, 200, None, Some("yes")),
    ];
    for (row, backend_status, expected_status, expected_mapped, expected_from_backend) in cases {
        let answer = answered(
            &emulator,
            &host,
            &format!("?status={backend_status}"),
            &[("x-caller", "the-client")],
            expected_status,
        )
        .await;
        assert_eq!(
            answer.status,
            expected_status,
            "case {case:?} / {row:?}: expected the client to read {expected_status}, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.header("x-mapped").as_deref(),
            expected_mapped,
            "case {case:?} / {row:?}: expected the appended header {expected_mapped:?}, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.header("x-from-backend").as_deref(),
            expected_from_backend,
            "case {case:?} / {row:?}: expected x-from-backend {expected_from_backend:?}, got {}",
            answer.summary()
        );
        // The mapping transforms the status and the headers; the body is the backend's own.
        assert_eq!(
            answer.text,
            r#"{"from": "backend"}"#,
            "case {case:?} / {row:?}: expected the backend's own body, got {}",
            answer.summary()
        );
    }
    // The header a `remove` dropped is gone from the answer the mapping selected it in, and
    // present in the one it did not.
    let dropped = answered(
        &emulator,
        &host,
        "?status=201",
        &[("x-caller", "the-client")],
        201,
    )
    .await;
    assert_eq!(
        dropped.header("content-type"),
        None,
        "case {case:?}: expected the removed header to be gone, got {}",
        dropped.summary()
    );
    let kept = answered(
        &emulator,
        &host,
        "?status=200",
        &[("x-caller", "the-client")],
        200,
    )
    .await;
    assert_eq!(
        kept.header("content-type").as_deref(),
        Some("application/json"),
        "case {case:?}: expected the backend's own content type where no mapping selected \
         it, got {}",
        kept.summary()
    );
}

#[tokio::test]
async fn a_proxy_response_the_format_cannot_be_read_as_is_the_gateways_own_fault() {
    /// One row: what the handler returns, under which payload format, and the status and body
    /// the client must read — `None` for the gateway's own integration-fault sentence.
    type Row = (&'static str, &'static str, &'static str, u16, Option<&'static str>);
    // Rows differ along the SHAPE of the function's return value: the structured response
    // both formats read, each member of it named as a type the format does not define, and
    // the two things `2.0` infers a response from.
    let cases: [Row; 9] = [
        (
            "the structured response under 1.0",
            "1.0",
            "{'statusCode': 200, 'body': 'served'}",
            200,
            Some("served"),
        ),
        (
            "a body that is an object, under 1.0",
            "1.0",
            "{'statusCode': 200, 'body': {'marker': 'object'}}",
            INTEGRATION_FAULT,
            None,
        ),
        (
            "a body that is an object, under 2.0",
            "2.0",
            "{'statusCode': 200, 'body': {'marker': 'object'}}",
            INTEGRATION_FAULT,
            None,
        ),
        (
            "a body that is a number",
            "2.0",
            "{'statusCode': 200, 'body': 7}",
            INTEGRATION_FAULT,
            None,
        ),
        (
            "headers that are not a map",
            "2.0",
            "{'statusCode': 200, 'headers': 'x-one: 1', 'body': 'served'}",
            INTEGRATION_FAULT,
            None,
        ),
        (
            "cookies that are not a list",
            "2.0",
            "{'statusCode': 200, 'cookies': 'a=1', 'body': 'served'}",
            INTEGRATION_FAULT,
            None,
        ),
        (
            "an isBase64Encoded that is not a flag",
            "2.0",
            "{'statusCode': 200, 'isBase64Encoded': 'yes', 'body': 'served'}",
            INTEGRATION_FAULT,
            None,
        ),
        (
            "a bare string, which 2.0 infers a 200 for",
            "2.0",
            "'inferred-text'",
            200,
            Some("inferred-text"),
        ),
        (
            "a bare string under 1.0, which infers nothing",
            "1.0",
            "'inferred-text'",
            INTEGRATION_FAULT,
            None,
        ),
    ];
    // ONE emulator for every row: each row needs a function of its own (its return value is
    // what the row varies) but not an engine of its own.
    let (emulator, engine) = emulator_with_lambda();
    for (index, (row, payload_format, returned, expected_status, expected_body)) in
        cases.into_iter().enumerate()
    {
        let source = format!("def lambda_handler(event, context):\n    return {returned}\n");
        let function = given_function(
            &emulator,
            &engine,
            row,
            &format!("shaped-function-{index}"),
            &source,
        )
        .await;
        let (api_id, host) = given_api(&emulator, row).await;
        given_route(
            &emulator,
            row,
            &api_id,
            lambda_integration(&function, payload_format),
            None,
        )
        .await;
        // A cold-start overrun of the INTEGRATION's own deadline is the gateway's `503`, not
        // its `500` ([`TIMED_OUT`]), so a row expecting the integration fault cannot be
        // satisfied by one: [`answered`] waits past the `503` and the assertion below is
        // about the shape the function returned.
        let answer = answered(&emulator, &host, "", &[], expected_status).await;
        assert_eq!(
            answer.status,
            expected_status,
            "case {row:?}: expected {expected_status} from the route, got {}",
            answer.summary()
        );
        match expected_body {
            Some(body) => assert_eq!(
                answer.text, body,
                "case {row:?}: expected the body {body:?}, got {}",
                answer.summary()
            ),
            None => assert_eq!(
                message(&answer),
                INTEGRATION_FAULT_MESSAGE,
                "case {row:?}: expected the integration-fault sentence, got {}",
                answer.summary()
            ),
        }
    }
}

#[tokio::test]
async fn the_backend_reads_the_observed_peer_appended_to_the_clients_forwarded_chain() {
    let case = "a route whose function echoes the headers the gateway relayed";
    let (emulator, engine) = emulator_with_lambda();
    let function = given_function(&emulator, &engine, case, "echo-function", ECHO_EVENT).await;
    let (api_id, host) = given_api(&emulator, case).await;
    given_route(
        &emulator,
        case,
        &api_id,
        lambda_integration(&function, "2.0"),
        None,
    )
    .await;

    /// One row: the chain the client claims, and the chain the backend must read.
    type Row = (&'static str, Option<&'static str>, String);
    // Rows differ along the chain the CLIENT sent: none at all, one element, several, and a
    // value that states nothing.
    let cases: [Row; 4] = [
        (
            "a client that sent no chain",
            None,
            OBSERVED_PEER.to_string(),
        ),
        (
            "a client that claimed one hop",
            Some("203.0.113.9"),
            format!("203.0.113.9, {OBSERVED_PEER}"),
        ),
        (
            "a client that claimed a chain of two",
            Some("203.0.113.9, 198.51.100.7"),
            format!("203.0.113.9, 198.51.100.7, {OBSERVED_PEER}"),
        ),
        (
            "a client whose chain is empty",
            Some(""),
            OBSERVED_PEER.to_string(),
        ),
    ];
    for (row, claimed, expected) in cases {
        let via = "2.0 client-claimed-hop, 1.1 another-claimed-hop";
        let mut headers: Vec<(&str, &str)> = vec![("via", via)];
        if let Some(claimed) = claimed {
            headers.push(("x-forwarded-for", claimed));
        }
        let answer = answered(&emulator, &host, "", &headers, 200).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?} / {row:?}: expected the function's 200, got {}",
            answer.summary()
        );
        let event: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
        let read = |name: &str| {
            event
                .pointer(&format!("/headers/{name}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            read("x-forwarded-for"),
            expected,
            "case {case:?} / {row:?}: expected the backend to read {expected:?}, got {}",
            answer.summary()
        );
        // The hop relays every other header as it arrived.
        assert_eq!(
            read("via"),
            via,
            "case {case:?} / {row:?}: expected the client's own Via, got {}",
            answer.summary()
        );
    }
}

#[tokio::test]
async fn a_lambda_authorizer_that_raises_unauthorized_refuses_the_call() {
    /// One row: the authorizer function's body, and the status and sentence the call earns.
    type Row = (&'static str, &'static str, u16, Option<&'static str>);
    // Rows differ along what the authorizer's FUNCTION did: raised the word that signals an
    // unauthenticated caller (in each of the spellings a handler raises it with), raised
    // something else, and answered a policy that allows.
    let cases: [Row; 4] = [
        (
            "an authorizer that raises Unauthorized",
            "raise Exception('Unauthorized')",
            UNAUTHORIZED,
            Some(UNAUTHORIZED_MESSAGE),
        ),
        (
            "an authorizer that raises Unauthorized from its own error type",
            "raise ValueError('Unauthorized')",
            UNAUTHORIZED,
            Some(UNAUTHORIZED_MESSAGE),
        ),
        (
            "an authorizer that raises any other error",
            "raise ValueError('boom')",
            INTEGRATION_FAULT,
            Some(INTEGRATION_FAULT_MESSAGE),
        ),
        (
            "an authorizer that allows the call",
            "return {'principalId': 'caller', 'policyDocument': {'Version': '2012-10-17', \
             'Statement': [{'Action': 'execute-api:Invoke', 'Effect': 'Allow', \
             'Resource': event['methodArn']}]}}",
            200,
            None,
        ),
    ];
    // ONE emulator and ONE backend for every row: what a row varies is the AUTHORIZER's
    // function, so each row creates that and an api of its own.
    let (emulator, engine) = emulator_with_lambda();
    let backend = given_function(
        &emulator,
        &engine,
        "the route behind the authorizer",
        "gated-function",
        "def lambda_handler(event, context):\n    \
         return {'statusCode': 200, 'body': 'served'}\n",
    )
    .await;
    for (index, (row, authorizer_body, expected_status, expected_message)) in
        cases.into_iter().enumerate()
    {
        let gate = given_function(
            &emulator,
            &engine,
            row,
            &format!("gate-function-{index}"),
            &format!("def lambda_handler(event, context):\n    {authorizer_body}\n"),
        )
        .await;
        // The authorizer belongs to the SAME api as the route it gates, so the api is
        // created first and both the authorizer and the route are made on it.
        let (api_id, host) = given_api(&emulator, row).await;
        let authorizer = given_authorizer(&emulator, row, &api_id, &gate).await;
        given_route(
            &emulator,
            row,
            &api_id,
            lambda_integration(&backend, "2.0"),
            Some(&authorizer),
        )
        .await;
        // The row whose expectation IS the gateway's own `500` is the one a cold start could
        // satisfy by accident: an authorizer whose decision could not be obtained AT ALL
        // answers that same `500`, whether the function raised or its runtime had not
        // started. So this row starts that runtime with a call whose answer is discarded, and
        // the assertion below is about what the function RAISED. Every other row waits for
        // the decision itself ([`answered`]).
        if expected_status == INTEGRATION_FAULT {
            let _started = invoke(&emulator, &host, "", &[("x-token", "presented")]).await;
        }
        let answer = answered(
            &emulator,
            &host,
            "",
            &[("x-token", "presented")],
            expected_status,
        )
        .await;
        assert_eq!(
            answer.status,
            expected_status,
            "case {row:?}: expected {expected_status} from the gated route, got {}",
            answer.summary()
        );
        match expected_message {
            Some(sentence) => assert_eq!(
                message(&answer),
                sentence,
                "case {row:?}: expected the sentence {sentence:?}, got {}",
                answer.summary()
            ),
            None => assert_eq!(
                answer.text, "served",
                "case {row:?}: expected the backend's own body, got {}",
                answer.summary()
            ),
        }
    }
}
