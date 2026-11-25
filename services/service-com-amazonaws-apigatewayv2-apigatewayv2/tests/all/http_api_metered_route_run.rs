// SPDX-License-Identifier: CC0-1.0

//! What a route the serving stage's `RouteSettings` meter answers over a RUN of requests, with
//! the function the route fronts actually running.
//!
//! The rule:
//!
//! > **A stage's `RouteSettings` for one route key are the token bucket that route is metered
//! > with — "ThrottlingRateLimit … the throttling rate limit", "ThrottlingBurstLimit … the
//! > throttling burst limit" (`smithy_query(query="shape", shape="RouteSettings")`) — and a
//! > route metered that way is SHED, never shut: a request the bucket has no capacity for is
//! > answered by the gateway itself, `429` `{"message":"Too Many Requests"}`, with the route's
//! > integration not reached for it, while a caller that waits for the rate to refill is served
//! > the function's own answer again and again. The metering is scoped to the one route key: a
//! > twin route of the same stage, on its `DefaultRouteSettings`, sheds nothing in the same
//! > instant, and `DeleteRouteSettings` returns the metered route to those defaults, after
//! > which the same burst sheds nothing either.**
//!
//! Why this suite exists beside the others: `http_api_throttle_burst` holds the bucket's
//! arithmetic for requests in flight together and `http_api_published_endpoint_throttled_stage`
//! holds it at the published address, both over an `HTTP_PROXY` route whose upstream this
//! emulator never calls. Neither drives a RUN of requests at a metered route whose backend RAN
//! — which is what says the route keeps serving its rate rather than closing once its bucket is
//! spent, and that a shed request carries nothing of the backend.
//!
//! Nothing here asserts on elapsed time, and nothing sleeps for a fixed span: each served
//! answer is WAITED FOR as a condition (the bucket the rate refills admits the request) under a
//! generous deadline, and the burst's own assertions are about which route shed, never about
//! how fast the host ran.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, given_function, lambda_integration,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The pair the metered route's own `RouteSettings` state: one request a second, a burst of
/// one — the pair the recorded scenario provisions.
const METERED_RATE: f64 = 1.0;
const METERED_BURST: i32 = 1;

/// The pair the stage's `DefaultRouteSettings` state, which every other route of the stage is
/// metered by: generous enough that nothing this suite sends is shed by it.
const DEFAULT_RATE: f64 = 500.0;
const DEFAULT_BURST: i32 = 200;

/// The two routes of the stage: the one its `RouteSettings` meter, and its twin.
const METERED_ROUTE: &str = "/limited";
const TWIN_ROUTE: &str = "/twin";

/// The status and sentence the gateway answers a request the stage's route settings shed with.
const THROTTLED: u16 = 429;
const THROTTLED_MESSAGE: &str = "Too Many Requests";

/// The statuses the gateway answers while the backend's runtime is still starting, which the
/// wait below looks past.
const INTEGRATION_FAULT: u16 = 500;
const TIMED_OUT: u16 = 503;

/// The body the function answers, so a served answer is told from any refusal by the backend's
/// own text rather than by a status alone.
const MARKER: &str = "served-by-the-function";
const BACKEND: &str = "def lambda_handler(event, context):\n    \
     return {'statusCode': 200, 'body': 'served-by-the-function'}\n";

/// How many successive SERVED answers the run asks for: enough that the route has been served
/// well past the burst its settings state, which is what says the bucket the rate refills keeps
/// admitting requests rather than closing once it was spent.
const SERVED_RUN: usize = 4;

/// How many requests the interleaved burst sends at each route — the scale the recorded
/// scenario fires — and how many of them at the metered route must be shed: its burst is one,
/// so everything after the first of them arrives while that one is still being answered.
const BURST: usize = 24;
const SHED_AT_LEAST: usize = 1;

/// How long a served answer is waited for, and how often it is asked for: generous, bounding no
/// behaviour any assertion is about. What is waited for is a CONDITION — the route's bucket
/// admitting the request, and the function having answered it.
const SERVED_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// Stand up the api of this suite: one Lambda proxy integration, the two routes over it, and
/// the `$default` stage that meters one of them. Answers the host its `ApiEndpoint` names.
async fn given_api(
    emulator: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
) -> (String, String) {
    let arn = given_function(emulator, engine, case, "metered-route", BACKEND).await;
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "metered-route-run", "protocolType": "HTTP" })),
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
    let (status, created) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(lambda_integration(&arn, "2.0")),
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
    for route in [METERED_ROUTE, TWIN_ROUTE] {
        let (status, created) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": format!("GET {route}"),
                "target": format!("integrations/{integration_id}")
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateRoute answered {status} {created}"
        );
    }
    let (status, stage) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({
            "stageName": "$default",
            "autoDeploy": true,
            "defaultRouteSettings": {
                "throttlingRateLimit": DEFAULT_RATE,
                "throttlingBurstLimit": DEFAULT_BURST
            },
            "routeSettings": {
                format!("GET {METERED_ROUTE}"): {
                    "throttlingRateLimit": METERED_RATE,
                    "throttlingBurstLimit": METERED_BURST
                }
            }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {stage}"
    );
    // The override the stage reports is the one the route is metered by, and it is stated for
    // that route key alone. Read with `get`, because the route key carries a `/` of its own.
    let settings = stage.get("routeSettings").cloned().unwrap_or(Value::Null);
    let reported = settings
        .get(format!("GET {METERED_ROUTE}"))
        .and_then(|held| held.get("throttlingRateLimit"))
        .and_then(Value::as_f64);
    assert_eq!(
        reported,
        Some(METERED_RATE),
        "case {case:?}: CreateStage must report the override for the metered route, got {stage}"
    );
    assert!(
        settings.get(format!("GET {TWIN_ROUTE}")).is_none(),
        "case {case:?}: the twin route carries no override of its own, got {stage}"
    );
    (api_id, host)
}

