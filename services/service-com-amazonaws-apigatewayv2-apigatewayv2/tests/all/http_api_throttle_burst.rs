// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what the THROTTLE of a deployed HTTP api stage does to the
//! requests a client sends to one route AT THE SAME TIME.
//!
//! The rule every table below is one slice of: **a stage's route settings state a
//! rate-and-burst pair, and the bucket the `ThrottlingRateLimit` refills is the one budget
//! every request the route is served takes from — while the `ThrottlingBurstLimit` is how many
//! of the requests in flight TOGETHER are served and how far they may draw on the rate before
//! it has refilled the bucket, so requests arriving at once at a throttled route are served up
//! to the burst limit and only the EXCESS is shed `429` `{"message":"Too Many Requests"}`.** A
//! throttled route is a rate limiter, never a gate that closes: requests arriving together are
//! never all shed, they are never served more than the burst, what they were served is charged
//! so the route serves again only once the rate has repaid it, and a caller that waits for
//! each answer before asking again takes only what the bucket holds.
//!
//! Every test drives the real data plane — `MultiServiceRouter::dispatch` over the api's own
//! invoke endpoint, one task per request — so what is asserted is what concurrent clients see.
//! What makes requests arrive TOGETHER here is the BACKEND: the route fronts a Lambda function
//! that waits before it answers ([`HOLDS_ITS_REQUEST`]), so the gateway is still answering the
//! request it was served while the others arrive, exactly as a backend that takes time to
//! answer does in production. The wait is orders of magnitude longer than the microseconds the
//! requests take to reach the gateway in process, and nothing here asserts on elapsed time.
//!
//! What this suite adds over its neighbours: `http_api_backend_dispatch` pins that a route the
//! stage throttles answers `429` while its twin is served, one request at a time;
//! `state::route_throttle`'s own table pins the arithmetic over a clock the case controls.
//! Neither drives requests that arrive at one route together, which is what a client that
//! sizes a throttle to protect a backend does and what the issue's reproduction does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function, lambda_integration};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The stage every row below serves on, and the route key its one route carries.
const STAGE: &str = "$default";
const ROUTE_KEY: &str = "GET /";
const PATH: &str = "/";

/// The status the route's backend answers, and the body it carries: a request the throttle
/// served reached the integration and carries what the function returned, which is how it is
/// told apart from a shed one.
const SERVED: u16 = 201;
const SERVED_BODY: &str = r#"{"held": true}"#;

/// The status and sentence a request the stage's route settings throttle sheds.
const THROTTLED: u16 = 429;
const THROTTLED_MESSAGE: &str = "Too Many Requests";

/// The backend of the route: a function that WAITS before it answers, so the gateway is still
/// answering the request it served while the other requests arrive. The wait is the function's
/// own (`time.sleep`, which this runtime clamps to what the invocation has left), so nothing
/// in the test sleeps on the emulator's behalf, and it is seconds long against the
/// microseconds the in-process requests take to reach the gateway — room enough that a loaded
/// host does not decide what the assertions see.
const HOLDS_ITS_REQUEST: &str = "import json, time\n\
     def lambda_handler(event, context):\n\
     \x20   time.sleep(3.0)\n\
     \x20   return {'statusCode': 201, 'headers': {'content-type': 'application/json'},\n\
     \x20            'body': json.dumps({'held': True})}\n";

/// How long a poll waits for the rate to repay what was drawn before it gives up — generous,
/// because what is asserted is that the route serves AGAIN and not how fast the host runs.
const REPAYMENT_BUDGET: Duration = Duration::from_secs(60);

/// How long each look of that poll waits before looking again.
const LOOK_AGAIN_IN: Duration = Duration::from_millis(50);

/// One row of the together-at-once table.
struct Row {
    case: &'static str,
    /// `ThrottlingRateLimit`: slow enough everywhere below that the rate refills nothing while
    /// the requests of one row arrive, so what each row asserts is the burst and not the clock.
    rate: f64,
    /// `ThrottlingBurstLimit` — how many of the requests in flight together are served.
    burst: i32,
    /// Whether the stage states the pair for THIS route key (`RouteSettings`) rather than as
    /// its `DefaultRouteSettings` — both are the settings the route is metered by.
    stated_for_the_route: bool,
    /// How many requests are sent at once, after the one the backend is still answering.
    submissions: usize,
}

