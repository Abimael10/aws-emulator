// SPDX-License-Identifier: CC0-1.0

//! The invoke address API Gateway v2 ANNOUNCES for a stage is one a client can carry in a
//! stage variable and then DIAL.
//!
//! RULE: `Stage$StageVariables` documents that "the values must match
//! `[A-Za-z0-9-._~:/?#&=,]+`", and `Api$ApiEndpoint` documents the invoke URI as
//! `{api-id}.execute-api.{region}.amazonaws.com` with "the stage name … typically appended"
//! — the reserved stage that serves the api's ROOT being reached with no stage segment at
//! all. So an address this service announces for a stage has to be spelled with characters
//! that pattern admits, and it has to lead BACK to that very stage of that very api: a client
//! parameterising an `HTTP_PROXY` backend per stage, which is what a stage variable is for,
//! both writes that address into `StageVariables` and dials it.
//!
//! Both halves are asserted on ONE emulator, so neither can drift out of the other's reach:
//! the value every row sends is read off the api the row just created, the `CreateStage` that
//! carries it must be accepted, and the address is then DIALLED through the front door — a
//! request the backend api's own route answers, which is what proves the announced address
//! resolves rather than merely being spellable.
//!
//! This suite lives beside the router because the `execute-api` endpoint space is SHARED by
//! the two API Gateway data planes: a REST api is reached at the same host shape and at the
//! same path-carried address, and that service is mounted here beside this one, so a dialled
//! address that resolves at all is one exactly one of them claimed.
//!
//! What is NOT restated here, because the owning crate's own suites pin it: that a value
//! inside the documented character set is stored and read back, and that an announced address
//! is one of those values
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/stage_variable_invoke_url_value.rs`),
//! and what the data plane DOES with a `${stageVariables.*}` reference, including the answer
//! for a reference the serving stage carries no variable for
//! (`services/.../tests/all/stage_variable_backend_selection.rs`). Those suites hold one
//! emulator carrying this service alone; what is added here is the two CONTROL-PLANE
//! addressings, which announce two different forms of invoke address, and the end-to-end DIAL
//! of the announced address on an emulator where the REST plane claims the same space.
//!
//! The rows span what the rule generalises over: the addressing the client reached the
//! control plane at (one of AWS's own service hosts, and the emulator's own endpoint, which
//! announce two different forms of invoke address), and the stage the address names (the
//! reserved stage served at the api's root, and a named stage appended to the endpoint).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every control-plane request below is signed for.
const REGION: &str = "eu-west-1";

/// The reserved stage that serves an api's root, so the address announced for it is the api's
/// own endpoint with no stage segment appended.
const ROOT_STAGE: &str = "$default";

/// The characters `Stage$StageVariables` admits in a value: "the values must match
/// `[A-Za-z0-9-._~:/?#&=,]+`" — the set is quoted from that member's documentation
/// (`smithy_query(query="doc", shape="Stage", member="StageVariables")`), and it is the
/// grammar an announced invoke address has to be spelled within.
const STAGE_VARIABLE_VALUE_PUNCTUATION: &str = "-._~:/?#&=,";

/// The redirect an `HTTP_PROXY` route answers with: the caller is sent to the upstream the
/// integration resolved rather than the gateway connecting to it.
const TEMPORARY_REDIRECT: u16 = 307;

/// The status and sentence an HTTP api answers for a matched route whose integration could
/// produce no response, which is what a reference to a variable the serving stage does not
/// carry names: no backend at all.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// One emulator carrying API Gateway v2 and the API Gateway service that owns REST apis —
/// the two planes that share the `execute-api` endpoint space — over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope routes the request to API Gateway;
/// the signature is never checked.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/apigateway\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One API Gateway v2 control-plane call, addressed at `host` — the endpoint the CLIENT
/// reached, which is what decides the address the api announces — as `(status, parsed body)`.
async fn control(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// What a dialled address answered: its status, its `location` header and its body.
struct Answer {
    status: u16,
    location: Option<String>,
    body: Value,
}

impl Answer {
    /// The `message` the body carries, or `""` for one that carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Enough of the answer to identify it in an assertion message.
    fn summary(&self) -> String {
        format!("{} {} (location: {:?})", self.status, self.body, self.location)
    }
}

/// DIAL `url` — a full `scheme://host[/path]` — the way a client dials an address it was
/// given: the authority becomes the request's `Host` and the rest becomes its path.
///
/// The request is handed straight to the assembled router IN PROCESS, so no socket is opened
/// and nothing leaves this test: the authority is only the address the request carries.
async fn dial(router: &MultiServiceRouter, url: &str) -> Answer {
    let without_scheme = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    let (host, path) = match without_scheme.find('/') {
        Some(end) => (&without_scheme[..end], &without_scheme[end..]),
        None => (without_scheme, "/"),
    };
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get("location")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Answer {
        status,
        location,
        body,
    }
}

/// The member `member` of `body`, as text.
fn as_str(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {body}"))
        .to_string()
}

/// The host-and-path of an announced endpoint: everything after the scheme delimiter, which
/// is what a client interpolates into an `HTTP_PROXY` `IntegrationUri`. The same reading the
/// owning crate's `tests/all/stage_variable_invoke_url_value.rs` applies to the same value;
/// it cannot be imported from here, because that is another crate's test target.
fn host_and_path(endpoint: &str) -> &str {
    endpoint
        .split_once("://")
        .map_or(endpoint, |(_scheme, rest)| rest)
}

/// Whether every character of `value` is one `Stage$StageVariables` admits.
fn within_the_documented_value_grammar(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || STAGE_VARIABLE_VALUE_PUNCTUATION.contains(character)
        })
}

