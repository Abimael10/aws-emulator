// SPDX-License-Identifier: CC0-1.0

//! Which `AWS/ApiGateway` series a deployed WEBSOCKET stage leaves behind for a client to
//! FIND, and how the stage-wide reading of each one relates to its per-route split — read
//! through CloudWatch's catalogue (`ListMetrics`) and its array reader (`GetMetricData`), the
//! two calls a dashboard resolves a panel with and an alarm is graded from.
//!
//! The rule the tables below are one slice of:
//!
//! > A deployed WEBSOCKET stage publishes, in namespace `AWS/ApiGateway`, a series per counter
//! > its own traffic produced — `ConnectCount`, `MessageCount`, `IntegrationLatency`,
//! > `ClientError`, `ExecutionError` and `IntegrationError` — under the dimension set
//! > {ApiId, Stage}, and the same counters additionally under {ApiId, Stage, Route} for each
//! > route a stage with `DetailedMetricsEnabled` carried a call for, so that the stage-wide
//! > reading of every series is the TOTAL of its per-route readings; a stage the setting is
//! > off for publishes the stage-wide series alone, and a route that carried no call has no
//! > series of its own at all.
//!
//! The rows span what the rule generalises over: the series (all six, each read both by the
//! total its calls produced and by the number of observations it holds), the stage (one with
//! `DetailedMetricsEnabled` and one without, carrying the SAME traffic), the route (the two
//! lifecycle routes, a two-way route that answered, a route whose function raised, and a
//! declared route no call reached), the dimension set each measurement lands under, and the
//! reader the client uses (the catalogue that says WHICH series exist, and the array reader
//! that says what each holds).
//!
//! What this suite pins beside [`crate::websocket_stage_metrics`], which asserts the values of
//! one stage's series: that the series are DISCOVERABLE without knowing their names, that no
//! series exists for a route the stage publishes none for, and that the stage-wide total and
//! the per-route split are two readings of the same traffic rather than two independent
//! accounts of it.
//!
//! Nothing here is a value this emulator fills in: the traffic is driven over a real socket
//! against functions AWS Lambda RAN, and every assertion is on what CloudWatch answers —
//! which is why Lambda, the `@connections` management API and CloudWatch are all mounted
//! beside this service on one engine ([`crate::websocket_stage_metrics::emulator`]).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::given_function;
use crate::websocket_stage_metrics::{
    closed, emulator, given_api, given_route, given_stage, ECHO_HANDLER, OK_HANDLER,
    RAISING_HANDLER,
};
use crate::websocket_transport_data_plane::{exchanged_reply, handshake_at, ClientSocket};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The namespace an API Gateway stage publishes its own metrics to, and the dimensions those
/// series are published under.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_ID_DIMENSION: &str = "ApiId";
const STAGE_DIMENSION: &str = "Stage";
const ROUTE_DIMENSION: &str = "Route";

/// The six series a WEBSOCKET stage publishes about its own traffic. No Smithy shape models
/// metric emission, so the vocabulary is the one the API Gateway developer guide publishes
/// under "Monitoring WebSocket API execution with CloudWatch metrics"; every entry is read
/// below, under both dimension sets.
const CONNECT_COUNT: &str = "ConnectCount";
const MESSAGE_COUNT: &str = "MessageCount";
const INTEGRATION_LATENCY: &str = "IntegrationLatency";
const CLIENT_ERROR: &str = "ClientError";
const EXECUTION_ERROR: &str = "ExecutionError";
const INTEGRATION_ERROR: &str = "IntegrationError";
const SERIES: [&str; 6] = [
    CONNECT_COUNT,
    MESSAGE_COUNT,
    INTEGRATION_LATENCY,
    CLIENT_ERROR,
    EXECUTION_ERROR,
    INTEGRATION_ERROR,
];

/// The two statistics every series is read by: the TOTAL its calls produced, and HOW MANY
/// observations it holds — which is what tells a counter standing at an explicit zero from one
/// no call ever published.
const SUM: &str = "Sum";
const SAMPLE_COUNT: &str = "SampleCount";

/// The stage with `DetailedMetricsEnabled` in its `DefaultRouteSettings`, and the stage that
/// states no route settings at all. Both carry the SAME traffic.
const DETAILED_STAGE: &str = "detailed";
const PLAIN_STAGE: &str = "plain";

/// Every route the api declares: the two lifecycle routes, the catch-all no call below
/// reaches, a two-way route that answers, and a route whose function raises.
const ROUTES: [&str; 5] = ["$connect", "$disconnect", "$default", "reply", "boom"];

