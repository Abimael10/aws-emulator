// SPDX-License-Identifier: CC0-1.0

//! How MANY of a WEBSOCKET stage's `AWS/ApiGateway` error counters one failing call lands in,
//! over traffic that fails several times over and beside a stage that fails nothing.
//!
//! The rule the table below is one slice of: **a WEBSOCKET stage publishes `ClientError`,
//! `ExecutionError` and `IntegrationError` once per call it served, and each FAILING call adds
//! exactly one to the ONE series that names whose failure it was — the client's for a call
//! answered with a client-error status, the route's execution for a route the gateway could not
//! execute to a successful end (a function that ran and ended in an unhandled error included),
//! and the integration's for a backend that answered the gateway nothing it could use — while
//! the other two take an explicit 0 from that same call.** So a stage's three error totals
//! partition the failures it served: each class stands at the number of failures it names, no
//! failure is reported in a second class, and a stage that fails nothing publishes all three
//! at an explicit 0 whatever a sibling stage of the same deployment did.
//!
//! The rows span what the rule generalises over: WHICH call fails (a connection request the
//! `$connect` backend refused, and a frame of an established connection), HOW it fails (a
//! client-error reply, a function that raised, a backend the invocation never reached, a frame
//! the api routes nowhere), HOW MANY failures the stage served (none, one, one of each class,
//! and two of one class), and which classes they fall in. Every row drives a clean twin stage
//! released from the SAME deployment, whose own three series must stay at nought.
//!
//! The totals are the per-event counting rule rather than one scenario's numbers: each row
//! states the failures it drives, and the number of observations every series holds is the
//! number of calls that row's traffic made.
//!
//! Nothing here is a value this emulator fills in: the traffic is driven over the api's own
//! socket against functions AWS Lambda RAN, and every assertion is on what CloudWatch answers
//! for the series.

use crate::http_api_backend_dispatch::given_function;
use crate::websocket_stage_metrics::{
    closed, counter, emulator, given_api, given_route, given_stages_on_one_deployment, reached,
    samples, Expected, ABSENT_FUNCTION_ARN, ECHO_HANDLER, ERROR_SERIES, OK_HANDLER,
    RAISING_HANDLER,
};
use crate::websocket_transport_data_plane::{exchanged_reply, handshake_at, ClientSocket};

/// The stage every row drives its failing traffic on, and the clean twin released from the
/// same deployment beside it.
const FAILING_STAGE: &str = "guard";
const CLEAN_STAGE: &str = "live";

/// The query string the `$connect` backend admits a handshake with. A handshake that carries
/// no token is refused by that backend's own reply, which is how a client is denied at the
/// connection request.
const ADMITTED_QUERY: &str = "?token=ok";

/// The status the `$connect` backend refuses a handshake with, and so the status the client
/// reads for a refused connection request.
const REFUSED_STATUS: u16 = 403;

/// The routes every row's api holds for the frames it drives. The api holds NO `$default`
/// route, so a frame naming none of these selects no route at all.
const SERVED_ROUTE: &str = "ok";
const RAISING_ROUTE: &str = "boom";
const UNREACHABLE_ROUTE: &str = "gone";

/// The `$connect` handler: it ADMITS a handshake carrying the token it looks for and refuses
/// every other one with a client-error status of its own, which is how a backend denies a
/// connection request rather than failing.
const GATING_CONNECT_HANDLER: &str = r#"def lambda_handler(event, context):
    parameters = event.get("queryStringParameters") or {}
    if parameters.get("token") != "ok":
        return {"statusCode": 403, "body": "denied"}
    return {"statusCode": 200}
"#;

/// The text the gateway's own frame carries for a call whose route produced no reply, and for
/// a message the api routes nowhere.
const INTERNAL_FRAME: &str = "Internal server error";
const FORBIDDEN_FRAME: &str = "Forbidden";

/// The text a two-way route's own reply carries: the `eventType` of the event it was handed,
/// which is what tells that route's answer from the gateway's own frame.
const MESSAGE_EVENT: &str = "MESSAGE";

