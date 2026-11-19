// SPDX-License-Identifier: CC0-1.0

//! What an invocation PRINTS can publish a CloudWatch metric: an Embedded Metric Format line
//! a handler writes to its own output materialises the metrics the line declares, with no
//! `PutMetricData` call anywhere.
//!
//! RULE: a line an invocation writes is one log event of the function's log group — created by
//! the first invocation that writes to it — and a log event whose message is an Embedded Metric
//! Format document publishes, on ingest, the metrics its `_aws.CloudWatchMetrics` directives
//! declare: into the namespace each directive names, under each dimension set it declares
//! (every dimension's value read off the document member of that name), with the unit the
//! metric declares, and one observation per value the named document member carries. So a
//! metric read over that namespace, that metric name and EXACTLY that dimension set reports
//! one datapoint per period whose `Sum`, `SampleCount`, `Minimum` and `Maximum` are the
//! aggregate of the declared values and whose `Unit` is the declared one — while a read naming
//! another unit, a subset or a superset of the declared dimensions, or another metric or
//! namespace, reports none, each of those being a different series. The line itself stays
//! readable in the group, byte for byte.
//!
//! Three services have to agree for that to hold — Lambda runs the handler and delivers the
//! run's log, CloudWatch Logs ingests it and extracts the declared metrics, CloudWatch answers
//! for the series — and IAM holds the execution role those writes are made under, so every row
//! here mounts the four on ONE engine and asserts through their own APIs. The execution role of
//! every function below explicitly DENIES `cloudwatch:PutMetricData`, so a datapoint a read
//! finds can only have come from the extraction.
//!
//! What the neighbours pin and this suite does not repeat:
//! [`crate::invocation_log_delivery`] pins WHAT one invocation leaves in the group over a table
//! of handlers and roles, and [`crate::invocation_log_group_search`] pins that a client can
//! find it. What only this suite reaches is the step past the log: the metric a printed line
//! publishes.
//!
//! The rows differ along what the rule generalises over: how many values the named member
//! carries (one, an array of two, an array of three), whether the metric declares a unit, how
//! many dimension sets the directive declares (one, two) and how many names each holds, and how
//! many metrics one directive declares — and, in the identity table, along how a read's
//! namespace, metric name, dimension set and unit differ from the declared ones.
//!
//! Every test here RUNS a handler, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::{call, router_on};
use crate::invocation_log_delivery::{
    create_function, groups, logs, record_text, Created, LOGS, LOG_WRITES, PYTHON_RUNTIME,
};

/// Where `Invoke` is bound, under the functions collection.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix CloudWatch's own model declares
/// (`smithy_query(query="service_info")`: service shape `GraniteServiceVersion20100801`).
const CLOUDWATCH_TARGET: &str = "GraniteServiceVersion20100801";

/// The metric write the execution role of every function here explicitly denies, so nothing a
/// read below finds can have come from a call of it.
const PUT_METRIC_DATA: &str = "cloudwatch:PutMetricData";

/// The `Period` every read below is answered at, in the seconds the member carries: one
/// minute, the default resolution an extracted metric is stored at.
const PERIOD: i64 = 60;

/// The `StandardUnit` value a datapoint whose publisher declared no unit carries, which is what
/// a read of such a series reports
/// (`smithy_query(query="shape",shape="StandardUnit")`: the enumeration's `None` value).
const UNIT_NONE: &str = "None";

/// The handler every row runs: it prints the lines the event hands it, and nothing else, so
/// what reaches the log is exactly what the row declared.
///
/// `pub(crate)` for the reason [`emulator`] is: the sibling suite over what the group's metric
/// filters count prints its own rows' lines through this one handler, whose runtime writes each
/// line as the record it wrote rather than decorating it.
pub(crate) const PRINTING_HANDLER: &str = r"
def lambda_handler(event, context):
    lines = event.get('lines', [])
    for line in lines:
        print(line)
    return {'printed': len(lines)}
";

