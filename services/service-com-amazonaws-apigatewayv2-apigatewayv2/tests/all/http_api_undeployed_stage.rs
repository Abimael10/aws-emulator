// SPDX-License-Identifier: CC0-1.0

//! What an HTTP api's `execute-api` endpoint answers for a stage that has NO deployment
//! released to it, and that the same address serves as soon as one is.
//!
//! The rule these cases assert, one level above the recorded case (one `$default` stage of an
//! HTTP api, created without a deployment and probed once):
//!
//! > "A Deployment must be associated with a Stage for it to be callable over the internet"
//! > (`Deployment`), so a stage with none holds NOTHING the endpoint can serve: a request
//! > naming it is answered `404` `{"message":"Not Found"}` — the same answer a path naming no
//! > stage of the api and a method and path no route matches earn, because to the client they
//! > are one fact, this endpoint has nothing at that address. The gate precedes everything the
//! > api's configuration would otherwise decide (route matching, the api's own CORS answer and
//! > the route's authorization), it is per STAGE rather than per api, and releasing a
//! > deployment to the stage makes the same request reach the route's integration with no
//! > other change. `403` `{"message":"Forbidden"}` stays the answer for a call whose
//! > CREDENTIAL did not admit it, so the two classes remain distinguishable.
//!
//! The rows span what the rule generalises over: the stage name (the `$default` stage that
//! serves the api's root with no stage segment at all, a plain named stage, and one at an
//! unusual form), how the request addresses the api (the api's own `execute-api` host, the
//! emulator's own endpoint with the api id behind the endpoint marker, and the stage path
//! alone), the form of the route key (a literal method and path, the `ANY` catch-all and the
//! `$default` route), the method (`GET`, `POST` and a CORS preflight's `OPTIONS`), whether
//! the api carries a `CorsConfiguration`, and the route's `AuthorizationType` (`NONE` and
//! `AWS_IAM`).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, REGION};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The status and sentence an HTTP api's endpoint answers for a request it has nothing for —
/// a stage with no deployment released to it among them.
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// The status and sentence a call whose credential did not admit it is refused with, which an
/// `AWS_IAM` route owes an unsigned request — asserted here to stay a DIFFERENT class from the
/// one above.
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// What a served route answers here: its `HTTP_PROXY` integration names "an endpoint over the
/// network" (`Integration$IntegrationUri`) this emulator opens no connection to, so the caller
/// is sent there with the redirect that preserves its method and body
/// (`disabled_execute_api_endpoint`).
const TEMPORARY_REDIRECT: u16 = 307;

/// The upstream that integration names, so a served answer is told from any refusal by the
/// address it carries.
const UPSTREAM: &str = "https://upstream.example.com/orders";

/// The error code the front door answers when NO service claims a request: asserted absent,
/// because a stage of an api this service holds must be REFUSED by this service rather than
/// left unclaimed.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The emulator's own endpoint, which is where a client that resolves no AWS host points.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// One api stood up by a row: its id, the host of its own default endpoint, and the stage the
/// row probes.
struct Served {
    api_id: String,
    endpoint_host: String,
    stage: String,
}

/// An HTTP api carrying no CORS configuration.
fn plain_api() -> Value {
    json!({ "name": "undeployed-stage", "protocolType": "HTTP" })
}

/// An HTTP api whose `CorsConfiguration` would answer a preflight itself for a request no
/// route of it matches, so a row can show the deployment gate coming first.
fn cors_api() -> Value {
    json!({
        "name": "undeployed-stage-cors",
        "protocolType": "HTTP",
        "corsConfiguration": {
            "allowOrigins": ["https://example.com"],
            "allowMethods": ["GET", "OPTIONS"],
            "allowHeaders": ["content-type"],
            "maxAge": 600
        }
    })
}

