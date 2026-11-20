// SPDX-License-Identifier: CC0-1.0

//! The three instruments a client reads one batch of log events with agree, because all three
//! are views of the SAME stored events.
//!
//! The rule under test: **the log events a log group holds are read by the metric filters the
//! group holds at ingest time (publishing each matching event's `metricValue` into
//! CloudWatch's own metric store, dimensioned by what each `dimensions` selector resolves to
//! in that event), by a Logs Insights query `StartQuery` starts and `GetQueryResults` reports
//! once the query has reached `Complete`, and by `FilterLogEvents` — so for one predicate over
//! one batch the published series, the query's rows and the filtered events report the same
//! events and the same numbers, and a predicate NO event satisfies publishes no datapoint at
//! all while its query still completes with an empty result list.**
//!
//! Three crates have to agree for that to hold — CloudWatch Logs ingests and queries,
//! CloudWatch answers for the metric — so the suite lives here: a crate-local test can only
//! ever see one side of it.
//!
//! The rows differ along what the rule generalises over: the form of the predicate (an
//! unstructured term, a JSON field comparison, a `%…%` regular expression, a space-delimited
//! positional pattern, the empty pattern that matches everything), whether the published value
//! is a literal or extracted from the event, whether the transformation declares a dimension
//! (so the aggregation is grouped) or none (so it folds into one series), and how many of the
//! batch's events satisfy the predicate — including none. Any one pattern or query string an
//! issue happens to mention is a single row.
//!
//! Every wait here is a poll for the CONDITION the query reports, looked at again until it
//! holds or a generous deadline passes: nothing asserts how long a step took.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `QueryStatus` value a finished query reports, from `GetQueryResults$status`.
const COMPLETE: &str = "Complete";

/// How long a case keeps looking at a started query's status before giving up. Many times over
/// the span the service owes that transition: what decides a case is the status the query
/// reports, never this bound, which only makes a query that never finishes a failure.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How often that status is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The window one `GetMetricStatistics` read covers around the instant the batch was stamped,
/// and the period it folds into — one period, so a series' whole contribution lands in one
/// datapoint.
const WINDOW_SECONDS: f64 = 300.0;
const PERIOD_SECONDS: i64 = 600;

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

/// Epoch milliseconds, the unit a CloudWatch Logs event timestamp carries.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
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

/// Ingest `messages` as one batch, all stamped at `timestamp_ms`.
async fn ingest(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
    timestamp_ms: i64,
    messages: &[&str],
) {
    let events: Vec<Value> = messages
        .iter()
        .map(|message| json!({"timestamp": timestamp_ms, "message": message}))
        .collect();
    let answered = logs(
        router,
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": events,
        }),
    )
    .await;
    assert!(
        answered.get("rejectedLogEventsInfo").is_none(),
        "case {case:?}: the batch must be accepted whole, got {answered}"
    );
}

/// The `dimensions` map of a metric transformation, from `(name, selector)` pairs.
fn filter_dimensions(pairs: &[(&str, &str)]) -> Value {
    let mut map = Map::new();
    for (name, selector) in pairs {
        map.insert((*name).to_string(), json!(selector));
    }
    Value::Object(map)
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

/// The datapoints CloudWatch reports for one series over the window around `at_ms`.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric_name: &str,
    dimensions: &Value,
    at_ms: i64,
) -> Vec<Value> {
    #[allow(clippy::cast_precision_loss)]
    let at_secs = at_ms as f64 / 1_000.0;
    let statistics = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": metric_name,
            "Dimensions": dimensions,
            "StartTime": at_secs - WINDOW_SECONDS,
            "EndTime": at_secs + WINDOW_SECONDS,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    statistics["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// What one published series holds over the window: the sum of the values it was published
/// with and how many events published one — `None` for a series holding no datapoint at all.
async fn series(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric_name: &str,
    dimensions: &Value,
    at_ms: i64,
) -> Option<(f64, f64)> {
    let points = datapoints(router, case, namespace, metric_name, dimensions, at_ms).await;
    assert!(
        points.len() <= 1,
        "case {case:?}: one period window holds at most one datapoint, got {points:?}"
    );
    points.first().map(|point| {
        let sum = point["Sum"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a datapoint carries a numeric Sum, got {point}")
        });
        let samples = point["SampleCount"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a datapoint carries a numeric SampleCount, got {point}")
        });
        (sum, samples)
    })
}

