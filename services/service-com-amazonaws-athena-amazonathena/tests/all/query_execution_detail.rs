// SPDX-License-Identifier: CC0-1.0

//! What a READ of one query execution reports beyond the execution's modelled members:
//!
//! * every `GetQueryExecution` 200 carries, beside the modelled `QueryExecution`, the
//!   FLATTENED view of the same execution (`QueryExecutionDetail`) — one object whose id,
//!   statement, context, results location, workgroup, status and counters are each the value
//!   the modelled view reports, under the name the flattened form gives it, with the rows the
//!   run processed beside them. So the two views of one execution can never disagree,
//!   whatever the statement was and whichever state the execution is in;
//! * an execution whose workgroup keeps its results in Athena-owned storage reports how long
//!   that storage keeps them (`ManagedQueryResultsConfiguration.TimeToLive`), which is the
//!   service's own retention and never a value from the request; an execution that writes its
//!   results to an output location reports no managed-results configuration at all;
//! * a failed execution carries the error TYPE Athena publishes for its failure class beside
//!   the category, so a client branches on the classification rather than on the message —
//!   including the class of a statement whose `QueryExecutionContext` names a data catalog
//!   this account has not registered;
//! * `GetQueryRuntimeStatistics` reports the RUNTIME statistics of that same stored
//!   execution: the timeline spans the execution's own `Statistics` report, member for
//!   member, which add up to the whole the query took, and — once its run has reported —
//!   the rows and bytes it read and returned and the stage it ran as. An id no execution
//!   was minted under is refused rather than answered.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the kind of
//! statement, how many rows it reads and returns, whether it succeeded or failed, the
//! workgroup it ran in, how that workgroup holds its results, and which failure class the
//! statement fell into.
//!
//! Every call goes over the wire the production front door uses: Athena and Glue through
//! `POST /` with `X-Amz-Target`, and Amazon S3 through its own REST data plane, all on one
//! `MultiServiceRouter` over ONE shared engine.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_QUEUED, QUERY_STATE_RUNNING, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{error_code, message};

/// The name Athena reaches the account's AWS Glue Data Catalog by.
const GLUE_CATALOG: &str = "AwsDataCatalog";

/// The bucket the table these suites read keeps its data in.
const BUCKET: &str = "athena-detail-data";

/// Where a query's results are written, which every submission has to name unless its
/// workgroup holds them in Athena-owned storage.
const OUTPUT_LOCATION: &str = "s3://athena-detail-data/results/";

/// The database the catalog statements in these suites run in.
const DATABASE: &str = "shop";

/// The character Hive separates fields with when a table's SerDe names none.
const UNIT_SEPARATOR: char = '\u{1}';

/// How many rows the catalog table these suites read holds, which is what a read of it
/// processes as INPUT rows.
const TABLE_ROWS: i64 = 3;

/// How long a poll for a terminal execution keeps looking before it gives up: generous, so
/// nothing here depends on how fast the host runs. The completion is owed a fixed span after
/// the submit and the loop waits for the STATE, not for a span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long the poll waits between two reads of the same execution.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The retention a read of an execution whose results Athena holds in its own storage must
/// report, as the wire carries it. Written out rather than read from the crate's own
/// constants, so the expectation is the form AWS reports and not whatever the service holds.
fn managed_results_retention() -> Value {
    json!({ "Unit": "DAYS", "Value": 1 })
}

/// The `AthenaError$ErrorCategory` of a failure the caller's own input caused: the documented
/// category list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` Athena reports for a failure the data-catalog layer raises — a
/// catalog name nothing registered, and a task its metastore refused.
const ERROR_TYPE_CATALOG_LAYER: i64 = 1006;

/// The `AthenaError$ErrorType` Athena reports for the NOT-FOUND class: a statement naming a
/// table or a database the data catalog does not hold.
const ERROR_TYPE_ENTITY_NOT_FOUND: i64 = 1301;

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the router claims a request by. Neither the key id nor the signature is a
/// credential: this harness turns request authentication off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena, Glue and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service: the catalog Glue holds is the catalog Athena resolves against, and the objects S3
/// holds are the data Athena reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One awsJson1_1 call to `service`, and its HTTP status and parsed body.
async fn call(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.us-east-1.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Athena call, as a client makes it.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(router, "athena", "AmazonAthena", operation, body).await
}

/// One Athena call that must answer 200.
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = athena(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answer:?}"
    );
    answer
}

/// Create the bucket the table's data and the query results live in.
async fn create_bucket(router: &MultiServiceRouter) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}"))
        .header("host", "s3.us-east-1.amazonaws.com")
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "setup: creating s3://{BUCKET} must succeed, got {}",
        response.status()
    );
}

