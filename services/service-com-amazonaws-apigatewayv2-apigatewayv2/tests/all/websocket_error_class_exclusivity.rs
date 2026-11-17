// SPDX-License-Identifier: CC0-1.0

//! The ONE `AWS/ApiGateway` error series a failing call of a WEBSOCKET stage lands in, and the
//! latency sample such a call leaves behind.
//!
//! The rule the table below is one slice of: **a WEBSOCKET stage publishes `ClientError`,
//! `ExecutionError` and `IntegrationError` once per call it served, and a call that FAILED
//! increments exactly ONE of them — the series that names whose failure it was, the client's
//! for a call answered with a client-error status, the route's execution for a route the
//! gateway could not execute to a successful end (a function that ran and ended in an
//! unhandled error included), and the integration's for a backend that answered the gateway
//! nothing it could use — while the other two carry an explicit 0 for that same call; and it
//! publishes one `IntegrationLatency` sample per invocation the backend ANSWERED, whether or
//! not that answer was one the gateway could use.** A client that alarms on one class
//! therefore breaches at the rate of the failures that class names, and no failure is reported
//! twice over.
//!
//! The rows span what the rule generalises over: WHICH call fails (the connection request and
//! a frame of an established connection), HOW it fails (a backend that refused the client, a
//! backend that refused it with a server-error status, a function that raised, a backend the
//! invocation never reached, a frame the api routes nowhere), and the dimension set the
//! counters land under (the stage's own, and the per-route series a stage with
//! `DetailedMetricsEnabled` publishes beside them). The healthy row is in the table too: a
//! call that failed nothing earns no class at all.
//!
//! Nothing here is a value this emulator fills in: the traffic is driven over the api's own
//! socket against functions AWS Lambda RAN, and every assertion is on what CloudWatch answers
//! for the series.

use crate::http_api_backend_dispatch::given_function;
use crate::websocket_stage_metrics::{
    closed, counter, emulator, given_api, given_route, given_stage, latency_samples, per_route,
    reached, samples, Expected,
};
use crate::websocket_transport_data_plane::{exchange, exchanged_reply, handshake_at, ClientSocket};

/// The three series a stage publishes once per call, in the order a failing assertion names
/// them.
const ERROR_SERIES: [&str; 3] = ["ClientError", "ExecutionError", "IntegrationError"];

/// What the function a route fronts does when it is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Answers `200`: a route that works.
    Answers,
    /// Ends in an unhandled error, so the function RAN and gave no reply to read.
    Raises,
    /// Refuses the caller with a client-error status of its own.
    RefusesClient,
    /// Refuses the caller with a server-error status of its own.
    RefusesServer,
    /// A function that does not exist, so the invocation reaches no backend at all.
    Absent,
}

