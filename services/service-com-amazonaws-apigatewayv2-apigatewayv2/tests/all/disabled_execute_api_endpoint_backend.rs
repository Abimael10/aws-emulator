// SPDX-License-Identifier: CC0-1.0

//! What a disabled default `execute-api` endpoint does to the BACKEND behind its routes, and
//! to a twin api that shares that backend.
//!
//! `disabled_execute_api_endpoint.rs` spans the addressings, protocols and stage shapes of
//! `Api$DisableExecuteApiEndpoint` over an `HTTP_PROXY` route, whose upstream this emulator
//! opens no connection to. This file adds the axis that table cannot carry, because it needs a
//! backend that RAN: an `AWS_PROXY` route over a Lambda function two apis share.
//!
//! The rule these cases assert:
//!
//! > While an api has `DisableExecuteApiEndpoint` set, its default endpoint serves NOTHING of
//! > the api: a request to a route of a deployed stage is answered `404`
//! > `{"message":"Not Found"}` — the gateway's own envelope, carrying nothing of the
//! > integration behind the route — and the refusal is the endpoint's, so a TWIN api over the
//! > same backend whose member is clear keeps answering that backend's own `200` in the same
//! > instant. Clearing the member makes the same address serve the backend again, with no
//! > redeployment of the stage and no change to the route or its integration.
//!
//! The rows span what the rule generalises over: how the stage was released (an `AutoDeploy`
//! `$default` stage that serves the api's root, and a named stage pinned by its own
//! `CreateDeployment`) and how the client addresses the api (its own `execute-api` host, and
//! the emulator's own endpoint with the api id behind the endpoint marker). Where the member's
//! first value comes from is the other file's axis; both rows here drive it with `UpdateApi`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, given_api, given_function, lambda_integration,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The status and sentence an HTTP api's endpoint answers for a request it has nothing for —
/// which is every request to an api that turned its default endpoint off.
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// The body the shared function answers with, so a served answer is told from any refusal by
/// the backend's own text rather than by a status alone.
const MARKER: &str = "served-by-the-backend";

/// The function every row's two apis front, which answers [`MARKER`].
const BACKEND: &str = "def lambda_handler(event, context):\n    \
     return {'statusCode': 200, 'body': 'served-by-the-backend'}\n";

/// The route both apis of a row carry, and the path a client reaches it at.
const ROUTE_KEY: &str = "GET /hit";
const ROUTE_PATH: &str = "/hit";

/// The statuses the gateway answers while a backend's runtime is still starting, which a row
/// expecting the backend's own answer looks past ([`answered`]).
const INTEGRATION_FAULT: u16 = 500;
const TIMED_OUT: u16 = 503;

/// How long a row keeps looking for the backend's own answer, and how often: generous, and
/// bounding no behaviour any row asserts on — what is waited for is a CONDITION (the backend
/// answered at all), never a fixed time.
const BACKEND_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator's own endpoint, which is where a client that resolves no AWS host points.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// One api of a row: its id, and the host and path a client of it addresses.
struct Addressed {
    api_id: String,
    host: String,
    path: String,
}

/// One row: the stage both apis of the row carry, and how a client addresses them.
struct Row {
    case: &'static str,
    /// The `CreateStage` body. A stage with `AutoDeploy` on has a deployment released to it;
    /// one without is released by the `CreateDeployment` below.
    stage: fn() -> Value,
    /// Whether the row releases a deployment to the stage itself.
    release: bool,
    /// The host the client addresses, from the api's own announced endpoint host.
    host: fn(&str, &str) -> String,
    /// The path that reaches the route at that host.
    path: fn(&str) -> String,
}

const ROWS: &[Row] = &[
    Row {
        case: "an AutoDeploy $default stage on the api's own execute-api host",
        stage: || json!({ "stageName": "$default", "autoDeploy": true }),
        release: false,
        host: |_api_id, endpoint_host| endpoint_host.to_string(),
        path: |_api_id| ROUTE_PATH.to_string(),
    },
    Row {
        case: "a named stage pinned by its own deployment, at the emulator's own endpoint",
        stage: || json!({ "stageName": "prod" }),
        release: true,
        host: |_api_id, _endpoint_host| EMULATOR_ENDPOINT.to_string(),
        path: |api_id| format!("/_aws/execute-api/{api_id}/prod{ROUTE_PATH}"),
    },
];

