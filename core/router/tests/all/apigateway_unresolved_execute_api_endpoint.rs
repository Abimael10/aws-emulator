// SPDX-License-Identifier: CC0-1.0

//! What the `execute-api` endpoint space answers for an api id it resolves to NO api.
//!
//! The rule under test, one level above the recorded case (a REST api deleted while a client
//! kept calling its stage URL):
//!
//! > AWS serves ONE `execute-api` endpoint space for both kinds of API Gateway api, and an
//! > address in it whose api id names no api is answered by API Gateway itself — not by the
//! > front door's "no service claimed this" refusal, and not by an api's own protocol, since
//! > nothing of an api is read for it. WHICH answer it gets follows whether the address names
//! > a STAGE to place: a path carrying a segment names one, and that request is turned away
//! > with `403` `{"message":"Forbidden"}` whichever addressing carried the id and whether the
//! > id was deleted or never minted, while the endpoint's ROOT names no stage anywhere and is
//! > answered `404` `{"message":"Not Found"}`. An api the space DOES resolve keeps answering
//! > from its own records, so a path that api serves nothing at is its own answer and not this
//! > one.
//!
//! Only a cross-service suite can pin it: the endpoint space is shared by API Gateway (REST)
//! and API Gateway V2, each of which holds its own apis, so an id neither holds is claimed by
//! exactly one of the two in the ASSEMBLED router — which is what this suite drives.
//!
//! The rows differ along what the rule generalises over: how the address carries the api id
//! (the `execute-api` host AWS documents, and the reserved path prefix a client that resolves
//! that host nowhere uses), why the id resolves to nothing (a REST api deleted, an HTTP api
//! deleted, an id nothing ever minted), whether the path names a stage or is the endpoint's
//! own root, and — as the contrast rows — a live REST stage that serves the request and a path
//! of that same api that serves nothing.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every invoke host names.
const REGION: &str = "us-east-1";

/// The stage the live api is deployed to, and the first segment of every invoke path below.
const STAGE: &str = "test";

/// The endpoint a client on the machine this emulator runs on really dials: a host that names
/// no service of it at all, which is why such a client carries the api's endpoint in the path.
const LOCAL_ENDPOINT: &str = "127.0.0.1:9297";

/// What the live api's `MOCK` method answers.
const SERVED_BODY: &str = r#"{"at":"root"}"#;

/// The answer the endpoint space owes an api id it resolves to nothing at an address that
/// NAMES a stage to place.
const FORBIDDEN: (u16, &str) = (403, "Forbidden");

/// The answer it owes the ROOT of such an endpoint, which names no stage at all: an HTTP
/// api's url is that root, so this is what a client polling the url of an api it deleted
/// reads (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/
/// http_api_unresolved_endpoint_root.rs` pins the rule over the addressings and methods).
const NOT_FOUND: (u16, &str) = (404, "Not Found");

/// The answer a LIVE api owes a path its deployment serves no resource for — the taxonomy
/// `services/service-com-amazonaws-apigateway-backplanecontrolservice/tests/all/stage_invoke.rs`
/// pins — which is what makes the rows above an answer about the ENDPOINT SPACE rather than a
/// blanket refusal.
const MISSING_TOKEN: (u16, &str) = (403, "Missing Authentication Token");

/// The error code the front door answers when NO service claims a request: the one answer an
/// `execute-api` address must never get, since it is the emulator refusing a URL API Gateway
/// itself published.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// A SigV4 credential scope naming the signing name both API Gateway control planes share,
/// which is how a control-plane call resolves its routing key. The signature itself is the
/// emulator's business and is not verified here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/apigateway/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// One control-plane call, answering the status and the parsed body.
async fn call(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{LOCAL_ENDPOINT}{uri}"))
        .header("host", LOCAL_ENDPOINT)
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION)
        .body(bytes::Bytes::from(payload))
        .expect("the control-plane request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body().as_ref()).to_string();
    let parsed = serde_json::from_str(&text).unwrap_or(Value::String(text));
    assert!(
        status < 500,
        "setup {case}: {method} {uri} answered {status} {parsed:?}"
    );
    (status, parsed)
}

/// One control-plane call asserted to have been accepted with `expected`.
async fn accepted(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let (status, answered) = call(router, case, method, uri, body).await;
    assert_eq!(
        status, expected,
        "setup {case}: expected {expected} from {method} {uri}, got {status} {answered:?}"
    );
    answered
}

/// One answered invoke: the status, the `message` member every API Gateway gateway response
/// carries, and the body verbatim.
struct Invoked {
    status: u16,
    message: Option<String>,
    body: String,
}

/// Issue one unsigned GET to `host` at `path` — what a client of a published invoke URL
/// sends.
async fn invoke(router: &MultiServiceRouter, host: &str, path: &str) -> Invoked {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the invoke request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body().as_ref()).to_string();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|parsed| parsed.get("message")?.as_str().map(str::to_owned));
    Invoked {
        status,
        message,
        body,
    }
}

/// The `execute-api` host AWS documents for the api `id`.
fn endpoint_host(id: &str) -> String {
    format!("{id}.execute-api.{REGION}.amazonaws.com")
}

/// The path a client that resolves [`endpoint_host`] nowhere carries the api's endpoint in,
/// with `path` behind it.
fn prefixed_path(id: &str, path: &str) -> String {
    format!("/_aws/execute-api/{id}{path}")
}