/// Write one object, which is what the table's location then holds.
async fn put_object(router: &MultiServiceRouter, key: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}/{key}"))
        .header("host", "s3.us-east-1.amazonaws.com")
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "setup: writing s3://{BUCKET}/{key} must succeed, got {}",
        response.status()
    );
}

/// The database and the table with [`TABLE_ROWS`] rows every read in these suites resolves.
async fn create_catalog_table(router: &MultiServiceRouter) {
    call(
        router,
        "glue",
        "AWSGlue",
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    call(
        router,
        "glue",
        "AWSGlue",
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": "events",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [
                        { "Name": "id", "Type": "bigint" },
                        { "Name": "name", "Type": "string" },
                    ],
                    "Location": format!("s3://{BUCKET}/events"),
                },
            },
        }),
    )
    .await;
    put_object(
        router,
        "events/rows.txt",
        &format!("1{UNIT_SEPARATOR}desk\n2{UNIT_SEPARATOR}lamp\n3{UNIT_SEPARATOR}rug\n"),
    )
    .await;
}

/// Start one statement and return the `QueryExecutionId` it minted.
async fn start(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
    catalog: &str,
    work_group: Option<&str>,
) -> String {
    let mut body = json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": DATABASE, "Catalog": catalog },
    });
    match work_group {
        // A workgroup that holds its results in Athena-owned storage needs no location of
        // its own, so a submission through one names none.
        Some(name) => body["WorkGroup"] = json!(name),
        None => body["ResultConfiguration"] = json!({ "OutputLocation": OUTPUT_LOCATION }),
    }
    let answer = athena_ok(router, case, "StartQueryExecution", &body).await;
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {answer:?}"))
        .to_string()
}

/// The whole `GetQueryExecution` body for `id`.
async fn read(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    athena_ok(
        router,
        case,
        "GetQueryExecution",
        &json!({ "QueryExecutionId": id }),
    )
    .await
}