/// A handler that answers `200` and nothing else.
const ANSWERING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   return {\"statusCode\": 200}\n";

/// A handler that RAISES, so its route's integration produces no reply at all.
const RAISING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   raise RuntimeError(\"this route fails\")\n";

/// A handler that refuses the caller with a client-error status of its own.
const CLIENT_REFUSING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   return {\"statusCode\": 403, \"body\": \"denied\"}\n";

/// A handler that refuses the caller with a server-error status of its own.
const SERVER_REFUSING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   return {\"statusCode\": 503, \"body\": \"busy\"}\n";

/// The function ARN a route fronts a backend that does not exist with: a well-formed Lambda
/// ARN of a function nothing created, so the invocation the gateway makes is refused by Lambda
/// itself and no backend ever answers it.
const ABSENT_FUNCTION_ARN: &str = "arn:aws:lambda:us-east-1:000000000000:function:never-created";

/// One row of the table: the api a case stands up, the traffic it drives, and the series that
/// traffic owes.
struct Row {
    case: &'static str,
    /// The routes the api holds, each with the backend its integration fronts.
    routes: &'static [(&'static str, Backend)],
    /// The frame the case sends over an established connection, or `None` for a case whose
    /// handshake is refused before any connection exists.
    frame: Option<&'static str>,
    /// The text the frame the api answers that message with must carry, or `None` for a
    /// one-way route, which sends nothing back.
    answer: Option<&'static str>,
    /// The status the refused handshake is answered with, for a case that drives no frame.
    refused_with: Option<u16>,
    /// The ONE series the failing call increments, or `None` for the row nothing failed in.
    earns: Option<&'static str>,
    /// How many CALLS the stage served, which is the number of observations each of the three
    /// series must hold — the explicit zeros included.
    calls: f64,
    /// How many of those calls' invocations the backend ANSWERED, which is the stage's
    /// `IntegrationLatency` sample count.
    answered: f64,
    /// The route the per-route series are additionally expected under, which also turns
    /// `DetailedMetricsEnabled` on for the stage.
    detailed_route: Option<&'static str>,
    /// Whether the backend of that route answered the one invocation it was sent, which is
    /// that route's own `IntegrationLatency` sample count.
    route_answered: f64,
}

/// The stage every row serves its traffic on.
const STAGE: &str = "metered";

/// The backend `routes` name, as the ARN of the function that behaves that way.
fn arn_of(backend: Backend, answering: &str, raising: &str, client: &str, server: &str) -> String {
    match backend {
        Backend::Answers => answering.to_string(),
        Backend::Raises => raising.to_string(),
        Backend::RefusesClient => client.to_string(),
        Backend::RefusesServer => server.to_string(),
        Backend::Absent => ABSENT_FUNCTION_ARN.to_string(),
    }
}

/// Every expectation one row owes: each of the three series carries 1 for the class the call
/// earned and 0 for the other two, over the same number of observations; the latency series
/// holds one sample per invocation the backend answered; and the per-route series of a detailed
/// stage split that same one class.
fn expectations(row: &Row) -> Vec<Expected> {
    let mut expected: Vec<Expected> = vec![latency_samples(row.answered)];
    for series in ERROR_SERIES {
        let earned = row.earns == Some(series);
        expected.push(counter(series, f64::from(u8::from(earned))));
        expected.push(samples(series, row.calls));
        if let Some(route) = row.detailed_route {
            expected.push(per_route(series, route, f64::from(u8::from(earned))));
        }
    }
    if let Some(route) = row.detailed_route {
        expected.push(per_route("IntegrationLatency", route, row.route_answered));
    }
    expected
}

/// One failing call of a WEBSOCKET stage is counted in exactly one error class, whichever call
/// failed and however it failed.
#[tokio::test]
async fn a_failing_call_is_counted_in_exactly_one_error_class() {
    let (emulator, engine) = emulator();
    let answering = given_function(
        &emulator,
        &engine,
        "error classes",
        "ws-class-answering",
        ANSWERING_HANDLER,
    )
    .await;
    let raising = given_function(
        &emulator,
        &engine,
        "error classes",
        "ws-class-raising",
        RAISING_HANDLER,
    )
    .await;
    let refusing_client = given_function(
        &emulator,
        &engine,
        "error classes",
        "ws-class-refusing-client",
        CLIENT_REFUSING_HANDLER,
    )
    .await;
    let refusing_server = given_function(
        &emulator,
        &engine,
        "error classes",
        "ws-class-refusing-server",
        SERVER_REFUSING_HANDLER,
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "a $connect integration that refuses the client with a client-error status",
            routes: &[("$connect", Backend::RefusesClient)],
            frame: None,
            answer: None,
            refused_with: Some(403),
            earns: Some("ClientError"),
            // The connection request, and nothing after it: no connection was established.
            calls: 1.0,
            // The integration answered — it refused the client rather than failing.
            answered: 1.0,
            detailed_route: None,
            route_answered: 0.0,
        },
        Row {
            case: "a refused connection request on a stage that publishes per-route series",
            routes: &[("$connect", Backend::RefusesClient)],
            frame: None,
            answer: None,
            refused_with: Some(403),
            // The same one class, split under {ApiId, Stage, Route} as well: the route that
            // serves a connection request is `$connect`.
            earns: Some("ClientError"),
            calls: 1.0,
            answered: 1.0,
            detailed_route: Some("$connect"),
            route_answered: 1.0,
        },
        Row {
            case: "a $connect integration that refuses the client with a server-error status",
            routes: &[("$connect", Backend::RefusesServer)],
            frame: None,
            answer: None,
            refused_with: Some(503),
            earns: Some("ExecutionError"),
            calls: 1.0,
            answered: 1.0,
            detailed_route: None,
            route_answered: 0.0,
        },
        Row {
            case: "a $connect function that ends in an unhandled error",
            routes: &[("$connect", Backend::Raises)],
            frame: None,
            answer: None,
            refused_with: Some(500),
            earns: Some("ExecutionError"),
            calls: 1.0,
            // The function RAN: it answered the invocation with the error document Lambda
            // reports for an unhandled error.
            answered: 1.0,
            detailed_route: None,
            route_answered: 0.0,
        },
        Row {
            case: "a $connect route whose integration names a function that does not exist",
            routes: &[("$connect", Backend::Absent)],
            frame: None,
            answer: None,
            refused_with: Some(500),
            earns: Some("IntegrationError"),
            calls: 1.0,
            // No backend answered the invocation, so there is no latency to report.
            answered: 0.0,
            detailed_route: None,
            route_answered: 0.0,
        },
        Row {
            case: "a frame whose route's function ends in an unhandled error",
            routes: &[
                ("$connect", Backend::Answers),
                ("$disconnect", Backend::Answers),
                ("boom", Backend::Raises),
            ],
            frame: Some(r#"{"action":"boom"}"#),
            // A route that produced no reply is answered with the gateway's own frame.
            answer: Some("Internal server error"),
            refused_with: None,
            earns: Some("ExecutionError"),
            // The connection request, the frame and the close.
            calls: 3.0,
            answered: 3.0,
            detailed_route: Some("boom"),
            route_answered: 1.0,
        },
        Row {
            case: "a frame whose route's integration names a function that does not exist",
            routes: &[
                ("$connect", Backend::Answers),
                ("$disconnect", Backend::Answers),
                ("gone", Backend::Absent),
            ],
            frame: Some(r#"{"action":"gone"}"#),
            answer: Some("Internal server error"),
            refused_with: None,
            earns: Some("IntegrationError"),
            calls: 3.0,
            // The connection request and the close; the frame's own backend answered nothing.
            answered: 2.0,
            detailed_route: Some("gone"),
            route_answered: 0.0,
        },
        Row {
            case: "a frame the api routes nowhere",
            routes: &[
                ("$connect", Backend::Answers),
                ("$disconnect", Backend::Answers),
            ],
            frame: Some(r#"{"action":"nowhere"}"#),
            // A message the api routes nowhere states the client's own error.
            answer: Some("Forbidden"),
            refused_with: None,
            earns: Some("ClientError"),
            calls: 3.0,
            // The frame reached no integration at all.
            answered: 2.0,
            // A message the api routes nowhere belongs to no route, so it has no per-route
            // series to split.
            detailed_route: None,
            route_answered: 0.0,
        },
        Row {
            case: "a connection and a frame nothing failed in",
            routes: &[
                ("$connect", Backend::Answers),
                ("$disconnect", Backend::Answers),
                ("$default", Backend::Answers),
            ],
            frame: Some(r#"{"action":"anything"}"#),
            // The `$default` route declares no `RouteResponse`, so it is one-way.
            answer: None,
            refused_with: None,
            earns: None,
            calls: 3.0,
            answered: 3.0,
            detailed_route: Some("$default"),
            route_answered: 1.0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let api = given_api(&emulator, case, &format!("error-class-api-{index}")).await;
        for (route_key, backend) in row.routes {
            let arn = arn_of(
                *backend,
                &answering,
                &raising,
                &refusing_client,
                &refusing_server,
            );
            given_route(&emulator, case, &api.api_id, route_key, &arn, false).await;
        }
        given_stage(
            &emulator,
            case,
            &api.api_id,
            STAGE,
            row.detailed_route.is_some(),
        )
        .await;

        let opened = handshake_at(case, &api.authority, &format!("/{STAGE}")).await;
        match (row.refused_with, row.frame) {
            (Some(status), _) => {
                let refused = opened.err();
                assert_eq!(
                    refused,
                    Some(status),
                    "case {case:?}: the handshake must be refused {status}, got {refused:?}"
                );
            }
            (None, frame) => {
                let mut socket: ClientSocket = opened.unwrap_or_else(|status| {
                    panic!("case {case:?}: the handshake must complete, got {status}")
                });
                if let Some(frame) = frame {
                    match row.answer {
                        Some(expected) => {
                            // The reply MUST come, so it is awaited for as long as
                            // `exchanged_reply` waits: the route's function has to run first.
                            let replied = exchanged_reply(case, &mut socket, frame).await;
                            assert!(
                                replied.contains(expected),
                                "case {case:?}: the api must answer a frame carrying \
                                 {expected:?}, got {replied:?}"
                            );
                        }
                        None => {
                            let replied = exchange(case, &mut socket, frame).await;
                            assert!(
                                replied.is_none(),
                                "case {case:?}: a one-way route sends nothing back, got \
                                 {replied:?}"
                            );
                        }
                    }
                }
                closed(case, socket).await;
            }
        }

        reached(&emulator, case, &api.api_id, STAGE, &expectations(row)).await;
    }
}