/// The rows span what the rule generalises over: how wide the burst the settings state is, how
/// many requests meet it (a pair, fewer than the burst, and many more than it), where the stage
/// states the pair, and a rate written in fractions of a request.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded shape: forty requests at once at a burst of two",
            rate: 0.05,
            burst: 2,
            stated_for_the_route: false,
            submissions: 40,
        },
        Row {
            case: "a pair of requests at once at a burst of two",
            rate: 0.05,
            burst: 2,
            stated_for_the_route: false,
            submissions: 2,
        },
        Row {
            case: "a burst of four the stage states for the route itself",
            rate: 0.05,
            burst: 4,
            stated_for_the_route: true,
            submissions: 40,
        },
        Row {
            case: "a burst wider than the requests that meet it",
            rate: 0.05,
            burst: 4,
            stated_for_the_route: false,
            submissions: 3,
        },
        Row {
            case: "a rate written in fractions of a request",
            rate: 0.5,
            burst: 3,
            stated_for_the_route: false,
            submissions: 20,
        },
    ]
}

/// RULE: of the requests in flight together at one route, the gateway serves the burst limit —
/// the one it is already answering included — and sheds the excess, whatever the bucket the
/// rate refills held when they arrived.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_in_flight_together_are_served_up_to_the_burst_limit() {
    for row in rows() {
        let case = row.case;
        let (emulator, engine) = emulator_with_lambda();
        let emulator = Arc::new(emulator);
        let function = given_function(&emulator, &engine, case, "held-function", HOLDS_ITS_REQUEST)
            .await;
        let (_api_id, host) = given_throttled_api(&emulator, &row, &function).await;
        let burst = usize::try_from(row.burst).expect("the row's burst is a count");

        // Spend the bucket down to ONE request's capacity, the way the requests already served
        // spend it — each of these is answered before the next is sent, so the bucket alone
        // answers them and what is left when the batch below arrives is one request.
        for taken in 1..=(burst - 1) {
            let served = send(&emulator, &host, "GET", PATH, &[]).await;
            assert_eq!(
                served.status,
                SERVED,
                "case {case:?}: expected request {taken} of the bucket's capacity served, got {}",
                served.summary()
            );
        }

        // The request the backend is still answering, and then the others — so every one of
        // them arrives while the route has a request in flight, and what the bucket holds for
        // them is the single request the spend above left.
        let held = {
            let emulator = Arc::clone(&emulator);
            let host = host.clone();
            tokio::spawn(async move { send(&emulator, &host, "GET", PATH, &[]).await })
        };
        let mut answers = at_once(&emulator, &host, row.submissions).await;
        answers.push(held.await.expect("the held request answered"));

        let served = answers
            .iter()
            .filter(|answer| answer.status == SERVED)
            .count();
        let shed: Vec<&Answer> = answers
            .iter()
            .filter(|answer| answer.status == THROTTLED)
            .collect();
        let statuses: Vec<u16> = answers.iter().map(|answer| answer.status).collect();
        // Of everything in flight together — the one the backend held and the ones that
        // arrived while it did — the burst is what was served, and the rest were shed: never
        // nothing, and never more than the burst at once, whether the bucket held capacity for
        // them or the rate had not refilled it yet.
        assert_eq!(
            served,
            burst.min(answers.len()),
            "case {case:?}: expected {} of {} requests in flight together served, got \
             {statuses:?}",
            burst.min(answers.len()),
            answers.len()
        );
        assert_eq!(
            shed.len(),
            answers.len() - burst.min(answers.len()),
            "case {case:?}: expected the excess of them shed, got {statuses:?}"
        );
        // A served request is the BACKEND's answer, so the route's integration was reached for
        // it; a shed one is the gateway's own sentence, and its integration was not.
        for answer in answers.iter().filter(|answer| answer.status == SERVED) {
            assert_eq!(
                answer.text, SERVED_BODY,
                "case {case:?}: expected a served request answered by the backend, got {}",
                answer.summary()
            );
        }
        for answer in &shed {
            assert_eq!(
                answer.body,
                json!({ "message": THROTTLED_MESSAGE }),
                "case {case:?}: expected the throttling body for a shed request, got {}",
                answer.summary()
            );
        }
    }
}

