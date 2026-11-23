// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a SIMULATED invocation of a method whose integration names an HTTP
//! ENDPOINT: `TestInvokeMethod` performs it against that endpoint.
//!
//! THE RULE UNDER TEST: `TestInvokeMethod` performs the addressed method's integration out of
//! the api's live records, including the two types that name an HTTP endpoint — so the backend
//! the `uri` names is what answers the simulation, with no stage serving the method and no
//! deployment of the api at all. The configuration is read against the `stageVariables` the
//! call supplies ("a key-value map of stage variables to simulate an invocation on a deployed
//! Stage"), so a `${stageVariables.*}` reference in the `uri` resolves to the value that map
//! gives it and two calls of ONE method reach two different backends. An endpoint nothing in
//! this emulator serves, and a reference the call supplies no value for, are each answered
//! through the api's `API_CONFIGURATION_ERROR` gateway response rather than with a backend
//! answer that was never obtained.
//!
//! The rows span what the rule generalises over: the integration type (`HTTP_PROXY`, whose
//! answer is the method response, and the standard `HTTP`, whose answer the integration's own
//! responses map), the value the supplied map gives the reference (two backends, so the
//! resolved address differs), whether the map carries the referenced name at all, and whether
//! the address names an endpoint of this emulator.
//!
//! Every case drives the real wire path — a `restJson1` request through the multi-service
//! router into the mounted service — and the subject api is NEVER deployed, which the
//! `GetDeployments` assertion below pins.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, json_template, method_response, mock_resource, put_integration, put_method,
};
use crate::rest_api_lifecycle::{create_api, page_items, resource_under, Gateway};
use crate::test_invoke_method::test_invoke;

/// The Region the backend's invoke host is addressed in.
const REGION: &str = "us-east-1";

/// The stage the BACKEND api is deployed to. The api under test is deployed to none.
const BACKEND_STAGE: &str = "back";

/// The two backend routes a stage variable selects between.
const ALPHA: &str = "alpha";

/// See [`ALPHA`].
const BETA: &str = "beta";

/// The name of the stage variable the integration's `uri` references.
const TARGET: &str = "target";

/// The body every uncustomized `API_CONFIGURATION_ERROR` gateway response carries, which is
/// what a call whose integration cannot be performed is answered with.
const INTERNAL_SERVER_ERROR: &str = "Internal server error";

/// The body the backend route `route` answers, which says WHICH backend the simulation
/// reached.
fn backend_body(route: &str) -> String {
    format!("{{\"route\":\"{route}\"}}")
}

/// The backend api: one deployed `MOCK` route per [`ALPHA`]/[`BETA`], each answering its own
/// body, as its id.
async fn backend(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "test-invoke-http-backend").await;
    for route in [ALPHA, BETA] {
        mock_resource(
            gateway,
            &api,
            &root,
            route,
            "GET",
            "200",
            json_template(&backend_body(route)),
        )
        .await;
    }
    deploy(gateway, &api, BACKEND_STAGE).await;
    api
}

/// The invoke URL of the backend's deployed stage, with the stage-variable reference where the
/// route goes: the address the method under test integrates with.
fn backend_uri(api: &str) -> String {
    format!(
        "https://{api}.execute-api.{REGION}.amazonaws.com/{BACKEND_STAGE}/${{stageVariables.{TARGET}}}"
    )
}

/// One resource of the api under test carrying a `GET` whose integration is `integration`, as
/// its resource id. A standard `HTTP` integration's answer is MAPPED by the integration's own
/// responses, so the method also declares the response that maps it.
async fn method_with(
    gateway: &Gateway,
    api: &str,
    root: &str,
    path_part: &str,
    integration: Value,
    mapped: bool,
) -> String {
    let resource = resource_under(gateway, api, root, path_part).await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(gateway, api, &resource, "GET", integration).await;
    if mapped {
        method_response(gateway, api, &resource, "GET", "200", json!({})).await;
        // No response template: the backend's own body is what the method response carries
        // where the integration response renders none.
        let answer = gateway
            .call(
                "PUT",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "case PutIntegrationResponse({path_part}): expected 201, got {} with {:?}",
            answer.status, answer.body
        );
    }
    resource
}

