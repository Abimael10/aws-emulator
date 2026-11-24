// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the precondition `CreateDeployment` weighs before it mints
//! anything.
//!
//! The rule these cases assert, one level above the recorded case:
//!
//! > A deployment FREEZES the api's routes for a stage to serve, so an api that holds no
//! > route has nothing to deploy: `CreateDeployment` refuses it `400 BadRequestException`
//! > with the sentence that error class is worded with, persists nothing (the api's
//! > deployments stay empty and a named stage stays on the deployment it already served),
//! > and the identical call succeeds once the api holds one route.
//!
//! The rows vary what the rule generalises over: the api's protocol type (`HTTP`,
//! `WEBSOCKET`), what the `CreateDeployment` request carries (nothing at all, a
//! description, the name of a stage of the api), whether the api holds a stage, and how the
//! api gets the one route that makes the same call succeed (a request-line key with no
//! integration for an HTTP api, a selector key wired to one for a WEBSOCKET api). The
//! recorded case that filed the issue — a freshly created HTTP api deployed with no routes
//! — is one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, deployable_route, emulator, error_code};

/// The sentence API Gateway refuses the deployment of an api that holds no route with.
///
/// The WHOLE sentence is this error class's fixed text: it carries no part taken from the
/// request or from the store, so every row expects it character for character.
const NO_VALID_ROUTES_MESSAGE: &str = "Unable to deploy API because no valid routes exist in this API";

/// The `DeploymentStatus` a released deployment carries, from the values
/// `DeploymentStatus` declares — `PENDING | FAILED | DEPLOYED`
/// (`smithy_query(query="shape", shape="DeploymentStatus")`).
const DEPLOYED: &str = "DEPLOYED";

/// One row: the api to deploy, the stage to give it first, and the `CreateDeployment` body
/// that is sent twice — once while the api holds no route, once after it holds one.
struct Row {
    case: &'static str,
    /// The `CreateApi` request the deployment is attempted under.
    api: Value,
    /// The name of a stage created under the api before deploying, if the row has one.
    stage: Option<&'static str>,
    /// The `CreateDeployment` body; `None` sends no payload at all.
    request: Option<Value>,
}

fn http_api(name: &str) -> Value {
    json!({ "name": name, "protocolType": "HTTP" })
}

/// A WebSocket api: `RouteSelectionExpression` "is required for WebSocket APIs"
/// (`CreateApi$RouteSelectionExpression`).
fn websocket_api(name: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": "$request.body.action"
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an HTTP api deployed by a request carrying only the api id",
            api: http_api("routeless-http-api"),
            stage: None,
            request: Some(json!({})),
        },
        Row {
            case: "an HTTP api deployed with a description",
            api: http_api("described-routeless-api"),
            stage: None,
            request: Some(json!({ "description": "the first cut" })),
        },
        Row {
            case: "an HTTP api deployed by a request with no payload at all",
            api: http_api("payloadless-routeless-api"),
            stage: None,
            request: None,
        },
        Row {
            case: "an HTTP api whose release names a stage of its own",
            api: http_api("staged-routeless-api"),
            stage: Some("dev"),
            request: Some(json!({ "stageName": "dev" })),
        },
        Row {
            case: "a WebSocket api deployed by a request carrying only the api id",
            api: websocket_api("routeless-socket-api"),
            stage: None,
            request: Some(json!({})),
        },
        Row {
            case: "a WebSocket api whose release names a stage of its own",
            api: websocket_api("staged-routeless-socket-api"),
            stage: Some("production"),
            request: Some(json!({ "stageName": "production" })),
        },
    ]
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

/// The `Items` of `GetDeployments` for one api.
async fn deployments(emulator: &MultiServiceRouter, case: &str, api_id: &str) -> Vec<Value> {
    let (status, listed) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/deployments"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetDeployments, got {status} {listed}"
    );
    listed["items"].as_array().cloned().unwrap_or_default()
}

