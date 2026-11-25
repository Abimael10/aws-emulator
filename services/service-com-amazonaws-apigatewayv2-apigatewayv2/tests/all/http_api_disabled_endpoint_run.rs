// SPDX-License-Identifier: CC0-1.0

//! What an api whose default `execute-api` endpoint is turned off answers over a RUN of
//! requests, with the function its route fronts actually running.
//!
//! The rule:
//!
//! > **While an api's `DisableExecuteApiEndpoint` is set — "specifies whether clients can
//! > invoke your API by using the default execute-api endpoint … To require that clients use a
//! > custom domain name to invoke your API, disable the default endpoint"
//! > (`smithy_query(query="doc",shape="Api",member="DisableExecuteApiEndpoint")`) — EVERY
//! > request to that endpoint is answered `404` `{"message":"Not Found"}`, with the route's
//! > integration not reached for any of them, and the refusal is that api's alone: a twin api
//! > over the same function answers the function's own 200 on every request of the same run.
//! > Clearing the member with `UpdateApi` returns the same address to the function's answer on
//! > every request of a run, with no redeployment and nothing else changed.**
//!
//! Why this suite exists beside the others: `disabled_execute_api_endpoint` spans the
//! addressings and stage shapes of the member and `disabled_execute_api_endpoint_backend` adds
//! a backend that RAN, and `published_invoke_socket_front_door` drives both at the address the
//! control plane publishes, over a real connection. None of them drives a RUN of requests
//! through one transition, which is what says the answer SETTLES — that every request of a run
//! is answered the same way rather than only the first one after each `UpdateApi`.
//!
//! Nothing here asserts on elapsed time: a served answer is waited for as a condition (the
//! function having answered at all) under a generous deadline, and a refusal is asserted on as
//! it arrives.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, given_api, given_function, lambda_integration,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The status and sentence an HTTP api's endpoint answers a request it serves nothing for,
/// which is every request to an api that turned its default endpoint off.
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// The statuses the gateway answers while the backend's runtime is still starting, which the
/// wait below looks past.
const INTEGRATION_FAULT: u16 = 500;
const TIMED_OUT: u16 = 503;

/// The route both apis carry, and the path that reaches it.
const ROUTE_KEY: &str = "GET /hit";
const ROUTE_PATH: &str = "/hit";

/// The body the shared function answers, so a served answer is told from any refusal by the
/// backend's own text rather than by a status alone.
const MARKER: &str = "served-by-the-function";
const BACKEND: &str = "def lambda_handler(event, context):\n    \
     return {'statusCode': 200, 'body': 'served-by-the-function'}\n";

/// How many requests each leg of the run makes: enough that what is asserted is the answer
/// SETTLING rather than the first answer after a transition.
const RUN: usize = 5;

/// How long a served answer is waited for, and how often it is asked for: generous, bounding
/// no behaviour any assertion is about.
const SERVED_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// Stand up one api over `function`: the route and the `$default` stage that serves the api's
/// root. Answers `(ApiId, the host its ApiEndpoint names)`.
async fn given_one(
    emulator: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> (String, String) {
    given_api(
        emulator,
        case,
        lambda_integration(function, "2.0"),
        ROUTE_KEY,
        json!({ "stageName": "$default", "autoDeploy": true }),
    )
    .await
}

/// Set `DisableExecuteApiEndpoint` on `api_id` and read the member back off the answer, so
/// each leg starts from a value the control plane reports.
async fn set_disabled(emulator: &MultiServiceRouter, case: &str, api_id: &str, disabled: bool) {
    let (status, updated) = control(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}"),
        Some(json!({ "disableExecuteApiEndpoint": disabled })),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: UpdateApi answered {status} {updated}"
    );
    assert_eq!(
        updated
            .get("disableExecuteApiEndpoint")
            .and_then(Value::as_bool),
        Some(disabled),
        "case {case:?}: UpdateApi must report the member it was given, got {updated}"
    );
    let (status, read_back) = control(emulator, "GET", &format!("/v2/apis/{api_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetApi answered {status} {read_back}"
    );
    assert_eq!(
        read_back
            .get("disableExecuteApiEndpoint")
            .and_then(Value::as_bool),
        Some(disabled),
        "case {case:?}: GetApi must report the member the update stored, got {read_back}"
    );
}

/// One served answer from `host`, waited for as a CONDITION: the request is made again while
/// the function's runtime is still starting, until the function answers or
/// [`SERVED_DEADLINE`] passes.
async fn served_once(emulator: &MultiServiceRouter, case: &str, host: &str) -> Answer {
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let answer = send(emulator, host, "GET", ROUTE_PATH, &[]).await;
        if answer.status == 200 {
            return answer;
        }
        assert!(
            matches!(answer.status, INTEGRATION_FAULT | TIMED_OUT) && Instant::now() < deadline,
            "case {case:?}: expected the function's own answer, got {}",
            answer.summary()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Assert every request of a run at `host` is answered by the function itself.
async fn serves_a_run(emulator: &MultiServiceRouter, case: &str, host: &str) {
    for request in 0..RUN {
        let case = format!("{case} [request {request} of the run]");
        let answer = served_once(emulator, &case, host).await;
        assert_eq!(
            answer.text, MARKER,
            "case {case:?}: expected the function's own body {MARKER:?}, got {}",
            answer.summary()
        );
    }
}

/// Assert every request of a run at `host` is refused by the endpoint itself, with nothing of
/// the integration behind the route in it.
async fn refuses_a_run(emulator: &MultiServiceRouter, case: &str, host: &str) {
    for request in 0..RUN {
        let case = format!("{case} [request {request} of the run]");
        let answer = send(emulator, host, "GET", ROUTE_PATH, &[]).await;
        assert_eq!(
            answer.status, NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND} on a disabled default endpoint, got {}",
            answer.summary()
        );
        assert_eq!(
            message(&answer),
            NOT_FOUND_MESSAGE,
            "case {case:?}: expected the {NOT_FOUND_MESSAGE:?} envelope, got {}",
            answer.summary()
        );
        assert!(
            !answer.text.contains(MARKER),
            "case {case:?}: a refusal carries nothing of the integration, got {}",
            answer.summary()
        );
    }
}

#[tokio::test]
async fn a_disabled_endpoint_answers_a_whole_run_of_refusals_and_the_twin_serves_through_it() {
    let case = "two apis over one running function, one of which turns its endpoint off";
    let (emulator, engine) = emulator_with_lambda();
    let function = given_function(&emulator, &engine, case, "disabled-run", BACKEND).await;
    let (target_id, target) = given_one(&emulator, case, &function).await;
    let (_twin_id, twin) = given_one(&emulator, case, &function).await;

    serves_a_run(&emulator, &format!("{case} [the target, before]"), &target).await;
    serves_a_run(&emulator, &format!("{case} [the twin, before]"), &twin).await;

    set_disabled(&emulator, case, &target_id, true).await;
    refuses_a_run(&emulator, &format!("{case} [the target, disabled]"), &target).await;
    serves_a_run(
        &emulator,
        &format!("{case} [the twin, while the target is disabled]"),
        &twin,
    )
    .await;

    set_disabled(&emulator, case, &target_id, false).await;
    serves_a_run(&emulator, &format!("{case} [the target, restored]"), &target).await;
    serves_a_run(&emulator, &format!("{case} [the twin, after]"), &twin).await;
}