/// What the traffic each stage carries owes its STAGE-WIDE series, as
/// `(metric, statistic, value)`.
///
/// The traffic is one connection: the connection request, a two-way frame its route answered,
/// a frame whose route's function raised and which the gateway answered with its own frame,
/// and the close. So the stage served FOUR calls and carried SIX messages — the four calls,
/// the route's reply and the gateway's own frame — four integrations answered, and exactly one
/// call ended in the route's own execution failing.
const STAGE_OWED: &[(&str, &str, f64)] = &[
    (CONNECT_COUNT, SUM, 1.0),
    (CONNECT_COUNT, SAMPLE_COUNT, 1.0),
    (MESSAGE_COUNT, SUM, 6.0),
    (MESSAGE_COUNT, SAMPLE_COUNT, 6.0),
    // A latency is whatever it was measured as, so this series states only how many
    // invocations were answered.
    (INTEGRATION_LATENCY, SAMPLE_COUNT, 4.0),
    (CLIENT_ERROR, SUM, 0.0),
    (CLIENT_ERROR, SAMPLE_COUNT, 4.0),
    (EXECUTION_ERROR, SUM, 1.0),
    (EXECUTION_ERROR, SAMPLE_COUNT, 4.0),
    (INTEGRATION_ERROR, SUM, 0.0),
    (INTEGRATION_ERROR, SAMPLE_COUNT, 4.0),
];

/// What the same traffic owes EACH ROUTE's own series on the stage that publishes them, as
/// `(route, metric, statistic, value)` — and, read as a set of `(route, metric)` pairs, the
/// whole of which per-route series exist at all: the catalogue expectation below is derived
/// from this table, so a series this table does not name must not be catalogued either.
///
/// `ConnectCount` is "the number of messages sent to the `$connect` route integration", so
/// only that route has one. The close is the one call of `$disconnect`, and the two frames are
/// a message in and a frame out of their own routes.
const PER_ROUTE_OWED: &[(&str, &str, &str, f64)] = &[
    ("$connect", CONNECT_COUNT, SUM, 1.0),
    ("$connect", CONNECT_COUNT, SAMPLE_COUNT, 1.0),
    ("$connect", MESSAGE_COUNT, SUM, 1.0),
    ("$connect", MESSAGE_COUNT, SAMPLE_COUNT, 1.0),
    ("$connect", INTEGRATION_LATENCY, SAMPLE_COUNT, 1.0),
    ("$connect", CLIENT_ERROR, SUM, 0.0),
    ("$connect", CLIENT_ERROR, SAMPLE_COUNT, 1.0),
    ("$connect", EXECUTION_ERROR, SUM, 0.0),
    ("$connect", EXECUTION_ERROR, SAMPLE_COUNT, 1.0),
    ("$connect", INTEGRATION_ERROR, SUM, 0.0),
    ("$connect", INTEGRATION_ERROR, SAMPLE_COUNT, 1.0),
    ("reply", MESSAGE_COUNT, SUM, 2.0),
    ("reply", MESSAGE_COUNT, SAMPLE_COUNT, 2.0),
    ("reply", INTEGRATION_LATENCY, SAMPLE_COUNT, 1.0),
    ("reply", CLIENT_ERROR, SUM, 0.0),
    ("reply", CLIENT_ERROR, SAMPLE_COUNT, 1.0),
    ("reply", EXECUTION_ERROR, SUM, 0.0),
    ("reply", EXECUTION_ERROR, SAMPLE_COUNT, 1.0),
    ("reply", INTEGRATION_ERROR, SUM, 0.0),
    ("reply", INTEGRATION_ERROR, SAMPLE_COUNT, 1.0),
    ("boom", MESSAGE_COUNT, SUM, 2.0),
    ("boom", MESSAGE_COUNT, SAMPLE_COUNT, 2.0),
    // The function RAN and ended in an unhandled error, so it ANSWERED the invocation with
    // the error document Lambda reports for it: the call has a latency, and the class it
    // earned is the route's own execution.
    ("boom", INTEGRATION_LATENCY, SAMPLE_COUNT, 1.0),
    ("boom", CLIENT_ERROR, SUM, 0.0),
    ("boom", CLIENT_ERROR, SAMPLE_COUNT, 1.0),
    ("boom", EXECUTION_ERROR, SUM, 1.0),
    ("boom", EXECUTION_ERROR, SAMPLE_COUNT, 1.0),
    ("boom", INTEGRATION_ERROR, SUM, 0.0),
    ("boom", INTEGRATION_ERROR, SAMPLE_COUNT, 1.0),
    ("$disconnect", MESSAGE_COUNT, SUM, 1.0),
    ("$disconnect", MESSAGE_COUNT, SAMPLE_COUNT, 1.0),
    ("$disconnect", INTEGRATION_LATENCY, SAMPLE_COUNT, 1.0),
    ("$disconnect", CLIENT_ERROR, SUM, 0.0),
    ("$disconnect", CLIENT_ERROR, SAMPLE_COUNT, 1.0),
    ("$disconnect", EXECUTION_ERROR, SUM, 0.0),
    ("$disconnect", EXECUTION_ERROR, SAMPLE_COUNT, 1.0),
    ("$disconnect", INTEGRATION_ERROR, SUM, 0.0),
    ("$disconnect", INTEGRATION_ERROR, SAMPLE_COUNT, 1.0),
];

