// SPDX-License-Identifier: CC0-1.0

//! Several Embedded Metric Format log events ingested into one log group aggregate, in
//! CloudWatch, into the metric series their documents declare.
//!
//! The rule under test: **the values an ingested Embedded Metric Format document declares are
//! metric OBSERVATIONS of the series its directive names, so the documents whose instants fall
//! in one period aggregate into exactly one window of that period per declared dimension set —
//! `SampleCount` the number of observations, `Sum` their total and `Minimum`/`Maximum` their
//! extremes, reported with the unit the metric declared — while the members the directive did
//! NOT declare as dimensions stay in Logs and name no series.** Two crates have to agree for
//! that to hold (CloudWatch Logs ingests, CloudWatch aggregates and catalogues), so the suite
//! lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: how many documents are ingested (one,
//! three, twelve), whether a document carries one value or an array of them, whether every
//! document carries the same value or distinct ones, how many dimension sets the directive
//! declares (the zero-dimension one, one, two) and which members supply them, and whether a
//! unit and a storage resolution are declared. Every document also carries two members no
//! directive declares, which is what makes the catalogue assertion an assertion about
//! undeclared high-cardinality properties. The recorded twelve-document minute an issue happens
//! to mention is a single row.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `StandardUnit` value CloudWatch reports for observations whose publisher declared no unit
/// of its own, as the model's `StandardUnit` enumeration spells it.
const UNIT_NONE: &str = "None";

/// The width, in seconds, of every window read here: the shortest period a regular-resolution
/// metric can be read at — "For metrics with regular resolution, a period can be as short as one
/// minute (60 seconds) and must be a multiple of 60" (`GetMetricStatisticsInput$Period`).
const PERIOD_SECS: i64 = 60;

/// The statistics every read here asks for, which is also what each row states.
const STATISTICS: [&str; 4] = ["SampleCount", "Sum", "Minimum", "Maximum"];

/// The members every document carries that NO directive declares as a dimension: a request and a
/// customer identifier, one distinct pair per document, standing for the high-cardinality
/// properties an Embedded Metric Format producer puts in its logs.
const UNDECLARED_MEMBERS: [&str; 2] = ["requestId", "customerId"];

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// The SigV4 header a request carries, naming the signing name the router routes on.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
    )
}

/// Call one operation of either service over its awsJson wire.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Call one CloudWatch Logs operation, requiring it to succeed.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, parsed) = call(
        router,
        LOGS_SIGNING_NAME,
        LOGS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        request,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from logs {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// Call one CloudWatch operation, requiring it to succeed.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, parsed) = call(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        request,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {parsed}"
    );
    parsed
}

/// Epoch milliseconds, the unit a CloudWatch Logs timestamp and an EMF `Timestamp` carry.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}

/// The start of the period `timestamp_ms` falls in, so a row's documents and the windows the
/// reads lay are anchored on the same instant whatever the clock says when the test runs.
fn period_start_ms(timestamp_ms: i64) -> i64 {
    timestamp_ms - timestamp_ms.rem_euclid(PERIOD_SECS * 1_000)
}

/// A log group and a stream inside it, both created.
async fn group_with_stream(router: &MultiServiceRouter, case: &str, group: &str, stream: &str) {
    logs(router, case, "CreateLogGroup", &json!({"logGroupName": group})).await;
    logs(
        router,
        case,
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
}

/// An Embedded Metric Format document: the `_aws` metadata the format defines, carrying
/// `directive`, plus the document's own members `members`.
fn emf_document(timestamp_ms: i64, directive: &Value, members: &[(&str, Value)]) -> String {
    let mut document = json!({
        "_aws": {
            "Timestamp": timestamp_ms,
            "CloudWatchMetrics": [directive],
        },
    });
    for (member, value) in members {
        document[*member] = value.clone();
    }
    document.to_string()
}

/// Ingest one batch of log events, each entry a `(timestamp, message)` pair.
async fn ingest(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
    events: &[(i64, String)],
) {
    let log_events: Vec<Value> = events
        .iter()
        .map(|(timestamp_ms, message)| json!({"timestamp": timestamp_ms, "message": message}))
        .collect();
    let response = logs(
        router,
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": log_events,
        }),
    )
    .await;
    assert!(
        response.get("rejectedLogEventsInfo").is_none(),
        "case {case:?}: the batch must be accepted whole, got {response}"
    );
}

