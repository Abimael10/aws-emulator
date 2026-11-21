// SPDX-License-Identifier: CC0-1.0

//! EVERY metric filter a log group holds extracts from each record another service VENDS into
//! that group, and what the filters publish is an ordinary CloudWatch series.
//!
//! RULE under test: **a record delivered into a CloudWatch Logs log group is an ingestion into
//! that group, so it is run against EVERY metric filter the group holds at that moment, and the
//! `metricTransformation` of each filter whose `filterPattern` matches it publishes its
//! `metricValue` into the `metricNamespace`/`metricName` that filter names — under one dimension
//! per `dimensions` entry, resolved in that record — while a filter nothing matches publishes no
//! series at all; the datapoints land in the one metric store, so `GetMetricStatistics`,
//! `GetMetricData` and `ListMetrics` all answer for them.**
//!
//! The sibling suite `logs_metric_filter_over_a_vended_log_group.rs` varies ONE filter over the
//! shape of the filter and of the runs filling the group. This one varies what that suite holds
//! fixed: how many filters the group holds (several at once, over the same delivered records),
//! how their namespaces relate (two filters sharing one namespace under different metric names,
//! two more in a second namespace), which record of a run each pattern selects (the event that
//! opens a run, the two that end it, a nested member of a record's own detail), the form of the
//! value (a literal 1, a weight of 2 per matching record), and which reader the series is read
//! back through. A group holding exactly one filter whose pattern counts the failures — the
//! recorded shape an issue happens to mention — is one row of the table.
//!
//! Three crates have to agree for the rule to hold (Step Functions vends a run's history,
//! CloudWatch Logs ingests it and extracts through the group's filters, CloudWatch answers for
//! the series), so the suite lives here.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and so the Region every ARN here names.
const REGION: &str = "us-west-2";

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// AWS Step Functions' `X-Amz-Target` prefix and signing name.
const STATES_TARGET_PREFIX: &str = "AWSStepFunctions";
const STATES_SIGNING_NAME: &str = "states";

/// The log group every run of this suite vends into, and the two namespaces the filters over it
/// publish into — one shared by several filters, one of its own.
const GROUP: &str = "/aws/vendedlogs/states/metric-filter-fan-out";
const SHARED_NAMESPACE: &str = "VendedFanOutShared";
const OTHER_NAMESPACE: &str = "VendedFanOutOther";

/// The role the state machines run as. No IAM is mounted here, so this account holds no record
/// for it — which is no evidence about it, and the create is served.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/observability";

/// The error and the cause the failing workflow's `Fail` state raises, which is what a record's
/// own detail carries and what a nested selector reads out of it.
const FAILURE_ERROR: &str = "Boom";

/// A workflow that fails at once, so each of its runs vends one failure.
const FAILING_DEFINITION: &str = r#"{"StartAt":"Boom","States":{"Boom":{"Type":"Fail","Error":"Boom","Cause":"this run fails on purpose"}}}"#;

/// A workflow that succeeds at once, so each of its runs vends one success.
const SUCCEEDING_DEFINITION: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Succeed"}}}"#;

/// The history event types this suite counts: the one that opens every run, and the two that end
/// one.
const EXECUTION_STARTED: &str = "ExecutionStarted";
const EXECUTION_FAILED: &str = "ExecutionFailed";
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";

/// How many runs of each workflow fill the group. Every run vends exactly one
/// [`EXECUTION_STARTED`] record and exactly one terminal record, so each count below is what a
/// filter selecting that record must publish.
const FAILING_RUNS: usize = 3;
const SUCCEEDING_RUNS: usize = 2;