/// How long a case waits for the series to hold what the traffic owes them, and how often it
/// looks again: the last call of a connection (`$disconnect`) is served after the client has
/// closed, so what is waited for is a datapoint the traffic ALREADY owes rather than anything
/// a clock schedules. Generous, so a loaded machine does not fail a case.
const SERIES_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The window every read below spans, before and after the instant it is made at: wide enough
/// to hold the whole of a case's traffic whichever minute bucket each datapoint landed in.
const WINDOW: f64 = 600.0;

/// The granularity a read lays its windows at: one minute, the resolution a regular-resolution
/// datapoint is stored at.
const PERIOD: i64 = 60;

/// How whole a count has to be to be the same count: a half unit apart is a different answer,
/// and nothing here compares two measured durations.
const COUNT_TOLERANCE: f64 = 0.5;

/// A SigV4 `Authorization` whose credential scope names CloudWatch, which is what routes the
/// request. The signature is never checked here.
fn monitoring_authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/monitoring/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One CloudWatch call over its `awsJson1_0` wire, as its status and parsed body.
async fn monitoring(emulator: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let host = format!("monitoring.{REGION}.amazonaws.com");
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header("authorization", monitoring_authorization())
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Now, as the epoch seconds a CloudWatch read states its window in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs_f64()
}

/// The dimension set a series of `stage` is published under: the stage-wide one, and the
/// per-route one when `route` names a route.
fn dimensions(api_id: &str, stage: &str, route: Option<&str>) -> Vec<Value> {
    let mut stated = vec![
        json!({ "Name": API_ID_DIMENSION, "Value": api_id }),
        json!({ "Name": STAGE_DIMENSION, "Value": stage }),
    ];
    if let Some(route_key) = route {
        stated.push(json!({ "Name": ROUTE_DIMENSION, "Value": route_key }));
    }
    stated
}

/// What `GetMetricData` answers for ONE series over the window this read spans: the statistic
/// folded over every value in it, which is nothing at all for a series no call ever published.
async fn reading(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    want: &Reading,
) -> f64 {
    let now = now_secs();
    let (status, answered) = monitoring(
        emulator,
        "GetMetricData",
        json!({
            "MetricDataQueries": [{
                "Id": "series",
                "MetricStat": {
                    "Metric": {
                        "Namespace": NAMESPACE,
                        "MetricName": want.metric,
                        "Dimensions": dimensions(api_id, want.stage, want.route),
                    },
                    "Period": PERIOD,
                    "Stat": want.statistic,
                },
                "ReturnData": true,
            }],
            "StartTime": now - WINDOW,
            "EndTime": now + WINDOW,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricData for {want:?} must answer 200, got {status} {answered}"
    );
    answered
        .pointer("/MetricDataResults/0/Values")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_f64).sum())
        .unwrap_or(0.0)
}

/// ONE series read of a stage: which series, under which dimension set, by which statistic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reading {
    stage: &'static str,
    /// The `Route` dimension of a per-route series; `None` for the stage-wide one.
    route: Option<&'static str>,
    metric: &'static str,
    statistic: &'static str,
}

/// One expectation: a read, and the value the traffic owes it.
#[derive(Debug, Clone, Copy)]
struct Expectation {
    read: Reading,
    value: f64,
}