/// A REST api carrying one `MOCK` `GET` on its root resource, deployed to [`STAGE`].
/// Answers the api id.
async fn deployed_rest_api(router: &MultiServiceRouter, name: &str) -> String {
    let created = accepted(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    let api = created["id"]
        .as_str()
        .expect("CreateRestApi reports the api id")
        .to_string();
    let root = created["rootResourceId"]
        .as_str()
        .expect("CreateRestApi reports the root resource id")
        .to_string();
    let method = format!("/restapis/{api}/resources/{root}/methods/GET");
    accepted(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    accepted(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;
    accepted(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    accepted(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({ "responseTemplates": { "application/json": SERVED_BODY } })),
        201,
    )
    .await;
    accepted(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// An HTTP api of the V2 service, created and then deleted: its id is one that USED to name
/// an api of the other half of the endpoint space.
async fn deleted_http_api(router: &MultiServiceRouter, name: &str) -> String {
    let created = accepted(
        router,
        "CreateApi",
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api = created["apiId"]
        .as_str()
        .expect("CreateApi reports the api id")
        .to_string();
    accepted(
        router,
        "DeleteApi",
        "DELETE",
        &format!("/v2/apis/{api}"),
        None,
        204,
    )
    .await;
    api
}

#[test]
fn an_execute_api_address_whose_api_id_resolves_to_nothing_is_answered_by_the_space() {
    std::thread::Builder::new()
        .name("apigateway-unresolved-endpoint".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to these cases: what
                // they are about is which api an address resolves to.
                router.set_request_authentication(false);

                // One live REST api, one deleted REST api, one deleted HTTP api, and an id
                // nothing ever minted.
                let live = deployed_rest_api(&router, "unresolved-endpoint-live").await;
                let gone = deployed_rest_api(&router, "unresolved-endpoint-gone").await;
                accepted(
                    &router,
                    "DeleteRestApi",
                    "DELETE",
                    &format!("/restapis/{gone}"),
                    None,
                    202,
                )
                .await;
                let gone_http = deleted_http_api(&router, "unresolved-endpoint-http").await;
                let never_minted = "zzz9999999";

                let stage_path = format!("/{STAGE}/");
                // Each row: the case, the host the request is addressed to, the path it
                // carries, and the answer the endpoint space owes it.
                let rows: Vec<(String, String, String, (u16, &str))> = vec![
                    (
                        "a deleted REST api, on the reserved path prefix".to_string(),
                        LOCAL_ENDPOINT.to_string(),
                        prefixed_path(&gone, &stage_path),
                        FORBIDDEN,
                    ),
                    (
                        "a deleted REST api, on its own execute-api host".to_string(),
                        endpoint_host(&gone),
                        stage_path.clone(),
                        FORBIDDEN,
                    ),
                    (
                        "a deleted HTTP api, on the reserved path prefix".to_string(),
                        LOCAL_ENDPOINT.to_string(),
                        prefixed_path(&gone_http, &stage_path),
                        FORBIDDEN,
                    ),
                    (
                        "a deleted HTTP api, on its own execute-api host".to_string(),
                        endpoint_host(&gone_http),
                        stage_path.clone(),
                        FORBIDDEN,
                    ),
                    (
                        "an api id nothing ever minted, on the reserved path prefix".to_string(),
                        LOCAL_ENDPOINT.to_string(),
                        prefixed_path(never_minted, &stage_path),
                        FORBIDDEN,
                    ),
                    (
                        "an api id nothing ever minted, on an execute-api host".to_string(),
                        endpoint_host(never_minted),
                        stage_path.clone(),
                        FORBIDDEN,
                    ),
                    (
                        "the ROOT of a deleted REST api's endpoint".to_string(),
                        endpoint_host(&gone),
                        "/".to_string(),
                        NOT_FOUND,
                    ),
                    (
                        "the ROOT of a deleted HTTP api's endpoint".to_string(),
                        endpoint_host(&gone_http),
                        "/".to_string(),
                        NOT_FOUND,
                    ),
                    (
                        "the ROOT of a deleted HTTP api's endpoint, on the reserved path \
                         prefix"
                            .to_string(),
                        LOCAL_ENDPOINT.to_string(),
                        prefixed_path(&gone_http, "/"),
                        NOT_FOUND,
                    ),
                    (
                        "the ROOT of an endpoint whose api id nothing ever minted".to_string(),
                        endpoint_host(never_minted),
                        "/".to_string(),
                        NOT_FOUND,
                    ),
                    (
                        "the live api's deployed stage".to_string(),
                        endpoint_host(&live),
                        stage_path.clone(),
                        (200, SERVED_BODY),
                    ),
                    (
                        "the live api's deployed stage, on the reserved path prefix".to_string(),
                        LOCAL_ENDPOINT.to_string(),
                        prefixed_path(&live, &stage_path),
                        (200, SERVED_BODY),
                    ),
                    (
                        "a path the live api's deployment serves nothing at".to_string(),
                        endpoint_host(&live),
                        format!("/{STAGE}/absent"),
                        MISSING_TOKEN,
                    ),
                ];

                for (case, host, path, (status, message)) in rows {
                    let answered = invoke(&router, &host, &path).await;
                    assert!(
                        !answered.body.contains(UNKNOWN_SERVICE),
                        "case {case:?}: an execute-api address must never be answered the \
                         front door's {UNKNOWN_SERVICE} refusal, got {} {:?}",
                        answered.status,
                        answered.body
                    );
                    assert_eq!(
                        answered.status, status,
                        "case {case:?}: expected HTTP {status}, got {} {:?}",
                        answered.status, answered.body
                    );
                    if answered.status == 200 {
                        assert_eq!(
                            answered.body, message,
                            "case {case:?}: expected the served body {message:?}, got {:?}",
                            answered.body
                        );
                    } else {
                        assert_eq!(
                            answered.message.as_deref(),
                            Some(message),
                            "case {case:?}: expected the sentence {message:?}, got {:?}",
                            answered.body
                        );
                    }
                }
            });
        })
        .expect("spawn the unresolved-endpoint thread")
        .join()
        .expect("the unresolved-endpoint thread panicked");
}
