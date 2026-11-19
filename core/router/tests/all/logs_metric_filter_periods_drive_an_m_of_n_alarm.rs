// SPDX-License-Identifier: CC0-1.0

//! A log-metric-filter alerting pipeline, over the WHOLE mounted service set — the one
//! `core/router/src/lib.rs` assembles for the binary — from what each PERIOD of ingestion
//! publishes to the notification an m-of-n alarm lands on a subscribed queue.
//!
//! The RULE under test: **a metric filter publishes into the CloudWatch metric its
//! transformation names only for the periods its log group ingested events into — a period
//! whose ingested events the `filterPattern` matched reports each match's `metricValue`, a
//! period whose ingested events it matched none of reports the transformation's `defaultValue`
//! where it declares one and nothing where it does not, and a period the group ingested nothing
//! into reports no datapoint at all — so a metric alarm over that metric reaches `ALARM` as soon
//! as `DatapointsToAlarm` of the datapoints it graded breach, even where that is fewer than its
//! `EvaluationPeriods`, explains itself with a `StateReason` opening "Threshold Crossed", and
//! publishes the notification to every topic its `AlarmActions` names, which SNS fans out to the
//! queue subscribed to that topic — while an otherwise identical alarm needing MORE breaching
//! datapoints than the ingestion produced stays out of `ALARM` and its own queue stays empty.**
//!
//! Four services have to agree for that to hold — CloudWatch Logs ingests and extracts,
//! CloudWatch holds the series and evaluates the alarm, SNS fans the notification out, SQS holds
//! what arrived — and each hop CloudWatch Logs and CloudWatch make is an in-process call
//! addressed the way a client's own scoped call is addressed, so only the production service set
//! can show the whole chain. What the neighbouring suites pin is the chain at ONE evaluation
//! period and with every datapoint breaching
//! (`core/router/tests/all/logs_metric_filter_drives_the_alarm_notification_chain.rs`), the
//! extraction forms over a single period
//! (`core/router/tests/all/logs_metric_filter_extraction.rs`) and the delivery of a transition a
//! client FORCES (`core/router/tests/all/cloudwatch_alarm_action_reaches_its_subscribed_queue.rs`).
//! What only this suite reaches is which PERIODS an ingestion fills — the difference between a
//! period that reports the default value and one that reports nothing at all — and the m-of-n
//! predicate decided over those periods, with the notification that follows it.
//!
//! The rows differ along what the rule generalises over: the form of the filter pattern (a JSON
//! pattern over a member, an unstructured term, a space-delimited field list), the form of the
//! metric value (a literal number, a space-delimited field, a JSON selector), whether and with
//! which sign a `defaultValue` is declared, which of the periods carry ingestion and whether its
//! events match, the `Statistic` the alarm aggregates a period with, the direction of its
//! `ComparisonOperator`, the `DatapointsToAlarm`/`EvaluationPeriods` pair, and the Region the
//! whole wiring lives in. Every expected aggregate is computed from the row's own messages.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, cw, drain, given_topic_with_subscriber, router as emulator, Grant,
};

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// How wide one period window is, in seconds: the granularity a metric filter's series is
/// published and read at here, and the `Period` every alarm below grades with.
const PERIOD_SECS: i64 = 60;

/// How many period windows the first table lays out, and how far back the first of them starts:
/// far enough that every window a row fills has CLOSED, and recent enough to be inside the
/// ingestion window `PutLogEvents` admits.
const PERIODS: i64 = 4;
const FIRST_PERIOD_AGE: i64 = PERIODS + 1;

/// How long a row keeps looking for something the clock owes before calling it undelivered.
///
/// Generous on purpose, and never measured: what a row asserts is WHAT arrived, never how long it
/// took. The transition is owed inside the framework's window for one the clock makes due and the
/// delivery follows it, so this deadline is only the point at which a delivery that is not coming
/// becomes a failure rather than a hang.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How many further rounds the control alarm's queue is read AFTER the subject alarm's
/// notification has arrived: enough to span several of the alarm clock's own wake-ups, each round
/// waiting on the queue's own long poll rather than on a span of this host's clock.
const QUIET_ROUNDS: usize = 3;

/// How long a poll loop that has nothing of its own to wait on pauses between rounds.
///
/// It PACES the polling — a loop watching the alarms has no long poll of its own to wait on, and
/// without this one it would hammer the router — and decides nothing: what bounds the wait is
/// [`DELIVERY_DEADLINE`], and what ends it is the condition.
const POLL: Duration = Duration::from_millis(50);