/// The messages `FilterLogEvents` reports for `pattern` over the whole group, in the order it
/// reports them.
async fn filtered_messages(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    pattern: &str,
) -> Vec<String> {
    let mut request = json!({"logGroupName": group});
    // The empty pattern is the absent one: a read naming no pattern reports every event, which
    // is what an empty `filterPattern` selects.
    if !pattern.is_empty() {
        request["filterPattern"] = json!(pattern);
    }
    let page = logs(router, case, "FilterLogEvents", &request).await;
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: FilterLogEvents reports events, got {page}"))
        .iter()
        .map(|event| {
            event["message"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: an event carries a message, got {event}"))
                .to_string()
        })
        .collect()
}

/// One answer of `GetQueryResults`: the query it belongs to, the rows it carries as field/value
/// maps, and the `resultCount` its statistics report.
struct Results {
    query_id: String,
    rows: Vec<BTreeMap<String, String>>,
    result_count: f64,
}

/// The rows of a `GetQueryResults` answer, each as the field/value pairs it carries.
fn rows_of(case: &str, answer: &Value) -> Vec<BTreeMap<String, String>> {
    answer["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: results are a list, got {answer}"))
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a result row is a list, got {row}"))
                .iter()
                .map(|field| {
                    (
                        field["field"].as_str().unwrap_or_default().to_string(),
                        field["value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .collect()
}

/// Start `query_string` over `group`, look at its status again until it reports [`COMPLETE`] or
/// [`COMPLETION_DEADLINE`] passes, and answer what the completed query reports.
///
/// The window is stated in the epoch SECONDS `StartQuery` documents its range in, and covers
/// the whole batch and the instant the query is started at.
async fn completed_query(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    query_string: &str,
) -> Results {
    let started = logs(
        router,
        case,
        "StartQuery",
        &json!({
            "logGroupName": group,
            "startTime": now_ms() / 1_000 - 3_600,
            "endTime": now_ms() / 1_000 + 3_600,
            "queryString": query_string,
        }),
    )
    .await;
    let query_id = started["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartQuery answers a queryId, got {started}"))
        .to_string();

    let deadline = Instant::now() + COMPLETION_DEADLINE;
    loop {
        let answer = logs(router, case, "GetQueryResults", &json!({"queryId": query_id})).await;
        if answer["status"].as_str() == Some(COMPLETE) {
            return results_of(case, query_id, &answer);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the query never reported {COMPLETE}, last answer {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one completed answer reports.
fn results_of(case: &str, query_id: String, answer: &Value) -> Results {
    let result_count = answer["statistics"]["resultCount"]
        .as_f64()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the statistics carry a resultCount, got {answer}")
        });
    Results {
        query_id,
        rows: rows_of(case, answer),
        result_count,
    }
}

/// Read ONE query's results again, by the `queryId` it was started under.
async fn read_again(router: &MultiServiceRouter, case: &str, query_id: &str) -> Results {
    let answer = logs(
        router,
        case,
        "GetQueryResults",
        &json!({"queryId": query_id}),
    )
    .await;
    assert_eq!(
        answer["status"].as_str(),
        Some(COMPLETE),
        "case {case:?}: a completed query stays completed, got {answer}"
    );
    results_of(case, query_id.to_string(), &answer)
}

/// What all three readers of one group of events must agree on: the key the group is published
/// and grouped under (the empty string where the filter declares no dimension and the query
/// names no grouping field), the sum of the values its events publish, and how many events it
/// holds.
type Group = (&'static str, f64, usize);

/// One row of the table.
struct Row {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// The predicate, as the metric filter's `filterPattern` and as the pattern
    /// `FilterLogEvents` reads the same events with.
    pattern: &'static str,
    /// The transformation's `metricValue`: a literal, or a selector resolved in each event.
    metric_value: &'static str,
    /// The transformation's one dimension, as `(name, selector)`, where the row declares one.
    dimension: Option<(&'static str, &'static str)>,
    /// The Insights query over the same events. It reports `n` per group, the grouping field
    /// where the row is dimensioned, and `total` where the row's value is extracted.
    query: &'static str,
    /// The grouping field the query's rows carry, where the row is dimensioned.
    group_field: Option<&'static str>,
    /// Whether the query reports a `total` column — the sum the published series carries.
    reports_total: bool,
    /// The messages ingested, in one batch.
    messages: &'static [&'static str],
    /// What the three readers must agree on, one entry per group.
    groups: &'static [Group],
    /// The messages the predicate selects, which `FilterLogEvents` must report.
    matching: &'static [&'static str],
    /// A key no event of the batch resolves, whose series must hold no datapoint.
    absent_key: &'static str,
}

/// RULE: the published series, the Insights rows and the filtered events are three views of one
/// batch, so they report the same events and the same numbers.
#[tokio::test]
async fn the_published_series_the_query_and_the_filtered_events_agree_over_one_batch() {
    let router = build_router();
    // A whole second a minute back: a query's window is stated in whole SECONDS, so an event
    // stamped on a second boundary falls inside the window by the rule rather than by where
    // the test's own clock landed inside a second.
    let at = now_ms() / 1_000 * 1_000 - 60_000;

    let cases: Vec<Row> = vec![
        Row {
            case: "an unstructured term, counting the events that carry it",
            pattern: "\"ERROR\"",
            metric_value: "1",
            dimension: None,
            query: "filter @message like /ERROR/ | stats count(*) as n",
            group_field: None,
            reports_total: false,
            messages: &[
                "ERROR disk full",
                "INFO all good, nothing to see here",
                "ERROR timeout reached",
                "WARN approaching the limit",
            ],
            groups: &[("", 2.0, 2)],
            matching: &["ERROR disk full", "ERROR timeout reached"],
            absent_key: "nothing-resolves-this",
        },
        Row {
            case: "a JSON field comparison, counting the events that satisfy it",
            pattern: "{ $.level = \"ERROR\" }",
            metric_value: "1",
            dimension: None,
            query: "filter level = \"ERROR\" | stats count(*) as n",
            group_field: None,
            reports_total: false,
            messages: &[
                r#"{"level":"ERROR","ms":12}"#,
                r#"{"level":"ERROR","ms":30}"#,
                r#"{"level":"INFO","ms":999}"#,
                r#"{"level":"ERROR","ms":5}"#,
            ],
            groups: &[("", 3.0, 3)],
            matching: &[
                r#"{"level":"ERROR","ms":12}"#,
                r#"{"level":"ERROR","ms":30}"#,
                r#"{"level":"ERROR","ms":5}"#,
            ],
            absent_key: "nothing-resolves-this",
        },
        Row {
            case: "a regular-expression pattern against the named captures of a parse",
            pattern: "%^XFER [a-z]+ [0-9]+$%",
            metric_value: "1",
            dimension: None,
            query: r"parse @message /^XFER (?<user>[a-z]+) (?<nbytes>[0-9]+)$/ | filter ispresent(user) | stats count(*) as n",
            group_field: None,
            reports_total: false,
            messages: &["XFER amy 100", "XFER bob 250", "PING amy 1", "XFER amy 7"],
            groups: &[("", 3.0, 3)],
            matching: &["XFER amy 100", "XFER bob 250", "XFER amy 7"],
            absent_key: "nothing-resolves-this",
        },
        Row {
            case: "a positional pattern whose extracted value is summed per dimension",
            pattern: "[kind = XFER, user, nbytes]",
            metric_value: "$nbytes",
            dimension: Some(("User", "$user")),
            query: r"parse @message /^XFER (?<user>[a-z]+) (?<nbytes>[0-9]+)$/ | filter ispresent(user) | stats sum(nbytes) as total, count(*) as n by user | sort user asc",
            group_field: Some("user"),
            reports_total: true,
            messages: &["XFER amy 100", "XFER bob 250", "XFER amy 7", "PING amy 1"],
            groups: &[("amy", 107.0, 2), ("bob", 250.0, 1)],
            matching: &["XFER amy 100", "XFER bob 250", "XFER amy 7"],
            absent_key: "carol",
        },
        Row {
            case: "a JSON selector summed per JSON dimension",
            pattern: "{ $.level = \"ERROR\" }",
            metric_value: "$.ms",
            dimension: Some(("Service", "$.service")),
            query: "filter level = \"ERROR\" | stats sum(ms) as total, count(*) as n by service | sort service asc",
            group_field: Some("service"),
            reports_total: true,
            messages: &[
                r#"{"service":"api","level":"ERROR","ms":12}"#,
                r#"{"service":"api","level":"ERROR","ms":30}"#,
                r#"{"service":"web","level":"ERROR","ms":5}"#,
                r#"{"service":"api","level":"INFO","ms":999}"#,
            ],
            groups: &[("api", 42.0, 2), ("web", 5.0, 1)],
            matching: &[
                r#"{"service":"api","level":"ERROR","ms":12}"#,
                r#"{"service":"api","level":"ERROR","ms":30}"#,
                r#"{"service":"web","level":"ERROR","ms":5}"#,
            ],
            absent_key: "search",
        },
        Row {
            case: "the empty pattern, which every event of the batch satisfies",
            pattern: "",
            metric_value: "1",
            dimension: None,
            query: "stats count(*) as n",
            group_field: None,
            reports_total: false,
            messages: &["anything", "at all", "whatever shape it has"],
            groups: &[("", 3.0, 3)],
            matching: &["anything", "at all", "whatever shape it has"],
            absent_key: "nothing-resolves-this",
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let case = row.case;
        let group = format!("/insights/agreement-{index}");
        let namespace = format!("InsightsAgreement{index}");
        let metric = "Extracted";
        group_with_stream(&router, case, &group, "events").await;

        let mut transformation = json!({
            "metricName": metric,
            "metricNamespace": &namespace,
            "metricValue": row.metric_value,
        });
        if let Some(dimension) = row.dimension {
            transformation["dimensions"] = filter_dimensions(&[dimension]);
        }
        logs(
            &router,
            case,
            "PutMetricFilter",
            &json!({
                "logGroupName": &group,
                "filterName": "agree",
                "filterPattern": row.pattern,
                "metricTransformations": [transformation],
            }),
        )
        .await;

        ingest(&router, case, &group, "events", at, row.messages).await;

        // What the metric filter published, per group.
        let mut published: BTreeMap<String, (f64, f64)> = BTreeMap::new();
        for (key, _, _) in row.groups {
            let read = match row.dimension {
                Some((name, _)) => read_dimensions(&[(name, key)]),
                None => read_dimensions(&[]),
            };
            let held = series(&router, case, &namespace, metric, &read, at)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the series keyed {key:?} must hold the datapoint its \
                         matching events published, got none"
                    )
                });
            published.insert((*key).to_string(), held);
        }
        // A key no event of the batch resolves is a series of its own, and it holds nothing:
        // what the filter published is an extraction, not a write of every key.
        if let Some((name, _)) = row.dimension {
            let read = read_dimensions(&[(name, row.absent_key)]);
            assert_eq!(
                series(&router, case, &namespace, metric, &read, at).await,
                None,
                "case {case:?}: the series keyed {:?} must hold no datapoint",
                row.absent_key
            );
        }

        // What the query reports over the same events, keyed the same way.
        let answered = completed_query(&router, case, &group, row.query).await;
        #[allow(clippy::cast_precision_loss)]
        let expected_rows = row.groups.len() as f64;
        assert_eq!(
            answered.result_count, expected_rows,
            "case {case:?}: the statistics must count the {expected_rows} rows the query \
             produced, got {:?} in {:?}",
            answered.result_count, answered.rows
        );
        assert_eq!(
            answered.rows.len(),
            row.groups.len(),
            "case {case:?}: expected {} query rows, got {:?}",
            row.groups.len(),
            answered.rows
        );

        // What FilterLogEvents reports for the same predicate.
        let filtered = filtered_messages(&router, case, &group, row.pattern).await;
        let mut wanted: Vec<String> = row.matching.iter().map(|m| (*m).to_string()).collect();
        let mut reported = filtered.clone();
        wanted.sort();
        reported.sort();
        assert_eq!(
            reported, wanted,
            "case {case:?}: FilterLogEvents must report the events the pattern {:?} selects, got \
             {filtered:?}",
            row.pattern
        );

        // The three views, group by group.
        for (position, (key, sum, lines)) in row.groups.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let lines_f64 = *lines as f64;
            let (published_sum, published_samples) = published[&(*key).to_string()];
            assert_eq!(
                published_sum, *sum,
                "case {case:?}: the series keyed {key:?} must report Sum {sum}, got \
                 {published_sum}"
            );
            assert_eq!(
                published_samples, lines_f64,
                "case {case:?}: the series keyed {key:?} must report one sample per matching \
                 event ({lines}), got {published_samples}"
            );

            let answered_row = &answered.rows[position];
            if let Some(field) = row.group_field {
                assert_eq!(
                    answered_row.get(field).map(String::as_str),
                    Some(*key),
                    "case {case:?}: query row {position} must be keyed {key:?} under {field:?}, \
                     got {answered_row:?}"
                );
            }
            let counted: usize = answered_row
                .get("n")
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every query row carries n, got {answered_row:?}")
                });
            assert_eq!(
                counted, *lines,
                "case {case:?}: the query must count the {lines} events the series sampled for \
                 {key:?}, got {counted} in {answered_row:?}"
            );
            if row.reports_total {
                let total: f64 = answered_row
                    .get("total")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: every query row carries total, got {answered_row:?}")
                    });
                assert_eq!(
                    total, *sum,
                    "case {case:?}: the query's total for {key:?} must be the Sum the series \
                     published ({sum}), got {total}"
                );
            }
        }

        // The same number of events, read all three ways.
        let matched_events: usize = row.groups.iter().map(|(_, _, lines)| *lines).sum();
        assert_eq!(
            filtered.len(),
            matched_events,
            "case {case:?}: the predicate selects {matched_events} events, so FilterLogEvents \
             reports that many, got {filtered:?}"
        );
    }
}

