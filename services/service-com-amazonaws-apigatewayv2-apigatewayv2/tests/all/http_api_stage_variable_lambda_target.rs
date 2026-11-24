// SPDX-License-Identifier: CC0-1.0

//! An `AWS_PROXY` route whose FUNCTION is named through a `${stageVariables.<name>}`
//! reference: which function each stage of one deployment reaches, and what the gateway
//! answers when that function admits it no invocation.
//!
//! The rule these cases assert, one level above the recorded case:
//!
//! > The gateway resolves every `${stageVariables.<name>}` reference in an integration's
//! > `IntegrationUri` against the INVOKED stage's own `StageVariables` before the backend is
//! > addressed — for an `AWS_PROXY` integration that is the Lambda function the route runs, so
//! > several stages released on ONE deployment, over ONE route and ONE integration record,
//! > each reach the function their own variable names, and the event that invocation carries
//! > states that stage's own `stageVariables` map and `requestContext.stage`. The resolved
//! > function is then invoked only where its own resource policy admits the gateway's
//! > invocation; a reference the invoked stage carries no variable for, and a resolved
//! > function that admits the gateway nothing or that Lambda refuses, name no response at all,
//! > so the request is answered `500` `{"message":"Internal Server Error"}` — repeatably, with
//! > nothing of the function's own reaching the caller — and the grant alone, with no change to
//! > the api, the route, the integration or the deployment, makes that same stage serve its OWN
//! > target.
//!
//! Nothing here is a value this emulator fills in: every served row is asserted on what a
//! function RAN and returned — each function names itself in its answer, so WHICH target the
//! stage resolved is read off the response rather than assumed — which is why AWS Lambda is
//! mounted beside this service on one engine.
//!
//! What is owned elsewhere and not restated: the grant itself, over every form a resource
//! policy takes and including the guarantee that a refused invocation never runs the function
//! at all, is `http_api_lambda_invoke_permission.rs`; the same stage-variable resolution of an
//! `HTTP_PROXY` upstream is `stage_variable_backend_selection.rs`; what a variable's name and
//! value may BE is `stage_variable_values.rs`; and the event's other members are
//! `http_api_event_path.rs` and `http_api_backend_dispatch.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::as_str;
use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, gateway_may_invoke, given_ungranted_function,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The route every stage below fronts, and the path under a stage a request to it carries.
const ROUTE_KEY: &str = "GET /who";
const ROUTE_PATH: &str = "/who";

/// The payload format the event is stated in; the format itself is not what varies here.
const PAYLOAD_FORMAT: &str = "2.0";

/// The name of the ONE stage variable the integration's URI references.
const VARIABLE: &str = "fn";

/// The status each handler below answers with, so a stage that served is told from one that
/// was refused by the status as well as by the marker in the body.
const SERVED: u16 = 200;

/// The status and sentence an HTTP api answers for an integration that produced no response,
/// which is the class both an unresolvable reference and an invocation the resolved function
/// does not admit fall into.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The function the `granted` stage's variable names, which carries the gateway's grant.
const GRANTED_TARGET: &str = "sv-target-granted";

/// The function the `ungranted` stage's variable names, which carries none.
const UNGRANTED_TARGET: &str = "sv-target-ungranted";

/// A value naming a function that was never created, which the gateway can reach no response
/// at.
const ABSENT_TARGET: &str = "sv-target-absent";

/// The variable map a stage carries: the one variable [`VARIABLE`] the integration's URI
/// references, naming the function that stage reaches. Keyed by that same constant, so the
/// map a stage is created with and the reference the URI carries cannot name two variables.
fn variables(target: &str) -> Value {
    Value::Object(
        [(VARIABLE.to_string(), json!(target))]
            .into_iter()
            .collect(),
    )
}

/// A handler that NAMES ITSELF in its answer, beside the two event members this suite asserts
/// the per-stage projection of: so a row reads which function the invoked stage resolved to,
/// and what that stage's event said about itself, off the response the client holds.
fn reporting_handler(name: &str) -> String {
    format!(
        "import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   body = {{\"servedBy\": {name:?},\n\
         \x20            \"stage\": event[\"requestContext\"][\"stage\"],\n\
         \x20            \"stageVariables\": event.get(\"stageVariables\")}}\n\
         \x20   return {{\"statusCode\": {SERVED},\n\
         \x20            \"headers\": {{\"content-type\": \"application/json\"}},\n\
         \x20            \"body\": json.dumps(body)}}\n"
    )
}