/// The `DeploymentId` a stage serves, or `None` when it serves none.
async fn served_deployment(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage_name: &str,
) -> Option<String> {
    let (status, stage) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/stages/{stage_name}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetStage, got {status} {stage}"
    );
    stage
        .get("deploymentId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[tokio::test]
async fn an_api_holding_no_route_cannot_be_deployed_and_one_route_makes_the_same_call_succeed() {
    let emulator = emulator();

    for row in rows() {
        let api_id = create_api(&emulator, row.case, &row.api).await;
        if let Some(stage_name) = row.stage {
            let (status, stage) = call(
                &emulator,
                "POST",
                &format!("/v2/apis/{api_id}/stages"),
                Some(json!({ "stageName": stage_name })),
            )
            .await;
            assert_eq!(
                status, 201,
                "case {:?}: expected 201 from CreateStage, got {status} {stage}",
                row.case
            );
        }

        // The api holds no route, so there is nothing to deploy.
        let (status, refused) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            row.request.clone(),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400 from CreateDeployment, got {status} {refused}",
            row.case
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {:?}: expected BadRequestException, got {refused}",
            row.case
        );
        assert_eq!(
            as_str(&refused, "message"),
            NO_VALID_ROUTES_MESSAGE,
            "case {:?}: expected the refusal to be worded {NO_VALID_ROUTES_MESSAGE:?}, \
             got {refused}",
            row.case
        );

        // Nothing was minted: the api lists no deployment, and a stage the refused release
        // named still serves none.
        let listed = deployments(&emulator, row.case, &api_id).await;
        assert!(
            listed.is_empty(),
            "case {:?}: a refused deployment must persist nothing, got {listed:?}",
            row.case
        );
        if let Some(stage_name) = row.stage {
            let served = served_deployment(&emulator, row.case, &api_id, stage_name).await;
            assert_eq!(
                served, None,
                "case {:?}: a refused release must leave the stage serving nothing, \
                 got {served:?}",
                row.case
            );
        }

        // One route, and the IDENTICAL call is accepted: the route is what the api had
        // nothing of, not anything about the request.
        deployable_route(&emulator, row.case, &api_id).await;
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            row.request.clone(),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: expected 201 from CreateDeployment once the api holds a route, \
             got {status} {created}",
            row.case
        );
        let deployment_id = as_str(&created, "deploymentId");

        // And the accepted call persisted the deployment the answer named: it reads back
        // through the single-resource reader, released.
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/deployments/{deployment_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetDeployment, got {status} {fetched}",
            row.case
        );
        assert_eq!(
            fetched.get("deploymentStatus"),
            Some(&json!(DEPLOYED)),
            "case {:?}: expected DeploymentStatus {DEPLOYED}, got {fetched}",
            row.case
        );
        let listed = deployments(&emulator, row.case, &api_id).await;
        assert_eq!(
            listed.len(),
            1,
            "case {:?}: expected the api to hold exactly the accepted deployment, \
             got {listed:?}",
            row.case
        );
        if let Some(stage_name) = row.stage {
            let served = served_deployment(&emulator, row.case, &api_id, stage_name).await;
            assert_eq!(
                served.as_deref(),
                Some(deployment_id.as_str()),
                "case {:?}: expected the stage to serve the accepted deployment, got {served:?}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn deleting_the_only_route_takes_the_api_back_to_having_nothing_to_deploy() {
    let emulator = emulator();
    let case = "an api whose only route is deleted again";

    // The gate reads the api's CURRENT routes rather than anything recorded at creation
    // time, so an api that held a route and holds none again is refused exactly as one that
    // never held any.
    let api_id = create_api(&emulator, case, &http_api("emptied-api")).await;
    let route_key = deployable_route(&emulator, case, &api_id).await;
    let (status, routes) = call(&emulator, "GET", &format!("/v2/apis/{api_id}/routes"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetRoutes, got {status} {routes}"
    );
    let route_id = routes["items"]
        .as_array()
        .and_then(|items| items.first())
        .map(|route| as_str(route, "routeId"))
        .unwrap_or_else(|| panic!("case {case:?}: expected the api to list {route_key:?}, got {routes}"));

    let (status, deployed) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateDeployment while the api holds a route, \
         got {status} {deployed}"
    );

    let (status, deleted) = call(
        &emulator,
        "DELETE",
        &format!("/v2/apis/{api_id}/routes/{route_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: expected 204 from DeleteRoute, got {status} {deleted}"
    );

    let (status, refused) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 from CreateDeployment once the route is gone, \
         got {status} {refused}"
    );
    assert_eq!(
        as_str(&refused, "message"),
        NO_VALID_ROUTES_MESSAGE,
        "case {case:?}: expected the refusal to be worded {NO_VALID_ROUTES_MESSAGE:?}, \
         got {refused}"
    );

    // The deployment released while the api held that route is untouched: the refusal
    // changed nothing.
    let listed = deployments(&emulator, case, &api_id).await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected the api to still hold its one deployment, got {listed:?}"
    );
}