/// The `Dimensions` list a CloudWatch read names, from `(name, value)` pairs.
fn read_dimensions(pairs: &[(&str, &str)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(name, value)| json!({"Name": name, "Value": value}))
            .collect(),
    )
}

/// The datapoints CloudWatch reports for one series over `periods` windows from `start_secs`.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric_name: &str,
    dimensions: &Value,
    start_secs: f64,
    periods: i64,
) -> Vec<Value> {
    let stats = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": metric_name,
            "Dimensions": dimensions,
            "StartTime": start_secs,
            "EndTime": start_secs + (periods * PERIOD_SECS) as f64,
            "Period": PERIOD_SECS,
            "Statistics": STATISTICS,
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// One numeric member of a datapoint: a statistic it reports, or the instant of its window.
fn number(point: &Value, name: &str) -> Option<f64> {
    point.get(name).and_then(Value::as_f64)
}

/// The catalogue CloudWatch holds for one namespace: every (metric name, dimension names) pair
/// `ListMetrics` reports, which is the set form the "and nothing else" half of the rule is
/// stated over.
async fn catalogue(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
) -> BTreeSet<(String, Vec<String>)> {
    let mut pairs: BTreeSet<(String, Vec<String>)> = BTreeSet::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut request = json!({"Namespace": namespace});
        if let Some(token) = &next_token {
            request["NextToken"] = json!(token);
        }
        let reply = cloudwatch(router, case, "ListMetrics", &request).await;
        for metric in reply["Metrics"].as_array().into_iter().flatten() {
            let name = metric["MetricName"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a catalogued metric is named: {metric}"))
                .to_string();
            let mut names: Vec<String> = metric["Dimensions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|dimension| {
                    dimension["Name"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: a catalogued dimension is named: {metric}")
                        })
                        .to_string()
                })
                .collect();
            names.sort();
            pairs.insert((name, names));
        }
        next_token = reply["NextToken"].as_str().map(ToString::to_string);
        if next_token.is_none() {
            return pairs;
        }
    }
}

/// The statistics one window must report.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Window {
    /// `SampleCount`: how many observations the window aggregated.
    sample_count: f64,
    /// `Sum`: their total.
    sum: f64,
    /// `Minimum`: the smallest of them.
    minimum: f64,
    /// `Maximum`: the largest of them.
    maximum: f64,
}

/// One row of the table: the directive to declare, the documents to ingest, and the window every
/// declared projection must report.
struct Row {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// The metric the directive declares, which is also the document member holding its values.
    metric: &'static str,
    /// The `Unit` the metric declares, when it declares one.
    unit: Option<&'static str>,
    /// The `StorageResolution` the metric declares, when it declares one.
    storage_resolution: Option<i64>,
    /// The dimension SETS the directive declares, each a list of document member names.
    dimension_sets: &'static [&'static [&'static str]],
    /// The string members every document carries, supplying the declared dimension values.
    members: &'static [(&'static str, &'static str)],
    /// One entry per document ingested: the value its metric member holds, a single number or an
    /// array of them.
    documents: Vec<Value>,
    /// The window every declared projection must report over the one period.
    expected: Window,
}

/// The `Dimensions` member of a directive, from the dimension sets a row declares.
fn declared_sets(sets: &[&[&str]]) -> Value {
    Value::Array(
        sets.iter()
            .map(|names| Value::Array(names.iter().map(|name| json!(name)).collect()))
            .collect(),
    )
}

/// The dimensions a declared set resolves to in a document carrying `members`: the value of the
/// member each declared name points at.
fn resolved_set<'row>(
    names: &'row [&'row str],
    members: &'row [(&'row str, &'row str)],
) -> Vec<(&'row str, &'row str)> {
    names
        .iter()
        .filter_map(|name| {
            members
                .iter()
                .find(|(member, _)| member == name)
                .map(|(member, value)| (*member, *value))
        })
        .collect()
}

