// SPDX-License-Identifier: CC0-1.0

//! A METRIC FILTER over a log group another service VENDS its own logs into publishes the
//! metric that filter declares, and CloudWatch reports it.
//!
//! RULE under test: **a record a service delivers into a CloudWatch Logs log group on the
//! account's behalf is an INGESTION into that group, so it is run against every metric filter
//! the group holds exactly as a client-put event is — each `metricTransformations` entry of a
//! filter whose `filterPattern` matches the delivered record publishes its `metricValue` into
//! `metricNamespace`/`metricName` with the declared `unit`, under one dimension per
//! `dimensions` entry whose value is what that entry's selector resolves to IN THAT RECORD —
//! and CloudWatch answers for the resulting series, so the `Sum` over a window holding every
//! delivered record is the total the matching records published and the `SampleCount` is how
//! many of them matched.**
//!
//! Three crates have to agree for that to hold — Step Functions vends a run's history into
//! the group, CloudWatch Logs ingests it and extracts through the group's filters, CloudWatch
//! answers for the metric — so the suite lives here: a crate-local test can only ever see one
//! side of it. The topology is the one an observability pipeline has: a
//! `/aws/vendedlogs/states/` group is the destination of a state machine's
//! `loggingConfiguration`, and the filter over it is what an operator alarms on.
//!
//! The rows differ along what the rule generalises over: the workflow type whose runs fill the
//! group (`EXPRESS`, whose executions this service's own reporting APIs do not serve, and
//! `STANDARD`), the form of the filter pattern (a JSON condition on a member of the vended
//! record, an unstructured term the record has to carry), which event type the pattern selects
//! (the runs' failures, the runs' successes), the form of the metric value (a literal 1, a
//! non-integer literal, a selector naming a numeric member of the record), whether the
//! transformation declares a unit, whether it declares a dimension and from which selector,
//! and how many of the runs match. The recorded "count the EXPRESS failures" filter an issue
//! happens to mention is a single row.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and so the Region every ARN here names.
const REGION: &str = "eu-west-2";

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// AWS Step Functions' `X-Amz-Target` prefix and signing name.
const STATES_TARGET_PREFIX: &str = "AWSStepFunctions";
const STATES_SIGNING_NAME: &str = "states";

/// The `StandardUnit` value CloudWatch reports for a datapoint whose publisher declared no
/// unit of its own. Source: `MetricTransformation$unit` — "The unit to assign to the metric.
/// If you omit this, the unit is set as None" — spelled as the model's `StandardUnit` enum
/// spells that value.
const UNIT_NONE: &str = "None";

/// The role the state machines run as. No IAM is mounted here, so this account holds no record
/// for it — which is no evidence about it, and the create is served.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/observability";

/// A workflow that fails at once, so each of its runs vends one failure.
const FAILING_DEFINITION: &str = r#"{"StartAt":"Boom","States":{"Boom":{"Type":"Fail","Error":"Boom","Cause":"this run fails on purpose"}}}"#;

/// A workflow that succeeds at once, so each of its runs vends one success.
const SUCCEEDING_DEFINITION: &str =
    r#"{"StartAt":"Done","States":{"Done":{"Type":"Succeed"}}}"#;

/// The two terminal history event types a run vends, which is what tells a settled group from
/// one still being written into.
const EXECUTION_FAILED: &str = "ExecutionFailed";
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";

/// How long a row waits for the vended deliveries and the metric they publish, and how often
/// it looks. The outcome is owed to the clock, so the wait is for the CONDITION; the budget is
/// far longer than the span either service chooses, so a loaded host is slower without the
/// rule being any different.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The window a row reads the metric over, and the period it reads it in: wide enough that
/// every record of the row falls inside it however long the runs took, and read as several
/// periods whose datapoints the row aggregates, so a run straddling a period boundary is the
/// same reading.
const WINDOW_SECONDS: f64 = 600.0;
const PERIOD_SECONDS: i64 = 300;