/// Wait until every expectation holds, failing with the case, each read and what was actually
/// answered when the deadline passes.
///
/// A counted datapoint is owed by a call that has already been made, so this looks again for
/// the condition rather than waiting a fixed time for it.
async fn reached(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    expected: &[Expectation],
) {
    let deadline = Instant::now() + SERIES_DEADLINE;
    loop {
        let mut unmet: Vec<String> = Vec::new();
        for want in expected {
            let got = reading(emulator, case, api_id, &want.read).await;
            if (got - want.value).abs() > COUNT_TOLERANCE {
                unmet.push(format!(
                    "{read:?} = {got} (expected {expected_value})",
                    read = want.read,
                    expected_value = want.value,
                ));
            }
        }
        if unmet.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: api {api_id:?} must publish what its traffic produced; these reads \
             do not hold: {unmet:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every series namespace `AWS/ApiGateway` holds for `api_id`, as `(stage, route, metric)` —
/// what a client that knows only the namespace and the api DISCOVERS, with every page the
/// catalogue answers in.
async fn catalogued(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
) -> BTreeSet<(String, Option<String>, String)> {
    let mut found: BTreeSet<(String, Option<String>, String)> = BTreeSet::new();
    let mut next: Option<String> = None;
    loop {
        let mut body = json!({ "Namespace": NAMESPACE });
        if let Some(token) = &next {
            body["NextToken"] = json!(token);
        }
        let (status, answered) = monitoring(emulator, "ListMetrics", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListMetrics must answer 200, got {status} {answered}"
        );
        let metrics = answered
            .get("Metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for metric in &metrics {
            let stated: BTreeMap<String, String> = metric
                .get("Dimensions")
                .and_then(Value::as_array)
                .map(|pairs| {
                    pairs
                        .iter()
                        .filter_map(|pair| {
                            Some((
                                pair.get("Name").and_then(Value::as_str)?.to_string(),
                                pair.get("Value").and_then(Value::as_str)?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if stated.get(API_ID_DIMENSION).map(String::as_str) != Some(api_id) {
                continue;
            }
            let name = metric
                .get("MetricName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let stage = stated.get(STAGE_DIMENSION).cloned().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a series of this api must name the stage it is of, got \
                     {metric}"
                )
            });
            let names: BTreeSet<&str> = stated.keys().map(String::as_str).collect();
            let route = stated.get(ROUTE_DIMENSION).cloned();
            let expected_names: BTreeSet<&str> = if route.is_some() {
                [API_ID_DIMENSION, STAGE_DIMENSION, ROUTE_DIMENSION].into()
            } else {
                [API_ID_DIMENSION, STAGE_DIMENSION].into()
            };
            assert_eq!(
                names, expected_names,
                "case {case:?}: a stage's series carries the stage dimensions and, for a \
                 per-route one, the route; got {metric}"
            );
            found.insert((stage, route, name));
        }
        next = answered
            .get("NextToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        if next.is_none() {
            return found;
        }
    }
}

/// Drive one connection's worth of traffic through `stage`: the connection request, a two-way
/// frame its route answers, a frame whose route's function raises and which the gateway answers
/// with its own frame, and the close.
///
/// The same traffic through every stage, so each stage's series are read against one account of
/// what happened.
async fn traffic_through(case: &str, authority: &str, stage: &str) {
    let mut socket: ClientSocket = handshake_at(case, authority, &format!("/{stage}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the handshake to {stage:?} must complete, got {status}")
        });
    let replied = exchanged_reply(case, &mut socket, r#"{"action":"reply","n":1}"#).await;
    assert!(
        replied.contains("MESSAGE"),
        "case {case:?}: stage {stage:?}'s two-way route must answer its own event, got \
         {replied:?}"
    );
    let failed = exchanged_reply(case, &mut socket, r#"{"action":"boom"}"#).await;
    assert!(
        failed.contains("Internal server error"),
        "case {case:?}: stage {stage:?} must answer a route that produced no reply with the \
         gateway's own frame, got {failed:?}"
    );
    closed(case, socket).await;
}

/// The series a client DISCOVERS for a WEBSOCKET api, what each holds, and the stage-wide
/// reading being the total of the per-route split.
#[tokio::test]
async fn a_websocket_stage_publishes_a_discoverable_series_per_counter_its_traffic_produced() {
    let case = "one WebSocket api, two stages carrying the same traffic, detailed on one";
    let (emulator, engine) = emulator();

    // One function per behaviour a route has, as a client provisions them.
    let lifecycle = given_function(&emulator, &engine, case, "cat-lifecycle", OK_HANDLER).await;
    let echo = given_function(&emulator, &engine, case, "cat-echo", ECHO_HANDLER).await;
    let raising = given_function(&emulator, &engine, case, "cat-raising", RAISING_HANDLER).await;

    let api = given_api(&emulator, case, "series-catalog-api").await;
    for (route_key, function, two_way) in [
        ("$connect", &lifecycle, false),
        ("$disconnect", &lifecycle, false),
        ("$default", &lifecycle, false),
        ("reply", &echo, true),
        ("boom", &raising, false),
    ] {
        given_route(&emulator, case, &api.api_id, route_key, function, two_way).await;
    }
    given_stage(&emulator, case, &api.api_id, DETAILED_STAGE, true).await;
    given_stage(&emulator, case, &api.api_id, PLAIN_STAGE, false).await;

    traffic_through(case, &api.authority, DETAILED_STAGE).await;
    traffic_through(case, &api.authority, PLAIN_STAGE).await;

    // What each stage's own series hold: the stage-wide table for both stages, the per-route
    // table for the stage that publishes one, and nothing at all for a per-route series
    // neither stage owes — the declared route no call reached, and every route of the stage
    // `DetailedMetricsEnabled` is off for.
    let mut expected: Vec<Expectation> = Vec::new();
    for stage in [DETAILED_STAGE, PLAIN_STAGE] {
        for (metric, statistic, value) in STAGE_OWED {
            expected.push(Expectation {
                read: Reading {
                    stage,
                    route: None,
                    metric,
                    statistic,
                },
                value: *value,
            });
        }
    }
    for (route, metric, statistic, value) in PER_ROUTE_OWED {
        expected.push(Expectation {
            read: Reading {
                stage: DETAILED_STAGE,
                route: Some(route),
                metric,
                statistic,
            },
            value: *value,
        });
    }
    let owes_per_route: BTreeSet<(&str, &str)> = PER_ROUTE_OWED
        .iter()
        .map(|(route, metric, _, _)| (*route, *metric))
        .collect();
    for stage in [DETAILED_STAGE, PLAIN_STAGE] {
        for route in ROUTES {
            for metric in SERIES {
                if stage == DETAILED_STAGE && owes_per_route.contains(&(route, metric)) {
                    continue;
                }
                expected.push(Expectation {
                    read: Reading {
                        stage,
                        route: Some(route),
                        metric,
                        statistic: SAMPLE_COUNT,
                    },
                    value: 0.0,
                });
            }
        }
    }
    reached(&emulator, case, &api.api_id, &expected).await;

    // The stage-wide reading of every series is the TOTAL of its per-route readings: two
    // readings of one account of the traffic, not two accounts of it.
    for (metric, statistic, _) in STAGE_OWED {
        let stage_wide = reading(
            &emulator,
            case,
            &api.api_id,
            &Reading {
                stage: DETAILED_STAGE,
                route: None,
                metric,
                statistic,
            },
        )
        .await;
        let mut split = 0.0;
        for route in ROUTES {
            split += reading(
                &emulator,
                case,
                &api.api_id,
                &Reading {
                    stage: DETAILED_STAGE,
                    route: Some(route),
                    metric,
                    statistic,
                },
            )
            .await;
        }
        assert!(
            (stage_wide - split).abs() <= COUNT_TOLERANCE,
            "case {case:?}: the stage-wide {statistic} of {metric:?} must be the total of its \
             per-route readings; got {stage_wide} stage-wide against {split} over the routes"
        );
    }

    // What a client that knows only the namespace and the api DISCOVERS: one stage-wide series
    // per counter for each stage, the per-route series of the detailed stage beside them, and
    // nothing else — no per-route series for the stage the setting is off for, and none for
    // the route no call reached.
    let mut owed_catalogue: BTreeSet<(String, Option<String>, String)> = BTreeSet::new();
    for stage in [DETAILED_STAGE, PLAIN_STAGE] {
        for metric in SERIES {
            owed_catalogue.insert((stage.to_string(), None, metric.to_string()));
        }
    }
    for (route, metric) in &owes_per_route {
        owed_catalogue.insert((
            DETAILED_STAGE.to_string(),
            Some((*route).to_string()),
            (*metric).to_string(),
        ));
    }
    let found = catalogued(&emulator, case, &api.api_id).await;
    assert_eq!(
        found, owed_catalogue,
        "case {case:?}: the catalogue must hold exactly the series this api's traffic \
         published; missing {missing:?}, unexpected {unexpected:?}",
        missing = owed_catalogue.difference(&found).collect::<Vec<_>>(),
        unexpected = found.difference(&owed_catalogue).collect::<Vec<_>>(),
    );
}
