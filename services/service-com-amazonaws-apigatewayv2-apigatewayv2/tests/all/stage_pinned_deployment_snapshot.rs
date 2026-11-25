// SPDX-License-Identifier: CC0-1.0

//! A stage serves the DEPLOYMENT IT IS BOUND TO, and nothing else moves it.
//!
//! RULE: **what a stage of an HTTP api serves is the configuration frozen by the deployment its
//! own `DeploymentId` names: a stage whose `AutoDeploy` is off keeps serving that deployment's
//! snapshot while the api's routes and integrations are changed under it, a stage whose
//! `AutoDeploy` is on has a NEW deployment released to it by the service on every such change
//! (so its `DeploymentId` moves and it converges on the new configuration), and an explicit
//! `CreateDeployment` naming a stage mints a deployment of the api's CURRENT configuration and
//! re-binds that stage to it.**
//!
//! `CreateStage` takes both `AutoDeploy` ("whether updates to an API automatically trigger a
//! new deployment", `Stage$AutoDeploy`) and `DeploymentId` ("the identifier of the Deployment
//! that the Stage is associated with"), `CreateDeployment` takes an optional `StageName`, and
//! `GetStage` reports both members back — so the binding is observable on the control plane as
//! well as in what the data plane serves.
//!
//! Every assertion below is on something a FUNCTION ran and returned: each backend answers the
//! marker ITS OWN environment names, so which snapshot a stage served is read off the body and
//! off nothing this emulator filled in.
//!
//! The rows span what the rule generalises over: the stage addressed (the reserved stage that
//! serves the api's root, reached with no stage segment, and a named stage reached behind its
//! own name) and the ADDRESSING the client used (the api's own documented `execute-api` host,
//! and the api behind the reserved data-plane path a client that resolves nothing else is
//! handed). What the stage is bound to is the same either way.
//!
//! What `auto_deploy_republishes.rs` already owns and this does not restate: the control-plane
//! half of the same rule over its own row table (the `DeploymentStatus` a release carries,
//! `AutoDeployed`, the abandoned deployment still being readable, a sibling stage left where it
//! was). What is added here is the recorded scenario's own shape — two backends that differ in
//! NOTHING but the `Environment` variable their one handler returns as its body, so which
//! snapshot answered is read off a value the function read out of its own environment at run
//! time rather than off its source.

use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, gateway_may_invoke, lambda, package,
};
use crate::websocket_handshake_data_plane::send;

/// The route every deployment below carries, and the path within a stage that matches it.
const ROUTE_KEY: &str = "GET /snap";
const ROUTE_PATH: &str = "/snap";

/// The stage pinned to one deployment, beside the reserved stage that serves the api's root.
const PINNED_STAGE: &str = "pinned";
const ROOT_STAGE: &str = "$default";

/// The marker each of the two backends answers with, named by its own environment.
const FIRST_MARKER: &str = "backend-a";
const SECOND_MARKER: &str = "backend-b";

/// This emulator's own endpoint, which is the address a client that resolves nothing else
/// holds and the one the api is announced behind the reserved data-plane path at. It names no
/// service of its own, which is what makes that path-style addressing readable at all.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// The environment variable the handler reads its marker out of.
const MARKER_VARIABLE: &str = "MARKER";

/// The handler both backends run: a `200` whose text body is the marker its own environment
/// names, so two functions differing only in that variable are told apart by the body alone.
const MARKING_HANDLER: &str = "import os\n\
     def lambda_handler(event, context):\n    \
     return {'statusCode': 200, 'headers': {'content-type': 'text/plain'},\n             \
     'body': os.environ['MARKER']}\n";

/// The handler every deployment package below exposes, and a well-formed execution role.
const HANDLER: &str = "lambda_function.lambda_handler";
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long a function is given to report itself invocable, and how often it is looked at.
/// The span is AWS Lambda's own; this is only the deadline past which the suite stops looking,
/// so it is generous enough for a loaded host and bounds nothing a case asserts on.
const READY_BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Create the function `name` holding [`MARKING_HANDLER`] with `marker` in its own
/// environment, wait until Lambda reports it invocable, grant the gateway its invocation, and
/// answer its ARN.
///
/// The create is written here rather than taken from `http_api_backend_dispatch`'s
/// `given_ungranted_function` because these two functions differ ONLY in the `Environment`
/// their handler reads its marker out of, and that helper configures none; the archive itself
/// is that module's ([`package`]).
async fn given_marking_function(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    marker: &str,
) -> String {
    let (status, created) = lambda(
        emulator,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Environment": { "Variables": { MARKER_VARIABLE: marker } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(MARKING_HANDLER)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction({name:?}) answered {status} {created}"
    );
    let arn = created
        .get("FunctionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateFunction answered no FunctionArn"))
        .to_string();
    wait_until_active(emulator, case, name).await;
    gateway_may_invoke(emulator, case, name, None).await;
    arn
}

