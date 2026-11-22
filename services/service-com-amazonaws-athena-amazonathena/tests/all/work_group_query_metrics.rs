// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch metrics a workgroup publishes about the queries it RAN, driven end to end
//! over the wire.
//!
//! THE RULE under test, stated over the class and not over one query:
//!
//! > A workgroup's configuration carries one switch saying whether Amazon CloudWatch metrics
//! > are enabled for it (`WorkGroupConfiguration$PublishCloudWatchMetricsEnabled`), and a
//! > workgroup it is ON for publishes, for EVERY query that reaches a terminal state in it,
//! > one datapoint per series of the `AWS/Athena` query metric family — under the `WorkGroup`
//! > dimension naming that workgroup, and under the finer set naming the `QueryState` it
//! > settled in and the `QueryType` of statement it carried beside it — each datapoint
//! > carrying the measure the execution's own `QueryExecutionStatistics` reports, in the
//! > `StandardUnit` that measure is expressed in. A workgroup the switch is OFF for,
//! > including one whose configuration never set it, publishes nothing at all; the switch
//! > decides the queries RUN while it is on, so flipping it moves no query already submitted.
//!
//! The rows span what the rule generalises over: the switch (named on, named off, not named
//! at all, and a workgroup created with no configuration at all, which Athena gives its own
//! defaults), the kind of statement submitted (one that queries data, one that defines
//! something, one that asks the engine about a statement), the terminal state the query
//! settles in (`SUCCEEDED` and `FAILED` — a query that failed still ran, and is still metered)
//! and the direction the switch is flipped in after a query has already run. The recorded
//! case — one `SUCCEEDED` query in a workgroup created with the switch on, read back as a
//! `ProcessedBytes` datapoint under its dimensions — is one row among them.
//!
//! Nothing here reaches into a state struct and nothing is a literal this suite carries: the
//! series are read back through CloudWatch's OWN `GetMetricStatistics` and `ListMetrics` —
//! the same store a client's `PutMetricData` writes into — mounted beside Athena on one
//! engine, and every value is compared against the statistics the SAME execution's
//! `GetQueryExecution` reports.
//!
//! Every wait here waits for the CONDITION — the query reporting a terminal state, the
//! datapoint being in the series — under a deadline far above the span a completion is due
//! after, so a loaded host cannot turn a slow run into a failure.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The namespace Athena publishes its query metrics in, and the dimensions they are reported
/// under — spelled out here rather than imported, so a wrong constant in the implementation
/// cannot make a test pass.
const NAMESPACE: &str = "AWS/Athena";
const WORK_GROUP_DIMENSION: &str = "WorkGroup";
const QUERY_STATE_DIMENSION: &str = "QueryState";
const QUERY_TYPE_DIMENSION: &str = "QueryType";

/// The `StandardUnit` values the family's quantities are expressed in
/// (`smithy_query(query="shape",shape="StandardUnit")` of the CloudWatch model: `Bytes` for a
/// size, `Milliseconds` for a span of time).
const BYTES: &str = "Bytes";
const MILLISECONDS: &str = "Milliseconds";

/// The whole family a terminal query publishes: the metric name, the
/// `QueryExecutionStatistics` member it reports, and the unit it carries.
///
/// The members are the ones `QueryExecutionStatistics` declares
/// (`smithy_query(query="shape",shape="QueryExecutionStatistics")`).
const FAMILY: &[(&str, &str, &str)] = &[
    ("ProcessedBytes", "DataScannedInBytes", BYTES),
    (
        "EngineExecutionTime",
        "EngineExecutionTimeInMillis",
        MILLISECONDS,
    ),
    (
        "QueryPlanningTime",
        "QueryPlanningTimeInMillis",
        MILLISECONDS,
    ),
    ("QueryQueueTime", "QueryQueueTimeInMillis", MILLISECONDS),
    (
        "ServiceProcessingTime",
        "ServiceProcessingTimeInMillis",
        MILLISECONDS,
    ),
    (
        "TotalExecutionTime",
        "TotalExecutionTimeInMillis",
        MILLISECONDS,
    ),
];

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where these queries write their results, which every submission has to resolve to.
const OUTPUT_LOCATION: &str = "s3://athena-metrics-results/queries/";

