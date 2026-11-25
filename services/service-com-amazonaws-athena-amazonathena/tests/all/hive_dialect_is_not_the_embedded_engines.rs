// SPDX-License-Identifier: CC0-1.0

//! The DIALECT a submitted statement is read in, and the words a refusal of one is stated in.
//!
//! THE RULE UNDER TEST, stated over the class and not over one statement:
//!
//! > A statement submitted to `StartQueryExecution` is read in ATHENA's own SQL — the Hive
//! > and Presto dialect its catalog statements are written in — and performed against the
//! > Data Catalog, never in the dialect of whatever engine computes its rows: a statement
//! > that dialect accepts settles in `QueryExecution.Status.State` `SUCCEEDED` with the
//! > `StatementType` and `SubstatementType` of its form and with NEITHER
//! > `Status.StateChangeReason` NOR `Status.AthenaError` (the model documents both as
//! > populated for a failed or cancelled query), and a statement it does not accept is
//! > answered in Athena's own words — the parser's diagnostic at submit, or, for a statement
//! > the catalog could not perform, the performing engine's line as the state change reason
//! > beside the condition's own sentence as the error message. No text a client reads back,
//! > on either channel, is a diagnostic of the engine the emulator computes rows with.
//!
//! The rows span what the rule generalises over: the KEYWORD the definition is spelled with
//! (`DATABASE`, the Presto `SCHEMA`), how its name is written (back-quoted, bare, mixed case
//! that folds), the CLAUSES it carries (`IF NOT EXISTS`, `COMMENT`, `LOCATION`), the class of
//! statement (a definition of a database, a definition of an external table, a query over the
//! rows that table's objects hold) and, on the refusal side, the channel the refusal comes
//! back on (synchronous at submit for text the grammar cannot read, terminal for a statement
//! the catalog refused). The recorded case — a back-quoted `CREATE DATABASE` polled to
//! `SUCCEEDED`, followed by the definition and the query a client runs after it — is one row
//! among them.
//!
//! Every statement is driven end to end over the same router the emulator boots, in a
//! workgroup whose configuration has Amazon CloudWatch metrics ENABLED, so the metering path
//! a terminal query takes is live while the flow runs; the series the flow publishes are read
//! back through CloudWatch's own `GetMetricStatistics`, and each value is compared against the
//! statistics the SAME execution reports rather than against a literal.
//!
//! Every wait here waits for the CONDITION — the query reporting a terminal state, the
//! datapoints accounting for the queries that ran — under a deadline far above the span a
//! completion is due after, so a loaded host cannot turn a slow run into a failure.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_QUEUED, QUERY_STATE_RUNNING, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where this suite's queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-dialect-results/queries/";

/// The bucket the tables this suite defines keep their data in, which a definition's
/// `LOCATION` has to name for the objects under it to be the table's rows.
const DATA_BUCKET: &str = "athena-dialect-data";

/// The workgroup the flow runs in: Amazon CloudWatch metrics are ON for it, which is the
/// configuration a client watching its queries' telemetry creates.
const WORK_GROUP: &str = "athena-dialect-wg";

/// The catalog a statement here resolves its names against: the account's Glue Data Catalog,
/// which is the one Athena reserves this name for.
const GLUE_CATALOG: &str = "AwsDataCatalog";

/// The `StatementType` values of the classes the flow's statements fall in
/// (`smithy_query(query="shape",shape="StatementType")`: DDL, DML, UTILITY).
const STATEMENT_TYPE_DDL: &str = "DDL";
const STATEMENT_TYPE_DML: &str = "DML";

/// The `SubstatementType` each form reports — "the kind of query statement that was run",
/// named in upper snake case.
const SUBSTATEMENT_CREATE_DATABASE: &str = "CREATE_DATABASE";
const SUBSTATEMENT_CREATE_TABLE: &str = "CREATE_TABLE";
const SUBSTATEMENT_SELECT: &str = "SELECT";

/// The namespace a workgroup's query metrics are published in, and the series this suite
/// reads back — spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a test pass.
const NAMESPACE: &str = "AWS/Athena";
const WORK_GROUP_DIMENSION: &str = "WorkGroup";
const PROCESSED_BYTES_METRIC: &str = "ProcessedBytes";
const ENGINE_EXECUTION_TIME_METRIC: &str = "EngineExecutionTime";