/// One frame a row drives over an established connection, by what serving it comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// A frame the route that serves it answers successfully: it fails nothing.
    Served,
    /// A frame whose route's function RAISES, so the route did not execute to a successful
    /// end.
    Raising,
    /// A frame whose route's integration names a function that does not exist, so no backend
    /// ever answers the invocation.
    Unreachable,
    /// A frame the api routes nowhere: it names no route of the api, which holds no
    /// `$default`.
    Unrouted,
}

impl Frame {
    /// The payload that drives this frame: its `action` is what the api's route selection
    /// expression reads.
    fn payload(self) -> &'static str {
        match self {
            Self::Served => r#"{"action":"ok"}"#,
            Self::Raising => r#"{"action":"boom"}"#,
            Self::Unreachable => r#"{"action":"gone"}"#,
            Self::Unrouted => r#"{"action":"nowhere"}"#,
        }
    }

    /// The text the frame the api answers this message with must carry: a two-way route's own
    /// reply, or the gateway's own frame for a call it could not serve.
    fn answer(self) -> &'static str {
        match self {
            Self::Served => MESSAGE_EVENT,
            Self::Raising | Self::Unreachable => INTERNAL_FRAME,
            Self::Unrouted => FORBIDDEN_FRAME,
        }
    }
}

/// One row of the table: the traffic it drives on the failing stage, and the total each of the
/// three error series owes once that traffic has been served.
struct Row {
    case: &'static str,
    /// How many connection requests the `$connect` backend refuses — each one a failing call
    /// of its own, and none of them a connection.
    refusals: usize,
    /// The frames driven over ONE admitted connection, in order.
    frames: &'static [Frame],
    /// What the three totals must stand at: the number of failures each class names.
    client: f64,
    execution: f64,
    integration: f64,
}

impl Row {
    /// How many CALLS this row's failing stage served, which is the number of observations
    /// each of the three series holds: the admitted connection request and its close, one per
    /// frame driven over it, and one per refused connection request.
    fn calls(&self) -> f64 {
        2.0 + self.frames.len() as f64 + self.refusals as f64
    }
}

/// How many calls the CLEAN twin serves: a connection request the backend admitted, one frame
/// its route served, and the close.
const CLEAN_CALLS: f64 = 3.0;

/// The three error series of one stage, each read by its total and by how many observations
/// that total is folded over. The totals are given in [`ERROR_SERIES`] order.
fn error_series(client: f64, execution: f64, integration: f64, calls: f64) -> Vec<Expected> {
    let mut expected = Vec::new();
    for (series, total) in ERROR_SERIES
        .into_iter()
        .zip([client, execution, integration])
    {
        expected.push(counter(series, total));
        // A zero is as much an observation as a one: what tells a counter standing at an
        // explicit nought from one no call ever published is its sample count.
        expected.push(samples(series, calls));
    }
    expected
}

/// Open a handshake the `$connect` backend admits on `stage`, and fail naming the status when
/// it is refused.
async fn admitted(case: &str, authority: &str, stage: &str) -> ClientSocket {
    handshake_at(case, authority, &format!("/{stage}{ADMITTED_QUERY}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the handshake on {stage:?} must complete, got {status}")
        })
}

