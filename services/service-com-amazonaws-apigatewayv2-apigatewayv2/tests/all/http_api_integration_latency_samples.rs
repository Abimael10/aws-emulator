// SPDX-License-Identifier: CC0-1.0

//! The `IntegrationLatency` series a deployed HTTP api stage publishes, when the same stage
//! carries BOTH traffic that reaches an integration and traffic the gateway refuses before
//! one.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A deployed HTTP api stage publishes one `AWS/ApiGateway` `IntegrationLatency` sample per
//! > request whose route's BACKEND ANSWERED the invocation the gateway relayed to it, under the
//! > dimension set {ApiId, Stage} (and under {ApiId} beside it), so that series' `SampleCount`
//! > over a window is the number of integration-reaching requests of the stage — whatever
//! > status those answers carried. A request the gateway refuses BEFORE the integration — the
//! > decision of the route's authorizer, and a request that presents none of the identity
//! > sources that authorizer declares, so its function is not invoked at all — is still traffic
//! > of the stage's route and still counted in `Count`, but it contributes NO sample, and a
//! > stage whose whole traffic was refused that way publishes no `IntegrationLatency` series at
//! > all.
//!
//! `http_api_request_metrics.rs` states the whole per-request contract and spans the status
//! classes of traffic a route served; this suite adds the one axis that table does not vary —
//! refused and integration-reaching requests MIXED on one stage — because that is what makes
//! `SampleCount` differ from the number of requests the stage answered, and what a client
//! separating backend time from gateway overhead reads. Its rows differ along how many
//! requests reach the integration (none, some, every one), how many are refused and in which
//! order, WHICH refusal they earn (the authorizer's deny, and the identity source absent), the
//! status class the backend answered the reaching ones with (success and server error), and
//! which stage served them (a named one and the one that serves the api's root).
//!
//! Only a cross-service suite can pin it: the api, its routes, its authorizer and its stage
//! are this service's, the decision and the answer belong to a function AWS Lambda holds and
//! runs, and the series land in CloudWatch's own metric store — all three on ONE engine, as
//! `core/router/src/lib.rs` mounts them. Every assertion reads the outcome back through
//! CloudWatch's own readers, so an observation that did not reach that store cannot pass.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{control, given_function, lambda, lambda_integration};
use crate::http_api_request_metrics::{
    emulator, pairs, samples, series, total, API_ID, COUNT, INTEGRATION_LATENCY, LATENCY, STAGE,
};
use crate::websocket_handshake_data_plane::send;

/// The Region every control-plane request below is signed for.
const REGION: &str = "us-east-1";

/// The header the api's authorizer declares as its one identity source, and the value its
/// function authorizes.
const AUTH_HEADER: &str = "x-sim-auth";
const AUTHORIZED_TOKEN: &str = "open";

/// A token the authorizer's own function refuses, which is any other value.
const REFUSED_TOKEN: &str = "shut";

/// The statuses the stage answers: the backend's own for a request it relayed, the `403` an
/// authorizer's deny earns, and the `401` a request presenting no identity source earns
/// without the authorizer's function being invoked at all.
const SERVED: u16 = 200;
const BACKEND_FAULT: u16 = 500;
const FORBIDDEN: u16 = 403;
const UNAUTHORIZED: u16 = 401;

/// The route keys the api holds: one fronted by the authorizer, one open to every caller.
const GATED_ROUTE_KEY: &str = "GET /gated/{item}";
const OPEN_ROUTE_KEY: &str = "GET /open/{item}";

/// ONE function in BOTH roles the mix needs — the route's `REQUEST` authorizer and the route's
/// own `AWS_PROXY` integration — told apart by the member an authorizer event carries and a
/// proxy event does not (`type`, "REQUEST"). As the authorizer it answers the simple decision
/// payload format `2.0` admits (`isAuthorized`) from the one header it is declared over; as the
/// integration it answers the server-error class for a path ending in `/boom` and the success
/// class otherwise, so one row can hold reaching requests of either class.
const GATE_AND_BACKEND: &str = r#"import json


def lambda_handler(event, context):
    if event.get("type") == "REQUEST":
        headers = event.get("headers") or {}
        return {"isAuthorized": headers.get("x-sim-auth") == "open"}
    path = event.get("rawPath") or ""
    status = 500 if path.endswith("/boom") else 200
    return {
        "statusCode": status,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({"path": path}),
    }
"#;

/// How long the warm-up below is given, and how often it looks: generous, and bounding no
/// behaviour any row asserts on.
const START_WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// One request of a row: the path within the stage, what it presents at the authorizer's
/// identity source, the status the stage answers it with, and whether it REACHED the
/// integration.
#[derive(Debug, Clone, Copy)]
struct Call {
    /// The path within the stage, which also decides the status the backend answers.
    path: &'static str,
    /// The value the request carries for [`AUTH_HEADER`], or `None` for a request that carries
    /// the header not at all.
    presents: Option<&'static str>,
    /// The status the stage answers.
    status: u16,
    /// Whether the request's invocation reached the route's backend and was answered — the one
    /// reading the `IntegrationLatency` series counts.
    reaches: bool,
}