/// CloudWatch Logs, CloudWatch and AWS Step Functions on ONE engine, the shape
/// `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; the credential
        // scope still travels, because it is what names the Region and the service.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        )
    }

    /// Dispatch one awsJson request as it arrives on the wire, answering `(status, body)`.
    async fn call(
        &self,
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
            .header("authorization", Self::authorization(signing_name))
            .body(Bytes::from(
                serde_json::to_vec(request).expect("the request serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(http_request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One CloudWatch Logs call, answering `(status, body)`.
    async fn logs(&self, operation: &str, request: &Value) -> (u16, Value) {
        self.call(
            LOGS_SIGNING_NAME,
            LOGS_TARGET_PREFIX,
            "application/x-amz-json-1.1",
            operation,
            request,
        )
        .await
    }

    /// One CloudWatch Logs call that must succeed.
    async fn logs_ok(&self, case: &str, operation: &str, request: &Value) -> Value {
        let (status, parsed) = self.logs(operation, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from logs {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// One CloudWatch call that must succeed.
    async fn cloudwatch_ok(&self, case: &str, operation: &str, request: &Value) -> Value {
        let (status, parsed) = self
            .call(
                CLOUDWATCH_SIGNING_NAME,
                CLOUDWATCH_TARGET_PREFIX,
                "application/x-amz-json-1.0",
                operation,
                request,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with \
             {parsed}"
        );
        parsed
    }

    /// One Step Functions call that must succeed.
    async fn states_ok(&self, case: &str, operation: &str, request: &Value) -> Value {
        let (status, parsed) = self
            .call(
                STATES_SIGNING_NAME,
                STATES_TARGET_PREFIX,
                "application/x-amz-json-1.0",
                operation,
                request,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from states {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// The `type` of every record the log group `group` holds, as a client reading the group
    /// sees them. A group nothing has written into yet is reported as holding nothing.
    ///
    /// The read itself must SUCCEED: the group was created before any run was started, so a
    /// refusal is a defect of the read rather than a group still being written into, and
    /// reporting it here is what keeps it from being read as "nothing delivered yet" until a
    /// settle deadline passes.
    async fn vended_event_types(&self, case: &str, group: &str) -> Vec<String> {
        let page = self
            .logs_ok(case, "FilterLogEvents", &json!({"logGroupName": group}))
            .await;
        page["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .filter_map(|message| serde_json::from_str::<Value>(&message).ok())
            .filter_map(|message| message["type"].as_str().map(str::to_string))
            .collect()
    }

    /// The datapoints CloudWatch reports for one series over the window around `at_ms`.
    async fn datapoints(
        &self,
        case: &str,
        namespace: &str,
        metric_name: &str,
        dimensions: &Value,
        at_ms: i64,
    ) -> Vec<Value> {
        #[allow(clippy::cast_precision_loss)]
        let at_secs = at_ms as f64 / 1_000.0;
        let statistics = self
            .cloudwatch_ok(
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
}

/// The qualified ARN of a log group, in the form that covers its streams — the form a state
/// machine's `cloudWatchLogsLogGroup$logGroupArn` names its destination by.
fn log_group_streams_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{name}:*",
        engine::emulator_account_id()
    )
}

/// Epoch milliseconds.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
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

/// The total a window's datapoints carry: the `Sum` of their sums and of their sample counts,
/// or `None` for a series CloudWatch reports no datapoint at all for.
///
/// The window is read as several periods, so a row's records are aggregated here rather than
/// assumed to fall in one of them.
fn aggregate(points: &[Value]) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let sum: f64 = points
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum();
    let samples: f64 = points
        .iter()
        .filter_map(|point| point["SampleCount"].as_f64())
        .sum();
    Some((sum, samples))
}

/// The dimension set a read names, as a row states it: `(name, value)` pairs.
type Dimensions = &'static [(&'static str, &'static str)];

/// One read a row expects: the dimensions it names, and the `(Sum, SampleCount)` over the
/// whole window — `None` for a series that must hold no datapoint.
type Read = (Dimensions, Option<(f64, f64)>);

/// One row: the workflow type whose runs fill the vended group, the filter stored over it, how
/// many runs of each outcome are started, and what CloudWatch must then report.
struct Row {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// Names this row's log group, state machines and filter, so no two rows share one.
    name: &'static str,
    /// `EXPRESS` or `STANDARD`.
    sm_type: &'static str,
    /// The filter's `filterPattern`.
    pattern: &'static str,
    /// The transformation's `metricValue`.
    metric_value: &'static str,
    /// The transformation's `dimensions`, as `(name, selector)` pairs.
    dimensions: Dimensions,
    /// The transformation's `unit`, when the row declares one.
    unit: Option<&'static str>,
    /// How many runs of the failing workflow the row starts.
    failing: usize,
    /// How many runs of the succeeding workflow the row starts.
    succeeding: usize,
    /// What a reader must find once every record is delivered.
    reads: &'static [Read],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an express workflow's failures counted by a JSON condition on the record",
            name: "express-failures",
            sm_type: "EXPRESS",
            pattern: r#"{ $.type = "ExecutionFailed" }"#,
            metric_value: "1",
            dimensions: &[],
            unit: Some("Count"),
            failing: 3,
            succeeding: 2,
            // Only the three failures match, however many other records the group took in.
            reads: &[(&[], Some((3.0, 3.0)))],
        },
        Row {
            case: "an express workflow's failures counted by an unstructured term",
            name: "express-term",
            sm_type: "EXPRESS",
            pattern: EXECUTION_FAILED,
            metric_value: "1",
            dimensions: &[],
            unit: None,
            failing: 1,
            succeeding: 1,
            reads: &[(&[], Some((1.0, 1.0)))],
        },
        Row {
            case: "an express workflow's failures under a dimension resolved from the record",
            name: "express-dimension",
            sm_type: "EXPRESS",
            pattern: r#"{ $.type = "ExecutionFailed" }"#,
            metric_value: "1",
            dimensions: &[("EventType", "$.type")],
            unit: None,
            failing: 2,
            succeeding: 0,
            reads: &[
                (&[("EventType", "ExecutionFailed")], Some((2.0, 2.0))),
                // The dimensioned series is its own series: the undimensioned one holds
                // nothing, and so does one naming a value no record resolved.
                (&[], None),
                (&[("EventType", "ExecutionSucceeded")], None),
            ],
        },
        Row {
            case: "a standard workflow's successes counted the same way",
            name: "standard-successes",
            sm_type: "STANDARD",
            pattern: r#"{ $.type = "ExecutionSucceeded" }"#,
            metric_value: "1",
            dimensions: &[],
            unit: None,
            failing: 1,
            succeeding: 3,
            reads: &[(&[], Some((3.0, 3.0)))],
        },
        Row {
            case: "a non-integer literal metric value published once per matching record",
            name: "express-fractional",
            sm_type: "EXPRESS",
            pattern: r#"{ $.type = "ExecutionFailed" }"#,
            metric_value: "2.5",
            dimensions: &[],
            unit: Some("Milliseconds"),
            failing: 2,
            succeeding: 0,
            reads: &[(&[], Some((5.0, 2.0)))],
        },
        Row {
            case: "a selector naming a numeric member of the vended record as the value",
            name: "express-selector",
            sm_type: "EXPRESS",
            pattern: r#"{ $.type = "ExecutionFailed" }"#,
            // `redrive_count` is the count of redrives as of the record, so a run nothing
            // redrove publishes zero — which is a datapoint, not an absence.
            metric_value: "$.redrive_count",
            dimensions: &[],
            unit: None,
            failing: 2,
            succeeding: 0,
            reads: &[(&[], Some((0.0, 2.0)))],
        },
    ]
}

/// The log group, the metric filter and the state machines of one row, with every run started.
async fn wire_and_start(emulator: &Emulator, row: &Row, group: &str, namespace: &str, metric: &str) {
    let case = row.case;
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": group}))
        .await;

    let mut transformation = json!({
        "metricName": metric,
        "metricNamespace": namespace,
        "metricValue": row.metric_value,
    });
    if !row.dimensions.is_empty() {
        transformation["dimensions"] = filter_dimensions(row.dimensions);
    }
    if let Some(unit) = row.unit {
        transformation["unit"] = json!(unit);
    }
    emulator
        .logs_ok(
            case,
            "PutMetricFilter",
            &json!({
                "logGroupName": group,
                "filterName": format!("{}-count", row.name),
                "filterPattern": row.pattern,
                "metricTransformations": [transformation],
            }),
        )
        .await;

    // The runs are started without waiting for any of them, so the row's own records are
    // vended concurrently rather than one settle span apart.
    for (definition, suffix, runs) in [
        (FAILING_DEFINITION, "fails", row.failing),
        (SUCCEEDING_DEFINITION, "passes", row.succeeding),
    ] {
        if runs == 0 {
            continue;
        }
        let machine_name = format!("{}-{suffix}", row.name);
        let created = emulator
            .states_ok(
                case,
                "CreateStateMachine",
                &json!({
                    "name": machine_name,
                    "definition": definition,
                    "roleArn": ROLE_ARN,
                    "type": row.sm_type,
                    "loggingConfiguration": {
                        "level": "ALL",
                        "includeExecutionData": true,
                        "destinations": [{
                            "cloudWatchLogsLogGroup": {"logGroupArn": log_group_streams_arn(group)},
                        }],
                    },
                }),
            )
            .await;
        let state_machine = created["stateMachineArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a created machine carries an ARN, got {created}")
            })
            .to_string();
        for run in 0..runs {
            emulator
                .states_ok(
                    case,
                    "StartExecution",
                    &json!({
                        "stateMachineArn": state_machine,
                        "name": format!("run-{run}"),
                        "input": "{}",
                    }),
                )
                .await;
        }
    }
}

/// Wait until the vended group holds every run's TERMINAL record — the last record a run
/// vends, so a group holding all of them is a group nothing more will be delivered into.
///
/// Waits for the CONDITION, bounded by [`SETTLE_DEADLINE`].
async fn wait_for_every_run_to_be_vended(emulator: &Emulator, row: &Row, group: &str) {
    let case = row.case;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let types = emulator.vended_event_types(case, group).await;
        let failed = types.iter().filter(|kind| *kind == EXECUTION_FAILED).count();
        let succeeded = types
            .iter()
            .filter(|kind| *kind == EXECUTION_SUCCEEDED)
            .count();
        if failed == row.failing && succeeded == row.succeeding {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the vended group to hold {} {EXECUTION_FAILED} and {} \
             {EXECUTION_SUCCEEDED} records, it holds {failed} and {succeeded} of them \
             ({types:?})",
            row.failing,
            row.succeeding
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until every series the row expects reads as it expects, then assert the unit each
/// datapoint carries.
///
/// The records are all delivered by the time this runs, so nothing more can be published; the
/// poll only covers the step between an ingested record becoming readable and the datapoint it
/// published becoming readable.
async fn assert_series(
    emulator: &Emulator,
    row: &Row,
    namespace: &str,
    metric: &str,
    at_ms: i64,
) {
    let case = row.case;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut actual: Vec<(Dimensions, Option<(f64, f64)>)> = Vec::new();
        for (dimensions, _) in row.reads {
            let read = read_dimensions(dimensions);
            let points = emulator
                .datapoints(case, namespace, metric, &read, at_ms)
                .await;
            actual.push((dimensions, aggregate(&points)));
        }
        let matched = row
            .reads
            .iter()
            .zip(&actual)
            .all(|((_, expected), (_, got))| expected == got);
        if matched {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the series {:?}, got {actual:?}",
            row.reads
        );
        tokio::time::sleep(POLL).await;
    }

    // The unit every published datapoint carries is the transformation's own, defaulted to the
    // modelled `None` where it declares none.
    let expected_unit = row.unit.unwrap_or(UNIT_NONE);
    for (dimensions, expected) in row.reads {
        if expected.is_none() {
            continue;
        }
        let read = read_dimensions(dimensions);
        let points = emulator
            .datapoints(case, namespace, metric, &read, at_ms)
            .await;
        for point in &points {
            assert_eq!(
                point["Unit"].as_str(),
                Some(expected_unit),
                "case {case:?}: {dimensions:?} must report the unit {expected_unit:?}, got \
                 {point}"
            );
        }
    }
}

/// Every form of filter over a vended log group publishes exactly the metric data its
/// transformation declares for the records another service delivered into that group.
#[tokio::test]
async fn a_metric_filter_over_a_vended_log_group_publishes_the_records_it_matches() {
    let emulator = Emulator::new();
    let rows = rows();
    let at = now_ms();
    const METRIC: &str = "VendedMatches";

    // Every row is wired and its runs started before any row is read, so the rows' records are
    // vended concurrently and the suite waits one settle span rather than one per row.
    let mut wired: Vec<(usize, String, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let group = format!("/aws/vendedlogs/states/{}", row.name);
        let namespace = format!("VendedMetricFilter{index}");
        wire_and_start(&emulator, row, &group, &namespace, METRIC).await;
        wired.push((index, group, namespace));
    }

    for (index, group, namespace) in &wired {
        let row = &rows[*index];
        wait_for_every_run_to_be_vended(&emulator, row, group).await;
        assert_series(&emulator, row, namespace, METRIC, at).await;
    }
}