/// Stand up one api of the row over `function`, and answer where a client reaches its route.
async fn given_one(
    emulator: &MultiServiceRouter,
    case: &str,
    row: &Row,
    function: &str,
) -> Addressed {
    let (api_id, endpoint_host) = given_api(
        emulator,
        case,
        lambda_integration(function, "2.0"),
        ROUTE_KEY,
        (row.stage)(),
    )
    .await;
    if row.release {
        let (status, deployment) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            Some(json!({ "stageName": "prod" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateDeployment answered {status} {deployment}"
        );
    }
    Addressed {
        host: (row.host)(&api_id, &endpoint_host),
        path: (row.path)(&api_id),
        api_id,
    }
}

/// Set `DisableExecuteApiEndpoint` on the api and read the member back, so each step starts
/// from a value the control plane reports.
async fn set_disabled(emulator: &MultiServiceRouter, case: &str, api_id: &str, disabled: bool) {
    let (status, updated) = control(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}"),
        Some(json!({ "disableExecuteApiEndpoint": disabled })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: UpdateApi answered {status} {updated}"
    );
    assert_eq!(
        updated
            .get("disableExecuteApiEndpoint")
            .and_then(Value::as_bool),
        Some(disabled),
        "case {case:?}: UpdateApi must report the member it was given, got {updated}"
    );
}

/// The route's answer once the backend produced one: looked at again while the gateway is
/// still answering for a runtime that had not started, and never past the deadline.
async fn answered(emulator: &MultiServiceRouter, at: &Addressed, expected: u16) -> Answer {
    let deadline = Instant::now() + BACKEND_DEADLINE;
    loop {
        let answer = send(emulator, &at.host, "GET", &at.path, &[]).await;
        let transient = answer.status != expected
            && (answer.status == INTEGRATION_FAULT || answer.status == TIMED_OUT);
        if !transient || Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Assert the backend's own answer reached the client.
fn served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the backend's own 200, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.text, MARKER,
        "case {case:?}: expected the backend's own body {MARKER:?}, got {}",
        answer.summary()
    );
}

/// Assert the endpoint refused the request itself, with nothing of the integration in it.
fn refused(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, NOT_FOUND,
        "case {case:?}: expected {NOT_FOUND} on a disabled default endpoint, got {}",
        answer.summary()
    );
    assert_eq!(
        message(answer),
        NOT_FOUND_MESSAGE,
        "case {case:?}: expected the {NOT_FOUND_MESSAGE:?} body, got {}",
        answer.summary()
    );
    assert!(
        !answer.text.contains(MARKER),
        "case {case:?}: a refusal carries nothing of the integration behind the route, got {}",
        answer.summary()
    );
}

#[tokio::test]
async fn a_disabled_endpoint_refuses_while_a_twin_api_over_the_same_backend_keeps_serving() {
    for row in ROWS {
        let case = row.case;
        let (emulator, engine) = emulator_with_lambda();
        let function = given_function(&emulator, &engine, case, "shared-backend", BACKEND).await;
        let target = given_one(&emulator, case, row, &function).await;
        let twin = given_one(&emulator, case, row, &function).await;

        // Both apis serve the same backend to begin with.
        served(
            &format!("{case} [the target, endpoint enabled]"),
            &answered(&emulator, &target, 200).await,
        );
        served(
            &format!("{case} [the twin, endpoint enabled]"),
            &answered(&emulator, &twin, 200).await,
        );

        // The target turns its default endpoint off: its own address serves nothing, while
        // the twin over the same backend answers in the same instant.
        set_disabled(&emulator, case, &target.api_id, true).await;
        let refusal = send(&emulator, &target.host, "GET", &target.path, &[]).await;
        refused(&format!("{case} [the target, endpoint disabled]"), &refusal);
        served(
            &format!("{case} [the twin, while the target is disabled]"),
            &answered(&emulator, &twin, 200).await,
        );

        // And clearing the member restores the backend's answer on the SAME address, with no
        // redeployment and nothing else changed.
        set_disabled(&emulator, case, &target.api_id, false).await;
        served(
            &format!("{case} [the target, endpoint restored]"),
            &answered(&emulator, &target, 200).await,
        );
    }
}
