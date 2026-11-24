// SPDX-License-Identifier: CC0-1.0

//! The `execute-api` data plane resolves a `${stageVariables.<name>}` reference in an
//! `HTTP_PROXY` integration's URI against the STAGE that serves the request.
//!
//! The rule these cases assert: **"you can use API Gateway stage variables … as part of an
//! HTTP integration URL", so the backend an `HTTP_PROXY` route reaches is the integration's
//! `IntegrationUri` with the SERVING stage's own variables interpolated — one api therefore
//! reaches a different backend per stage, from ONE route and ONE integration record — and a
//! reference the serving stage carries no variable for names no backend at all, which is
//! answered as an integration that produced no response (`500`
//! `{"message":"Internal Server Error"}`) rather than by reaching a URI that still carries the
//! reference.**
//!
//! What a value may BE, and that the invoke URL this control plane announces is one of
//! them, is `stage_variable_values.rs`'s rule and is not restated here; this suite starts
//! from values that suite admits and asserts what the data plane DOES with them.
//!
//! Each backend is an api of this emulator, so the front api's route really is forwarded to
//! the backend its stage selected ([`backend_upstream`] is how each backend's own answer says
//! which one it is). That the hop is made at all for an endpoint this emulator serves, and the
//! redirect every other upstream earns, is `http_api_backend_dispatch.rs`'s rule.
//!
//! The rows span what the rule generalises over: which stage of the api serves the request
//! (two stages naming two different backends, and one naming none), and where the reference
//! sits in the URI (its host, with path following it).

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator};
use crate::websocket_handshake_data_plane::{message, send};

/// The stage that serves an api's root, which is reached with no stage segment in the path
/// at all — so the invoke URL announced for it is the api's own endpoint.
const ROOT_STAGE: &str = "$default";

/// The status and sentence an HTTP api answers for a matched route whose integration could
/// produce no response — which is what a reference to a stage variable the serving stage
/// does not carry names: no backend.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The status an `HTTP_PROXY` upstream this emulator serves nothing at is answered with: the
/// caller is sent there, because this emulator makes no request of its own to it. It is what
/// each BACKEND api below answers, and so what the front api's hop passes back to the client.
const TEMPORARY_REDIRECT: u16 = 307;

/// The address the backend `name` answers its own callers with: one URI per backend, which is
/// how the front api's answer says which backend served it. Nothing in this emulator serves
/// it, so it is the backend's caller's to reach.
fn backend_upstream(name: &str) -> String {
    format!("https://{name}.example.test/items")
}

/// Create an api and answer `(ApiId, the ApiEndpoint it announces)`.
async fn create_api(emulator: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, api) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {api}"
    );
    (as_str(&api, "apiId"), as_str(&api, "apiEndpoint"))
}

/// One `HTTP_PROXY` integration over `upstream` and one route over it, on `api_id`.
/// Answers the `IntegrationId` the api minted.
async fn create_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    upstream: &str,
    integration_method: &str,
) -> String {
    let (status, integration) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": upstream,
            "integrationMethod": integration_method,
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegration, got {status} {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");
    let (status, route) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": route_key,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {route}"
    );
    integration_id
}

/// One stage of `api_id`, auto-deploying, carrying `variables` where a row names any.
async fn create_stage(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage_name: &str,
    backend: Option<&str>,
) {
    let mut request = json!({ "stageName": stage_name, "autoDeploy": true });
    if let Some(value) = backend {
        request["stageVariables"] = json!({ "backendUrl": value });
    }
    let (status, stage) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(request),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateStage, got {status} {stage}"
    );
}