/// The `IntegrationUri` the stages below resolve: the ARN of a function of this account and
/// Region with the function NAME replaced by the documented reference form, which is what makes
/// the target the INVOKED stage's own choice. Built from an ARN this emulator minted, so no
/// field of it is written here by hand.
fn targeted_by_a_stage_variable(arn: &str, name: &str) -> String {
    let prefix = arn.strip_suffix(name).unwrap_or_else(|| {
        panic!("the created function's ARN {arn:?} ends in its name {name:?}")
    });
    format!("{prefix}${{stageVariables.{VARIABLE}}}")
}

/// One stage of the deployment: its name, and the function its own `fn` variable names — or
/// `None` for a stage that carries no such variable at all.
struct Stage {
    name: &'static str,
    target: Option<&'static str>,
}

/// An api, ONE `AWS_PROXY` integration whose URI names its function through a stage variable,
/// ONE route over it and ONE deployment, with every `stages` entry released on THAT deployment.
/// Answers the host the api's `ApiEndpoint` names.
async fn deployed(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    integration_uri: &str,
    stages: &[Stage],
) -> String {
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = as_str(&api, "apiId");
    let host = api
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();
    let (status, integration) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": integration_uri,
            "payloadFormatVersion": PAYLOAD_FORMAT
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration answered {status} {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");
    let (status, route) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": ROUTE_KEY,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute answered {status} {route}"
    );
    let (status, deployment) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDeployment answered {status} {deployment}"
    );
    let deployment_id = as_str(&deployment, "deploymentId");
    for stage in stages {
        let mut request = json!({ "stageName": stage.name, "deploymentId": deployment_id });
        if let Some(target) = stage.target {
            request["stageVariables"] = variables(target);
        }
        let (status, created) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(request),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateStage for {:?} answered {status} {created}",
            stage.name
        );
    }
    host
}

/// Invoke `stage`'s own invoke URL for the one route the api carries.
async fn invoke(emulator: &MultiServiceRouter, host: &str, stage: &str) -> Answer {
    send(
        emulator,
        host,
        "GET",
        &format!("/{stage}{ROUTE_PATH}"),
        &[],
    )
    .await
}

/// Assert the answer is the FUNCTION's own: the status it returned, the marker naming which
/// function ran, and the two event members whose values are the INVOKED stage's own.
fn served_by(case: &str, answer: &Answer, stage: &str, function: &str) {
    assert_eq!(
        answer.status,
        SERVED,
        "case {case:?}: expected the stage's own target to have answered, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.body.get("servedBy").and_then(Value::as_str),
        Some(function),
        "case {case:?}: expected the function {function:?} this stage's variable names to have \
         run, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.body.get("stage").and_then(Value::as_str),
        Some(stage),
        "case {case:?}: expected the event's requestContext.stage to be {stage:?}, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.body.get("stageVariables"),
        Some(&variables(function)),
        "case {case:?}: expected the event to carry this stage's own variable map, got {}",
        answer.summary()
    );
}

/// Assert the answer is the GATEWAY's own refusal for an integration that produced no response,
/// and that nothing of a function's own reached the caller with it.
fn refused(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        INTEGRATION_FAULT,
        "case {case:?}: expected {INTEGRATION_FAULT} for a target that names no response, got {}",
        answer.summary()
    );
    assert_eq!(
        message(answer),
        INTEGRATION_FAULT_MESSAGE,
        "case {case:?}: expected the gateway's own sentence, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.body.get("servedBy"),
        None,
        "case {case:?}: no function's own answer may reach the caller for a refused invoke, \
         got {}",
        answer.summary()
    );
}

/// What one stage of the one deployment is owed.
struct Row {
    case: &'static str,
    stage: &'static str,
    /// The function this stage's own `fn` variable names, or `None` for a stage carrying no
    /// such variable at all.
    target: Option<&'static str>,
    /// The function whose own answer this stage must serve, or `None` when the gateway can
    /// reach no response for it.
    served: Option<&'static str>,
}