/// Create the api of `api_body` with one `HTTP_PROXY` route at `route_key` and the stage
/// `stage` — released to NO deployment, which is what every row below starts from.
async fn given_undeployed(
    emulator: &MultiServiceRouter,
    case: &str,
    api_body: Value,
    route_key: &str,
    authorization_type: Option<&str>,
    stage: &str,
) -> Served {
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(api_body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {status} {created}"
    );
    let api_id = as_str(&created, "apiId");

    let (status, integration) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": UPSTREAM,
            "integrationMethod": "ANY",
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {status} {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");

    let mut route = json!({
        "routeKey": route_key,
        "target": format!("integrations/{integration_id}")
    });
    if let Some(authorization_type) = authorization_type {
        route["authorizationType"] = json!(authorization_type);
    }
    let (status, created_route) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(route),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {status} {created_route}"
    );

    // The stage asks for neither `AutoDeploy` nor a `DeploymentId`, so the service releases
    // nothing to it — read back, so each row starts from a state the control plane reports.
    let (status, created_stage) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {status} {created_stage}"
    );
    assert!(
        created_stage
            .get("deploymentId")
            .and_then(Value::as_str)
            .is_none(),
        "case {case:?}: a stage asking for no deployment must serve none, got {created_stage}"
    );

    Served {
        endpoint_host: format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        api_id,
        stage: stage.to_string(),
    }
}

/// Release a deployment of the api to `stage`, the one change a row makes between its two
/// probes.
async fn release(emulator: &MultiServiceRouter, case: &str, served: &Served) {
    let (status, deployment) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{}/deployments", served.api_id),
        Some(json!({ "stageName": served.stage })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDeployment must answer 201, got {status} {deployment}"
    );
    let (status, stage) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{}/stages/{}", served.api_id, served.stage),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetStage must answer 200, got {status} {stage}"
    );
    assert!(
        !as_str(&stage, "deploymentId").is_empty(),
        "case {case:?}: the stage must carry the released deployment, got {stage}"
    );
}

/// What the row's request must be answered with once a deployment IS released to the stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deployed {
    /// The route's integration answered — the upstream redirect this emulator sends an
    /// `HTTP_PROXY` caller to.
    Upstream,
    /// The route's `AuthorizationType` refused the call: the credential class, which only
    /// becomes observable once the stage is callable at all.
    Forbidden,
}

/// One row: the api it stands up, the request a client sends, and what the stage owes that
/// request once it is callable.
struct Row {
    case: &'static str,
    api: fn() -> Value,
    route_key: &'static str,
    authorization_type: Option<&'static str>,
    stage: &'static str,
    method: &'static str,
    /// Request header fields beyond the ones every request carries.
    headers: &'static [(&'static str, &'static str)],
    /// The host the client addresses.
    host: fn(&Served) -> String,
    /// The path it addresses within that host.
    path: fn(&Served) -> String,
    deployed: Deployed,
}

const ROWS: &[Row] = &[
    Row {
        case: "the $default stage, reached with no stage segment on the api's own host",
        api: plain_api,
        route_key: "ANY /orders",
        authorization_type: None,
        stage: "$default",
        method: "GET",
        headers: &[],
        host: |served| served.endpoint_host.clone(),
        path: |_| "/orders".to_string(),
        deployed: Deployed::Upstream,
    },
    Row {
        case: "a named stage on the api's own host, at a literal method and path",
        api: plain_api,
        route_key: "GET /orders",
        authorization_type: Some("NONE"),
        stage: "prod",
        method: "GET",
        headers: &[],
        host: |served| served.endpoint_host.clone(),
        path: |served| format!("/{}/orders", served.stage),
        deployed: Deployed::Upstream,
    },
    Row {
        case: "a stage name of an unusual form, addressed by its stage path alone",
        api: plain_api,
        route_key: "POST /orders",
        authorization_type: None,
        stage: "Stage_1-x",
        method: "POST",
        headers: &[],
        host: |_| EMULATOR_ENDPOINT.to_string(),
        path: |served| format!("/{}/orders", served.stage),
        deployed: Deployed::Upstream,
    },
    Row {
        case: "the emulator's own endpoint with the api id behind the endpoint marker, on the \
               $default route",
        api: plain_api,
        route_key: "$default",
        authorization_type: None,
        stage: "beta",
        method: "GET",
        headers: &[],
        host: |_| EMULATOR_ENDPOINT.to_string(),
        path: |served| {
            format!(
                "/_aws/execute-api/{}/{}/anything/deeper",
                served.api_id, served.stage
            )
        },
        deployed: Deployed::Upstream,
    },
    Row {
        case: "a CORS preflight against an api whose configuration would answer one itself",
        api: cors_api,
        route_key: "ANY /orders",
        authorization_type: None,
        stage: "$default",
        method: "OPTIONS",
        headers: &[
            ("origin", "https://example.com"),
            ("access-control-request-method", "GET"),
        ],
        host: |served| served.endpoint_host.clone(),
        path: |_| "/orders".to_string(),
        deployed: Deployed::Upstream,
    },
    Row {
        case: "an AWS_IAM route probed unsigned, where the credential class waits behind the \
               deployment gate",
        api: plain_api,
        route_key: "GET /orders",
        authorization_type: Some("AWS_IAM"),
        stage: "prod",
        method: "GET",
        headers: &[],
        host: |served| served.endpoint_host.clone(),
        path: |served| format!("/{}/orders", served.stage),
        deployed: Deployed::Forbidden,
    },
];