/// One row: the stage that serves the traffic, and the calls made through it in order.
#[derive(Debug)]
struct Row {
    case: &'static str,
    stage_name: &'static str,
    calls: &'static [Call],
}

/// The rows: every mix of refused and integration-reaching traffic on one stage.
const ROWS: &[Row] = &[
    Row {
        case: "every request is authorized, so every one of them reaches the integration",
        stage_name: "live",
        calls: &[
            Call {
                path: "/gated/ok",
                presents: Some(AUTHORIZED_TOKEN),
                status: SERVED,
                reaches: true,
            },
            Call {
                path: "/gated/boom",
                presents: Some(AUTHORIZED_TOKEN),
                status: BACKEND_FAULT,
                reaches: true,
            },
            Call {
                path: "/open/ok",
                presents: None,
                status: SERVED,
                reaches: true,
            },
        ],
    },
    Row {
        case: "every request is refused before the integration, so the stage publishes no \
               backend-latency series at all",
        stage_name: "prod",
        calls: &[
            Call {
                path: "/gated/ok",
                presents: Some(REFUSED_TOKEN),
                status: FORBIDDEN,
                reaches: false,
            },
            Call {
                path: "/gated/ok",
                presents: None,
                status: UNAUTHORIZED,
                reaches: false,
            },
            Call {
                path: "/gated/boom",
                presents: Some(REFUSED_TOKEN),
                status: FORBIDDEN,
                reaches: false,
            },
        ],
    },
    Row {
        case: "the recorded shape: refusals and authorized requests mixed on one stage",
        stage_name: "live",
        calls: &[
            Call {
                path: "/gated/ok",
                presents: Some(REFUSED_TOKEN),
                status: FORBIDDEN,
                reaches: false,
            },
            Call {
                path: "/gated/ok",
                presents: Some(AUTHORIZED_TOKEN),
                status: SERVED,
                reaches: true,
            },
            Call {
                path: "/open/boom",
                presents: None,
                status: BACKEND_FAULT,
                reaches: true,
            },
            Call {
                path: "/gated/ok",
                presents: None,
                status: UNAUTHORIZED,
                reaches: false,
            },
            Call {
                path: "/gated/boom",
                presents: Some(AUTHORIZED_TOKEN),
                status: BACKEND_FAULT,
                reaches: true,
            },
        ],
    },
    Row {
        case: "a refusal and a reaching request on the stage that serves the api's root",
        stage_name: "$default",
        calls: &[
            Call {
                path: "/gated/ok",
                presents: Some(REFUSED_TOKEN),
                status: FORBIDDEN,
                reaches: false,
            },
            Call {
                path: "/gated/ok",
                presents: Some(AUTHORIZED_TOKEN),
                status: SERVED,
                reaches: true,
            },
        ],
    },
];

/// The stage name that serves the api's ROOT, whose requests carry no stage segment.
const ROOT_STAGE: &str = "$default";

/// The path within the api's endpoint a call is made at: the route path itself on the stage
/// that serves the api's root, and the stage name in front of it otherwise.
fn invoke_path(row: &Row, call: &Call) -> String {
    if row.stage_name == ROOT_STAGE {
        return call.path.to_string();
    }
    format!("/{}{}", row.stage_name, call.path)
}

/// An api holding the two routes above over ONE Lambda proxy integration, the `REQUEST`
/// authorizer that fronts the gated one, and `row`'s stage with auto-deploy on. Answers
/// `(ApiId, the host its ApiEndpoint names)`.
async fn given_gated_api(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stage_name: &str,
) -> (String, String) {
    let (status, api) = control(
        router,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "integration-latency-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = member(&api, "apiId");
    let host = api
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();

    let (status, integration) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(lambda_integration(function, "2.0")),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration answered {status} {integration}"
    );
    let integration_id = member(&integration, "integrationId");

    let (status, authorizer) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(json!({
            "name": "deciding-gate",
            "authorizerType": "REQUEST",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function}/\
                 invocations"
            ),
            "authorizerPayloadFormatVersion": "2.0",
            "enableSimpleResponses": true,
            "authorizerResultTtlInSeconds": 0,
            "identitySource": [format!("$request.header.{AUTH_HEADER}")]
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateAuthorizer answered {status} {authorizer}"
    );
    let authorizer_id = member(&authorizer, "authorizerId");

    let (status, route) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": GATED_ROUTE_KEY,
            "target": format!("integrations/{integration_id}"),
            "authorizationType": "CUSTOM",
            "authorizerId": authorizer_id
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute answered {status} {route} for the gated route"
    );
    let (status, route) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": OPEN_ROUTE_KEY,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute answered {status} {route} for the open route"
    );
    let (status, stage) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage_name, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {stage}"
    );
    (api_id, host)
}

/// The string member `name` of `body`, which the call that answered it states.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the answer states no {name}: {body}"))
        .to_string()
}