/// How long the suite waits for the vended deliveries and the metrics they publish, and how often
/// it looks. The outcome of a run is owed to the clock, so the wait is for the CONDITION; the
/// budget is far longer than the span either service chooses, so a loaded host is slower without
/// the rule being any different.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The window every read covers, and the period it is read in: wide enough that every record of
/// the suite falls inside it however long the runs took, and read as several periods whose
/// datapoints are aggregated, so a run straddling a period boundary is the same reading.
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
        router.register_service(
            service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
    /// the request. The signature itself is never checked here.
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

    /// One CloudWatch Logs call that must succeed.
    async fn logs_ok(&self, case: &str, operation: &str, request: &Value) -> Value {
        let (status, parsed) = self
            .call(
                LOGS_SIGNING_NAME,
                LOGS_TARGET_PREFIX,
                "application/x-amz-json-1.1",
                operation,
                request,
            )
            .await;
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

    /// The `type` of every record the vended log group holds, as a client reading the group sees
    /// them.
    ///
    /// The read itself must SUCCEED: the group was created before any run was started, so a
    /// refusal is a defect of the read rather than a group still being written into.
    async fn vended_event_types(&self, case: &str) -> Vec<String> {
        let page = self
            .logs_ok(case, "FilterLogEvents", &json!({"logGroupName": GROUP}))
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

    /// What `GetMetricStatistics` reports for one series over the window around `at_ms`: the
    /// `(Sum, SampleCount)` totalled over the window's periods, or `None` for a series it reports
    /// no datapoint at all for.
    async fn statistics(
        &self,
        case: &str,
        namespace: &str,
        metric_name: &str,
        dimensions: &Value,
        at_ms: i64,
    ) -> Option<(f64, f64)> {
        #[allow(clippy::cast_precision_loss)]
        let at_secs = at_ms as f64 / 1_000.0;
        let answered = self
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
        let points = answered["Datapoints"]
            .as_array()
            .cloned()
            .unwrap_or_default();
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

    /// The same series through the OTHER reader: what `GetMetricData` answers for it over the
    /// same window — the `(Sum, SampleCount)` totalled the same way, or `None` for a series it
    /// reports no value for.
    async fn metric_data(
        &self,
        case: &str,
        namespace: &str,
        metric_name: &str,
        dimensions: &Value,
        at_ms: i64,
    ) -> Option<(f64, f64)> {
        #[allow(clippy::cast_precision_loss)]
        let at_secs = at_ms as f64 / 1_000.0;
        let metric = json!({
            "Namespace": namespace,
            "MetricName": metric_name,
            "Dimensions": dimensions,
        });
        let answered = self
            .cloudwatch_ok(
                case,
                "GetMetricData",
                &json!({
                    "MetricDataQueries": [
                        {
                            "Id": "sums",
                            "MetricStat": {"Metric": metric, "Period": PERIOD_SECONDS, "Stat": "Sum"},
                            "ReturnData": true,
                        },
                        {
                            "Id": "samples",
                            "MetricStat": {
                                "Metric": metric,
                                "Period": PERIOD_SECONDS,
                                "Stat": "SampleCount",
                            },
                            "ReturnData": true,
                        },
                    ],
                    "StartTime": at_secs - WINDOW_SECONDS,
                    "EndTime": at_secs + WINDOW_SECONDS,
                }),
            )
            .await;
        let results = answered["MetricDataResults"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let totals: Vec<(usize, f64)> = ["sums", "samples"]
            .iter()
            .map(|id| {
                let values = results
                    .iter()
                    .find(|result| result["Id"].as_str() == Some(*id))
                    .map(|result| result["Values"].as_array().cloned().unwrap_or_default())
                    .unwrap_or_default();
                (
                    values.len(),
                    values.iter().filter_map(Value::as_f64).sum::<f64>(),
                )
            })
            .collect();
        if totals.iter().all(|(count, _)| *count == 0) {
            return None;
        }
        Some((totals[0].1, totals[1].1))
    }

    /// The catalogue CloudWatch holds for one namespace: every (metric name, dimension names)
    /// pair `ListMetrics` reports, which is the set form the exactness assertion is stated over.
    async fn catalogue(&self, case: &str, namespace: &str) -> BTreeSet<(String, Vec<String>)> {
        let mut pairs: BTreeSet<(String, Vec<String>)> = BTreeSet::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut request = json!({"Namespace": namespace});
            if let Some(token) = &next_token {
                request["NextToken"] = json!(token);
            }
            let reply = self.cloudwatch_ok(case, "ListMetrics", &request).await;
            for metric in reply["Metrics"].as_array().into_iter().flatten() {
                let name = metric["MetricName"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a catalogued metric is named, got {metric}")
                    })
                    .to_string();
                let mut names: Vec<String> = metric["Dimensions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|dimension| {
                        dimension["Name"]
                            .as_str()
                            .unwrap_or_else(|| {
                                panic!(
                                    "case {case:?}: a catalogued dimension is named, got {metric}"
                                )
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

/// The dimension set a read names, as a row states it: `(name, value)` pairs.
type Dimensions = &'static [(&'static str, &'static str)];

/// One read a row expects: the dimensions it names, and the `(Sum, SampleCount)` over the whole
/// window — `None` for a series that must hold no datapoint.
type Read = (Dimensions, Option<(f64, f64)>);

/// One filter installed on the vended group, and what the runs must make it publish.
struct Filter {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The filter's own name, unique inside the group.
    name: &'static str,
    /// The filter's `filterPattern`.
    pattern: &'static str,
    /// The namespace its transformation publishes into.
    namespace: &'static str,
    /// The metric name its transformation publishes under.
    metric: &'static str,
    /// The transformation's `metricValue`.
    metric_value: &'static str,
    /// The transformation's `dimensions`, as `(name, selector)` pairs — a selector resolved in
    /// the record, not the value a read names.
    dimensions: Dimensions,
    /// What a reader must find once every record is delivered.
    reads: &'static [Read],
}

/// Every filter the group holds, over the same delivered records.
fn filters() -> Vec<Filter> {
    vec![
        Filter {
            case: "the terminal failure of every failing run, counted one per record",
            name: "count-failures",
            pattern: r#"{ $.type = "ExecutionFailed" }"#,
            namespace: SHARED_NAMESPACE,
            metric: "Failures",
            metric_value: "1",
            dimensions: &[],
            reads: &[(&[], Some((3.0, 3.0)))],
        },
        Filter {
            case: "the terminal success of every succeeding run, in the same namespace",
            name: "count-successes",
            pattern: r#"{ $.type = "ExecutionSucceeded" }"#,
            namespace: SHARED_NAMESPACE,
            metric: "Successes",
            metric_value: "1",
            dimensions: &[],
            reads: &[(&[], Some((2.0, 2.0)))],
        },
        Filter {
            case: "the event that opens a run, so every run is counted whatever its outcome",
            name: "count-starts",
            pattern: r#"{ $.type = "ExecutionStarted" }"#,
            namespace: SHARED_NAMESPACE,
            metric: "Started",
            metric_value: "1",
            dimensions: &[],
            reads: &[(&[], Some((5.0, 5.0)))],
        },
        Filter {
            case: "an unstructured term weighing each matching record 2",
            name: "weigh-failures",
            pattern: EXECUTION_FAILED,
            namespace: OTHER_NAMESPACE,
            metric: "WeightedFailures",
            metric_value: "2",
            dimensions: &[],
            reads: &[(&[], Some((6.0, 3.0)))],
        },
        Filter {
            case: "a nested member of a record's own detail, as condition and as dimension",
            name: "count-boom",
            pattern: r#"{ $.details.error = "Boom" }"#,
            namespace: OTHER_NAMESPACE,
            metric: "BoomFailures",
            metric_value: "1",
            dimensions: &[("Error", "$.details.error")],
            reads: &[
                (&[("Error", FAILURE_ERROR)], Some((3.0, 3.0))),
                // The dimensioned series is its own series: the undimensioned one holds
                // nothing, and so does one naming a value no record resolved.
                (&[], None),
                (&[("Error", "States.Timeout")], None),
            ],
        },
        Filter {
            case: "a pattern no delivered record matches publishes no series at all",
            name: "count-aborted",
            pattern: r#"{ $.type = "ExecutionAborted" }"#,
            namespace: OTHER_NAMESPACE,
            metric: "AbortedRuns",
            metric_value: "1",
            dimensions: &[],
            reads: &[(&[], None)],
        },
    ]
}

/// Install every filter on the group, then start every run.
async fn wire_and_start(emulator: &Emulator, filters: &[Filter]) {
    let case = "the vended group and its filters";
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;

    for filter in filters {
        let mut transformation = json!({
            "metricName": filter.metric,
            "metricNamespace": filter.namespace,
            "metricValue": filter.metric_value,
        });
        if !filter.dimensions.is_empty() {
            transformation["dimensions"] = filter_dimensions(filter.dimensions);
        }
        emulator
            .logs_ok(
                filter.case,
                "PutMetricFilter",
                &json!({
                    "logGroupName": GROUP,
                    "filterName": filter.name,
                    "filterPattern": filter.pattern,
                    "metricTransformations": [transformation],
                }),
            )
            .await;
    }

    // The runs are started without waiting for any of them, so the records are vended
    // concurrently rather than one settle span apart.
    for (definition, suffix, runs) in [
        (FAILING_DEFINITION, "fails", FAILING_RUNS),
        (SUCCEEDING_DEFINITION, "passes", SUCCEEDING_RUNS),
    ] {
        let created = emulator
            .states_ok(
                case,
                "CreateStateMachine",
                &json!({
                    "name": format!("fan-out-{suffix}"),
                    "definition": definition,
                    "roleArn": ROLE_ARN,
                    "type": "EXPRESS",
                    "loggingConfiguration": {
                        "level": "ALL",
                        "includeExecutionData": true,
                        "destinations": [{
                            "cloudWatchLogsLogGroup": {"logGroupArn": log_group_streams_arn(GROUP)},
                        }],
                    },
                }),
            )
            .await;
        let state_machine = created["stateMachineArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a created machine carries an ARN, got {created}"))
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

/// Wait until the vended group holds every run's opening and terminal record — the records a
/// filter of the table selects — so a group holding all of them is a group nothing more will be
/// delivered into.
///
/// Waits for the CONDITION, bounded by [`SETTLE_DEADLINE`].
async fn wait_for_every_run_to_be_vended(emulator: &Emulator) {
    let case = "every run is vended";
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let types = emulator.vended_event_types(case).await;
        let counted = |wanted: &str| types.iter().filter(|kind| *kind == wanted).count();
        let started = counted(EXECUTION_STARTED);
        let failed = counted(EXECUTION_FAILED);
        let succeeded = counted(EXECUTION_SUCCEEDED);
        if started == FAILING_RUNS + SUCCEEDING_RUNS
            && failed == FAILING_RUNS
            && succeeded == SUCCEEDING_RUNS
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the vended group to hold {} {EXECUTION_STARTED}, \
             {FAILING_RUNS} {EXECUTION_FAILED} and {SUCCEEDING_RUNS} {EXECUTION_SUCCEEDED} \
             records, it holds {started}, {failed} and {succeeded} of them ({types:?})",
            FAILING_RUNS + SUCCEEDING_RUNS
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until every series of `filter` reads as it expects through `GetMetricStatistics`.
///
/// The records are all delivered by the time this runs, so nothing more can be published; the
/// poll only covers the step between an ingested record becoming readable and the datapoint it
/// published becoming readable.
async fn wait_for_the_series(emulator: &Emulator, filter: &Filter, at_ms: i64) {
    let case = filter.case;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut actual: Vec<(Dimensions, Option<(f64, f64)>)> = Vec::new();
        for (dimensions, _) in filter.reads {
            let read = read_dimensions(dimensions);
            let got = emulator
                .statistics(case, filter.namespace, filter.metric, &read, at_ms)
                .await;
            actual.push((dimensions, got));
        }
        let matched = filter
            .reads
            .iter()
            .zip(&actual)
            .all(|((_, expected), (_, got))| expected == got);
        if matched {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {}/{} to read {:?}, got {actual:?}",
            filter.namespace,
            filter.metric,
            filter.reads
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every filter a vended log group holds publishes exactly the series its own transformation
/// declares for the records another service delivered into that group, through every reader
/// CloudWatch answers a series with.
#[tokio::test]
async fn every_filter_of_a_vended_log_group_publishes_the_records_it_matches() {
    let emulator = Emulator::new();
    let filters = filters();
    let at = now_ms();

    wire_and_start(&emulator, &filters).await;
    wait_for_every_run_to_be_vended(&emulator).await;

    for filter in &filters {
        wait_for_the_series(&emulator, filter, at).await;
        // The SAME series through the other reader: one metric store, so the two readers cannot
        // disagree about what the filter published.
        for (dimensions, expected) in filter.reads {
            let read = read_dimensions(dimensions);
            let got = emulator
                .metric_data(filter.case, filter.namespace, filter.metric, &read, at)
                .await;
            assert_eq!(
                got, *expected,
                "case {:?}: GetMetricData must report {expected:?} for {}/{} under \
                 {dimensions:?}, got {got:?}",
                filter.case, filter.namespace, filter.metric
            );
        }
    }

    // The catalogue of each namespace is EXACTLY the series the filters published into it: a
    // filter nothing matched created none, so its metric is not listed.
    for (namespace, expected) in [
        (
            SHARED_NAMESPACE,
            vec![
                ("Failures", Vec::<String>::new()),
                ("Started", Vec::new()),
                ("Successes", Vec::new()),
            ],
        ),
        (
            OTHER_NAMESPACE,
            vec![
                ("BoomFailures", vec!["Error".to_string()]),
                ("WeightedFailures", Vec::new()),
            ],
        ),
    ] {
        let case = format!("the catalogue of {namespace}");
        let listed = emulator.catalogue(&case, namespace).await;
        let wanted: BTreeSet<(String, Vec<String>)> = expected
            .into_iter()
            .map(|(metric, dimensions)| (metric.to_string(), dimensions))
            .collect();
        assert_eq!(
            listed, wanted,
            "case {case:?}: ListMetrics must catalogue exactly the published series, got \
             {listed:?}"
        );
    }
}