/// Lambda, CloudWatch Logs, IAM and CloudWatch over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the log the run's delivery writes is ingested by the
/// same store this suite reads the metric out of.
///
/// `pub(crate)` so the sibling suite over what the group's METRIC FILTERS count
/// ([`crate::invocation_log_metric_filter_counting`]) reads its series out of these same four
/// services on one engine rather than mounting a second copy of them.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
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

/// Epoch milliseconds, the unit an `InputLogEvent$timestamp` and an EMF `Timestamp` carry.
///
/// `pub(crate)` so the sibling suite that writes a log event with the SDK its runtime ships
/// ([`crate::handler_observability_sdk`]) stamps it from this one writer of the unit rather
/// than from a second copy.
pub(crate) fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}

/// The statements the execution role of every function here carries: the three log writes a
/// run's delivery makes, and an explicit denial of the metric write.
fn role_statements() -> Value {
    json!([
        allow(LOGS, &LOG_WRITES, "*"),
        json!({ "Effect": "Deny", "Action": [PUT_METRIC_DATA], "Resource": "*" }),
    ])
}

/// An Embedded Metric Format document: the `_aws` metadata the format defines, carrying one
/// `directive`, plus the document's own `members` the directive reads its values and its
/// dimension values from.
fn emf_document(timestamp_ms: i64, directive: &Value, members: &Value) -> String {
    let mut document = json!({
        "_aws": {
            "Timestamp": timestamp_ms,
            "CloudWatchMetrics": [directive],
        },
    });
    if let Some(members) = members.as_object() {
        for (member, value) in members {
            document[member] = value.clone();
        }
    }
    document.to_string()
}

/// One CloudWatch call (`awsJson1_0`, `POST /` with `X-Amz-Target`) that must answer 200.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/monitoring/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{CLOUDWATCH_TARGET}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
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
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// How a read of one series is addressed: the identity a `GetMetricStatistics` names.
struct Read<'a> {
    namespace: &'a str,
    metric: &'a str,
    /// The dimension set, as the `Dimensions` member carries it.
    dimensions: Value,
    /// The unit the read selects, or `None` to take every unit.
    unit: Option<&'a str>,
}

/// The datapoints CloudWatch reports for the series `read` names, over the one period the
/// instant `at_ms` falls in.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    read: &Read<'_>,
    at_ms: i64,
) -> Vec<Value> {
    let at_secs = at_ms as f64 / 1_000.0;
    let mut request = json!({
        "Namespace": read.namespace,
        "MetricName": read.metric,
        "Dimensions": read.dimensions,
        "StartTime": at_secs - 30.0,
        "EndTime": at_secs + 30.0,
        "Period": PERIOD,
        "Statistics": ["Sum", "SampleCount", "Minimum", "Maximum"],
    });
    if let Some(unit) = read.unit {
        request["Unit"] = json!(unit);
    }
    let answered = cloudwatch(router, case, "GetMetricStatistics", &request).await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// `Invoke` the function `name` synchronously with the lines it is to print, and answer the
/// payload the handler returned.
///
/// `pub(crate)` for the reason [`emulator`] is: the sibling suite over what the group's metric
/// filters count drives its own rows' runs through this one invocation path.
pub(crate) async fn invoke_printing(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    lines: &[String],
) -> Value {
    let answer = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({ "lines": lines }),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answer.status,
        answer.text()
    );
    let faulted = answer.header("x-amz-function-error");
    assert_eq!(
        faulted, None,
        "case {case:?}: expected the run to answer without a function error, got {faulted:?}"
    );
    answer.json()
}