/// RULE: a predicate no ingested event satisfies publishes NO datapoint, while a query over the
/// same predicate still reaches `Complete` with an empty result list — and the matching event
/// that follows publishes its datapoint and becomes that query's one row, so the absence was an
/// absence rather than extraction that never runs.
#[tokio::test]
async fn an_unmatched_predicate_publishes_nothing_and_still_completes_an_empty_query() {
    let router = build_router();
    let at = now_ms() / 1_000 * 1_000 - 60_000;
    const GROUP: &str = "/insights/negative-delivery";
    const NAMESPACE: &str = "InsightsNegativeDelivery";
    const METRIC: &str = "ErrorLineCount";
    const QUERY: &str = "fields @message | filter @message like /ERROR/ | sort @timestamp asc";
    const NON_MATCHING: &str = "INFO all good, nothing to see here";
    const MATCHING: &str = "ERROR something failed";

    let case = "before any matching event";
    group_with_stream(&router, case, GROUP, "events").await;
    logs(
        &router,
        case,
        "PutMetricFilter",
        &json!({
            "logGroupName": GROUP,
            "filterName": "count-errors",
            "filterPattern": "\"ERROR\"",
            "metricTransformations": [{
                "metricName": METRIC,
                "metricNamespace": NAMESPACE,
                "metricValue": "1",
            }],
        }),
    )
    .await;

    let no_dimensions = read_dimensions(&[]);
    ingest(&router, case, GROUP, "events", at, &[NON_MATCHING]).await;
    assert_eq!(
        series(&router, case, NAMESPACE, METRIC, &no_dimensions, at).await,
        None,
        "case {case:?}: an event the pattern does not match publishes no datapoint"
    );
    let empty = completed_query(&router, case, GROUP, QUERY).await;
    assert!(
        empty.rows.is_empty(),
        "case {case:?}: a query nothing matched completes with an empty result list, got {:?}",
        empty.rows
    );
    assert_eq!(
        empty.result_count, 0.0,
        "case {case:?}: the statistics of an empty result set count no rows, got {}",
        empty.result_count
    );

    let case = "the matching event that follows";
    ingest(&router, case, GROUP, "events", at, &[MATCHING]).await;
    assert_eq!(
        series(&router, case, NAMESPACE, METRIC, &no_dimensions, at).await,
        Some((1.0, 1.0)),
        "case {case:?}: one matching event publishes one datapoint of the transformation's value"
    );
    let answered = completed_query(&router, case, GROUP, QUERY).await;
    assert_eq!(
        answered.rows.len(),
        1,
        "case {case:?}: the query reports the one matching event, got {:?}",
        answered.rows
    );
    assert_eq!(
        answered.rows[0].get("@message").map(String::as_str),
        Some(MATCHING),
        "case {case:?}: the row carries the matching event's message, got {:?}",
        answered.rows[0]
    );

    // Reading the results changes nothing a later caller sees: the SAME query, read again by
    // the id it was started under, reports the same rows and the same count.
    let case = "the same query read again";
    let again = read_again(&router, case, &answered.query_id).await;
    assert_eq!(
        again.rows, answered.rows,
        "case {case:?}: the results of one query are stable, got {:?} against {:?}",
        again.rows, answered.rows
    );
    assert_eq!(
        again.result_count, answered.result_count,
        "case {case:?}: the statistics of one query are stable, got {} against {}",
        again.result_count, answered.result_count
    );
}