/// Every form of document aggregates, inside one period, into one window per declared projection
/// — and the namespace catalogues exactly those projections.
#[tokio::test]
async fn the_documents_of_one_period_aggregate_into_one_window_per_declared_projection() {
    let router = build_router();
    // A period boundary a few periods back: every document of a row carries an instant inside
    // that one period, which is the window the reads below lay.
    let period_ms = period_start_ms(now_ms() - 4 * PERIOD_SECS * 1_000);
    let start_secs = period_ms as f64 / 1_000.0;

    let rows: Vec<Row> = vec![
        Row {
            case: "twelve documents counting one each under two dimension sets",
            metric: "OrderCount",
            unit: Some("Count"),
            storage_resolution: None,
            dimension_sets: &[&["Service", "Operation"], &["Service"]],
            members: &[("Service", "checkout"), ("Operation", "PlaceOrder")],
            documents: vec![json!(1); 12],
            expected: Window {
                sample_count: 12.0,
                sum: 12.0,
                minimum: 1.0,
                maximum: 1.0,
            },
        },
        Row {
            case: "twelve distinct high-resolution latencies under two dimension sets",
            metric: "OrderLatency",
            unit: Some("Milliseconds"),
            storage_resolution: Some(1),
            dimension_sets: &[&["Service", "Operation"], &["Service"]],
            members: &[("Service", "checkout"), ("Operation", "PlaceOrder")],
            documents: vec![
                json!(11.0),
                json!(53.0),
                json!(17.0),
                json!(29.0),
                json!(41.0),
                json!(23.0),
                json!(35.0),
                json!(19.0),
                json!(47.0),
                json!(31.0),
                json!(13.0),
                json!(45.0),
            ],
            expected: Window {
                sample_count: 12.0,
                // 11 + 53 + 17 + 29 + 41 + 23 + 35 + 19 + 47 + 31 + 13 + 45.
                sum: 364.0,
                minimum: 11.0,
                maximum: 53.0,
            },
        },
        Row {
            case: "one document only, declaring no unit",
            metric: "Faults",
            unit: None,
            storage_resolution: None,
            dimension_sets: &[&["Service"]],
            members: &[("Service", "search")],
            documents: vec![json!(7.5)],
            expected: Window {
                sample_count: 1.0,
                sum: 7.5,
                minimum: 7.5,
                maximum: 7.5,
            },
        },
        Row {
            case: "three documents carrying arrays of values",
            metric: "Bytes",
            unit: Some("Bytes"),
            storage_resolution: None,
            dimension_sets: &[&["Service"]],
            members: &[("Service", "api")],
            documents: vec![json!([10.0, 20.0]), json!([30.0]), json!([40.0, 50.5])],
            expected: Window {
                sample_count: 5.0,
                sum: 150.5,
                minimum: 10.0,
                maximum: 50.5,
            },
        },
        Row {
            case: "three documents under the zero-dimension projection",
            metric: "Ticks",
            unit: Some("Count"),
            storage_resolution: Some(1),
            dimension_sets: &[&[]],
            members: &[("Service", "billing")],
            documents: vec![json!(2), json!(3), json!(4)],
            expected: Window {
                sample_count: 3.0,
                sum: 9.0,
                minimum: 2.0,
                maximum: 4.0,
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let group = format!("/emf/period-aggregation-{index}");
        let namespace = format!("EmfPeriodAggregation{index}");
        group_with_stream(&router, case, &group, "emf").await;

        let mut declaration = json!({"Name": row.metric});
        if let Some(unit) = row.unit {
            declaration["Unit"] = json!(unit);
        }
        if let Some(resolution) = row.storage_resolution {
            declaration["StorageResolution"] = json!(resolution);
        }
        let directive = json!({
            "Namespace": &namespace,
            "Dimensions": declared_sets(row.dimension_sets),
            "Metrics": [declaration],
        });

        // One log event per document, each a second apart inside the one period, and each
        // carrying its own undeclared identifiers so no two documents share them.
        let events: Vec<(i64, String)> = row
            .documents
            .iter()
            .enumerate()
            .map(|(nth, value)| {
                let at = period_ms + nth as i64 * 1_000;
                let mut members: Vec<(&str, Value)> = row
                    .members
                    .iter()
                    .map(|(member, supplied)| (*member, json!(supplied)))
                    .collect();
                members.push((row.metric, value.clone()));
                for member in UNDECLARED_MEMBERS {
                    members.push((member, json!(format!("{member}-{index}-{nth}"))));
                }
                (at, emf_document(at, &directive, &members))
            })
            .collect();
        ingest(&router, case, &group, "emf", &events).await;

        for names in row.dimension_sets {
            let resolved = resolved_set(names, row.members);
            let dimensions = read_dimensions(&resolved);
            let points = datapoints(
                &router,
                case,
                &namespace,
                row.metric,
                &dimensions,
                start_secs,
                1,
            )
            .await;
            assert_eq!(
                points.len(),
                1,
                "case {case:?}: {resolved:?} must report exactly one period window, got {points:?}"
            );
            let point = &points[0];
            let actual = Window {
                sample_count: number(point, "SampleCount").unwrap_or_default(),
                sum: number(point, "Sum").unwrap_or_default(),
                minimum: number(point, "Minimum").unwrap_or_default(),
                maximum: number(point, "Maximum").unwrap_or_default(),
            };
            assert_eq!(
                actual, row.expected,
                "case {case:?}: {resolved:?} must report {:?}, got {actual:?} in {point}",
                row.expected
            );
            assert_eq!(
                point["Unit"].as_str(),
                Some(row.unit.unwrap_or(UNIT_NONE)),
                "case {case:?}: {resolved:?} must report the declared unit, got {point}"
            );
            assert_eq!(
                number(point, "Timestamp"),
                Some(start_secs),
                "case {case:?}: {resolved:?} must report the window the documents fall in, got \
                 {point}"
            );
        }

        // The namespace holds exactly the declared projections: the identifiers every document
        // carried are properties of the log event, not dimensions of a series.
        let expected: BTreeSet<(String, Vec<String>)> = row
            .dimension_sets
            .iter()
            .map(|names| {
                let mut resolved: Vec<String> = resolved_set(names, row.members)
                    .into_iter()
                    .map(|(name, _)| name.to_string())
                    .collect();
                resolved.sort();
                (row.metric.to_string(), resolved)
            })
            .collect();
        let actual = catalogue(&router, case, &namespace).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: the namespace must catalogue exactly {expected:?}, got {actual:?}"
        );
    }
}

/// The period a value falls in is the one the DOCUMENT's own `Timestamp` names, whatever instant
/// the log event carrying it was stamped at.
#[tokio::test]
async fn a_documents_own_timestamp_decides_the_window_its_value_falls_in() {
    let router = build_router();
    let case = "the document's own Timestamp decides the window";
    const GROUP: &str = "/emf/period-timestamps";
    const NAMESPACE: &str = "EmfPeriodTimestamps";
    const METRIC: &str = "Observations";
    group_with_stream(&router, case, GROUP, "emf").await;

    // Two periods back and the period after it, while every log event is stamped at one later
    // instant: the ingestion time cannot be what the windows are laid from.
    let ingested_at = now_ms() - 1_000;
    let first_period_ms = period_start_ms(now_ms() - 3 * PERIOD_SECS * 1_000);
    let second_period_ms = first_period_ms + PERIOD_SECS * 1_000;
    let directive = json!({
        "Namespace": NAMESPACE,
        "Dimensions": [["Service"]],
        "Metrics": [{"Name": METRIC, "Unit": "Count"}],
    });

    // (the instant the document declares, the value it declares)
    let declared: [(i64, f64); 3] = [
        (first_period_ms, 10.0),
        (first_period_ms + 30_000, 20.0),
        (second_period_ms, 45.0),
    ];
    let events: Vec<(i64, String)> = declared
        .iter()
        .map(|(declared_at, value)| {
            let members: Vec<(&str, Value)> =
                vec![("Service", json!("api")), (METRIC, json!(value))];
            (
                ingested_at,
                emf_document(*declared_at, &directive, &members),
            )
        })
        .collect();
    ingest(&router, case, GROUP, "emf", &events).await;

    let points = datapoints(
        &router,
        case,
        NAMESPACE,
        METRIC,
        &read_dimensions(&[("Service", "api")]),
        first_period_ms as f64 / 1_000.0,
        2,
    )
    .await;
    let mut actual: Vec<(f64, Window)> = points
        .iter()
        .map(|point| {
            (
                number(point, "Timestamp").unwrap_or_default(),
                Window {
                    sample_count: number(point, "SampleCount").unwrap_or_default(),
                    sum: number(point, "Sum").unwrap_or_default(),
                    minimum: number(point, "Minimum").unwrap_or_default(),
                    maximum: number(point, "Maximum").unwrap_or_default(),
                },
            )
        })
        .collect();
    actual.sort_by(|left, right| left.0.total_cmp(&right.0));
    let expected: Vec<(f64, Window)> = vec![
        (
            first_period_ms as f64 / 1_000.0,
            Window {
                sample_count: 2.0,
                sum: 30.0,
                minimum: 10.0,
                maximum: 20.0,
            },
        ),
        (
            second_period_ms as f64 / 1_000.0,
            Window {
                sample_count: 1.0,
                sum: 45.0,
                minimum: 45.0,
                maximum: 45.0,
            },
        ),
    ];
    assert_eq!(
        actual, expected,
        "case {case:?}: the two periods must report {expected:?}, got {actual:?} from {points:?}"
    );
}