/// Assert this endpoint answered the request itself, with `status` and `sentence`.
fn refused(case: &str, answer: &Answer, status: u16, sentence: &str) {
    assert_ne!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case {case:?}: a stage of an api this service holds must be answered by this \
         service, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected {status}, got {}",
        answer.summary()
    );
    assert_eq!(
        message(answer),
        sentence,
        "case {case:?}: expected the {sentence:?} body, got {}",
        answer.summary()
    );
}

/// Assert the route's integration answered: the upstream the `HTTP_PROXY` integration names.
fn served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, TEMPORARY_REDIRECT,
        "case {case:?}: expected the route's upstream redirect {TEMPORARY_REDIRECT}, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.location.as_deref(),
        Some(UPSTREAM),
        "case {case:?}: expected the upstream {UPSTREAM:?} in Location, got {:?}",
        answer.location
    );
}

#[tokio::test]
async fn an_undeployed_stage_has_nothing_at_that_address_and_serves_once_it_is_deployed() {
    for row in ROWS {
        let emulator = emulator();
        let served_api = given_undeployed(
            &emulator,
            row.case,
            (row.api)(),
            row.route_key,
            row.authorization_type,
            row.stage,
        )
        .await;
        let host = (row.host)(&served_api);
        let path = (row.path)(&served_api);

        // While the stage is callable over nothing, the endpoint has nothing at that address.
        let case = format!("{} [no deployment released]", row.case);
        let answer = send(&emulator, &host, row.method, &path, row.headers).await;
        refused(&case, &answer, NOT_FOUND, NOT_FOUND_MESSAGE);

        // The ONE change: a deployment released to that same stage. Nothing about the
        // request, the route or the integration moved.
        release(&emulator, row.case, &served_api).await;
        let case = format!("{} [a deployment released]", row.case);
        let answer = send(&emulator, &host, row.method, &path, row.headers).await;
        match row.deployed {
            Deployed::Upstream => served(&case, &answer),
            Deployed::Forbidden => refused(&case, &answer, FORBIDDEN, FORBIDDEN_MESSAGE),
        }
    }
}

#[tokio::test]
async fn the_deployment_gate_is_per_stage_of_the_same_api() {
    let case = "two stages of one api, one of them released to a deployment";
    let emulator = emulator();
    // The api, its route and its FIRST stage, which this case leaves undeployed.
    let undeployed = given_undeployed(
        &emulator,
        case,
        plain_api(),
        "GET /orders",
        None,
        "undeployed",
    )
    .await;
    let (status, stage) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{}/stages", undeployed.api_id),
        Some(json!({ "stageName": "live", "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {status} {stage}"
    );
    assert!(
        !as_str(&stage, "deploymentId").is_empty(),
        "case {case:?}: an AutoDeploy stage must carry a released deployment, got {stage}"
    );

    // In the same instant: the stage with a deployment serves its route, the one without has
    // nothing at its own address.
    let live = send(
        &emulator,
        &undeployed.endpoint_host,
        "GET",
        "/live/orders",
        &[],
    )
    .await;
    served(&format!("{case} [the deployed stage]"), &live);
    let dark = send(
        &emulator,
        &undeployed.endpoint_host,
        "GET",
        "/undeployed/orders",
        &[],
    )
    .await;
    refused(
        &format!("{case} [the undeployed stage]"),
        &dark,
        NOT_FOUND,
        NOT_FOUND_MESSAGE,
    );

    // And releasing a deployment to the second stage makes it serve too, with nothing else
    // changed about the api.
    release(&emulator, case, &undeployed).await;
    let answer = send(
        &emulator,
        &undeployed.endpoint_host,
        "GET",
        "/undeployed/orders",
        &[],
    )
    .await;
    served(&format!("{case} [the second stage, released]"), &answer);
}