/// The messages a group-wide search of `group` returns, which is where a client reads what a
/// run wrote.
///
/// `pub(crate)` for the reason [`emulator`] is.
pub(crate) async fn messages(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let answered = logs(
        router,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    answered["events"]
        .as_array()
        .map(|found| {
            found
                .iter()
                // Each event is one record of the run, closed by the break that ended the write
                // it was made in; what this suite reads is the line the handler printed.
                .map(|event| record_text(event["message"].as_str().unwrap_or_default()).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One series a row's document has to publish, and the aggregate a read of it must report.
struct Series {
    /// The metric name the series is read back under.
    metric: &'static str,
    /// The dimension set it is read back under, as a `Dimensions` member.
    dimensions: Value,
    /// The unit the datapoint must carry.
    unit: &'static str,
    /// The aggregate of the declared values: `(Sum, SampleCount, Minimum, Maximum)`.
    aggregate: (f64, f64, f64, f64),
}

/// One row: the document a handler prints, and the series it must publish.
struct Row {
    case: &'static str,
    /// The namespace the row's directive publishes into — one per row, so no row reads
    /// another's series.
    namespace: &'static str,
    /// The `_aws.CloudWatchMetrics` directive the document carries.
    directive: Value,
    /// The document's own members, which the directive reads its values and its dimension
    /// values from.
    members: Value,
    /// Every series the document declares.
    published: Vec<Series>,
}

/// The table the rule is asserted over: the forms a printed document's metrics take, and the
/// aggregate each publishes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one value under a two-name dimension set, with a declared unit",
            namespace: "Emulator/EmfPrintedOne",
            directive: json!({
                "Namespace": "Emulator/EmfPrintedOne",
                "Dimensions": [["Service", "Stage"]],
                "Metrics": [{"Name": "CheckoutLatency", "Unit": "Milliseconds"}],
            }),
            members: json!({
                "Service": "checkout",
                "Stage": "prod",
                "CheckoutLatency": 12,
            }),
            published: vec![Series {
                metric: "CheckoutLatency",
                dimensions: json!([
                    {"Name": "Service", "Value": "checkout"},
                    {"Name": "Stage", "Value": "prod"},
                ]),
                unit: "Milliseconds",
                aggregate: (12.0, 1.0, 12.0, 12.0),
            }],
        },
        Row {
            case: "an array of three values under the same dimension set",
            namespace: "Emulator/EmfPrintedMany",
            directive: json!({
                "Namespace": "Emulator/EmfPrintedMany",
                "Dimensions": [["Service", "Stage"]],
                "Metrics": [{"Name": "CheckoutLatency", "Unit": "Milliseconds"}],
            }),
            members: json!({
                "Service": "checkout",
                "Stage": "prod",
                "CheckoutLatency": [12, 30, 48],
            }),
            published: vec![Series {
                metric: "CheckoutLatency",
                dimensions: json!([
                    {"Name": "Service", "Value": "checkout"},
                    {"Name": "Stage", "Value": "prod"},
                ]),
                unit: "Milliseconds",
                aggregate: (90.0, 3.0, 12.0, 48.0),
            }],
        },
        Row {
            case: "a metric declaring no unit, under a one-name dimension set",
            namespace: "Emulator/EmfPrintedUnitless",
            directive: json!({
                "Namespace": "Emulator/EmfPrintedUnitless",
                "Dimensions": [["Service"]],
                "Metrics": [{"Name": "QueueDepth"}],
            }),
            members: json!({"Service": "orders", "QueueDepth": [4, 7]}),
            published: vec![Series {
                metric: "QueueDepth",
                dimensions: json!([{"Name": "Service", "Value": "orders"}]),
                unit: UNIT_NONE,
                aggregate: (11.0, 2.0, 4.0, 7.0),
            }],
        },
        Row {
            case: "two metrics under two dimension sets: one series each",
            namespace: "Emulator/EmfPrintedSets",
            directive: json!({
                "Namespace": "Emulator/EmfPrintedSets",
                "Dimensions": [["Service", "Stage"], ["Service"]],
                "Metrics": [
                    {"Name": "Faults", "Unit": "Count"},
                    {"Name": "Payload", "Unit": "Bytes"},
                ],
            }),
            members: json!({
                "Service": "checkout",
                "Stage": "prod",
                "Faults": 2,
                "Payload": [1024, 2048],
            }),
            published: vec![
                Series {
                    metric: "Faults",
                    dimensions: json!([
                        {"Name": "Service", "Value": "checkout"},
                        {"Name": "Stage", "Value": "prod"},
                    ]),
                    unit: "Count",
                    aggregate: (2.0, 1.0, 2.0, 2.0),
                },
                Series {
                    metric: "Faults",
                    dimensions: json!([{"Name": "Service", "Value": "checkout"}]),
                    unit: "Count",
                    aggregate: (2.0, 1.0, 2.0, 2.0),
                },
                Series {
                    metric: "Payload",
                    dimensions: json!([
                        {"Name": "Service", "Value": "checkout"},
                        {"Name": "Stage", "Value": "prod"},
                    ]),
                    unit: "Bytes",
                    aggregate: (3072.0, 2.0, 1024.0, 2048.0),
                },
                Series {
                    metric: "Payload",
                    dimensions: json!([{"Name": "Service", "Value": "checkout"}]),
                    unit: "Bytes",
                    aggregate: (3072.0, 2.0, 1024.0, 2048.0),
                },
            ],
        },
    ]
}

/// One statistic of a datapoint, named in the report of a mismatch.
fn statistic(point: &Value, name: &str) -> Option<f64> {
    point.get(name).and_then(Value::as_f64)
}

/// Create the role and the function of the row `index`, invoke it on `lines`, and answer the
/// group the run logged to.
async fn printed(
    router: &MultiServiceRouter,
    case: &str,
    index: usize,
    lines: &[String],
) -> String {
    let role = given_role(
        router,
        case,
        &format!("emf-printer-{index}"),
        Some(role_statements()),
    )
    .await;
    let name = format!("emf-printer-{index}");
    let group = format!("/aws/lambda/{name}");
    create_function(
        router,
        &Created {
            case,
            name: &name,
            source: PRINTING_HANDLER,
            runtime: PYTHON_RUNTIME,
            role: &role,
            log_group: None,
        },
    )
    .await;

    // The group is created by the first invocation, so a client that derives it from the
    // function's name finds nothing until the run has happened.
    let before = groups(router, case, &group).await;
    assert!(
        before.is_empty(),
        "case {case:?}: expected no log group before the first invocation, got {before:?}"
    );

    let payload = invoke_printing(router, case, &name, lines).await;
    assert_eq!(
        payload["printed"].as_u64(),
        Some(lines.len() as u64),
        "case {case:?}: expected the handler to report {} printed line(s), got {payload}",
        lines.len()
    );
    let found = groups(router, case, &group).await;
    assert_eq!(
        found,
        vec![group.clone()],
        "case {case:?}: expected the invocation to create {group:?}, got {found:?}"
    );
    group
}

/// Every form a printed document's metrics take publishes exactly the series it declares, and
/// the line itself stays in the function's own log group.
#[tokio::test]
async fn a_printed_emf_document_publishes_the_metrics_it_declares() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let at = now_ms() - 60_000;
        let document = emf_document(at, &row.directive, &row.members);
        let group = printed(&router, case, index, std::slice::from_ref(&document)).await;

        // The line a client reads back is the one the handler printed, byte for byte.
        let written = messages(&router, case, &group).await;
        assert!(
            written.contains(&document),
            "case {case:?}: expected the EMF line to be readable verbatim in {group:?}, got \
             {written:?}"
        );

        for series in &row.published {
            let read = Read {
                namespace: row.namespace,
                metric: series.metric,
                dimensions: series.dimensions.clone(),
                unit: None,
            };
            let points = datapoints(&router, case, &read, at).await;
            assert_eq!(
                points.len(),
                1,
                "case {case:?}: expected one datapoint for {} under {}, got {points:?}",
                series.metric,
                series.dimensions
            );
            let (sum, samples, minimum, maximum) = series.aggregate;
            for (name, expected) in [
                ("Sum", sum),
                ("SampleCount", samples),
                ("Minimum", minimum),
                ("Maximum", maximum),
            ] {
                assert_eq!(
                    statistic(&points[0], name),
                    Some(expected),
                    "case {case:?}: expected {} under {} to report {name} {expected}, got {:?}",
                    series.metric,
                    series.dimensions,
                    points[0]
                );
            }
            assert_eq!(
                points[0]["Unit"].as_str(),
                Some(series.unit),
                "case {case:?}: expected {} under {} to carry the declared unit {:?}, got {:?}",
                series.metric,
                series.dimensions,
                series.unit,
                points[0]
            );
        }
    }
}