/// The three `StateValue`s a metric alarm reports: the one it reports once its series breaches,
/// the one it reports while the series it graded did not, and the one it reports while nothing
/// could be graded at all. Source: the model's `StateValue` enum.
const STATE_ALARM: &str = "ALARM";
const STATE_OK: &str = "OK";
const STATE_INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// What a metric alarm's `StateReason` opens with when the evaluation decided the state by
/// comparing the series against the threshold. Source: the sentence CloudWatch answers a graded
/// evaluation with.
const CROSSED: &str = "Threshold Crossed";

/// One Amazon CloudWatch Logs call over its `awsJson1_1` wire, which must answer 200.
///
/// The `Authorization` is built by the suite that owns this router's wire harness, so a request
/// here is addressed exactly the way its CloudWatch, SNS and SQS calls are.
async fn logs(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{LOGS_SIGNING_NAME}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(LOGS_SIGNING_NAME, region))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// Wall-clock now as whole epoch seconds.
fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_secs(),
    )
    .expect("the epoch seconds fit in an i64")
}

/// The start of the first period window a row fills, in epoch seconds, on the minute boundary the
/// reads below lay their windows on.
fn first_period_start() -> i64 {
    (now_secs() / PERIOD_SECS - FIRST_PERIOD_AGE) * PERIOD_SECS
}

/// A log group with one stream inside it, both created.
async fn given_group_with_stream(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    group: &str,
    stream: &str,
) {
    logs(
        router,
        case,
        region,
        "CreateLogGroup",
        &json!({"logGroupName": group}),
    )
    .await;
    logs(
        router,
        case,
        region,
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
}

/// Store one metric filter on `group`, whose single transformation publishes into
/// `namespace`/`metric` — with a `defaultValue` only where one is stated.
#[allow(clippy::too_many_arguments)]
async fn given_metric_filter(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    group: &str,
    filter: &str,
    pattern: &str,
    namespace: &str,
    metric: &str,
    metric_value: &str,
    default_value: Option<f64>,
) {
    let mut transformation = json!({
        "metricName": metric,
        "metricNamespace": namespace,
        "metricValue": metric_value,
    });
    if let Some(default_value) = default_value {
        transformation["defaultValue"] = json!(default_value);
    }
    logs(
        router,
        case,
        region,
        "PutMetricFilter",
        &json!({
            "logGroupName": group,
            "filterName": filter,
            "filterPattern": pattern,
            "metricTransformations": [transformation],
        }),
    )
    .await;
}

/// Ingest `messages` as one batch of log events inside the period window starting at
/// `period_start`, each a second apart so the batch is in chronological order.
async fn ingest_into_period(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    group: &str,
    stream: &str,
    period_start: i64,
    messages: &[&str],
) {
    if messages.is_empty() {
        return;
    }
    let events: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(offset, message)| {
            let within = i64::try_from(offset).expect("the batch length fits in an i64") + 1;
            assert!(
                within < PERIOD_SECS,
                "case {case:?}: a batch must fit inside one period window"
            );
            json!({
                "timestamp": (period_start + within) * 1_000,
                "message": message,
            })
        })
        .collect();
    let accepted = logs(
        router,
        case,
        region,
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": events,
        }),
    )
    .await;
    assert!(
        accepted.get("rejectedLogEventsInfo").is_none(),
        "case {case:?}: the batch must be accepted whole, got {accepted}"
    );
}

/// The datapoints CloudWatch reports for `namespace`/`metric`, one per `PERIOD_SECS` window from
/// `start` for `windows` windows, keyed by the index of the window each fell in.
#[allow(clippy::cast_precision_loss)]
async fn datapoints_by_period(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    namespace: &str,
    metric: &str,
    start: i64,
    windows: i64,
) -> Vec<(i64, Value)> {
    let answered = cw(
        router,
        case,
        region,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": metric,
            "StartTime": start as f64,
            "EndTime": (start + windows * PERIOD_SECS) as f64,
            "Period": PERIOD_SECS,
            "Statistics": ["SampleCount", "Average", "Sum", "Minimum", "Maximum"],
        }),
    )
    .await;
    let points = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut by_period: Vec<(i64, Value)> = points
        .into_iter()
        .map(|point| {
            let at = point["Timestamp"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: a datapoint carries a numeric Timestamp, got {point}")
            });
            let index = ((at - start as f64) / PERIOD_SECS as f64).round() as i64;
            (index, point)
        })
        .collect();
    by_period.sort_by_key(|(index, _)| *index);
    by_period
}