/// How long a poll keeps asking before it gives up.
///
/// Generous: what a case asserts is the datapoint a query publishes, never how fast the host
/// gets there, so the deadline is only here to fail a wait that will never end.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many further looks a row that expects NO datapoint takes at the namespace, AFTER its
/// query has settled. Together with [`POLL_INTERVAL`] a window well past the instant a
/// published datapoint would be stamped at, so an emptiness a row asserts is one that HELD.
const QUIET_LOOKS: usize = 20;

/// How wide the window a statistics read asks about is, each way from now: far wider than a
/// query's own span, so the datapoint cannot fall outside it.
const WINDOW_SECONDS: f64 = 600.0;

/// The period a statistics read aggregates by, in seconds: regular resolution.
const PERIOD_SECONDS: i64 = 60;

/// One emulator carrying Athena and CloudWatch over one shared engine: the metric store a
/// query's series are published into is CloudWatch's own. The bucket the results go to is held
/// in that same engine's object store, the way every suite of this crate holds its own.
struct Pairing {
    router: MultiServiceRouter,
}

impl Pairing {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        // Athena verifies the resolved output location before it accepts a submission, so the
        // bucket this suite's location names is held before its first query runs.
        crate::results_bucket::hold_results_bucket(&engine, OUTPUT_LOCATION);
        router.register_service(
            service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name`.
    async fn call(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDQUERYMETRICS/20261110/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, payload)
    }

    /// One Athena call that must answer 200.
    async fn athena(&self, operation: &str, body: Value) -> Value {
        let (status, payload) = self
            .call("athena", "AmazonAthena", "1.1", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} must answer 200, got {status} with {payload}"
        );
        payload
    }

    /// One CloudWatch call that must answer 200.
    async fn cloudwatch(&self, operation: &str, body: Value) -> Value {
        let (status, payload) = self
            .call(
                "monitoring",
                "GraniteServiceVersion20100801",
                "1.0",
                operation,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "CloudWatch {operation} must answer 200, got {status} with {payload}"
        );
        payload
    }

    /// Create the workgroup `name` with the configuration `configuration` — `None` for a
    /// create that carries none at all.
    async fn create_work_group(&self, name: &str, configuration: Option<Value>) {
        let mut request = json!({ "Name": name });
        if let Some(configuration) = configuration {
            request["Configuration"] = configuration;
        }
        self.athena("CreateWorkGroup", request).await;
    }

    /// Turn the metrics switch of the workgroup `name` to `publish`, through Athena's own
    /// update.
    async fn set_metrics(&self, name: &str, publish: bool) {
        self.athena(
            "UpdateWorkGroup",
            json!({
                "WorkGroup": name,
                "ConfigurationUpdates": { "PublishCloudWatchMetricsEnabled": publish },
            }),
        )
        .await;
    }

    /// Start one query in `work_group` and answer the `QueryExecutionId` it minted.
    async fn start(&self, case: &str, work_group: &str, sql: &str) -> String {
        let answer = self
            .athena(
                "StartQueryExecution",
                json!({
                    "QueryString": sql,
                    "WorkGroup": work_group,
                    "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
                }),
            )
            .await;
        answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueryExecutionId, got {answer}"))
            .to_string()
    }

    /// Poll `GetQueryExecution` until `id` reports a terminal state, answering the execution
    /// as it settled.
    async fn settled(&self, case: &str, id: &str) -> Value {
        let deadline = Instant::now() + BUDGET;
        loop {
            let execution = self
                .athena("GetQueryExecution", json!({ "QueryExecutionId": id }))
                .await["QueryExecution"]
                .clone();
            let state = execution["Status"]["State"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state != "QUEUED" && state != "RUNNING" {
                return execution;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the query {id} must reach a terminal state, last saw \
                 {state:?} in {execution}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every datapoint CloudWatch holds for the series `metric` identified by `dimensions`,
    /// read the way a client reads a series.
    async fn datapoints(&self, dimensions: &Value, metric: &str) -> Vec<Value> {
        let now = epoch_seconds();
        let answer = self
            .cloudwatch(
                "GetMetricStatistics",
                json!({
                    "Namespace": NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": dimensions,
                    "StartTime": now - WINDOW_SECONDS,
                    "EndTime": now + WINDOW_SECONDS,
                    "Period": PERIOD_SECONDS,
                    "Statistics": ["Sum", "SampleCount", "Maximum", "Minimum"],
                }),
            )
            .await;
        answer["Datapoints"].as_array().cloned().unwrap_or_default()
    }

    /// Poll the series `metric` identified by `dimensions` until its datapoints account for
    /// at least `observations` observations, answering them.
    ///
    /// The CONDITION is what is waited on: a datapoint is published as the completion it
    /// reports falls due, so a read that finds fewer looks again rather than sleeping out a
    /// span.
    async fn await_observations(
        &self,
        case: &str,
        dimensions: &Value,
        metric: &str,
        observations: f64,
    ) -> Vec<Value> {
        let deadline = Instant::now() + BUDGET;
        loop {
            let datapoints = self.datapoints(dimensions, metric).await;
            if sample_count(&datapoints) >= observations {
                return datapoints;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {observations} observation(s) of {NAMESPACE}/{metric} \
                 under {dimensions}, got {datapoints:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every series CloudWatch lists in this namespace for `work_group`, each rendered as
    /// [`series_label`] renders it, in order.
    async fn listed_series(&self, work_group: &str) -> Vec<String> {
        let answer = self
            .cloudwatch(
                "ListMetrics",
                json!({
                    "Namespace": NAMESPACE,
                    "Dimensions": [{ "Name": WORK_GROUP_DIMENSION, "Value": work_group }],
                }),
            )
            .await;
        let mut labels: Vec<String> = answer["Metrics"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|metric| {
                Some(series_label(
                    metric["MetricName"].as_str()?,
                    &metric["Dimensions"],
                ))
            })
            .collect();
        labels.sort();
        labels
    }
}

/// ONE series, rendered as its metric name and its dimension set in name order, so a listing
/// and the sets a row expects are compared as the same strings.
fn series_label(metric: &str, dimensions: &Value) -> String {
    let mut pairs: Vec<String> = dimensions
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|dimension| {
            Some(format!(
                "{}={}",
                dimension["Name"].as_str()?,
                dimension["Value"].as_str()?
            ))
        })
        .collect();
    pairs.sort();
    format!("{metric}[{}]", pairs.join(","))
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// How many observations a window of datapoints accounts for.
fn sample_count(datapoints: &[Value]) -> f64 {
    datapoints
        .iter()
        .filter_map(|point| point["SampleCount"].as_f64())
        .sum()
}

/// The largest value any datapoint of a window reports, and `None` for no datapoint at all.
fn maximum(datapoints: &[Value]) -> Option<f64> {
    datapoints
        .iter()
        .filter_map(|point| point["Maximum"].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// The `Sum` a window of datapoints reports.
fn sum(datapoints: &[Value]) -> f64 {
    datapoints
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// The units the datapoints of a window carry, deduplicated and in order.
fn units(datapoints: &[Value]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for point in datapoints {
        if let Some(unit) = point["Unit"].as_str() {
            if !found.iter().any(|held| held == unit) {
                found.push(unit.to_string());
            }
        }
    }
    found
}

/// One row: the workgroup a query runs in, the configuration it was created with, the
/// statement submitted, and whether that workgroup publishes the family.
struct Row {
    case: &'static str,
    work_group: &'static str,
    /// The `Configuration` the create carries, and `None` for a create that carries none.
    configuration: Option<Value>,
    sql: &'static str,
    /// Whether Amazon CloudWatch metrics are enabled for it.
    publishes: bool,
}

/// A workgroup configuration naming this suite's results location, with the metrics switch
/// set to `publish` where a row names one.
fn configuration(publish: Option<bool>) -> Value {
    let mut configuration = json!({
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    });
    if let Some(publish) = publish {
        configuration["PublishCloudWatchMetricsEnabled"] = json!(publish);
    }
    configuration
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a workgroup with metrics enabled, running a projection the engine computes",
            work_group: "metrics-on-dml",
            configuration: Some(configuration(Some(true))),
            sql: "SELECT 1 AS one",
            publishes: true,
        },
        Row {
            case: "a workgroup with metrics enabled, running a definition statement",
            work_group: "metrics-on-ddl",
            configuration: Some(configuration(Some(true))),
            sql: "CREATE TABLE query_metrics_ddl (id integer, name varchar)",
            publishes: true,
        },
        Row {
            case: "a workgroup with metrics enabled, running a statement that asks the engine \
                   about a statement",
            work_group: "metrics-on-utility",
            configuration: Some(configuration(Some(true))),
            sql: "EXPLAIN SELECT 1",
            publishes: true,
        },
        Row {
            case: "a workgroup with metrics enabled, running a query that FAILS",
            work_group: "metrics-on-failing",
            configuration: Some(configuration(Some(true))),
            sql: "SELECT * FROM not_a_table",
            publishes: true,
        },
        Row {
            case: "a workgroup created with no configuration at all, which Athena gives its \
                   own defaults",
            work_group: "metrics-default",
            configuration: None,
            sql: "SELECT 2 AS two",
            publishes: true,
        },
        Row {
            case: "a workgroup with metrics disabled",
            work_group: "metrics-off",
            configuration: Some(configuration(Some(false))),
            sql: "SELECT 3 AS three",
            publishes: false,
        },
        Row {
            case: "a workgroup whose configuration names no switch at all",
            work_group: "metrics-unset",
            configuration: Some(configuration(None)),
            sql: "SELECT 4 AS four",
            publishes: false,
        },
    ]
}

/// The dimension set naming one workgroup alone, which is how a client watching that
/// workgroup's queries as a whole identifies the series.
fn work_group_dimensions(work_group: &str) -> Value {
    json!([{ "Name": WORK_GROUP_DIMENSION, "Value": work_group }])
}

/// The finer dimension set: the state the query settled in and the kind of statement it
/// carried, beside the workgroup.
fn settled_query_dimensions(work_group: &str, state: &str, query_type: &str) -> Value {
    json!([
        { "Name": QUERY_STATE_DIMENSION, "Value": state },
        { "Name": QUERY_TYPE_DIMENSION, "Value": query_type },
        { "Name": WORK_GROUP_DIMENSION, "Value": work_group },
    ])
}

/// RULE: a query that reached a terminal state in a workgroup Amazon CloudWatch metrics are
/// enabled for publishes one datapoint per series of the `AWS/Athena` family, under the
/// workgroup's own dimension and under the finer set naming the state it settled in and the
/// kind of statement it carried, each carrying the measure that execution's own statistics
/// report in the unit that measure is expressed in; a workgroup the switch is off for
/// publishes none.
#[tokio::test]
async fn a_metrics_enabled_work_group_publishes_the_family_of_every_terminal_query() {
    let pairing = Pairing::new();
    let rows = rows();

    // Every workgroup is created and every query submitted first, so the completions the
    // clock owes run beside one another rather than one after the other.
    for row in &rows {
        pairing
            .create_work_group(row.work_group, row.configuration.clone())
            .await;
    }
    let mut started: Vec<String> = Vec::new();
    for row in &rows {
        started.push(pairing.start(row.case, row.work_group, row.sql).await);
    }

    for (row, id) in rows.iter().zip(&started) {
        let execution = pairing.settled(row.case, id).await;
        let statistics = execution["Statistics"].clone();
        let state = execution["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // The kind of statement the series is dimensioned by is the one the execution itself
        // reports, so neither side of the comparison is a literal this suite carries.
        let query_type = execution["StatementType"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the execution to report its StatementType, got \
                     {execution}",
                    row.case
                )
            })
            .to_string();
        let sets = [
            work_group_dimensions(row.work_group),
            settled_query_dimensions(row.work_group, &state, &query_type),
        ];

        if !row.publishes {
            // The query has SETTLED, so a datapoint it owed would be there: the looks below
            // are what makes the emptiness one that held rather than one read too early.
            for _ in 0..QUIET_LOOKS {
                for (metric, _, _) in FAMILY {
                    for dimensions in &sets {
                        let datapoints = pairing.datapoints(dimensions, metric).await;
                        assert!(
                            datapoints.is_empty(),
                            "case {:?}: expected no {NAMESPACE}/{metric} datapoint at all \
                             under {dimensions}, got {datapoints:?}",
                            row.case
                        );
                    }
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            let listed = pairing.listed_series(row.work_group).await;
            assert!(
                listed.is_empty(),
                "case {:?}: expected no {NAMESPACE} series for this workgroup, got {listed:?}",
                row.case
            );
            continue;
        }

        for (metric, member, unit) in FAMILY {
            let expected = statistics[member].as_f64().unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the execution to report {member}, got {statistics}",
                    row.case
                )
            });
            for dimensions in &sets {
                let datapoints = pairing
                    .await_observations(row.case, dimensions, metric, 1.0)
                    .await;
                assert!(
                    (sample_count(&datapoints) - 1.0).abs() < f64::EPSILON,
                    "case {:?}: expected exactly one observation of {metric} under \
                     {dimensions} for one query, got {datapoints:?}",
                    row.case
                );
                let highest = maximum(&datapoints);
                assert_eq!(
                    highest,
                    Some(expected),
                    "case {:?} ({state}): expected {metric} under {dimensions} to report the \
                     execution's own {member} {expected}, got {highest:?} in {datapoints:?}",
                    row.case
                );
                assert_eq!(
                    units(&datapoints),
                    vec![(*unit).to_string()],
                    "case {:?}: expected {metric} to be published in {unit}, got \
                     {datapoints:?}",
                    row.case
                );
            }
        }

        let listed = pairing.listed_series(row.work_group).await;
        let mut expected: Vec<String> = FAMILY
            .iter()
            .flat_map(|(metric, _, _)| {
                sets.iter()
                    .map(|dimensions| series_label(metric, dimensions))
                    .collect::<Vec<String>>()
            })
            .collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {:?}: expected the whole family under both dimension sets to be listed for \
             this workgroup, got {listed:?}",
            row.case
        );
    }
}

/// RULE: the family is published PER TERMINAL QUERY, so a workgroup that ran several queries
/// holds one observation of each series per query, and the `Sum` over the window is the sum of
/// what those queries measured.
#[tokio::test]
async fn every_query_of_a_work_group_contributes_its_own_observation() {
    let case = "two queries in one metrics-enabled workgroup";
    let work_group = "metrics-two-queries";
    let pairing = Pairing::new();
    pairing
        .create_work_group(work_group, Some(configuration(Some(true))))
        .await;

    // Two statements that measure different things, so the sum below is not the same number
    // twice.
    let statements = [
        "SELECT 1 AS one",
        "SELECT n FROM (SELECT 2 AS n UNION ALL SELECT 1) ORDER BY n",
    ];
    let mut started: Vec<String> = Vec::new();
    for sql in statements {
        started.push(pairing.start(case, work_group, sql).await);
    }

    let mut expected_bytes = 0.0;
    for id in &started {
        expected_bytes += scanned_bytes(&pairing.settled(case, id).await, case);
    }

    let datapoints = pairing
        .await_observations(
            case,
            &work_group_dimensions(work_group),
            "ProcessedBytes",
            2.0,
        )
        .await;
    assert!(
        (sample_count(&datapoints) - 2.0).abs() < f64::EPSILON,
        "case {case:?}: expected exactly one observation per query, got {datapoints:?}"
    );
    assert!(
        (sum(&datapoints) - expected_bytes).abs() < f64::EPSILON,
        "case {case:?}: expected the Sum to be the {expected_bytes} bytes the two executions \
         report, got {} in {datapoints:?}",
        sum(&datapoints)
    );
}

/// RULE: the publication is owed by the CLOCK, not by a reader — a client that submits a query
/// and then watches only CloudWatch sees the datapoints appear without any further Athena call.
#[tokio::test]
async fn the_family_appears_with_no_further_athena_call() {
    let case = "a client that polls nothing but CloudWatch";
    let work_group = "metrics-unpolled";
    let pairing = Pairing::new();
    pairing
        .create_work_group(work_group, Some(configuration(Some(true))))
        .await;
    pairing.start(case, work_group, "SELECT 1 AS one").await;

    // Nothing is asked of Athena from here on: the only thing waited for is the series.
    for (metric, _, _) in FAMILY {
        let datapoints = pairing
            .await_observations(case, &work_group_dimensions(work_group), metric, 1.0)
            .await;
        assert!(
            !datapoints.is_empty(),
            "case {case:?}: expected a {metric} datapoint to appear unasked, got \
             {datapoints:?}"
        );
    }
}

/// RULE: the switch decides the queries RUN while it is on, so flipping it moves no query
/// already submitted: turning the metrics on does not meter the queries that ran while they
/// were off, and turning them off does not withhold the metrics of a query that ran while they
/// were on.
///
/// The rows differ along the direction of the flip, which is what the rule generalises over.
#[tokio::test]
async fn flipping_the_switch_meters_only_the_queries_submitted_after_it() {
    // (case, the switch the workgroup is created with, the switch it is updated to, the
    // workgroup, whether the query submitted BEFORE the update is metered, whether the one
    // submitted AFTER it is)
    let rows: Vec<(&str, bool, bool, &str, bool, bool)> = vec![
        (
            "metrics turned ON after a query already ran",
            false,
            true,
            "metrics-turned-on",
            false,
            true,
        ),
        (
            "metrics turned OFF after a query already ran",
            true,
            false,
            "metrics-turned-off",
            true,
            false,
        ),
    ];

    for (case, created, updated, work_group, before_metered, after_metered) in rows {
        let pairing = Pairing::new();
        pairing
            .create_work_group(work_group, Some(configuration(Some(created))))
            .await;

        // The first query is driven all the way to its terminal state BEFORE the switch is
        // flipped, so what the flip could change is a decision already taken.
        let before = pairing.start(case, work_group, "SELECT 1 AS one").await;
        let before_bytes = scanned_bytes(&pairing.settled(case, &before).await, case);
        pairing.set_metrics(work_group, updated).await;

        let after = pairing
            .start(
                case,
                work_group,
                "SELECT n FROM (SELECT 2 AS n UNION ALL SELECT 1) ORDER BY n",
            )
            .await;
        let after_bytes = scanned_bytes(&pairing.settled(case, &after).await, case);

        let mut expected_bytes = 0.0;
        let mut expected_observations = 0.0;
        for (metered, bytes) in [(before_metered, before_bytes), (after_metered, after_bytes)] {
            if metered {
                expected_bytes += bytes;
                expected_observations += 1.0;
            }
        }

        let dimensions = work_group_dimensions(work_group);
        pairing
            .await_observations(case, &dimensions, "ProcessedBytes", expected_observations)
            .await;
        // One further round of looks, so a datapoint the rule says must NOT be there would
        // have shown up by the time the series is compared.
        for _ in 0..QUIET_LOOKS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let datapoints = pairing.datapoints(&dimensions, "ProcessedBytes").await;
        assert!(
            (sample_count(&datapoints) - expected_observations).abs() < f64::EPSILON,
            "case {case:?}: expected {expected_observations} observation(s) of ProcessedBytes, \
             got {datapoints:?}"
        );
        assert!(
            (sum(&datapoints) - expected_bytes).abs() < f64::EPSILON,
            "case {case:?}: expected the Sum to be the {expected_bytes} bytes the metered \
             query reports, got {} in {datapoints:?}",
            sum(&datapoints)
        );
    }
}

/// The `DataScannedInBytes` an execution reports, which is what its `ProcessedBytes`
/// observation carries.
fn scanned_bytes(execution: &Value, case: &str) -> f64 {
    execution["Statistics"]["DataScannedInBytes"]
        .as_f64()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the execution to report its statistics, got {execution}")
        })
}