/// Assert `answer` is the function's own answer.
fn served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the function's own 200, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.text, MARKER,
        "case {case:?}: expected the function's own body {MARKER:?}, got {}",
        answer.summary()
    );
}

/// Assert `answer` is the refusal the stage's throttle sheds a request with, carrying nothing
/// of the integration behind the route.
fn shed(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, THROTTLED,
        "case {case:?}: expected {THROTTLED} from the stage's throttle, got {}",
        answer.summary()
    );
    assert_eq!(
        message(answer),
        THROTTLED_MESSAGE,
        "case {case:?}: expected the {THROTTLED_MESSAGE:?} envelope, got {}",
        answer.summary()
    );
    assert!(
        !answer.text.contains(MARKER),
        "case {case:?}: a shed request must carry nothing of the integration, got {}",
        answer.summary()
    );
}

/// One served answer from `route`, waited for as a CONDITION: the request is made again while
/// the route's bucket has no capacity for it (which is the gateway's own `429`) and while the
/// function's runtime is still starting, until the function answers or [`SERVED_DEADLINE`]
/// passes. Every refusal met on the way is held to the shed envelope.
async fn served_once(emulator: &MultiServiceRouter, case: &str, host: &str, route: &str) -> Answer {
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let answer = send(emulator, host, "GET", route, &[]).await;
        if answer.status == 200 {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the function's own answer at {route}, got {}",
            answer.summary()
        );
        if answer.status == THROTTLED {
            shed(case, &answer);
        } else {
            assert!(
                matches!(answer.status, INTEGRATION_FAULT | TIMED_OUT),
                "case {case:?}: expected the function's answer or the stage's own refusal at \
                 {route}, got {}",
                answer.summary()
            );
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The answers of one interleaved burst: `BURST` requests at each route, alternating, each
/// arriving while the ones before it are still being answered.
async fn interleaved_burst(emulator: &MultiServiceRouter, host: &str) -> (Vec<Answer>, Vec<Answer>) {
    let mut metered = Vec::new();
    let mut twin = Vec::new();
    for _ in 0..BURST {
        metered.push(send(emulator, host, "GET", METERED_ROUTE, &[]));
        twin.push(send(emulator, host, "GET", TWIN_ROUTE, &[]));
    }
    (
        ::futures_util::future::join_all(metered).await,
        ::futures_util::future::join_all(twin).await,
    )
}

#[tokio::test]
async fn a_metered_route_sheds_a_burst_and_keeps_serving_a_caller_that_waits() {
    let case = "a $default stage metering one of two routes over one running function";
    let (emulator, engine) = emulator_with_lambda();
    let (api_id, host) = given_api(&emulator, &engine, case).await;

    // A caller that waits for each answer is served the function's own answer again and
    // again: the route is SHED when its bucket is spent, never shut.
    for served_answer in 0..SERVED_RUN {
        let case = format!("{case} [served answer {served_answer} of the metered route]");
        served(&case, &served_once(&emulator, &case, &host, METERED_ROUTE).await);
    }
    // And so is its twin, which the stage's defaults meter.
    for served_answer in 0..SERVED_RUN {
        let case = format!("{case} [served answer {served_answer} of the twin route]");
        served(&case, &served_once(&emulator, &case, &host, TWIN_ROUTE).await);
    }

    // An interleaved burst: the metered route sheds, its twin does not.
    let (metered, twin) = interleaved_burst(&emulator, &host).await;
    let sheds = metered
        .iter()
        .filter(|answer| answer.status == THROTTLED)
        .count();
    assert!(
        sheds >= SHED_AT_LEAST,
        "case {case:?}: expected at least {SHED_AT_LEAST} of {BURST} requests at the metered \
         route shed, got the statuses {:?}",
        metered
            .iter()
            .map(|answer| answer.status)
            .collect::<Vec<u16>>()
    );
    for answer in metered.iter().filter(|answer| answer.status == THROTTLED) {
        shed(&format!("{case} [a shed request of the burst]"), answer);
    }
    for answer in &twin {
        assert_ne!(
            answer.status, THROTTLED,
            "case {case:?}: the twin route is metered by the stage's generous defaults and \
             sheds nothing in the same instant, got {}",
            answer.summary()
        );
    }

    // `DeleteRouteSettings` drops the override, so the same burst meets the stage's defaults.
    // The route key arrives percent-encoded inside the operation's one path label: its space
    // is `%20` and its `/` is `%2F`.
    let route_label = format!("GET%20%2F{}", &METERED_ROUTE[1..]);
    let (status, deleted) = control(
        &emulator,
        "DELETE",
        &format!("/v2/apis/{api_id}/stages/$default/routesettings/{route_label}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteRouteSettings answered {status} {deleted}"
    );
    let (status, stage) = control(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/stages/$default"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetStage answered {status} {stage}"
    );
    assert!(
        stage
            .get("routeSettings")
            .and_then(|settings| settings.get(format!("GET {METERED_ROUTE}")))
            .is_none(),
        "case {case:?}: GetStage must report the override gone, got {stage}"
    );
    let (metered, twin) = interleaved_burst(&emulator, &host).await;
    for (route, answers) in [(METERED_ROUTE, &metered), (TWIN_ROUTE, &twin)] {
        for answer in answers.iter() {
            assert_ne!(
                answer.status, THROTTLED,
                "case {case:?}: {route} is metered by the stage's defaults once its override \
                 is deleted and sheds nothing, got {}",
                answer.summary()
            );
        }
    }
}