/// A stage's error totals partition the failures it served: each failing call adds one to one
/// class, however many failures the stage served and whichever classes they fall in, and a
/// sibling stage of the same deployment keeps all three at nought.
#[tokio::test]
async fn each_failing_call_adds_one_to_exactly_one_error_class() {
    let setup = "the functions every row's routes front";
    let (emulator, engine) = emulator();
    let gating = given_function(
        &emulator,
        &engine,
        setup,
        "ws-split-connect",
        GATING_CONNECT_HANDLER,
    )
    .await;
    let lifecycle =
        given_function(&emulator, &engine, setup, "ws-split-lifecycle", OK_HANDLER).await;
    let echo = given_function(&emulator, &engine, setup, "ws-split-echo", ECHO_HANDLER).await;
    let raising =
        given_function(&emulator, &engine, setup, "ws-split-raising", RAISING_HANDLER).await;

    let rows: &[Row] = &[
        Row {
            case: "a refused connection request and a raising route on one stage",
            refusals: 1,
            frames: &[Frame::Raising],
            // The refusal is the client's failure and the raising route is the route's: one
            // each, and nothing at all for the integration's own class.
            client: 1.0,
            execution: 1.0,
            integration: 0.0,
        },
        Row {
            case: "one failure of each class on one stage",
            refusals: 0,
            frames: &[Frame::Unrouted, Frame::Raising, Frame::Unreachable],
            client: 1.0,
            execution: 1.0,
            integration: 1.0,
        },
        Row {
            case: "two failures of each of two classes on one stage",
            refusals: 2,
            frames: &[Frame::Raising, Frame::Raising],
            // One increment per failing EVENT: two refusals and two raising routes stand at
            // two apiece rather than at one or at four.
            client: 2.0,
            execution: 2.0,
            integration: 0.0,
        },
        Row {
            case: "a stage whose every call was served",
            refusals: 0,
            frames: &[Frame::Served],
            client: 0.0,
            execution: 0.0,
            integration: 0.0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let api = given_api(&emulator, case, &format!("error-split-api-{index}")).await;
        for (route_key, function, two_way) in [
            ("$connect", gating.as_str(), false),
            ("$disconnect", lifecycle.as_str(), false),
            // The route that SERVES a frame answers it, so a case reads its reply instead of
            // waiting on a one-way route that sends nothing back.
            (SERVED_ROUTE, echo.as_str(), true),
            (RAISING_ROUTE, raising.as_str(), false),
            (UNREACHABLE_ROUTE, ABSENT_FUNCTION_ARN, false),
        ] {
            given_route(&emulator, case, &api.api_id, route_key, function, two_way).await;
        }
        // Both stages are released from ONE deployment, so the two sets of series below differ
        // in nothing but the traffic each stage carried.
        given_stages_on_one_deployment(
            &emulator,
            case,
            &api.api_id,
            &[CLEAN_STAGE, FAILING_STAGE],
        )
        .await;

        // The clean twin's own traffic: a connection the backend admitted, one frame its route
        // served, and the close.
        let mut clean = admitted(case, &api.authority, CLEAN_STAGE).await;
        let replied = exchanged_reply(case, &mut clean, Frame::Served.payload()).await;
        assert!(
            replied.contains(Frame::Served.answer()),
            "case {case:?}: the clean stage's two-way route must answer its own event, got \
             {replied:?}"
        );
        closed(case, clean).await;

        // Every connection request this row's backend refuses: each is a failing call of the
        // failing stage, and none of them establishes a connection.
        for _ in 0..row.refusals {
            let refused = handshake_at(case, &api.authority, &format!("/{FAILING_STAGE}"))
                .await
                .err();
            assert_eq!(
                refused,
                Some(REFUSED_STATUS),
                "case {case:?}: a handshake carrying no token must be refused \
                 {REFUSED_STATUS}, got {refused:?}"
            );
        }

        // The frames of this row, over one connection the backend admitted.
        let mut socket = admitted(case, &api.authority, FAILING_STAGE).await;
        for frame in row.frames {
            let replied = exchanged_reply(case, &mut socket, frame.payload()).await;
            assert!(
                replied.contains(frame.answer()),
                "case {case:?}: the frame {payload:?} must be answered with a frame carrying \
                 {expected:?}, got {replied:?}",
                payload = frame.payload(),
                expected = frame.answer()
            );
        }
        closed(case, socket).await;

        reached(
            &emulator,
            case,
            &api.api_id,
            FAILING_STAGE,
            &error_series(row.client, row.execution, row.integration, row.calls()),
        )
        .await;
        // The twin released from the same deployment fails nothing, and publishes all three
        // series at an explicit nought for each of the calls it served.
        reached(
            &emulator,
            case,
            &api.api_id,
            CLEAN_STAGE,
            &error_series(0.0, 0.0, 0.0, CLEAN_CALLS),
        )
        .await;
    }
}