/// RULE, the other half: what the requests in flight together were served is charged to the
/// same bucket, so the route is a rate limiter rather than a gate — it serves again once the
/// rate has repaid that draw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_throttled_route_serves_again_once_the_rate_repays_what_was_drawn() {
    let case = "a route metered at five requests a second, burst two, after a shed set";
    let (emulator, engine) = emulator_with_lambda();
    let emulator = Arc::new(emulator);
    let function =
        given_function(&emulator, &engine, case, "repaid-function", HOLDS_ITS_REQUEST).await;
    let row = Row {
        case,
        rate: 5.0,
        burst: 2,
        stated_for_the_route: false,
        submissions: 8,
    };
    let (_api_id, host) = given_throttled_api(&emulator, &row, &function).await;

    // Spend the bucket to one request's capacity, then the request the backend holds and the
    // set that arrives while it does: the burst is served and the rest are shed.
    let spent = send(&emulator, &host, "GET", PATH, &[]).await;
    assert_eq!(
        spent.status,
        SERVED,
        "case {case:?}: expected the first of the bucket's capacity served, got {}",
        spent.summary()
    );
    let held = {
        let emulator = Arc::clone(&emulator);
        let host = host.clone();
        tokio::spawn(async move { send(&emulator, &host, "GET", PATH, &[]).await })
    };
    let mut answers = at_once(&emulator, &host, row.submissions).await;
    answers.push(held.await.expect("the held request answered"));
    let shed = answers
        .iter()
        .filter(|answer| answer.status == THROTTLED)
        .count();
    assert_eq!(
        shed,
        answers.len() - 2,
        "case {case:?}: expected the excess of the requests in flight together shed, got {:?}",
        answers
            .iter()
            .map(|answer| answer.status)
            .collect::<Vec<u16>>()
    );

    // Look again until the rate has repaid what they drew: the condition is that the route
    // serves, not that any particular amount of time has gone by.
    let deadline = Instant::now() + REPAYMENT_BUDGET;
    let mut last = send(&emulator, &host, "GET", PATH, &[]).await;
    while last.status != SERVED && Instant::now() < deadline {
        tokio::time::sleep(LOOK_AGAIN_IN).await;
        last = send(&emulator, &host, "GET", PATH, &[]).await;
    }
    assert_eq!(
        last.status,
        SERVED,
        "case {case:?}: expected the route served again once the rate repaid what was drawn, \
         got {}",
        last.summary()
    );
    assert!(
        message(&last).is_empty(),
        "case {case:?}: expected the repaid route answered by its backend rather than with a \
         sentence, got {}",
        last.summary()
    );
}

/// Send `submissions` requests to the route AT ONCE, each from its own task, and answer them
/// in the order they were started.
async fn at_once(
    emulator: &Arc<MultiServiceRouter>,
    host: &str,
    submissions: usize,
) -> Vec<Answer> {
    let mut pending = Vec::with_capacity(submissions);
    for _ in 0..submissions {
        let emulator = Arc::clone(emulator);
        let host = host.to_string();
        pending.push(tokio::spawn(async move {
            send(&emulator, &host, "GET", PATH, &[]).await
        }));
    }
    let mut answers = Vec::with_capacity(submissions);
    for task in pending {
        answers.push(task.await.expect("the request answered"));
    }
    answers
}

/// An HTTP api with one route over `function` and a `$default` stage whose route settings state
/// the row's pair. Answers `(ApiId, the host its ApiEndpoint names)`.
async fn given_throttled_api(
    emulator: &MultiServiceRouter,
    row: &Row,
    function: &str,
) -> (String, String) {
    let case = row.case;
    let (status, api) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "throttled-burst-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = member(&api, "apiId");
    let host = member(&api, "apiEndpoint")
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();

    let (status, integration) = call(
        emulator,
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
    let (status, route) = call(
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

    let settings = json!({
        "throttlingRateLimit": row.rate,
        "throttlingBurstLimit": row.burst
    });
    let mut stage = json!({ "stageName": STAGE, "autoDeploy": true });
    if row.stated_for_the_route {
        stage["routeSettings"] = json!({ ROUTE_KEY: settings });
    } else {
        stage["defaultRouteSettings"] = settings;
    }
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(stage),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {created}"
    );
    (api_id, host)
}

/// One control-plane request against this service, signed the way this crate's suites sign.
async fn call(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    crate::api_lifecycle::call(emulator, method, uri, body).await
}

/// One string member of an answer.
fn member(answer: &Value, name: &str) -> String {
    answer
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the answer carries no {name}: {answer}"))
        .to_string()
}