/// A statistic off one reported datapoint.
fn statistic(case: &str, point: &Value, name: &str) -> f64 {
    point[name].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: a datapoint carries a numeric {name}, got {point}")
    })
}

/// Two aggregates are the same number, to the precision a sum of a few small values keeps.
fn same(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

/// What one period window must report.
#[derive(Debug)]
enum Expected {
    /// No datapoint at all: the log group ingested nothing into this window, so the metric has no
    /// value for it — the hole an alarm's `TreatMissingData` stands in for.
    Missing,
    /// Every event the window took in MATCHED, so it reports each match's `metricValue`: their
    /// total as `Sum` and their bounds as `Minimum` and `Maximum`.
    Matched(&'static [f64]),
    /// No event the window took in matched, so it reports the transformation's `defaultValue`.
    ///
    /// Asserted through `Minimum`, `Maximum` and `Average`, each of which is that value however
    /// the service spreads it over the window — the rule is about WHICH windows carry the default
    /// value, not about how many times it is written into one.
    Defaulted(f64),
}

/// One row of the first table: the filter to store, what each period window takes in, and what
/// each must then report.
struct PeriodRow {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The Region the whole wiring lives in.
    region: &'static str,
    /// The filter's `filterPattern`.
    pattern: &'static str,
    /// The transformation's `metricValue`.
    metric_value: &'static str,
    /// The transformation's `defaultValue`, where the row declares one.
    default_value: Option<f64>,
    /// The messages ingested into each period window, oldest window first. A window given no
    /// message takes in nothing at all.
    ingested: &'static [&'static [&'static str]],
    /// What each of those windows must report, in the same order.
    expected: &'static [Expected],
}

const PERIOD_ROWS: &[PeriodRow] = &[
    PeriodRow {
        case: "a JSON pattern with a zero default value",
        region: "us-east-1",
        pattern: r#"{ $.level = "ERROR" }"#,
        metric_value: "1",
        default_value: Some(0.0),
        ingested: &[
            &[r#"{"level":"INFO"}"#, r#"{"level":"DEBUG"}"#],
            &[],
            &[
                r#"{"level":"ERROR"}"#,
                r#"{"level":"ERROR"}"#,
                r#"{"level":"ERROR"}"#,
            ],
            &[r#"{"level":"WARN"}"#],
        ],
        expected: &[
            Expected::Defaulted(0.0),
            Expected::Missing,
            Expected::Matched(&[1.0, 1.0, 1.0]),
            Expected::Defaulted(0.0),
        ],
    },
    PeriodRow {
        case: "an unstructured term with a positive default value, in another Region",
        region: "us-west-2",
        pattern: "ERROR",
        metric_value: "1",
        default_value: Some(2.5),
        ingested: &[
            &["INFO all healthy"],
            &[],
            &["ERROR disk full", "ERROR connection timeout"],
            &[],
        ],
        expected: &[
            Expected::Defaulted(2.5),
            Expected::Missing,
            Expected::Matched(&[1.0, 1.0]),
            Expected::Missing,
        ],
    },
    PeriodRow {
        case: "no default value at all: a window that matched nothing reports nothing",
        region: "eu-west-1",
        pattern: r#"{ $.level = "ERROR" }"#,
        metric_value: "1",
        default_value: None,
        ingested: &[
            &[r#"{"level":"INFO"}"#, r#"{"level":"INFO"}"#],
            &[],
            &[r#"{"level":"ERROR"}"#],
            &[r#"{"level":"INFO"}"#],
        ],
        expected: &[
            Expected::Missing,
            Expected::Missing,
            Expected::Matched(&[1.0]),
            Expected::Missing,
        ],
    },
    PeriodRow {
        case: "a space-delimited field as the metric value, with a negative default value",
        region: "us-east-1",
        pattern: "[ip, status, size]",
        metric_value: "$size",
        default_value: Some(-1.5),
        ingested: &[
            &["unparseable"],
            &["10.0.0.1 500 2048", "10.0.0.2 500 512"],
            &[],
            &["also unparseable"],
        ],
        expected: &[
            Expected::Defaulted(-1.5),
            Expected::Matched(&[2048.0, 512.0]),
            Expected::Missing,
            Expected::Defaulted(-1.5),
        ],
    },
];

/// A metric filter fills the periods its log group ingested into and only those: the default
/// value where a period's events matched nothing, the matches where they did, and no datapoint
/// at all for a period that took in nothing.
#[tokio::test]
async fn a_metric_filter_fills_only_the_periods_its_group_ingested_into() {
    for (index, row) in PERIOD_ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        // One emulator per row, so what a row reads is that row's alone whichever Region it signs
        // for.
        let router = emulator();

        let group = format!("/metric-filter-periods/{index}");
        let namespace = format!("Custom/MetricFilterPeriods{index}");
        let metric = "Extracted";
        assert_eq!(
            row.ingested.len(),
            row.expected.len(),
            "case {case:?}: the row must state one expectation per period window it fills"
        );

        given_group_with_stream(&router, case, region, &group, "events").await;
        given_metric_filter(
            &router,
            case,
            region,
            &group,
            "extract",
            row.pattern,
            &namespace,
            metric,
            row.metric_value,
            row.default_value,
        )
        .await;

        let start = first_period_start();
        for (window, messages) in row.ingested.iter().enumerate() {
            let offset = i64::try_from(window).expect("the window index fits in an i64");
            ingest_into_period(
                &router,
                case,
                region,
                &group,
                "events",
                start + offset * PERIOD_SECS,
                messages,
            )
            .await;
        }

        let windows = i64::try_from(row.expected.len()).expect("the window count fits in an i64");
        let reported =
            datapoints_by_period(&router, case, region, &namespace, metric, start, windows).await;
        let filled: Vec<i64> = reported.iter().map(|(index, _)| *index).collect();
        let wanted: Vec<i64> = row
            .expected
            .iter()
            .enumerate()
            .filter(|(_, expected)| !matches!(expected, Expected::Missing))
            .map(|(window, _)| i64::try_from(window).expect("the window index fits in an i64"))
            .collect();
        assert_eq!(
            filled, wanted,
            "case {case:?}: expected the metric to hold a datapoint in exactly the period \
             windows {wanted:?} — the ones the group ingested events into — got {filled:?} from \
             {reported:?}"
        );

        for (window, expected) in row.expected.iter().enumerate() {
            let offset = i64::try_from(window).expect("the window index fits in an i64");
            let Some((_, point)) = reported.iter().find(|(index, _)| *index == offset) else {
                continue;
            };
            match expected {
                Expected::Missing => unreachable!("a missing window reports no datapoint"),
                Expected::Matched(values) => {
                    let total: f64 = values.iter().sum();
                    let smallest = values.iter().copied().fold(f64::INFINITY, f64::min);
                    let largest = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    for (name, want) in [
                        ("Sum", total),
                        ("Minimum", smallest),
                        ("Maximum", largest),
                        ("SampleCount", values.len() as f64),
                    ] {
                        let got = statistic(case, point, name);
                        assert!(
                            same(got, want),
                            "case {case:?}: period window {window} took in {} matching events, so \
                             its {name} must be {want}, got {got} from {point}",
                            values.len()
                        );
                    }
                }
                Expected::Defaulted(default_value) => {
                    for name in ["Minimum", "Maximum", "Average"] {
                        let got = statistic(case, point, name);
                        assert!(
                            same(got, *default_value),
                            "case {case:?}: period window {window} matched nothing, so its {name} \
                             must be the transformation's default value {default_value}, got \
                             {got} from {point}"
                        );
                    }
                }
            }
        }
    }
}

/// One row of the second table: the filter the alarms watch, what the two most recent closed
/// period windows take in, and how the two alarms grade them.
struct AlarmRow {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The Region the whole wiring lives in.
    region: &'static str,
    /// The filter's `filterPattern` and `metricValue`.
    pattern: &'static str,
    metric_value: &'static str,
    /// The messages ingested into each of the two closed period windows, oldest first — every one
    /// of them matching, so each window publishes a datapoint that breaches.
    ingested: &'static [&'static [&'static str]],
    /// The `Statistic` both alarms aggregate a window with, the direction they grade it in and
    /// the `Threshold` it has to cross.
    statistic: &'static str,
    comparison: &'static str,
    threshold: f64,
    /// The aggregate each ingested window reports, oldest first: what the row's own messages
    /// publish under `statistic`, which every alarm below grades.
    window_values: &'static [f64],
    /// `EvaluationPeriods` both alarms are configured with, the `DatapointsToAlarm` the SUBJECT
    /// alarm needs — fewer than the windows that breach — and the one the CONTROL alarm needs,
    /// which is more than the ingestion produced.
    evaluation_periods: i64,
    subject_datapoints: i64,
    control_datapoints: i64,
}

const ALARM_ROWS: &[AlarmRow] = &[
    AlarmRow {
        case: "two breaching windows reach a 2-of-3 alarm, graded by Sum above its threshold",
        region: "us-east-1",
        pattern: r#"{ $.level = "ERROR" }"#,
        metric_value: "1",
        ingested: &[
            &[
                r#"{"level":"ERROR","request":1}"#,
                r#"{"level":"ERROR","request":2}"#,
                r#"{"level":"ERROR","request":3}"#,
            ],
            &[
                r#"{"level":"ERROR","request":4}"#,
                r#"{"level":"ERROR","request":5}"#,
                r#"{"level":"ERROR","request":6}"#,
            ],
        ],
        statistic: "Sum",
        comparison: "GreaterThanThreshold",
        threshold: 2.0,
        window_values: &[3.0, 3.0],
        evaluation_periods: 3,
        subject_datapoints: 2,
        control_datapoints: 3,
    },
    AlarmRow {
        case: "two breaching windows reach a 2-of-4 alarm, graded by Maximum at its threshold, \
               in another Region",
        region: "eu-west-1",
        pattern: "{ $.ms > 100 }",
        metric_value: "$.ms",
        ingested: &[
            &[r#"{"ms":700}"#, r#"{"ms":300}"#],
            &[r#"{"ms":900}"#],
        ],
        statistic: "Maximum",
        comparison: "GreaterThanOrEqualToThreshold",
        threshold: 500.0,
        window_values: &[700.0, 900.0],
        evaluation_periods: 4,
        subject_datapoints: 2,
        control_datapoints: 4,
    },
];

/// Whether `value` crosses `threshold` in the direction `comparison` names.
///
/// Source: the model's `ComparisonOperator` enum; the two arms here are the ones the rows state.
fn breaches(case: &str, comparison: &str, value: f64, threshold: f64) -> bool {
    match comparison {
        "GreaterThanThreshold" => value > threshold,
        "GreaterThanOrEqualToThreshold" => value >= threshold,
        other => panic!("case {case:?}: no row states the comparison {other:?}"),
    }
}

/// Create one alarm over `namespace`/`metric`, with `topic` as its one `AlarmActions` entry.
///
/// `TreatMissingData` is left to the documented default — "If `TreatMissingData` is omitted, the
/// default behavior of `missing` is used" — because the rule is about an alarm deciding on the
/// datapoints the ingested periods really published, with the periods nothing was ingested into
/// standing in for nothing.
#[allow(clippy::too_many_arguments)]
async fn given_alarm(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    namespace: &str,
    metric: &str,
    row: &AlarmRow,
    datapoints_to_alarm: i64,
    topic: &str,
) {
    cw(
        router,
        case,
        region,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": namespace,
            "MetricName": metric,
            "Statistic": row.statistic,
            "Period": PERIOD_SECS,
            "EvaluationPeriods": row.evaluation_periods,
            "DatapointsToAlarm": datapoints_to_alarm,
            "Threshold": row.threshold,
            "ComparisonOperator": row.comparison,
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// What `DescribeAlarms` reports as `name`'s `StateValue` and `StateReason`.
async fn alarm_state(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
) -> (String, String) {
    let described = cw(
        router,
        case,
        region,
        "DescribeAlarms",
        &json!({"AlarmNames": [name]}),
    )
    .await;
    let alarm = &described["MetricAlarms"][0];
    (
        alarm["StateValue"].as_str().unwrap_or_default().to_string(),
        alarm["StateReason"].as_str().unwrap_or_default().to_string(),
    )
}

/// Read the queue at `url` until `wanted` notifications have arrived on it, or the deadline
/// passes, and answer every notification that did arrive.
///
/// The wait is on the CONDITION: each round is one long-polling receive, which answers the instant
/// the queue has something and otherwise costs that poll's own wait — so a slow host delays a row
/// instead of deciding it, and nothing here sleeps for a span of its own. Every round also reads
/// the alarms, which materialises whatever their clock has made due.
async fn delivered_until(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    alarm: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, region, url).await);
        if arrived.len() >= wanted || Instant::now() >= deadline {
            return arrived;
        }
        let _ = alarm_state(router, case, region, alarm).await;
    }
}

/// An alarm whose `DatapointsToAlarm` is below its `EvaluationPeriods` fires on that many
/// breaching windows of a metric-filter series and notifies its subscribed queue, while the same
/// alarm needing more breaching windows than the ingestion produced stays out of ALARM.
#[tokio::test]
async fn an_m_of_n_alarm_over_a_metric_filter_series_notifies_its_subscribed_queue() {
    for (index, row) in ALARM_ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let router = emulator();

        let group = format!("/metric-filter-alarm/{index}");
        let namespace = format!("Custom/MetricFilterAlarm{index}");
        let metric = "Breaches";
        let subject = format!("metric-filter-m-of-n-{index}");
        let control = format!("metric-filter-n-of-n-{index}");
        assert_eq!(
            row.ingested.len(),
            row.window_values.len(),
            "case {case:?}: the row must state one aggregate per window it ingests into"
        );
        for value in row.window_values {
            assert!(
                breaches(case, row.comparison, *value, row.threshold),
                "case {case:?}: every ingested window must breach: {value} {} {}",
                row.comparison,
                row.threshold
            );
        }
        let breaching = i64::try_from(row.window_values.len()).expect("the count fits in an i64");
        assert!(
            row.subject_datapoints <= breaching && breaching < row.control_datapoints,
            "case {case:?}: the row must breach enough windows for the subject alarm and too few \
             for the control"
        );

        // 1. The log group and the filter whose matches drive both alarms, and the metric filter's
        //    `defaultValue` so a window that ingested non-matching events reports it — which is
        //    what makes the windows nothing was ingested into the only holes in the series.
        given_group_with_stream(&router, case, region, &group, "events").await;
        given_metric_filter(
            &router,
            case,
            region,
            &group,
            "extract",
            row.pattern,
            &namespace,
            metric,
            row.metric_value,
            Some(0.0),
        )
        .await;

        // 2. A topic per alarm with a queue of this emulator subscribed to it. The control's queue
        //    grants SNS the send UNSCOPED, so its silence cannot be a policy decision.
        let (subject_topic, subject_url) = given_topic_with_subscriber(
            &router,
            case,
            region,
            &format!("metric-filter-m-of-n-{index}"),
            Grant::ScopedToTopic,
        )
        .await;
        let (control_topic, control_url) = given_topic_with_subscriber(
            &router,
            case,
            region,
            &format!("metric-filter-n-of-n-{index}"),
            Grant::Unscoped,
        )
        .await;

        // 3. The two alarms, identical but for how many breaching datapoints each needs.
        given_alarm(
            &router,
            case,
            region,
            &subject,
            &namespace,
            metric,
            row,
            row.subject_datapoints,
            &subject_topic,
        )
        .await;
        given_alarm(
            &router,
            case,
            region,
            &control,
            &namespace,
            metric,
            row,
            row.control_datapoints,
            &control_topic,
        )
        .await;
        let (before, _) = alarm_state(&router, case, region, &subject).await;
        assert_ne!(
            before, STATE_ALARM,
            "case {case:?}: the alarm must not be in {STATE_ALARM} before anything is ingested, \
             got {before:?}"
        );

        // 4. The ingest: one batch per closed period window, each window's events all matching, so
        //    each publishes a datapoint that breaches.
        let windows = i64::try_from(row.ingested.len()).expect("the window count fits in an i64");
        let start = (now_secs() / PERIOD_SECS - windows) * PERIOD_SECS;
        for (window, messages) in row.ingested.iter().enumerate() {
            let offset = i64::try_from(window).expect("the window index fits in an i64");
            ingest_into_period(
                &router,
                case,
                region,
                &group,
                "events",
                start + offset * PERIOD_SECS,
                messages,
            )
            .await;
        }

        // 5. What the filter made of those events: one datapoint per ingested window, each
        //    carrying the aggregate the row's own messages publish.
        let reported =
            datapoints_by_period(&router, case, region, &namespace, metric, start, windows).await;
        assert_eq!(
            reported.len(),
            row.window_values.len(),
            "case {case:?}: each ingested window must publish its own datapoint, got {reported:?}"
        );
        for ((window, point), want) in reported.iter().zip(row.window_values) {
            let got = statistic(case, point, row.statistic);
            assert!(
                same(got, *want),
                "case {case:?}: period window {window} must report {} {want}, got {got} from \
                 {point}",
                row.statistic
            );
        }

        // 6. The chain: the evaluation the clock owes moves the subject alarm on the breaching
        //    windows, its action publishes to the topic, and the subscribed queue receives the
        //    notification — with no call forcing any of it.
        let delivered =
            delivered_until(&router, case, region, &subject, &subject_url, 1).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the m-of-n alarm's action must reach the subscribed queue exactly \
             once, got {delivered:?}"
        );
        let notification = &delivered[0];
        assert_eq!(
            notification["AlarmName"].as_str(),
            Some(subject.as_str()),
            "case {case:?}: the notification must name the alarm, got {notification}"
        );
        assert_eq!(
            notification["NewStateValue"].as_str(),
            Some(STATE_ALARM),
            "case {case:?}: the notification must carry the state moved to, got {notification}"
        );
        let reason = notification["NewStateReason"].as_str().unwrap_or_default();
        assert!(
            reason.starts_with(CROSSED),
            "case {case:?}: the notification's reason must open with {CROSSED:?}, got \
             {reason:?}"
        );
        assert_eq!(
            notification["Trigger"]["MetricName"].as_str(),
            Some(metric),
            "case {case:?}: the Trigger must report the filter's metric, got {notification}"
        );
        assert_eq!(
            notification["Trigger"]["Namespace"].as_str(),
            Some(namespace.as_str()),
            "case {case:?}: the Trigger must report the filter's namespace, got {notification}"
        );
        assert_eq!(
            notification["Trigger"]["DatapointsToAlarm"].as_i64(),
            Some(row.subject_datapoints),
            "case {case:?}: the Trigger must report the alarm's DatapointsToAlarm, got \
             {notification}"
        );
        assert_eq!(
            notification["Trigger"]["EvaluationPeriods"].as_i64(),
            Some(row.evaluation_periods),
            "case {case:?}: the Trigger must report the alarm's EvaluationPeriods, got \
             {notification}"
        );
        let (state, state_reason) = alarm_state(&router, case, region, &subject).await;
        assert_eq!(
            state, STATE_ALARM,
            "case {case:?}: the alarm the breaching windows crossed must report {STATE_ALARM}, \
             got {state:?} ({state_reason:?})"
        );
        assert!(
            state_reason.starts_with(CROSSED),
            "case {case:?}: the alarm's StateReason must open with {CROSSED:?}, got \
             {state_reason:?}"
        );

        // 7. And the control, which needs more breaching datapoints than the ingestion produced:
        //    it never reached ALARM and its own queue stayed empty, across several further
        //    wake-ups of the alarm clock. Each round reads the queue first, so nothing that
        //    arrived at any point is missed, and the `DescribeAlarms` it then makes materialises
        //    whatever the clock has made due.
        let mut quiet: Vec<Value> = Vec::new();
        for _ in 0..QUIET_ROUNDS {
            quiet.extend(drain(&router, case, region, &control_url).await);
            let (control_state, control_reason) =
                alarm_state(&router, case, region, &control).await;
            assert_ne!(
                control_state, STATE_ALARM,
                "case {case:?}: an alarm needing {} breaching datapoints may not reach \
                 {STATE_ALARM} on {breaching} of them, got {control_reason:?}",
                row.control_datapoints
            );
        }
        assert!(
            quiet.is_empty(),
            "case {case:?}: an alarm that never fired may deliver nothing, got {quiet:?}"
        );
    }
}

/// One row of the third table: a transformation's `defaultValue`, and the state an alarm over the
/// series it publishes into then reports for a period the log group ingested into and the filter
/// matched NONE of.
struct DefaultRow {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The name the filter is stored under, and the metric its transformation publishes into — one
    /// series per row, all on the same log group, so every row sees the same ingestion.
    filter: &'static str,
    metric: &'static str,
    /// The transformation's `defaultValue`, where the row declares one.
    default_value: Option<f64>,
    /// The `StateValue` an alarm over that series must report: `OK` for a default the comparison
    /// does not cross, `ALARM` for one it does, and `INSUFFICIENT_DATA` where the transformation
    /// declares no default at all, nothing having been published for the alarm to grade.
    expected: &'static str,
}

const DEFAULT_ROWS: &[DefaultRow] = &[
    DefaultRow {
        case: "a default value the comparison does not cross",
        filter: "with-zero-default",
        metric: "ZeroDefault",
        default_value: Some(0.0),
        expected: STATE_OK,
    },
    DefaultRow {
        case: "no default value at all",
        filter: "without-default",
        metric: "NoDefault",
        default_value: None,
        expected: STATE_INSUFFICIENT_DATA,
    },
    DefaultRow {
        case: "a default value the comparison crosses",
        filter: "with-breaching-default",
        metric: "BreachingDefault",
        default_value: Some(5.0),
        expected: STATE_ALARM,
    },
];

/// An alarm tells a period that reported a transformation's default value apart from a period the
/// series has no value for: the default is a datapoint, graded against the threshold like any
/// other, while a transformation declaring none leaves the period with nothing to grade — so the
/// alarm over it stays in insufficient data however often it is evaluated.
#[tokio::test]
async fn an_alarm_tells_a_default_value_apart_from_a_series_with_no_value() {
    let case = "a period the group ingested into whose events the pattern matched none of";
    let region = "us-east-1";
    let router = emulator();
    let group = "/metric-filter-default/alarms";
    let namespace = "Custom/MetricFilterDefault";
    // The threshold both directions are measured from: a default of zero does not cross it and a
    // positive one does, so the same ingestion decides the three rows differently.
    let threshold = 0.0;

    given_group_with_stream(&router, case, region, group, "events").await;
    for row in DEFAULT_ROWS {
        given_metric_filter(
            &router,
            case,
            region,
            group,
            row.filter,
            r#"{ $.level = "ERROR" }"#,
            namespace,
            row.metric,
            "1",
            row.default_value,
        )
        .await;
        cw(
            &router,
            case,
            region,
            "PutMetricAlarm",
            &json!({
                "AlarmName": row.filter,
                "Namespace": namespace,
                "MetricName": row.metric,
                "Statistic": "Maximum",
                "Period": PERIOD_SECS,
                "EvaluationPeriods": 1,
                "Threshold": threshold,
                "ComparisonOperator": "GreaterThanThreshold",
                "TreatMissingData": "missing",
            }),
        )
        .await;
    }

    // One batch into the period window that has already closed, carrying only events the pattern
    // matches NONE of: the only thing any of these series can report for that window is a default
    // value, where its transformation declares one.
    let start = (now_secs() / PERIOD_SECS - 1) * PERIOD_SECS;
    ingest_into_period(
        &router,
        case,
        region,
        group,
        "events",
        start,
        &[
            r#"{"level":"INFO"}"#,
            r#"{"level":"WARN"}"#,
            r#"{"level":"DEBUG"}"#,
        ],
    )
    .await;

    // What each series holds for that window: the default value where one is declared, and nothing
    // at all where none is.
    for row in DEFAULT_ROWS {
        let reported =
            datapoints_by_period(&router, case, region, namespace, row.metric, start, 1).await;
        match row.default_value {
            None => assert!(
                reported.is_empty(),
                "case {case:?} {:?}: a transformation declaring no default value publishes \
                 nothing for it, got {reported:?}",
                row.case
            ),
            Some(default_value) => {
                assert_eq!(
                    reported.len(),
                    1,
                    "case {case:?} {:?}: the window must report the default value, got \
                     {reported:?}",
                    row.case
                );
                for name in ["Minimum", "Maximum", "Average"] {
                    let got = statistic(case, &reported[0].1, name);
                    assert!(
                        same(got, default_value),
                        "case {case:?} {:?}: the window's {name} must be the default value \
                         {default_value}, got {got}",
                        row.case
                    );
                }
            }
        }
    }

    // The alarms the clock owes an evaluation to: the watch waits for the two rows whose expected
    // state is one no alarm STARTS in, which is what establishes that the evaluations have run —
    // every round's `DescribeAlarms` materialises whatever the clock has made due, and the three
    // alarms were created together, so by then the third's evaluation is long due as well.
    // [`DELIVERY_DEADLINE`] only decides when a settle that never comes is a failure rather than a
    // hang.
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let moved: Vec<&DefaultRow> = DEFAULT_ROWS
        .iter()
        .filter(|row| row.expected != STATE_INSUFFICIENT_DATA)
        .collect();
    loop {
        let mut settled = true;
        for row in &moved {
            let (state, _) = alarm_state(&router, case, region, row.filter).await;
            settled &= state == row.expected;
        }
        if settled || Instant::now() >= deadline {
            break;
        }
        ::tokio::time::sleep(POLL).await;
    }
    // Read twice: the first pass materialises each alarm's own due evaluation, the second grades
    // the state that evaluation left behind, so a row that moves on being evaluated is caught
    // rather than read before it was.
    for pass in 0..2 {
        for row in DEFAULT_ROWS {
            let (state, reason) = alarm_state(&router, case, region, row.filter).await;
            assert_eq!(
                state, row.expected,
                "case {case:?} {:?} (pass {pass}): expected the alarm to report {} for a default \
                 value of {:?}, got {state:?} ({reason:?})",
                row.case, row.expected, row.default_value
            );
        }
    }
}