/// The rows span what the rule generalises over: which stage serves the request (so the SAME
/// route, integration and deployment answer from a different function per stage), a resolved
/// function that admits the gateway no invocation, a stage carrying no such variable at all,
/// and a stage whose variable names a function that does not exist.
const ROWS: &[Row] = &[
    Row {
        case: "the stage whose variable names a function the gateway may invoke",
        stage: "granted",
        target: Some(GRANTED_TARGET),
        served: Some(GRANTED_TARGET),
    },
    Row {
        case: "the stage whose variable names a function granting the gateway nothing",
        stage: "ungranted",
        target: Some(UNGRANTED_TARGET),
        served: None,
    },
    Row {
        case: "the stage carrying no such variable, which names no function at all",
        stage: "unset",
        target: None,
        served: None,
    },
    Row {
        case: "the stage whose variable names a function that does not exist",
        stage: "absent",
        target: Some(ABSENT_TARGET),
        served: None,
    },
];

#[tokio::test(flavor = "multi_thread")]
async fn each_stage_reaches_the_function_its_own_variable_names() {
    let case = "the stages of one deployment, each naming its own target";
    let (emulator, engine) = emulator_with_lambda();
    let granted_arn = given_ungranted_function(
        &emulator,
        &engine,
        case,
        GRANTED_TARGET,
        &reporting_handler(GRANTED_TARGET),
    )
    .await;
    gateway_may_invoke(&emulator, case, GRANTED_TARGET, None).await;
    given_ungranted_function(
        &emulator,
        &engine,
        case,
        UNGRANTED_TARGET,
        &reporting_handler(UNGRANTED_TARGET),
    )
    .await;

    let stages: Vec<Stage> = ROWS
        .iter()
        .map(|row| Stage {
            name: row.stage,
            target: row.target,
        })
        .collect();
    let host = deployed(
        &emulator,
        case,
        "stage-variable-lambda-target",
        &targeted_by_a_stage_variable(&granted_arn, GRANTED_TARGET),
        &stages,
    )
    .await;

    // Each row is asked TWICE: both the serving and the refusing answer are properties of the
    // stage's own configuration, so neither is a one-off.
    for attempt in 1..=2 {
        for row in ROWS {
            let case = format!("{} (attempt {attempt})", row.case);
            let answer = invoke(&emulator, &host, row.stage).await;
            match row.served {
                Some(function) => served_by(&case, &answer, row.stage, function),
                None => refused(&case, &answer),
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_grant_alone_makes_a_refused_stage_serve_its_own_target() {
    let case = "the grant is added on the function the refused stage's variable names";
    let (emulator, engine) = emulator_with_lambda();
    let granted_arn = given_ungranted_function(
        &emulator,
        &engine,
        case,
        GRANTED_TARGET,
        &reporting_handler(GRANTED_TARGET),
    )
    .await;
    gateway_may_invoke(&emulator, case, GRANTED_TARGET, None).await;
    given_ungranted_function(
        &emulator,
        &engine,
        case,
        UNGRANTED_TARGET,
        &reporting_handler(UNGRANTED_TARGET),
    )
    .await;
    let host = deployed(
        &emulator,
        case,
        "stage-variable-lambda-target-grant",
        &targeted_by_a_stage_variable(&granted_arn, GRANTED_TARGET),
        &[
            Stage {
                name: "granted",
                target: Some(GRANTED_TARGET),
            },
            Stage {
                name: "ungranted",
                target: Some(UNGRANTED_TARGET),
            },
        ],
    )
    .await;

    // Before the grant: the sibling stage of the same deployment keeps serving its own target,
    // and the one whose target admits nothing is refused.
    refused(
        &format!("{case}: before the grant"),
        &invoke(&emulator, &host, "ungranted").await,
    );
    served_by(
        &format!("{case}: the sibling stage before the grant"),
        &invoke(&emulator, &host, "granted").await,
        "granted",
        GRANTED_TARGET,
    );

    // The grant alone — no api, route, integration, deployment or stage changes with it.
    gateway_may_invoke(&emulator, case, UNGRANTED_TARGET, None).await;

    served_by(
        &format!("{case}: after the grant"),
        &invoke(&emulator, &host, "ungranted").await,
        "ungranted",
        UNGRANTED_TARGET,
    );
    served_by(
        &format!("{case}: the sibling stage after the grant"),
        &invoke(&emulator, &host, "granted").await,
        "granted",
        GRANTED_TARGET,
    );
}