/// Look until Lambda first reports `name` `Active` — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — which is the condition this waits on rather than a span,
/// and which is read through that service's own reporter rather than out of its store.
async fn wait_until_active(emulator: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = std::time::Instant::now() + READY_BUDGET;
    loop {
        let (status, read) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration({name:?}) answered {status} {read}"
        );
        if read.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: Lambda never reported {name:?} Active: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One control-plane call that has to answer `expected`, and its body parsed.
async fn step(
    emulator: &MultiServiceRouter,
    case: &str,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let (status, answered) = control(emulator, method, path, body).await;
    assert_eq!(
        status, expected,
        "case {case:?}: {what} answered {status} {answered}"
    );
    answered
}

/// A required string member of a response body.
fn as_str(case: &str, what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: {what} must carry a non-empty {member}: {body}"))
}

/// How a client addresses a stage of the api: the authority it dials and the base path it
/// carries ahead of the stage.
struct Addressing {
    case: &'static str,
    host: String,
    base_path: String,
}

/// Both addressings this plane serves an api at, for the api `api_id` whose own announced
/// endpoint authority is `announced`: the `execute-api` host AWS documents
/// (`{api-id}.execute-api.{region}.amazonaws.com`), and the api behind the reserved
/// data-plane path a client that resolves nothing but this emulator is handed — addressed at
/// this emulator's OWN endpoint, which is the address such a client holds and which names no
/// service of its own.
fn addressings(api_id: &str, announced: &str) -> Vec<Addressing> {
    vec![
        Addressing {
            case: "the api's own execute-api host",
            host: announced.to_string(),
            base_path: String::new(),
        },
        Addressing {
            case: "the api behind the reserved data-plane path",
            host: EMULATOR_ENDPOINT.to_string(),
            base_path: format!("/_aws/execute-api/{api_id}"),
        },
    ]
}

/// The path within an addressing at which `stage` serves the route: the stage that serves the
/// api's root is reached with no stage segment at all, a named stage behind its own name.
fn invoke_path(addressing: &Addressing, stage: &str) -> String {
    if stage == ROOT_STAGE {
        format!("{}{ROUTE_PATH}", addressing.base_path)
    } else {
        format!("{}/{stage}{ROUTE_PATH}", addressing.base_path)
    }
}

/// What a client is answered for the route of `stage` at `addressing`: `(status, body text)`.
async fn served(
    emulator: &MultiServiceRouter,
    addressing: &Addressing,
    stage: &str,
) -> (u16, String) {
    let path = invoke_path(addressing, stage);
    let answer = send(emulator, &addressing.host, "GET", &path, &[]).await;
    (answer.status, answer.text)
}

/// Assert every addressing serves `stage` the backend `marker` names.
async fn assert_serves(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    announced: &str,
    stage: &str,
    marker: &str,
) {
    for addressing in addressings(api_id, announced) {
        let (status, body) = served(emulator, &addressing, stage).await;
        assert_eq!(
            (status, body.as_str()),
            (200, marker),
            "case {case:?}: stage {stage:?} at {} ({}) must serve {marker:?}, got {status} \
             {body:?}",
            addressing.case,
            invoke_path(&addressing, stage)
        );
    }
}

/// The `DeploymentId` `GetStage` reports for `stage`, or `None` for a stage serving none.
async fn stage_deployment(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
) -> Option<String> {
    let read = step(
        emulator,
        case,
        &format!("GetStage({stage})"),
        "GET",
        &format!("/v2/apis/{api_id}/stages/{stage}"),
        None,
        200,
    )
    .await;
    read.get("deploymentId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_stage_serves_its_own_deployments_snapshot() {
    let case = "an auto-deploying $default stage beside a stage pinned to one deployment";
    let (emulator, _engine) = emulator_with_lambda();
    let first = given_marking_function(&emulator, case, "snapshot-first", FIRST_MARKER).await;
    let second = given_marking_function(&emulator, case, "snapshot-second", SECOND_MARKER).await;

    // The api, its integration over the FIRST backend, and the route that fronts it.
    let api = step(
        &emulator,
        case,
        "CreateApi",
        "POST",
        "/v2/apis",
        Some(json!({ "name": "snapshot-api", "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = as_str(case, "CreateApi", &api, "apiId");
    let announced = as_str(case, "CreateApi", &api, "apiEndpoint")
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: the announced endpoint carries a scheme"));
    let integration = step(
        &emulator,
        case,
        "CreateIntegration",
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": first,
            "integrationMethod": "POST",
            "payloadFormatVersion": "2.0",
        })),
        201,
    )
    .await;
    let integration_id = as_str(case, "CreateIntegration", &integration, "integrationId");
    step(
        &emulator,
        case,
        "CreateRoute",
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": ROUTE_KEY,
            "target": format!("integrations/{integration_id}"),
        })),
        201,
    )
    .await;

    // The auto-deploying stage that serves the api's root, an explicit deployment of the api as
    // it stands now, and the stage PINNED to that deployment.
    step(
        &emulator,
        case,
        "CreateStage($default)",
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": ROOT_STAGE, "autoDeploy": true })),
        201,
    )
    .await;
    let released = step(
        &emulator,
        case,
        "CreateDeployment",
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
        201,
    )
    .await;
    let pinned_deployment = as_str(case, "CreateDeployment", &released, "deploymentId");
    let pinned = step(
        &emulator,
        case,
        "CreateStage(pinned)",
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": PINNED_STAGE, "deploymentId": pinned_deployment })),
        201,
    )
    .await;
    assert_eq!(
        pinned.get("deploymentId").and_then(Value::as_str),
        Some(pinned_deployment.as_str()),
        "case {case:?}: CreateStage must store the DeploymentId it was given: {pinned}"
    );

    // Both stages serve the first backend, at either addressing.
    for stage in [ROOT_STAGE, PINNED_STAGE] {
        assert_serves(
            &emulator,
            case,
            &api_id,
            &announced,
            stage,
            FIRST_MARKER,
        )
        .await;
    }

    // The api's integration is re-pointed at the SECOND backend.
    step(
        &emulator,
        case,
        "UpdateIntegration",
        "PATCH",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        Some(json!({ "integrationUri": second })),
        200,
    )
    .await;

    // The auto-deploying stage had a new deployment released to it and converges on the new
    // configuration; the pinned stage keeps its own deployment and its snapshot.
    let root_deployment = stage_deployment(&emulator, case, &api_id, ROOT_STAGE).await;
    assert!(
        root_deployment.is_some_and(|released| released != pinned_deployment),
        "case {case:?}: the auto-deploying stage must move onto a deployment of its own"
    );
    assert_serves(
        &emulator,
        case,
        &api_id,
        &announced,
        ROOT_STAGE,
        SECOND_MARKER,
    )
    .await;
    assert_eq!(
        stage_deployment(&emulator, case, &api_id, PINNED_STAGE)
            .await
            .as_deref(),
        Some(pinned_deployment.as_str()),
        "case {case:?}: the pinned stage must keep the deployment it was created with"
    );
    assert_serves(
        &emulator,
        case,
        &api_id,
        &announced,
        PINNED_STAGE,
        FIRST_MARKER,
    )
    .await;

    // Only an explicit deployment for that stage re-binds it, and then it serves the api as it
    // stands now.
    let rebound = step(
        &emulator,
        case,
        "CreateDeployment(StageName=pinned)",
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({ "stageName": PINNED_STAGE })),
        201,
    )
    .await;
    let rebound = as_str(case, "CreateDeployment", &rebound, "deploymentId");
    assert_ne!(
        rebound, pinned_deployment,
        "case {case:?}: a new deployment carries an id of its own"
    );
    assert_eq!(
        stage_deployment(&emulator, case, &api_id, PINNED_STAGE)
            .await
            .as_deref(),
        Some(rebound.as_str()),
        "case {case:?}: CreateDeployment(StageName=…) must re-bind the stage it names"
    );
    assert_serves(
        &emulator,
        case,
        &api_id,
        &announced,
        PINNED_STAGE,
        SECOND_MARKER,
    )
    .await;
}