/// Start the function's runtime BEFORE any request is counted, by invoking it through AWS
/// Lambda's own `Invoke`.
///
/// A function's first invocation starts that function's runtime, and on a loaded host that
/// start can overrun the budget the gateway gives the invocation it relays — a transient answer
/// that would be counted like any other and would make a row's assertion about how many
/// requests reached the integration depend on how fast the host starts an interpreter. Started
/// here instead: this invocation is Lambda's own, so no series of the api under test observes
/// it. The wait is for the runtime to ANSWER — a condition — and nothing here asserts on the
/// clock.
async fn started(router: &MultiServiceRouter, case: &str, function: &str) {
    let deadline = Instant::now() + START_WAIT;
    loop {
        let (status, answered) = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            Some(json!({})),
        )
        .await;
        if status == 200 && answered.get("errorMessage").is_none() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function must answer an invocation before the traffic the rows \
             count is made, got {status} {answered}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The stage's `IntegrationLatency` `SampleCount` is the number of its requests that reached an
/// integration, never the number it answered: a refusal decided in front of the integration
/// contributes none, and the series exists exactly when something reached one.
#[tokio::test]
async fn the_backend_latency_sample_count_is_the_integration_reaching_traffic_of_the_stage() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so each row's series hold exactly its own traffic.
        let (router, engine) = emulator();
        let function =
            given_function(&router, &engine, case, "gate-and-backend", GATE_AND_BACKEND).await;
        started(&router, case, &function).await;
        let (api_id, host) = given_gated_api(&router, case, &function, row.stage_name).await;

        for (attempt, call) in row.calls.iter().enumerate() {
            let path = invoke_path(row, call);
            let headers: Vec<(&str, &str)> = call
                .presents
                .map(|token| vec![(AUTH_HEADER, token)])
                .unwrap_or_default();
            let answered = send(&router, &host, "GET", &path, &headers).await;
            assert_eq!(
                answered.status, call.status,
                "case {case:?}: expected request {attempt} at {path:?} presenting {:?} to be \
                 answered {}, got {}",
                call.presents,
                call.status,
                answered.summary()
            );
        }

        let answered = row.calls.len() as f64;
        let reaching = row.calls.iter().filter(|call| call.reaches).count() as f64;
        let api_dimensions = vec![(API_ID, api_id.clone())];
        let stage_dimensions = vec![
            (API_ID, api_id.clone()),
            (STAGE, row.stage_name.to_string()),
        ];
        for (scope, dimensions) in [
            ("the api's own series", api_dimensions),
            ("the stage's series", stage_dimensions.clone()),
        ] {
            // Every request the stage's routes answered is counted, refused or not — which is
            // what makes the sample count below a different number from this one.
            let actual = total(&router, COUNT, &dimensions).await;
            assert_eq!(
                actual,
                Some(answered),
                "case {case:?}: expected {scope} to hold {answered} in {COUNT}, got {actual:?}"
            );
            let actual = samples(&router, INTEGRATION_LATENCY, &dimensions).await;
            assert_eq!(
                actual,
                Some(reaching),
                "case {case:?}: expected {scope} to hold {reaching} {INTEGRATION_LATENCY} \
                 samples, got {actual:?}"
            );
            // What those samples MEASURE: the backend's share of answering the request, so for
            // a stage something reached an integration through the measurement is a real
            // duration (never a placeholder zero) and it is part of the time answering those
            // requests took, never more than it. A relation between the two series the same
            // requests produced, so no row depends on how fast the host runs.
            let measured = total(&router, INTEGRATION_LATENCY, &dimensions)
                .await
                .unwrap_or_default();
            let answering = total(&router, LATENCY, &dimensions).await.unwrap_or_default();
            if reaching > 0.0 {
                assert!(
                    measured > 0.0,
                    "case {case:?}: expected {scope} to measure the backend round trip of its \
                     {reaching} reaching requests, got {INTEGRATION_LATENCY} {measured}"
                );
            }
            assert!(
                measured <= answering,
                "case {case:?}: expected {scope} to hold no more {INTEGRATION_LATENCY} than \
                 {LATENCY}, the time answering the same requests took, got {measured} against \
                 {answering}"
            );
        }

        // WHICH series the measurement exists as: the stage's own dimension set and the api's,
        // for a stage something reached an integration through — and none at all for a stage
        // whose whole traffic was refused in front of the integration.
        let held = series(&router, &api_id).await;
        let mut latency_sets: Vec<Vec<(String, String)>> = held
            .iter()
            .filter(|(metric, _)| metric == INTEGRATION_LATENCY)
            .map(|(_, dimensions)| dimensions.clone())
            .collect();
        latency_sets.sort();
        let mut expected_sets: Vec<Vec<(String, String)>> = if reaching > 0.0 {
            vec![
                pairs(&[(API_ID, api_id.clone())]),
                pairs(&stage_dimensions),
            ]
        } else {
            Vec::new()
        };
        expected_sets.sort();
        assert_eq!(
            latency_sets, expected_sets,
            "case {case:?}: expected exactly these dimension sets for {INTEGRATION_LATENCY}, \
             got {held:?}"
        );
    }
}