/// The `GetQueryExecution` body once `id` has reached a terminal state.
///
/// Waits for the STATE rather than for a span: it looks again until the execution is no
/// longer queued or running, or until a generous deadline passes.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let body = read(router, case, id).await;
        let state = body["QueryExecution"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state != QUERY_STATE_QUEUED && state != QUERY_STATE_RUNNING {
            return body;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution {id} never reached a terminal state, last {state:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that the flattened view in `body` reports the same execution, member for member, as
/// the modelled `QueryExecution` beside it.
fn assert_detail_mirrors_execution(case: &str, body: &Value) {
    let execution = &body["QueryExecution"];
    let detail = &body["QueryExecutionDetail"];
    assert!(
        detail.is_object(),
        "case {case:?}: the response must carry a QueryExecutionDetail object, got {body:?}"
    );

    for (member, expected) in [
        ("QueryExecutionId", &execution["QueryExecutionId"]),
        ("Query", &execution["Query"]),
        ("QueryExecutionContext", &execution["QueryExecutionContext"]),
        ("ResultConfiguration", &execution["ResultConfiguration"]),
        ("WorkGroup", &execution["WorkGroup"]),
    ] {
        assert_eq!(
            &detail[member], expected,
            "case {case:?}: QueryExecutionDetail.{member} must mirror the execution, got \
             {detail:?}"
        );
    }

    assert_eq!(
        detail["OutputLocation"], execution["ResultConfiguration"]["OutputLocation"],
        "case {case:?}: QueryExecutionDetail.OutputLocation must be where the results went, \
         got {detail:?}"
    );

    for member in [
        "State",
        "StateChangeReason",
        "SubmissionDateTime",
        "CompletionDateTime",
    ] {
        assert_eq!(
            detail["Status"][member], execution["Status"][member],
            "case {case:?}: QueryExecutionDetail.Status.{member} must mirror the execution, \
             got {detail:?}"
        );
    }

    let statistics = &execution["Statistics"];
    let stats = &detail["Stats"];
    assert_eq!(
        stats["EngineExecutionTimeInMillis"], statistics["EngineExecutionTimeInMillis"],
        "case {case:?}: the flattened engine span must mirror the execution's, got {stats:?}"
    );
    assert_eq!(
        stats["ProcessedBytes"], statistics["DataScannedInBytes"],
        "case {case:?}: Stats.ProcessedBytes must be the bytes the run read, got {stats:?}"
    );
    assert_eq!(
        stats["ProcessedInputBytes"].as_i64(),
        Some(0),
        "case {case:?}: a run handed no input beside the data it read itself must report no \
         input bytes, got {stats:?}"
    );
    assert_eq!(
        stats["ResultReuseInformation"], statistics["ResultReuseInformation"],
        "case {case:?}: the flattened reuse report must mirror the execution's, got {stats:?}"
    );
}

/// One row of the flattened-view table: a statement, and the rows a read of its execution
/// must report it having processed.
struct DetailCase {
    /// What this row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// Whether the execution must settle SUCCEEDED or FAILED.
    state: &'static str,
    /// The rows the run READ from the objects its statement named, which is what both of
    /// the flattened view's row counters report.
    rows_read: i64,
}

/// The rows: a statement differs from the next along what the catalog does with it (a DDL
/// task, a read of a table, a projection of literals), how many rows it reads, how many it
/// returns, and whether it runs at all.
fn detail_cases() -> Vec<DetailCase> {
    vec![
        DetailCase {
            case: "a catalog DDL statement",
            sql: "CREATE DATABASE `detail_db`",
            state: QUERY_STATE_SUCCEEDED,
            rows_read: 0,
        },
        DetailCase {
            case: "a projection of one literal row",
            sql: "SELECT 1 AS one",
            state: QUERY_STATE_SUCCEEDED,
            rows_read: 0,
        },
        DetailCase {
            case: "a projection of three literal rows",
            sql: "SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3",
            state: QUERY_STATE_SUCCEEDED,
            rows_read: 0,
        },
        DetailCase {
            case: "a read of every row of a catalog table",
            sql: "SELECT id, name FROM shop.events",
            state: QUERY_STATE_SUCCEEDED,
            rows_read: TABLE_ROWS,
        },
        DetailCase {
            // The rows it RETURNS are one; the rows it PROCESSES are every row of the
            // table, which is what both counters report.
            case: "an aggregate over a catalog table",
            sql: "SELECT count(*) AS rows_held FROM shop.events",
            state: QUERY_STATE_SUCCEEDED,
            rows_read: TABLE_ROWS,
        },
        DetailCase {
            case: "a read of one row of a catalog table",
            sql: "SELECT id FROM shop.events WHERE id = 1",
            state: QUERY_STATE_SUCCEEDED,
            rows_read: TABLE_ROWS,
        },
        DetailCase {
            case: "a read of a table the catalog does not hold",
            sql: "SELECT id FROM shop.not_a_table",
            state: QUERY_STATE_FAILED,
            rows_read: 0,
        },
    ]
}

/// A read of a query execution reports the FLATTENED view of it beside the modelled one, and
/// every member of that view is the value the modelled view reports — for a statement the
/// catalog performs, for a projection the engine answers, for a read of a catalog table and
/// for a statement that failed. Its row counters are what the run actually processed: the rows
/// it read from the objects its statement named, whatever it then projected or aggregated
/// them into.
#[tokio::test]
async fn the_flattened_view_mirrors_the_stored_execution() {
    let router = emulator();
    create_bucket(&router).await;
    create_catalog_table(&router).await;

    // Started together, so the suite waits one completion span rather than one per row.
    let mut started: Vec<(&DetailCase, String)> = Vec::new();
    let cases = detail_cases();
    for case in &cases {
        let id = start(&router, case.case, case.sql, GLUE_CATALOG, None).await;
        started.push((case, id));
    }

    for (case, id) in &started {
        let body = settled(&router, case.case, id).await;
        let execution = &body["QueryExecution"];
        assert_eq!(
            execution["Status"]["State"], case.state,
            "case {:?}: the execution must settle {}, got {execution:?}",
            case.case, case.state
        );
        assert_eq!(
            execution["QueryExecutionId"], *id,
            "case {:?}: the execution must report the id it was started under, got \
             {execution:?}",
            case.case
        );
        assert_detail_mirrors_execution(case.case, &body);

        let stats = &body["QueryExecutionDetail"]["Stats"];
        for member in ["ProcessedInputRows", "ProcessedRows"] {
            assert_eq!(
                stats[member].as_i64(),
                Some(case.rows_read),
                "case {:?}: expected Stats.{member} to be the {} rows the run processed, got \
                 {stats:?}",
                case.case,
                case.rows_read
            );
        }
        // The bytes counter is a MEASURE, not a constant: a statement that read a table's
        // objects reports what they held, and one that read none reports nothing read.
        let processed_bytes = stats["ProcessedBytes"].as_i64().unwrap_or_default();
        if case.rows_read > 0 {
            assert!(
                processed_bytes > 0,
                "case {:?}: a statement that read {} rows of stored data must report the \
                 bytes it read, got {stats:?}",
                case.case,
                case.rows_read
            );
        } else {
            assert_eq!(
                processed_bytes, 0,
                "case {:?}: a statement that read no stored data must report no bytes read, \
                 got {stats:?}",
                case.case
            );
        }
        assert_eq!(
            body["QueryExecutionDetail"]["Query"], case.sql,
            "case {:?}: the flattened view must carry the statement submitted, got {body:?}",
            case.case
        );
    }
}

/// The flattened view is reported for an execution that has NOT reached a terminal state
/// either — a poller that reads it rather than the modelled view sees the state from the
/// first read on — and for one that ran in a workgroup of its own.
#[tokio::test]
async fn the_flattened_view_is_reported_before_the_query_is_terminal() {
    let router = emulator();
    create_bucket(&router).await;
    athena_ok(
        &router,
        "a workgroup with a results location of its own",
        "CreateWorkGroup",
        &json!({
            "Name": "detail_wg",
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            },
        }),
    )
    .await;

    let id = start(
        &router,
        "a query in a workgroup of its own",
        "SELECT 1 AS one",
        GLUE_CATALOG,
        Some("detail_wg"),
    )
    .await;

    // Read straight back: whatever state the execution is in, the flattened view is there
    // and agrees with the modelled one.
    let immediate = read(&router, "the first read of a fresh execution", &id).await;
    assert_detail_mirrors_execution("the first read of a fresh execution", &immediate);
    let state = immediate["QueryExecutionDetail"]["Status"]["State"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        [
            QUERY_STATE_QUEUED,
            QUERY_STATE_RUNNING,
            QUERY_STATE_SUCCEEDED
        ]
        .contains(&state.as_str()),
        "the first read must report a state the execution can be in, got {state:?}"
    );

    let settled = settled(&router, "the terminal read", &id).await;
    assert_detail_mirrors_execution("the terminal read", &settled);
    assert_eq!(
        settled["QueryExecutionDetail"]["WorkGroup"], "detail_wg",
        "the flattened view must name the workgroup the query ran in, got {settled:?}"
    );
    assert_eq!(
        settled["QueryExecutionDetail"]["Status"]["State"], QUERY_STATE_SUCCEEDED,
        "the query must settle SUCCEEDED, got {settled:?}"
    );
}

/// One row of the managed-results table: how a workgroup holds its query results, and the
/// retention a read of an execution in it must report.
struct ManagedCase {
    /// What this row is about.
    case: &'static str,
    /// The workgroup's name.
    name: &'static str,
    /// The `Configuration` the workgroup is created with.
    configuration: Value,
    /// Whether an execution in it must report a managed-results configuration at all.
    managed: bool,
}

/// The rows: a workgroup differs from the next along whether it holds its results in
/// Athena-owned storage, whether it encrypts them there, whether the request tried to set the
/// retention itself, and whether it writes them to a location instead.
fn managed_cases() -> Vec<ManagedCase> {
    vec![
        ManagedCase {
            case: "results in Athena-owned storage",
            name: "managed_plain",
            configuration: json!({ "ManagedQueryResultsConfiguration": { "Enabled": true } }),
            managed: true,
        },
        ManagedCase {
            case: "results in Athena-owned storage, encrypted",
            name: "managed_encrypted",
            configuration: json!({
                "ManagedQueryResultsConfiguration": {
                    "Enabled": true,
                    "EncryptionConfiguration": {
                        "KmsKey": "arn:aws:kms:us-east-1:123456789012:key/managed-results",
                    },
                },
            }),
            managed: true,
        },
        ManagedCase {
            case: "a request that tries to set the retention itself",
            name: "managed_requested_ttl",
            configuration: json!({
                "ManagedQueryResultsConfiguration": {
                    "Enabled": true,
                    "TimeToLive": { "Unit": "HOURS", "Value": 99 },
                },
            }),
            managed: true,
        },
        ManagedCase {
            case: "results at an output location",
            name: "managed_absent",
            configuration: json!({
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
            managed: false,
        },
        ManagedCase {
            case: "Athena-owned storage turned off",
            name: "managed_off",
            configuration: json!({
                "ManagedQueryResultsConfiguration": { "Enabled": false },
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
            managed: false,
        },
    ]
}

/// An execution whose workgroup keeps its results in Athena-owned storage reports how long
/// that storage keeps them, and the retention is the SERVICE's: it is the same for every such
/// execution, whether or not those results are encrypted, and a request that carries a
/// retention of its own does not change it. An execution that writes its results to an output
/// location reports no managed-results configuration, and so no retention either.
#[tokio::test]
async fn managed_query_results_report_the_retention_athena_applies() {
    let router = emulator();
    create_bucket(&router).await;

    let cases = managed_cases();
    let mut started: Vec<(&ManagedCase, String)> = Vec::new();
    for case in &cases {
        athena_ok(
            &router,
            case.case,
            "CreateWorkGroup",
            &json!({ "Name": case.name, "Configuration": case.configuration }),
        )
        .await;
        let id = start(
            &router,
            case.case,
            "SELECT 1 AS one",
            GLUE_CATALOG,
            Some(case.name),
        )
        .await;
        started.push((case, id));
    }

    for (case, id) in &started {
        let body = settled(&router, case.case, id).await;
        let managed = &body["QueryExecution"]["ManagedQueryResultsConfiguration"];
        if !case.managed {
            assert!(
                managed.is_null(),
                "case {:?}: an execution whose results are not held by Athena must report no \
                 managed-results configuration, got {managed:?}",
                case.case
            );
            continue;
        }
        assert_eq!(
            managed["Enabled"], true,
            "case {:?}: the managed-results configuration the query ran under must be \
             reported, got {managed:?}",
            case.case
        );
        assert_eq!(
            managed["TimeToLive"],
            managed_results_retention(),
            "case {:?}: expected the retention Athena applies to managed results, got \
             {managed:?}",
            case.case
        );
    }
}

/// One row of the failure-classification table: a statement that fails, and the
/// classification its execution must carry.
struct FailureCase {
    /// What this row is about.
    case: &'static str,
    /// The data catalog the submission's context names.
    catalog: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `AthenaError$ErrorCategory` the execution must report.
    category: i64,
    /// The `AthenaError$ErrorType` the execution must report.
    error_type: i64,
    /// The message the execution must report, where the class words one with a variable
    /// part; `None` where the row asserts the classification only.
    message: Option<String>,
}

/// The rows: a failure differs from the next along which class it belongs to — a catalog name
/// nothing registered, a table the catalog does not hold, a database it does not hold, a task
/// its metastore refused — and, for the catalog class, along the catalog name itself.
fn failure_cases() -> Vec<FailureCase> {
    vec![
        FailureCase {
            case: "a catalog this account has not registered",
            catalog: "no_such_catalog",
            sql: "SELECT id FROM shop.events",
            category: ERROR_CATEGORY_USER,
            error_type: ERROR_TYPE_CATALOG_LAYER,
            // "SELECT id FROM " is 15 characters, so the relation starts at column 16.
            message: Some(
                "CATALOG_NOT_FOUND: line 1:16: Catalog 'no_such_catalog' does not exist"
                    .to_string(),
            ),
        },
        FailureCase {
            case: "a catalog named in upper case that nothing registered",
            catalog: "MISSING_CATALOG",
            sql: "SELECT count(*) FROM shop.events",
            category: ERROR_CATEGORY_USER,
            error_type: ERROR_TYPE_CATALOG_LAYER,
            // "SELECT count(*) FROM " is 21 characters; the catalog is reported as the
            // catalog layer folds a name.
            message: Some(
                "CATALOG_NOT_FOUND: line 1:22: Catalog 'missing_catalog' does not exist"
                    .to_string(),
            ),
        },
        FailureCase {
            case: "a table the data catalog does not hold",
            catalog: GLUE_CATALOG,
            sql: "SELECT id FROM shop.not_a_table",
            category: ERROR_CATEGORY_USER,
            error_type: ERROR_TYPE_ENTITY_NOT_FOUND,
            message: None,
        },
        FailureCase {
            case: "a database the data catalog does not hold",
            catalog: GLUE_CATALOG,
            sql: "SELECT id FROM not_a_database.events",
            category: ERROR_CATEGORY_USER,
            error_type: ERROR_TYPE_ENTITY_NOT_FOUND,
            message: None,
        },
        FailureCase {
            case: "a task the catalog's own metastore refused",
            catalog: GLUE_CATALOG,
            sql: "CREATE DATABASE `shop`",
            category: ERROR_CATEGORY_USER,
            error_type: ERROR_TYPE_CATALOG_LAYER,
            message: None,
        },
    ]
}

/// A failed execution carries the error TYPE Athena publishes for its failure class beside
/// the category, so a client can tell one class from another without reading the message —
/// for a catalog name nothing registered as much as for an entity the catalog does not hold
/// and a task its metastore refused. The message of the catalog class is Athena's own: fixed
/// text around the catalog the request named and the line and column where the statement
/// addressed it.
#[tokio::test]
async fn a_failed_execution_reports_the_error_type_of_its_class() {
    let router = emulator();
    create_bucket(&router).await;
    create_catalog_table(&router).await;

    let cases = failure_cases();
    let mut started: Vec<(&FailureCase, String)> = Vec::new();
    for case in &cases {
        let id = start(&router, case.case, case.sql, case.catalog, None).await;
        started.push((case, id));
    }

    for (case, id) in &started {
        let body = settled(&router, case.case, id).await;
        let status = &body["QueryExecution"]["Status"];
        assert_eq!(
            status["State"], QUERY_STATE_FAILED,
            "case {:?}: the execution must settle FAILED, got {status:?}",
            case.case
        );
        let error = &status["AthenaError"];
        assert_eq!(
            error["ErrorCategory"].as_i64(),
            Some(case.category),
            "case {:?}: expected error category {}, got {error:?}",
            case.case,
            case.category
        );
        assert_eq!(
            error["ErrorType"].as_i64(),
            Some(case.error_type),
            "case {:?}: expected error type {}, got {error:?}",
            case.case,
            case.error_type
        );
        assert_eq!(
            error["Retryable"], false,
            "case {:?}: a failed execution is terminal with no automatic retry, got {error:?}",
            case.case
        );
        if let Some(message) = &case.message {
            assert_eq!(
                error["ErrorMessage"].as_str(),
                Some(message.as_str()),
                "case {:?}: expected the class's own message, got {error:?}",
                case.case
            );
            assert_eq!(
                status["StateChangeReason"].as_str(),
                Some(message.as_str()),
                "case {:?}: the state change reason must say what the failure said, got \
                 {status:?}",
                case.case
            );
        }
        // The flattened view reports the same failure, so a client reading it is not left
        // without the state or the reason.
        assert_detail_mirrors_execution(case.case, &body);
    }
}

/// The phases of a run a read of its runtime statistics reports, in the order a query passes
/// through them, each under the name the execution's own `Statistics` reports it under as
/// well — the member list the modelled `QueryRuntimeStatisticsTimeline` and
/// `QueryExecutionStatistics` share.
const TIMELINE_PHASES: [&str; 5] = [
    "QueryQueueTimeInMillis",
    "ServicePreProcessingTimeInMillis",
    "QueryPlanningTimeInMillis",
    "EngineExecutionTimeInMillis",
    "ServiceProcessingTimeInMillis",
];

/// The span those phases add up to: the whole the query took.
const TIMELINE_TOTAL: &str = "TotalExecutionTimeInMillis";

/// The `StageId` of the stage whose output is the query's own, which is the first stage of
/// a run this engine performs as one stage.
const OUTPUT_STAGE_ID: i64 = 0;

/// The stage state of a run whose work is over.
const STAGE_STATE_FINISHED: &str = "FINISHED";

/// The error code a read of an execution this account holds none of is refused with, and the
/// shape that refusal travels as.
const QUERY_EXECUTION_NOT_FOUND: &str = "QUERY_EXECUTION_NOT_FOUND";
/// See [`QUERY_EXECUTION_NOT_FOUND`].
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The `QueryRuntimeStatistics` a read of `id` reports.
async fn runtime_statistics(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let body = athena_ok(
        router,
        case,
        "GetQueryRuntimeStatistics",
        &json!({ "QueryExecutionId": id }),
    )
    .await;
    body["QueryRuntimeStatistics"].clone()
}

/// A byte counter is a MEASURE of the rows it belongs to: a run that read or returned rows
/// reports the bytes those rows held, and one that handled none reports no bytes.
fn assert_bytes_follow_rows(case: &str, member: &str, counters: &Value, rows: i64) {
    let bytes = counters[member]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} counter, got {counters:?}"));
    if rows > 0 {
        assert!(
            bytes > 0,
            "case {case:?}: {rows} rows must carry the bytes they held under {member}, got \
             {bytes}"
        );
    } else {
        assert_eq!(
            bytes, 0,
            "case {case:?}: a run that handled no rows must report no bytes under {member}, got \
             {bytes}"
        );
    }
}

/// One row of the runtime-statistics table: a statement, and what the run of it measured.
struct RuntimeCase {
    /// What this row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// Whether the execution must settle SUCCEEDED or FAILED.
    state: &'static str,
    /// Whether the run reported counters at all: a run that produced results did, and one
    /// that failed measured no rows to report.
    counted: bool,
    /// The rows the run READ out of the objects its statement named.
    input_rows: i64,
    /// The rows it RETURNED.
    output_rows: i64,
}

/// The rows: a statement differs from the next along how many rows it reads (none, every row
/// of a catalog table), how many it returns (none, one, several), whether what it returns is
/// rows at all (a catalog task returns the engine's text output), and whether it runs.
fn runtime_cases() -> Vec<RuntimeCase> {
    vec![
        RuntimeCase {
            case: "a projection of one literal row",
            sql: "SELECT 1 AS one",
            state: QUERY_STATE_SUCCEEDED,
            counted: true,
            input_rows: 0,
            output_rows: 1,
        },
        RuntimeCase {
            case: "a projection of three literal rows",
            sql: "SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3",
            state: QUERY_STATE_SUCCEEDED,
            counted: true,
            input_rows: 0,
            output_rows: 3,
        },
        RuntimeCase {
            case: "a read of every row of a catalog table",
            sql: "SELECT id, name FROM shop.events",
            state: QUERY_STATE_SUCCEEDED,
            counted: true,
            input_rows: TABLE_ROWS,
            output_rows: TABLE_ROWS,
        },
        RuntimeCase {
            // It READS every row of the table and RETURNS one, so the two counters differ.
            case: "an aggregate over a catalog table",
            sql: "SELECT count(*) AS rows_held FROM shop.events",
            state: QUERY_STATE_SUCCEEDED,
            counted: true,
            input_rows: TABLE_ROWS,
            output_rows: 1,
        },
        RuntimeCase {
            case: "a read of one row of a catalog table",
            sql: "SELECT id FROM shop.events WHERE id = 1",
            state: QUERY_STATE_SUCCEEDED,
            counted: true,
            input_rows: TABLE_ROWS,
            output_rows: 1,
        },
        RuntimeCase {
            // A catalog task returns no rows at all: its result is the text the task wrote.
            case: "a catalog DDL statement",
            sql: "CREATE DATABASE `statistics_db`",
            state: QUERY_STATE_SUCCEEDED,
            counted: true,
            input_rows: 0,
            output_rows: 0,
        },
        RuntimeCase {
            case: "a read of a table the catalog does not hold",
            sql: "SELECT id FROM shop.not_a_table",
            state: QUERY_STATE_FAILED,
            counted: false,
            input_rows: 0,
            output_rows: 0,
        },
    ]
}

/// A read of an execution's RUNTIME statistics reports what that run measured: the timeline
/// is the same set of spans the execution's own `Statistics` report, member for member, so
/// the two reads of one run cannot disagree; the row counters are the rows it read out of the
/// objects its statement named and the rows it returned, with the bytes each of those held;
/// and the output stage reports those same measures for the one stage the run performed,
/// timed by the engine's own span. A run that measured no rows — one whose statement failed —
/// reports the timeline and no counters. Reading twice reports the same statistics, because a
/// read derives them rather than moving the execution along.
#[tokio::test]
async fn runtime_statistics_report_what_the_run_measured() {
    let router = emulator();
    create_bucket(&router).await;
    create_catalog_table(&router).await;

    // Started together, so the suite waits one completion span rather than one per row.
    let cases = runtime_cases();
    let mut started: Vec<(&RuntimeCase, String)> = Vec::new();
    for case in &cases {
        let id = start(&router, case.case, case.sql, GLUE_CATALOG, None).await;
        started.push((case, id));
    }

    for (case, id) in &started {
        let body = settled(&router, case.case, id).await;
        let execution = &body["QueryExecution"];
        assert_eq!(
            execution["Status"]["State"], case.state,
            "case {:?}: the execution must settle {}, got {execution:?}",
            case.case, case.state
        );

        let statistics = runtime_statistics(&router, case.case, id).await;
        let again = runtime_statistics(&router, case.case, id).await;
        assert_eq!(
            again, statistics,
            "case {:?}: two reads of a finished execution must report the same statistics",
            case.case
        );

        let timeline = &statistics["Timeline"];
        let reported = &execution["Statistics"];
        let mut phases = 0;
        for span in TIMELINE_PHASES.iter().chain([&TIMELINE_TOTAL]) {
            let millis = timeline[*span].as_i64().unwrap_or_else(|| {
                panic!("case {:?}: expected Timeline.{span}, got {timeline:?}", case.case)
            });
            assert_eq!(
                Some(millis),
                reported[*span].as_i64(),
                "case {:?}: Timeline.{span} must be the span the execution reports, got \
                 {timeline:?} beside {reported:?}",
                case.case
            );
            assert!(
                millis >= 0,
                "case {:?}: Timeline.{span} must be a span, got {millis}",
                case.case
            );
            if *span != TIMELINE_TOTAL {
                phases += millis;
            }
        }
        let total = timeline[TIMELINE_TOTAL].as_i64().unwrap_or_default();
        assert_eq!(
            phases, total,
            "case {:?}: the phases the query passed through must add up to the whole it took, \
             got {timeline:?}",
            case.case
        );
        let engine = timeline["EngineExecutionTimeInMillis"].as_i64().unwrap_or_default();
        assert!(
            total >= engine,
            "case {:?}: the whole execution cannot be shorter than the engine's part of it, \
             got {total} and {engine}",
            case.case
        );

        let counters = &statistics["Rows"];
        let stage = &statistics["OutputStage"];
        if !case.counted {
            assert!(
                counters.is_null() && stage.is_null(),
                "case {:?}: a run that measured no rows must report no counters and no stage, \
                 got {statistics:?}",
                case.case
            );
            continue;
        }

        assert_eq!(
            counters["InputRows"].as_i64(),
            Some(case.input_rows),
            "case {:?}: expected Rows.InputRows to be the {} rows the run read, got \
             {counters:?}",
            case.case,
            case.input_rows
        );
        assert_eq!(
            counters["OutputRows"].as_i64(),
            Some(case.output_rows),
            "case {:?}: expected Rows.OutputRows to be the {} rows the run returned, got \
             {counters:?}",
            case.case,
            case.output_rows
        );
        assert_bytes_follow_rows(case.case, "InputBytes", counters, case.input_rows);
        assert_bytes_follow_rows(case.case, "OutputBytes", counters, case.output_rows);

        assert_eq!(
            stage["StageId"].as_i64(),
            Some(OUTPUT_STAGE_ID),
            "case {:?}: expected the output stage of a single-stage run, got {stage:?}",
            case.case
        );
        assert_eq!(
            stage["State"].as_str(),
            Some(STAGE_STATE_FINISHED),
            "case {:?}: the stage of a run that completed must report its work over, got \
             {stage:?}",
            case.case
        );
        for member in ["InputRows", "InputBytes", "OutputRows", "OutputBytes"] {
            assert_eq!(
                stage[member], counters[member],
                "case {:?}: OutputStage.{member} must be the measure the run reports, got \
                 {stage:?} beside {counters:?}",
                case.case
            );
        }
        assert_eq!(
            stage["ExecutionTime"], timeline["EngineExecutionTimeInMillis"],
            "case {:?}: the stage's execution time must be the engine's own span, got \
             {stage:?}",
            case.case
        );
        assert!(
            stage["SubStages"].is_null(),
            "case {:?}: a run performed as one stage feeds no substage, got {stage:?}",
            case.case
        );
    }
}

/// The TIMELINE is reported from the first read on, before the query is terminal; the
/// counters and the stage appear once the run has reported, so a poller reading the
/// statistics of a query that has not finished is answered rather than refused.
#[tokio::test]
async fn runtime_statistics_are_reported_before_the_query_is_terminal() {
    let router = emulator();
    create_bucket(&router).await;

    let case = "a fresh execution";
    let id = start(&router, case, "SELECT 1 AS one", GLUE_CATALOG, None).await;

    let fresh = runtime_statistics(&router, case, &id).await;
    // Read the STATE after the statistics: a query that is still queued or running now was
    // also one then, so the expectation holds whatever the host's timing is.
    let state_after = read(&router, case, &id).await["QueryExecution"]["Status"]["State"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        fresh["Timeline"].is_object(),
        "{case}: the timeline must be reported from the first read on, got {fresh:?}"
    );
    if [QUERY_STATE_QUEUED, QUERY_STATE_RUNNING].contains(&state_after.as_str()) {
        assert!(
            fresh["Rows"].is_null() && fresh["OutputStage"].is_null(),
            "{case}: a run that has not reported has no counters to report, got {fresh:?}"
        );
    }

    let settled = settled(&router, case, &id).await;
    assert_eq!(
        settled["QueryExecution"]["Status"]["State"], QUERY_STATE_SUCCEEDED,
        "{case}: the query must settle SUCCEEDED, got {settled:?}"
    );
    let terminal = runtime_statistics(&router, case, &id).await;
    assert!(
        terminal["Rows"].is_object() && terminal["OutputStage"].is_object(),
        "{case}: once the run has reported, its counters and its stage must be there, got \
         {terminal:?}"
    );
}

/// A read of the runtime statistics of an id this account holds no execution for is REFUSED,
/// with the class of refusal that says the id names nothing and the sentence Athena states
/// it with — whatever form the id takes, up to the longest the model admits.
#[tokio::test]
async fn runtime_statistics_of_an_unknown_execution_are_refused() {
    let router = emulator();

    // (case, the id the request names). The rows differ along the FORM of the id: one shaped
    // like a minted execution id, one that is not, and one of the maximum modelled length
    // (`QueryExecutionId` is 1 to 128 characters, pattern `^\S+$`).
    let unknown: Vec<(&str, String)> = vec![
        (
            "an id shaped like a minted one",
            "11111111-2222-4333-8444-555555555555".to_string(),
        ),
        ("an id of no particular shape", "not-a-query".to_string()),
        ("an id of the longest modelled length", "q".repeat(128)),
    ];

    for (case, id) in &unknown {
        let (status, body) = athena(
            &router,
            "GetQueryRuntimeStatistics",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: an id naming no execution must be refused, got {status} with \
             {body:?}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST_EXCEPTION,
            "case {case:?}: expected the refusal Athena states for a bad request, got {body:?}"
        );
        for member in ["ErrorCode", "AthenaErrorCode"] {
            assert_eq!(
                body[member].as_str(),
                Some(QUERY_EXECUTION_NOT_FOUND),
                "case {case:?}: the refusal must carry its class under {member}, got {body:?}"
            );
        }
        assert_eq!(
            message(&body),
            format!("QueryExecution {id} was not found"),
            "case {case:?}: expected the sentence Athena states that miss with, got {body:?}"
        );
    }
}