/// The host-and-path of the invoke URL a client builds for `stage` of the api that announced
/// `endpoint`: "the stage name is typically appended to this URI to form a complete path to
/// a deployed API stage" (`Api$ApiEndpoint`), and the stage that serves the api's root is
/// reached at the endpoint itself. The scheme is dropped, because what the integration URI
/// below interpolates the variable into is the backend's host.
fn announced_host_and_path(endpoint: &str, stage: &str) -> String {
    let host_and_path = endpoint
        .split_once("://")
        .map_or(endpoint, |(_scheme, rest)| rest);
    if stage == ROOT_STAGE {
        host_and_path.to_string()
    } else {
        format!("{host_and_path}/{stage}")
    }
}

#[tokio::test]
async fn an_http_proxy_route_reaches_the_backend_the_serving_stage_s_variable_names() {
    let emulator = emulator();

    // Two BACKEND apis, each announcing its own invoke URL: the hosts the front api's
    // stages will each select. Both values are ones `stage_variable_values.rs` admits. Each
    // serves `GET /items` with an upstream of its OWN, which is what its answer identifies it
    // by.
    let (first_id, first_endpoint) = create_api(&emulator, "the first backend", "backend-one").await;
    create_route(
        &emulator,
        "the first backend",
        &first_id,
        "GET /items",
        &backend_upstream("backend-one"),
        "GET",
    )
    .await;
    create_stage(&emulator, "the first backend", &first_id, "prod", None).await;
    let (second_id, second_endpoint) =
        create_api(&emulator, "the second backend", "backend-two").await;
    create_route(
        &emulator,
        "the second backend",
        &second_id,
        "GET /items",
        &backend_upstream("backend-two"),
        "GET",
    )
    .await;
    create_stage(&emulator, "the second backend", &second_id, ROOT_STAGE, None).await;
    let first_backend = announced_host_and_path(&first_endpoint, "prod");
    let second_backend = announced_host_and_path(&second_endpoint, ROOT_STAGE);

    // The FRONT api: one `HTTP_PROXY` integration whose URI names the backend through a
    // stage variable, and one route over it.
    let (api_id, front_endpoint) = create_api(&emulator, "the front api", "front-api").await;
    create_route(
        &emulator,
        "the front api",
        &api_id,
        "GET /items",
        "https://${stageVariables.backendUrl}/items",
        "ANY",
    )
    .await;

    // Three stages of that ONE api: two naming a backend each, and one naming none.
    for (case, stage_name, variables) in [
        (
            "the stage pointed at the first backend",
            "one",
            Some(first_backend.clone()),
        ),
        (
            "the stage pointed at the second backend",
            "two",
            Some(second_backend.clone()),
        ),
        ("the stage carrying no such variable", "bare", None),
    ] {
        create_stage(&emulator, case, &api_id, stage_name, variables.as_deref()).await;
    }

    let host = announced_host_and_path(&front_endpoint, ROOT_STAGE);
    // The rows differ along which stage serves the request, so the SAME route and the SAME
    // integration record reach a different backend per stage — and each backend's own answer
    // says which one was reached.
    for (case, stage_name, expected) in [
        (
            "the stage pointed at the first backend",
            "one",
            Some(backend_upstream("backend-one")),
        ),
        (
            "the stage pointed at the second backend",
            "two",
            Some(backend_upstream("backend-two")),
        ),
        ("the stage carrying no such variable", "bare", None),
    ] {
        let answer = send(
            &emulator,
            &host,
            "GET",
            &format!("/{stage_name}/items"),
            &[],
        )
        .await;
        match expected {
            Some(location) => {
                assert_eq!(
                    answer.status,
                    TEMPORARY_REDIRECT,
                    "case {case:?}: expected the answer of the backend this stage selects, \
                     got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.location.as_deref(),
                    Some(location.as_str()),
                    "case {case:?}: expected the selected backend's own answer to name \
                     {location:?}, got {}",
                    answer.summary()
                );
            }
            None => {
                assert_eq!(
                    answer.status,
                    INTEGRATION_FAULT,
                    "case {case:?}: a reference to a variable this stage does not carry \
                     names no backend, got {}",
                    answer.summary()
                );
                assert_eq!(
                    message(&answer),
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
        }
    }
}