/// An HTTP api with one `HTTP_PROXY` route over `upstream`, deployed on `stage`; answers the
/// address a client builds to invoke that stage.
///
/// The route's integration names its upstream literally, so the answer a dial of the returned
/// address produces identifies THIS api and THIS stage: a redirect to that upstream.
async fn given_backend(
    router: &MultiServiceRouter,
    case: &str,
    control_host: &str,
    name: &str,
    stage: &str,
    upstream: &str,
) -> String {
    let (status, api) = control(
        router,
        control_host,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi for the backend, got {status} {api}"
    );
    let api_id = as_str(case, &api, "apiId");
    let announced = as_str(case, &api, "apiEndpoint");

    let (status, integration) = control(
        router,
        control_host,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": upstream,
            "integrationMethod": "ANY",
            "payloadFormatVersion": "1.0",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegration for the backend, got {status} \
         {integration}"
    );
    let (status, route) = control(
        router,
        control_host,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": "GET /front-invoke",
            "target": format!("integrations/{}", as_str(case, &integration, "integrationId")),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute for the backend, got {status} {route}"
    );
    let (status, created) = control(
        router,
        control_host,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateStage for the backend, got {status} {created}"
    );

    // The address a client builds for that stage: the endpoint the api ANNOUNCED, with the
    // stage name appended for a named stage and nothing appended for the stage that serves
    // the api's root.
    let endpoint = host_and_path(&announced);
    if stage == ROOT_STAGE {
        endpoint.to_string()
    } else {
        format!("{endpoint}/{stage}")
    }
}

/// One row: the endpoint the client reached the control plane at, and the backend stage whose
/// announced address the front api's stage variable carries.
struct Row {
    case: &'static str,
    control_host: &'static str,
    backend_stage: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client of AWS's own hosts, reading the root stage's address",
            control_host: "apigateway.eu-west-1.amazonaws.com",
            backend_stage: ROOT_STAGE,
        },
        Row {
            case: "a client of AWS's own hosts, reading a named stage's address",
            control_host: "apigateway.eu-west-1.amazonaws.com",
            backend_stage: "prod",
        },
        Row {
            case: "a client at the emulator's own endpoint, reading the root stage's address",
            // A loopback authority, which is what a client configured with one local endpoint
            // URL sends: it is carried in the request's `Host` only — nothing is dialled over
            // a socket here.
            control_host: "127.0.0.1:9297",
            backend_stage: ROOT_STAGE,
        },
        Row {
            case: "a client at the emulator's own endpoint, reading a named stage's address",
            control_host: "127.0.0.1:9297",
            backend_stage: "blue_green-2",
        },
    ]
}