/// One row of the identity table: how a read's own identity differs from the declared series,
/// and whether it finds the datapoint.
struct Asked {
    case: &'static str,
    namespace: &'static str,
    metric: &'static str,
    dimensions: Value,
    unit: Option<&'static str>,
    /// Whether the read must report the published datapoint.
    found: bool,
}

/// A read whose namespace, metric, dimension set or unit differs from the declared one is a
/// read of another series, so it reports nothing.
#[tokio::test]
async fn only_the_declared_identity_reads_the_published_datapoint() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    const NAMESPACE: &str = "Emulator/EmfPrintedIdentity";
    const METRIC: &str = "CheckoutLatency";
    const UNIT: &str = "Milliseconds";
    let case = "the declared series";
    let router = emulator();
    let at = now_ms() - 60_000;
    let document = emf_document(
        at,
        &json!({
            "Namespace": NAMESPACE,
            "Dimensions": [["Service", "Stage"]],
            "Metrics": [{"Name": METRIC, "Unit": UNIT}],
        }),
        &json!({"Service": "checkout", "Stage": "prod", "CheckoutLatency": [12, 30, 48]}),
    );
    printed(&router, case, 0, &[document]).await;

    let declared = json!([
        {"Name": "Service", "Value": "checkout"},
        {"Name": "Stage", "Value": "prod"},
    ]);
    let asked: Vec<Asked> = vec![
        Asked {
            case: "the declared identity, with the declared unit",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: declared.clone(),
            unit: Some(UNIT),
            found: true,
        },
        Asked {
            case: "the declared identity, naming no unit at all",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: declared.clone(),
            unit: None,
            found: true,
        },
        Asked {
            case: "another unit",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: declared.clone(),
            unit: Some("Seconds"),
            found: false,
        },
        Asked {
            case: "the unit value for no unit, where one was declared",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: declared.clone(),
            unit: Some(UNIT_NONE),
            found: false,
        },
        Asked {
            case: "a subset of the declared dimensions",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: json!([{"Name": "Service", "Value": "checkout"}]),
            unit: None,
            found: false,
        },
        Asked {
            case: "no dimensions at all",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: json!([]),
            unit: None,
            found: false,
        },
        Asked {
            case: "a superset of the declared dimensions",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: json!([
                {"Name": "Service", "Value": "checkout"},
                {"Name": "Stage", "Value": "prod"},
                {"Name": "Region", "Value": REGION},
            ]),
            unit: None,
            found: false,
        },
        Asked {
            case: "one declared dimension carrying another value",
            namespace: NAMESPACE,
            metric: METRIC,
            dimensions: json!([
                {"Name": "Service", "Value": "checkout"},
                {"Name": "Stage", "Value": "staging"},
            ]),
            unit: None,
            found: false,
        },
        Asked {
            case: "another metric name",
            namespace: NAMESPACE,
            metric: "CheckoutFaults",
            dimensions: declared.clone(),
            unit: None,
            found: false,
        },
        Asked {
            case: "another namespace",
            namespace: "Emulator/EmfPrintedElsewhere",
            metric: METRIC,
            dimensions: declared.clone(),
            unit: None,
            found: false,
        },
    ];

    for row in &asked {
        let case = row.case;
        let read = Read {
            namespace: row.namespace,
            metric: row.metric,
            dimensions: row.dimensions.clone(),
            unit: row.unit,
        };
        let points = datapoints(&router, case, &read, at).await;
        assert_eq!(
            !points.is_empty(),
            row.found,
            "case {case:?}: expected a datapoint: {}, got {points:?}",
            row.found
        );
        if row.found {
            assert_eq!(
                statistic(&points[0], "Sum"),
                Some(90.0),
                "case {case:?}: expected the aggregate of the declared values, got {:?}",
                points[0]
            );
        }
    }
}