/// What a row expects of the simulated response.
#[derive(Debug)]
enum Expect {
    /// The backend was reached and answered the body of this route.
    Backend(&'static str),
    /// The integration could not be performed, so the api's `API_CONFIGURATION_ERROR`
    /// gateway response is what the simulation reports.
    NotPerformed,
}

/// RULE: a simulated invocation performs an integration that names an HTTP endpoint against
/// that endpoint, resolving the `uri`'s stage-variable references from the `stageVariables` the
/// call supplies — with no deployment of the api under test.
#[tokio::test]
async fn a_simulated_invocation_reaches_the_backend_the_stage_variables_select() {
    let gateway = Gateway::new();
    let backend = backend(&gateway).await;
    let (api, root) = create_api(&gateway, "test-invoke-http-subject").await;
    let passed_through = method_with(
        &gateway,
        &api,
        &root,
        "route",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": backend_uri(&backend),
        }),
        false,
    )
    .await;
    let standard = method_with(
        &gateway,
        &api,
        &root,
        "mapped",
        json!({
            "type": "HTTP",
            "httpMethod": "GET",
            "uri": backend_uri(&backend),
        }),
        true,
    )
    .await;
    let unclaimed = method_with(
        &gateway,
        &api,
        &root,
        "elsewhere",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": format!("https://backend.example.com/${{stageVariables.{TARGET}}}"),
        }),
        false,
    )
    .await;

    // The api under test is never deployed: the simulation reaches no stage, which is what
    // makes `stageVariables` the only source of the reference's value.
    let deployments = gateway
        .call("GET", &format!("/restapis/{api}/deployments"), None)
        .await;
    assert_eq!(
        deployments.status, 200,
        "case \"the api under test holds no deployment\": GetDeployments expected 200, got {} \
         with {:?}",
        deployments.status, deployments.body
    );
    assert!(
        page_items(&deployments.body, "the api under test holds no deployment").is_empty(),
        "case \"the api under test holds no deployment\": expected no deployment, got {:?}",
        deployments.body
    );

    // Rows differ along the integration type, the value the supplied map gives the reference,
    // whether it carries the name at all, and whether the address names an endpoint this
    // emulator serves.
    type Row<'r> = (&'r str, &'r str, Option<&'r str>, Expect);
    let rows: Vec<Row<'_>> = vec![
        (
            "a proxy integration, the variable selecting the first backend",
            &passed_through,
            Some(ALPHA),
            Expect::Backend(ALPHA),
        ),
        (
            "the same method, the variable selecting the other backend",
            &passed_through,
            Some(BETA),
            Expect::Backend(BETA),
        ),
        (
            "a standard integration, whose answer its own responses map",
            &standard,
            Some(ALPHA),
            Expect::Backend(ALPHA),
        ),
        (
            "the same standard method, selecting the other backend",
            &standard,
            Some(BETA),
            Expect::Backend(BETA),
        ),
        (
            "a call supplying no value for the referenced variable",
            &passed_through,
            None,
            Expect::NotPerformed,
        ),
        (
            "an address naming no endpoint this emulator serves",
            &unclaimed,
            Some(ALPHA),
            Expect::NotPerformed,
        ),
    ];

    for (case, resource, target, expected) in &rows {
        let request = match target {
            Some(value) => json!({ "stageVariables": { TARGET: value } }),
            None => json!({}),
        };
        let simulated = test_invoke(&gateway, &api, resource, "GET", request).await;
        assert_eq!(
            simulated.http_status, 200,
            "case {case:?}: TestInvokeMethod expected 200, got {} with {:?}",
            simulated.http_status, simulated.body
        );
        match expected {
            Expect::Backend(route) => {
                assert_eq!(
                    simulated.status(),
                    200,
                    "case {case:?}: expected the backend's 200, got {} with {:?}",
                    simulated.status(),
                    simulated.body
                );
                assert_eq!(
                    simulated.simulated_body(),
                    backend_body(route),
                    "case {case:?}: expected the {route} backend's body, got {:?}",
                    simulated.simulated_body()
                );
            }
            Expect::NotPerformed => {
                assert_eq!(
                    simulated.status(),
                    500,
                    "case {case:?}: expected 500, got {} with {:?}",
                    simulated.status(),
                    simulated.body
                );
                assert_eq!(
                    message_of(&simulated.simulated_body()).as_deref(),
                    Some(INTERNAL_SERVER_ERROR),
                    "case {case:?}: expected the configuration-error body, got {:?}",
                    simulated.simulated_body()
                );
            }
        }
    }
}

/// The `message` member of a JSON body, which is what every API Gateway gateway response
/// carries.
fn message_of(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("message")?
        .as_str()
        .map(str::to_owned)
}