#[tokio::test]
async fn an_announced_stage_address_is_an_accepted_stage_variable_value_that_leads_back_to_it() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let upstream = format!("https://upstream-{index}.backend.test/reached");
        let backend_address = given_backend(
            &router,
            case,
            row.control_host,
            &format!("backend-{index}"),
            row.backend_stage,
            &upstream,
        )
        .await;

        // Half one: the address this service announced is spelled within the grammar its own
        // `StageVariables` validator enforces.
        assert!(
            within_the_documented_value_grammar(&backend_address),
            "case {case:?}: the announced address {backend_address:?} carries a character \
             outside the documented stage-variable value set"
        );

        // The FRONT api: one `HTTP_PROXY` integration naming its backend through a stage
        // variable, one route over it, and one stage carrying the announced address.
        let (status, api) = control(
            &router,
            row.control_host,
            "POST",
            "/v2/apis",
            Some(json!({ "name": format!("front-{index}"), "protocolType": "HTTP" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateApi for the front api, got {status} {api}"
        );
        let front_id = as_str(case, &api, "apiId");
        let front_endpoint = as_str(case, &api, "apiEndpoint");

        let (status, integration) = control(
            &router,
            row.control_host,
            "POST",
            &format!("/v2/apis/{front_id}/integrations"),
            Some(json!({
                "integrationType": "HTTP_PROXY",
                "integrationUri": "https://${stageVariables.backendUrl}/front-invoke",
                "integrationMethod": "GET",
                "payloadFormatVersion": "1.0",
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateIntegration, got {status} {integration}"
        );
        let (status, route) = control(
            &router,
            row.control_host,
            "POST",
            &format!("/v2/apis/{front_id}/routes"),
            Some(json!({
                "routeKey": "GET /front-invoke",
                "target": format!("integrations/{}", as_str(case, &integration, "integrationId")),
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoute, got {status} {route}"
        );

        let front_stage = format!("sa{index}");
        let (status, created) = control(
            &router,
            row.control_host,
            "POST",
            &format!("/v2/apis/{front_id}/stages"),
            Some(json!({
                "stageName": front_stage,
                "autoDeploy": true,
                "stageVariables": { "backendUrl": backend_address },
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateStage for the announced address \
             {backend_address:?}, got {status} {created}"
        );

        // The round trip: the reader reports the address the create persisted, character for
        // character.
        let (status, read) = control(
            &router,
            row.control_host,
            "GET",
            &format!("/v2/apis/{front_id}/stages/{front_stage}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetStage, got {status} {read}"
        );
        assert_eq!(
            read.get("stageVariables"),
            Some(&json!({ "backendUrl": backend_address })),
            "case {case:?}: expected GetStage to report the announced address it stored, got \
             {read}"
        );

        // Half two: the front api's route resolves the variable into the announced address …
        let front_address = format!(
            "https://{}/{front_stage}/front-invoke",
            host_and_path(&front_endpoint)
        );
        let answer = dial(&router, &front_address).await;
        let expected = format!("https://{backend_address}/front-invoke");
        assert_eq!(
            answer.status,
            TEMPORARY_REDIRECT,
            "case {case:?}: expected the front api's route to send the caller to the backend \
             its stage selects, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.location.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the caller to be sent to {expected:?}, got {}",
            answer.summary()
        );

        // … and that address, DIALLED, is answered by the backend api's own route, which is
        // what identifies the api and the stage it reached.
        let reached = dial(&router, &expected).await;
        assert_eq!(
            reached.status,
            TEMPORARY_REDIRECT,
            "case {case:?}: expected the announced address {expected:?} to be served by the \
             backend api's route, got {}",
            reached.summary()
        );
        assert_eq!(
            reached.location.as_deref(),
            Some(upstream.as_str()),
            "case {case:?}: expected the dialled address to reach the backend whose route \
             fronts {upstream:?}, got {}",
            reached.summary()
        );
    }
}

/// A reference the serving stage carries no variable for names no backend at all. The status
/// and sentence that is answered with are
/// `services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/stage_variable_backend_selection.rs`'s
/// rule, asserted here too because the other half belongs to the address rule above: the
/// caller is sent NOWHERE, so no `Location` naming a URI that still carries the unresolved
/// reference reaches it.
#[tokio::test]
async fn an_unresolved_backend_reference_sends_the_caller_nowhere() {
    let router = emulator();
    let control_host = "apigateway.eu-west-1.amazonaws.com";
    let case = "a stage carrying no variable the integration's URI names";

    let (status, api) = control(
        &router,
        control_host,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "front-bare", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(case, &api, "apiId");
    let endpoint = as_str(case, &api, "apiEndpoint");

    let (status, integration) = control(
        &router,
        control_host,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": "https://${stageVariables.backendUrl}/front-invoke",
            "integrationMethod": "GET",
            "payloadFormatVersion": "1.0",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegration, got {status} {integration}"
    );
    let (status, route) = control(
        &router,
        control_host,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": "GET /front-invoke",
            "target": format!("integrations/{}", as_str(case, &integration, "integrationId")),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {route}"
    );
    let (status, created) = control(
        &router,
        control_host,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": "bare", "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateStage, got {status} {created}"
    );

    let answer = dial(
        &router,
        &format!("https://{}/bare/front-invoke", host_and_path(&endpoint)),
    )
    .await;
    assert_eq!(
        answer.status,
        INTEGRATION_FAULT,
        "case {case:?}: a reference the serving stage carries no variable for names no \
         backend, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.message(),
        INTEGRATION_FAULT_MESSAGE,
        "case {case:?}: expected the integration-fault sentence, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.location, None,
        "case {case:?}: nothing may be sent to an unresolved backend, got {}",
        answer.summary()
    );
}