/// The `AthenaError$ErrorCategory` of a failure the caller's own statement caused: the
/// documented list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// Fragments that belong to the dialect and the diagnostics of an EMBEDDED SQL engine rather
/// than to Athena: a client reading a state change reason, an error message or a refusal of a
/// statement must never be handed one of them.
///
/// Compared in lower case against the lower-cased text, so a difference in capitalisation
/// cannot hide one.
const ENGINE_DIAGNOSTIC_FRAGMENTS: &[&str] = &[
    "syntax error",
    "at offset",
    "no such table",
    "no such column",
    "no such function",
    "sql logic error",
    "sqlite",
    "rusqlite",
    "unrecognized token",
];

/// How long a poll keeps asking before it gives up.
///
/// Generous: what a case asserts is the state a query reaches and the datapoints it
/// publishes, never how fast the host gets there, so the deadline is only here to fail a wait
/// that will never end.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How wide the window a statistics read asks about is, each way from now: far wider than a
/// query's own span, so a datapoint cannot fall outside it.
const WINDOW_SECONDS: f64 = 600.0;

/// The period a statistics read aggregates by, in seconds: regular resolution.
const PERIOD_SECONDS: i64 = 60;

/// One emulator carrying Athena, the object store its tables read and CloudWatch, all over
/// one shared engine: the catalog a statement writes, the objects a table's rows come from and
/// the metric store a terminal query publishes into are each the one this emulator has.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Pinned off so a suite does not depend on how the emulator process was started:
        // what is asserted here is Athena's behaviour, not the emulator's SigV4 check.
        router.set_request_authentication(false);
        // Athena verifies the resolved output location before it accepts a submission, so the
        // bucket this suite writes results into is held before its first query runs, exactly
        // as a client creates its results bucket first.
        crate::results_bucket::hold_results_bucket(&engine, OUTPUT_LOCATION);
        crate::results_bucket::hold_bucket(&engine, DATA_BUCKET);
        router.register_service(
            service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
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
            .header("authorization", authorization(signing_name))
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

    /// One Athena call, with its status and parsed body.
    async fn athena(&self, operation: &str, body: Value) -> (u16, Value) {
        self.call("athena", "AmazonAthena", "1.1", operation, body)
            .await
    }

    /// One Athena call that must answer 200.
    async fn athena_ok(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, payload) = self.athena(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200, got {status} with {payload}"
        );
        payload
    }

    /// One CloudWatch call that must answer 200.
    async fn cloudwatch_ok(&self, operation: &str, body: Value) -> Value {
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

    /// Write one object, which is what a table's location then holds.
    async fn put_object(&self, case: &str, key: &str, body: &str) {
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("/{DATA_BUCKET}/{key}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        assert!(
            response.status().is_success(),
            "case {case:?}: writing s3://{DATA_BUCKET}/{key} must succeed, got {} with {}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
    }

    /// Create the workgroup the flow runs in, with Amazon CloudWatch metrics enabled.
    async fn create_work_group(&self) {
        self.athena_ok(
            "setup",
            "CreateWorkGroup",
            json!({
                "Name": WORK_GROUP,
                "Configuration": {
                    "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
                    "PublishCloudWatchMetricsEnabled": true,
                },
            }),
        )
        .await;
    }

    /// Submit `sql` in `database` and answer the status and body the submit itself answered.
    async fn submit(&self, database: &str, sql: &str) -> (u16, Value) {
        self.athena(
            "StartQueryExecution",
            json!({
                "QueryString": sql,
                "QueryExecutionContext": { "Database": database, "Catalog": GLUE_CATALOG },
                "WorkGroup": WORK_GROUP,
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
        )
        .await
    }

    /// Submit `sql` in `database`, assert the submission was ACCEPTED, and answer the
    /// `QueryExecutionId` it minted.
    async fn start(&self, case: &str, database: &str, sql: &str) -> String {
        let (status, answer) = self.submit(database, sql).await;
        assert_eq!(
            status, 200,
            "case {case:?}: StartQueryExecution({sql}) must be accepted, got {status} with \
             {answer}"
        );
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
                .athena_ok(case, "GetQueryExecution", json!({ "QueryExecutionId": id }))
                .await["QueryExecution"]
                .clone();
            let state = execution["Status"]["State"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state != QUERY_STATE_QUEUED && state != QUERY_STATE_RUNNING {
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

    /// The rows `GetQueryResults` answers for the execution `id`.
    async fn result_rows(&self, case: &str, id: &str) -> Vec<Value> {
        let answer = self
            .athena_ok(
                case,
                "GetQueryResults",
                json!({ "QueryExecutionId": id, "MaxResults": 100 }),
            )
            .await;
        answer["ResultSet"]["Rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Every datapoint CloudWatch holds for the series `metric` of this workgroup, read the
    /// way a client reads a series.
    async fn datapoints(&self, metric: &str) -> Vec<Value> {
        let now = epoch_seconds();
        let answer = self
            .cloudwatch_ok(
                "GetMetricStatistics",
                json!({
                    "Namespace": NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": [
                        { "Name": WORK_GROUP_DIMENSION, "Value": WORK_GROUP },
                    ],
                    "StartTime": now - WINDOW_SECONDS,
                    "EndTime": now + WINDOW_SECONDS,
                    "Period": PERIOD_SECONDS,
                    "Statistics": ["Sum", "SampleCount"],
                }),
            )
            .await;
        answer["Datapoints"].as_array().cloned().unwrap_or_default()
    }

    /// Poll the series `metric` of this workgroup until its datapoints account for at least
    /// `observations` observations, answering them.
    ///
    /// The CONDITION is what is waited on: a datapoint is published as the completion it
    /// reports falls due, so a read that finds fewer looks again rather than sleeping out a
    /// span.
    async fn await_observations(&self, metric: &str, observations: f64) -> Vec<Value> {
        let deadline = Instant::now() + BUDGET;
        loop {
            let datapoints = self.datapoints(metric).await;
            if sample_count(&datapoints) >= observations {
                return datapoints;
            }
            assert!(
                Instant::now() < deadline,
                "expected at least {observations} observation(s) of {NAMESPACE}/{metric} under \
                 {WORK_GROUP_DIMENSION}={WORK_GROUP}, got {datapoints:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The `Authorization` header a request carries: the router picks a service by the SigV4
/// credential scope's service field, so every request names the one it is for. Nothing is a
/// credential — [`Emulator::new`] turns request authentication off — the scope only routes.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
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

/// The `Sum` a window of datapoints reports.
fn sum(datapoints: &[Value]) -> f64 {
    datapoints
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// Assert that `text` — something a client reads back about a statement — carries no fragment
/// of an embedded SQL engine's own dialect or diagnostics.
fn assert_no_engine_diagnostic(case: &str, what: &str, text: &str) {
    let folded = text.to_lowercase();
    for fragment in ENGINE_DIAGNOSTIC_FRAGMENTS {
        assert!(
            !folded.contains(fragment),
            "case {case:?}: {what} must be stated in Athena's own words, but it carries the \
             engine fragment {fragment:?}: {text:?}"
        );
    }
}

/// Assert that the settled execution `execution` SUCCEEDED, carrying the two kinds its form
/// names and NEITHER of the two members a failure populates.
fn assert_succeeded(case: &str, execution: &Value, statement_type: &str, substatement_type: &str) {
    let status = &execution["Status"];
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: expected {QUERY_STATE_SUCCEEDED}, got {execution}"
    );
    // Both members are documented as populated for a failed or cancelled query, so a query
    // that succeeded reports neither — an absent member and an explicit null are the same
    // answer to a client, and both are accepted here.
    assert!(
        status["StateChangeReason"].is_null(),
        "case {case:?}: a succeeded query reports no StateChangeReason, got {execution}"
    );
    assert!(
        status["AthenaError"].is_null(),
        "case {case:?}: a succeeded query reports no AthenaError, got {execution}"
    );
    assert_eq!(
        (
            execution["StatementType"].as_str(),
            execution["SubstatementType"].as_str(),
        ),
        (Some(statement_type), Some(substatement_type)),
        "case {case:?}: expected the kinds {statement_type}/{substatement_type}, got {execution}"
    );
}

/// One row of the flow table: the three statements a client runs to stand a catalog object up
/// and query it, and the data the query must find.
struct Flow {
    /// What this row is.
    case: &'static str,
    /// The definition of the database, as submitted.
    database_sql: String,
    /// The name the catalog then holds that database under, which the statements after it
    /// address it by.
    database: &'static str,
    /// The definition of the table, as submitted.
    table_sql: String,
    /// The query over that table, as submitted.
    select_sql: String,
    /// The object the table's location holds, and its bytes.
    object_key: String,
    object_body: &'static str,
    /// How many DATA rows the query must answer, beside the row of column labels Athena puts
    /// in front of a data-manipulating statement's rows.
    data_rows: usize,
}

/// The rows: the same three-statement flow, written four ways.
///
/// They differ along the keyword the definition is spelled with, how its name is written, the
/// clauses it carries, the delimiter the table's objects are written with and the shape of the
/// query — everything the rule generalises over. The back-quoted row is the recorded case.
fn flows() -> Vec<Flow> {
    vec![
        Flow {
            case: "a back-quoted CREATE DATABASE, then its table, then a query over it",
            database_sql: "CREATE DATABASE `dialect_quoted`".to_string(),
            database: "dialect_quoted",
            table_sql: format!(
                "CREATE EXTERNAL TABLE `dialect_quoted`.`items` (id int, name string) ROW \
                 FORMAT DELIMITED FIELDS TERMINATED BY ',' LOCATION \
                 's3://{DATA_BUCKET}/dialect_quoted/items/'"
            ),
            select_sql: "SELECT id, name FROM `dialect_quoted`.`items`".to_string(),
            object_key: "dialect_quoted/items/part-0.csv".to_string(),
            object_body: "1,desk\n2,lamp\n",
            data_rows: 2,
        },
        Flow {
            case: "a bare name under IF NOT EXISTS with a comment, and a counting query",
            database_sql: "CREATE DATABASE IF NOT EXISTS dialect_plain COMMENT 'the shop'"
                .to_string(),
            database: "dialect_plain",
            table_sql: format!(
                "CREATE EXTERNAL TABLE IF NOT EXISTS dialect_plain.items (id int, name \
                 string) ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' STORED AS TEXTFILE \
                 LOCATION 's3://{DATA_BUCKET}/dialect_plain/items/'"
            ),
            select_sql: "SELECT COUNT(*) AS n FROM dialect_plain.items".to_string(),
            object_key: "dialect_plain/items/part-0.csv".to_string(),
            object_body: "1,desk\n2,lamp\n3,chair\n",
            data_rows: 1,
        },
        Flow {
            case: "the Presto SCHEMA spelling with a location, and a filtered query",
            database_sql: format!(
                "CREATE SCHEMA dialect_schema LOCATION 's3://{DATA_BUCKET}/dialect_schema/'"
            ),
            database: "dialect_schema",
            table_sql: format!(
                "CREATE EXTERNAL TABLE dialect_schema.items (id int, name string) ROW FORMAT \
                 DELIMITED FIELDS TERMINATED BY '|' LOCATION \
                 's3://{DATA_BUCKET}/dialect_schema/items/'"
            ),
            select_sql: "SELECT name FROM dialect_schema.items WHERE id = 2".to_string(),
            object_key: "dialect_schema/items/part-0.psv".to_string(),
            object_body: "1|desk\n2|lamp\n",
            data_rows: 1,
        },
        Flow {
            case: "a mixed-case name the catalog folds, and a query of every column",
            database_sql: "CREATE DATABASE Dialect_Folded".to_string(),
            database: "dialect_folded",
            table_sql: format!(
                "CREATE EXTERNAL TABLE dialect_folded.items (id int, name string) ROW FORMAT \
                 DELIMITED FIELDS TERMINATED BY ',' LOCATION \
                 's3://{DATA_BUCKET}/dialect_folded/items/'"
            ),
            select_sql: "SELECT * FROM dialect_folded.items".to_string(),
            object_key: "dialect_folded/items/part-0.csv".to_string(),
            object_body: "7,crate\n",
            data_rows: 1,
        },
    ]
}

/// The documented client flow — define a database, define a table in it, query that table —
/// settles statement by statement in `SUCCEEDED`, each execution naming the kind of statement
/// it carried and reporting neither of the members a failure populates; and the rows the query
/// answers are the ones the table's objects hold, so the catalog object the definition created
/// is one the query really read.
#[tokio::test]
async fn the_documented_definition_then_query_flow_settles_succeeded_statement_by_statement() {
    let emulator = Emulator::new();
    emulator.create_work_group().await;

    let flows = flows();
    for flow in &flows {
        emulator
            .put_object(flow.case, &flow.object_key, flow.object_body)
            .await;
    }

    // Each flow's statements depend on the one before it, so they are submitted in order and
    // each is polled to its terminal state before the next is submitted.
    let mut executions: Vec<Value> = Vec::new();
    for flow in &flows {
        for (sql, statement_type, substatement_type) in [
            (
                &flow.database_sql,
                STATEMENT_TYPE_DDL,
                SUBSTATEMENT_CREATE_DATABASE,
            ),
            (
                &flow.table_sql,
                STATEMENT_TYPE_DDL,
                SUBSTATEMENT_CREATE_TABLE,
            ),
            (&flow.select_sql, STATEMENT_TYPE_DML, SUBSTATEMENT_SELECT),
        ] {
            let id = emulator.start(flow.case, flow.database, sql).await;
            let execution = emulator.settled(flow.case, &id).await;
            assert_succeeded(flow.case, &execution, statement_type, substatement_type);
            assert_eq!(
                execution["Query"].as_str(),
                Some(sql.as_str()),
                "case {:?}: the execution reports the statement submitted, got {execution}",
                flow.case
            );
            executions.push(execution);
        }

        // The query read the table the definition created: its result set carries the row of
        // column labels Athena puts in front of a DML statement's rows, and then one row per
        // record the objects under the table's location hold.
        let id = executions
            .last()
            .and_then(|execution| execution["QueryExecutionId"].as_str())
            .unwrap_or_default()
            .to_string();
        let rows = emulator.result_rows(flow.case, &id).await;
        assert_eq!(
            rows.len(),
            flow.data_rows + 1,
            "case {:?}: expected {} data row(s) under the labels, got {rows:?}",
            flow.case,
            flow.data_rows
        );
    }

    // Not one of the statements above was answered in another engine's words, on either of
    // the two fields a failure would have stated.
    for execution in &executions {
        for member in ["StateChangeReason", "AthenaError"] {
            assert!(
                execution["Status"][member].is_null(),
                "a succeeded statement reports no {member}, got {execution}"
            );
        }
    }

    // The workgroup has CloudWatch metrics enabled, so every one of those terminal queries is
    // metered: the series carry one observation per query, and what they carry is what the
    // SAME executions report in their own statistics.
    let queries = executions.len() as f64;
    let engine_time = emulator
        .await_observations(ENGINE_EXECUTION_TIME_METRIC, queries)
        .await;
    assert_eq!(
        sample_count(&engine_time),
        queries,
        "every query that ran in a metered workgroup publishes one observation of \
         {ENGINE_EXECUTION_TIME_METRIC}, got {engine_time:?}"
    );

    let reported_bytes: Vec<f64> = executions
        .iter()
        .filter_map(|execution| execution["Statistics"]["DataScannedInBytes"].as_f64())
        .collect();
    let expected_observations = reported_bytes.len() as f64;
    let expected_bytes: f64 = reported_bytes.iter().sum();
    let processed = emulator
        .await_observations(PROCESSED_BYTES_METRIC, expected_observations)
        .await;
    assert_eq!(
        (sample_count(&processed), sum(&processed)),
        (expected_observations, expected_bytes),
        "the {PROCESSED_BYTES_METRIC} series must account for the bytes the executions \
         themselves report, got {processed:?}"
    );
}

/// Text Athena's grammar cannot read as one of its statements is refused SYNCHRONOUSLY, in
/// the words of ATHENA's own parser: the refusal states where that grammar stopped, and
/// carries no diagnostic of the engine that computes a query's rows.
///
/// The rows differ along the FORM of the text the grammar stopped in — which is the other
/// channel the dialect decides, beside the terminal state of a statement it did read.
#[tokio::test]
async fn text_the_athena_grammar_cannot_read_is_refused_in_athenas_own_parser_words() {
    let emulator = Emulator::new();
    emulator.create_work_group().await;

    // The rows differ along what the text is: a misspelled query word, a definition of a
    // database whose name the catalog dialect does not read, a definition of a table with the
    // same kind of name, and a statement of another engine's dialect written over a host file.
    let cases: &[(&str, &str)] = &[
        ("a misspelled query word", "SELEKT 1"),
        (
            "a definition whose database name the catalog dialect does not read",
            "CREATE DATABASE dialect-bad-name",
        ),
        (
            "a definition whose table name the catalog dialect does not read",
            "CREATE EXTERNAL TABLE dialect_held.bad-name (id int)",
        ),
        (
            "a statement of another engine's dialect, written over a host file",
            "VACUUM INTO 'host-copy.db'",
        ),
    ];

    for (case, sql) in cases {
        let (status, refused) = emulator.submit("default", sql).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {sql} must be refused at submit, got {status} with {refused}"
        );
        assert_eq!(
            refused["QueryExecutionId"].as_str(),
            None,
            "case {case:?}: a refused submission mints no execution id, got {refused}"
        );
        let message = refused["Message"]
            .as_str()
            .or_else(|| refused["message"].as_str())
            .unwrap_or_default();
        assert!(
            message.starts_with("line "),
            "case {case:?}: the refusal states where Athena's grammar stopped, got {refused}"
        );
        assert_no_engine_diagnostic(case, "the refusal message", message);
    }
}

/// A statement Athena's grammar DID read but the catalog could not perform settles `FAILED`
/// with Athena's own account of the condition: a state change reason the performing engine
/// wrote, an error message of the user category, and no diagnostic of the engine that computes
/// a query's rows in either of them.
#[tokio::test]
async fn a_statement_the_catalog_refuses_fails_in_athenas_own_words() {
    let emulator = Emulator::new();
    emulator.create_work_group().await;

    // The setup the refusals are stated against: one database the catalog holds.
    let defined = emulator
        .start("setup", "default", "CREATE DATABASE dialect_held")
        .await;
    let execution = emulator.settled("setup", &defined).await;
    assert_succeeded(
        "setup",
        &execution,
        STATEMENT_TYPE_DDL,
        SUBSTATEMENT_CREATE_DATABASE,
    );

    // The rows differ along the condition that failed: a definition of a database the catalog
    // already holds, a drop of one it does not hold, a definition in a database it does not
    // hold, and a query of a table it does not hold.
    let cases: &[(&str, &str, &str)] = &[
        (
            "a definition of a database the catalog already holds",
            "default",
            "CREATE DATABASE dialect_held",
        ),
        (
            "a drop of a database the catalog does not hold",
            "default",
            "DROP DATABASE dialect_absent",
        ),
        (
            "a definition in a database the catalog does not hold",
            "default",
            "CREATE EXTERNAL TABLE dialect_absent.items (id int) ROW FORMAT DELIMITED FIELDS \
             TERMINATED BY ',' LOCATION 's3://athena-dialect-data/absent/'",
        ),
        (
            "a query of a table the catalog does not hold",
            "dialect_held",
            "SELECT id FROM dialect_held.not_defined",
        ),
    ];

    let mut started: Vec<(&str, &str, String)> = Vec::new();
    for (case, database, sql) in cases {
        let id = emulator.start(case, database, sql).await;
        started.push((case, sql, id));
    }
    for (case, sql, id) in started {
        let execution = emulator.settled(case, &id).await;
        let status = &execution["Status"];
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: {sql} must fail, got {execution}"
        );
        let reason = status["StateChangeReason"].as_str().unwrap_or_default();
        let error_message = status["AthenaError"]["ErrorMessage"]
            .as_str()
            .unwrap_or_default();
        assert!(
            !reason.is_empty() && !error_message.is_empty(),
            "case {case:?}: a failed query states both of its failure fields, got {execution}"
        );
        assert_eq!(
            status["AthenaError"]["ErrorCategory"].as_i64(),
            Some(ERROR_CATEGORY_USER),
            "case {case:?}: a failure the submitted statement caused is of the user category, \
             got {execution}"
        );
        assert_no_engine_diagnostic(case, "the state change reason", reason);
        assert_no_engine_diagnostic(case, "the error message", error_message);
    }
}
