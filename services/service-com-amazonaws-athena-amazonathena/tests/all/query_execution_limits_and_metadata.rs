// SPDX-License-Identifier: CC0-1.0

//! The LIMITS a query execution is held to, and what a read of it REPORTS about the run.
//!
//! The rules under test, each on a table of its own:
//!
//! * a workgroup's data usage cutoff (`WorkGroupConfiguration$BytesScannedCutoffPerQuery`,
//!   "the upper data usage limit (cutoff) for the amount of bytes a single query in a
//!   workgroup is allowed to scan") is ENFORCED: a query that would scan past it is aborted
//!   at the cutoff — interrupted rather than failed — and reports the cutoff as the data it
//!   scanned, while a query inside the cutoff runs to an end and reports what it really read;
//! * `ResultConfiguration$ExpectedBucketOwner` is CONSULTED: "if the `ExpectedBucketOwner`
//!   Amazon Web Services account ID does not match the actual owner of the Amazon S3 bucket,
//!   the call fails with a permissions error" — so such a query settles FAILED, carries that
//!   refusal and writes nothing where its results would have gone;
//! * every FAILED query carries the whole of Athena's classification: `AthenaError$ErrorType`
//!   ("an integer value that provides specific information about an Athena query error")
//!   beside the category, whatever class the failure belongs to;
//! * `QueryExecutionStatistics$EngineExecutionTimeInMillis` is the time the query took to
//!   execute, so a query that really ran reports more than none of it, and the total
//!   execution time is at least that;
//! * `ColumnInfo$Type` is the type of the EXPRESSION projected: an aggregate or window
//!   function whose result type is its argument's reports the type the data catalog declares
//!   for the column that argument names, not the storage class the engine held its values
//!   in;
//! * `ResultReuseConfiguration` is HONOURED: an identical query submitted inside the window
//!   it asks for is answered out of the earlier execution's result — reporting
//!   `ResultReuseInformation$ReusedPreviousResult` true, serving that execution's rows even
//!   where the data under the query has changed since, and scanning nothing.
//!
//! Every table, object and query here is created over the real wire of the service that owns
//! it — Glue's `CreateTable`, Amazon S3's REST data plane, Athena's awsJson1_1 front door —
//! against the one shared engine all three are mounted on, so every assertion is about what
//! an SDK client observes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_CANCELLED, QUERY_STATE_FAILED, QUERY_STATE_QUEUED, QUERY_STATE_RUNNING,
    QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table below keeps its data in, and where the results go.
const BUCKET: &str = "athena-limits-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-limits-data/results/";

/// The database every table below is registered in.
const DATABASE: &str = "limits_shop";

/// The `AthenaError$ErrorCategory` of a failure the caller's own input is the cause of: the
/// documented list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` of the GENERIC user-error class: a statement the caller has to
/// change, of no narrower published class.
const GENERIC_USER_ERROR_TYPE: i64 = 1000;

/// The `AthenaError$ErrorType` of the NOT-FOUND class: a statement naming a table or a
/// database the data catalog does not hold.
const ENTITY_NOT_FOUND_ERROR_TYPE: i64 = 1301;

/// The smallest data usage cutoff the model admits —
/// `WorkGroupConfiguration$BytesScannedCutoffPerQuery` carries `@range` min 10000000.
const SMALLEST_CUTOFF: i64 = 10_000_000;

/// A cutoff above the data the oversized table below holds, so the same scan is inside it.
const ROOMY_CUTOFF: i64 = 13_000_000;

/// How long one record of the oversized table is, padding included: long records keep the
/// row count of a multi-megabyte object small while the BYTES are what the cutoff measures.
const WIDE_RECORD_BYTES: usize = 1_000;

/// How many records the oversized table holds, which puts it above [`SMALLEST_CUTOFF`] and
/// below [`ROOMY_CUTOFF`].
const WIDE_RECORDS: usize = 11_000;

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` header naming `service` in its credential scope — the routing key
/// the router claims a request by — for the principal `access_key_id` names. Nothing is
/// verified: this harness turns request authentication off, so the key id only names the
/// principal a request speaks for.
fn authorization_as(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// The access key id every call here presents unless the suite is about WHO calls: the
/// account's own published credential.
const ACCOUNT_ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// The same header for the account's own credential.
fn authorization(service: &str) -> String {
    authorization_as(service, ACCOUNT_ACCESS_KEY_ID)
}

/// Athena, Glue and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service.
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
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
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

/// One Athena call made by the principal `access_key_id` names, for the suite whose subject
/// is WHO submitted a query.
async fn athena_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header("authorization", authorization_as("athena", access_key_id))
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

/// One Glue call that must succeed.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "glue", "AWSGlue", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object at `path` over Amazon S3's own data plane; a bare bucket name creates the
/// bucket.
async fn s3_put(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3://{path} must succeed, got {}",
        response.status()
    );
}

/// The HTTP status a read of the object at `path` answers, so a test can ask whether anything
/// was written there.
async fn s3_status(router: &MultiServiceRouter, path: &str) -> u16 {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    router.dispatch(request).await.status().as_u16()
}

/// The SerDe every table below is read by: one record per line, fields separated by a comma.
fn serde_info() -> Value {
    json!({
        "SerializationLibrary": "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
        "Parameters": { "field.delim": "," },
    })
}

/// Register the external table `table` over `s3://BUCKET/<table>/`, declaring `columns`.
async fn create_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    columns: &[(&str, &str)],
) {
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}/"),
                    "Columns": columns
                        .iter()
                        .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                        .collect::<Vec<Value>>(),
                    "SerdeInfo": serde_info(),
                },
            },
        }),
    )
    .await;
}

/// The bucket, the database and the two tables every suite here reads.
async fn seed(router: &MultiServiceRouter) {
    s3_put(router, "setup", &format!("/{BUCKET}"), "").await;
    glue_ok(
        router,
        "setup",
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    create_table(
        router,
        "setup",
        "sales",
        &[
            ("cat", "string"),
            ("amount", "bigint"),
            ("qty", "int"),
            ("price", "double"),
        ],
    )
    .await;
    s3_put(
        router,
        "setup",
        &format!("/{BUCKET}/sales/rows.csv"),
        "desks,10,2,10.5\ndesks,20,3,20.25\nlamps,30,4,30.75\n",
    )
    .await;
}

/// Start one query and answer the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, answer) = athena(router, "StartQueryExecution", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartQueryExecution({body}) must be accepted, got {status} with {answer}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answer}"))
        .to_string()
}

/// A `StartQueryExecution` body for `sql` in the suite's database, with the client-side
/// results location.
fn query(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": DATABASE },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// The `QueryExecution` of `id` once it is TERMINAL, polled until it is — the completion is
/// owed a span after the submit, so what is waited for is the CONDITION and never a span.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = athena(router, "GetQueryExecution", &json!({ "QueryExecutionId": id }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution({id}) must answer 200, got {status} with {body}"
        );
        let execution = body["QueryExecution"].clone();
        let state = execution["Status"]["State"].as_str().unwrap_or_default();
        if state != QUERY_STATE_QUEUED && state != QUERY_STATE_RUNNING {
            return execution;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the query {id} never reached a terminal state, last state {state:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The whole result set of `id`, as the one page it fits in.
async fn results(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = athena(
        router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults({id}) must answer 200, got {status} with {body}"
    );
    body
}

/// The `(name, type)` of every column a read of `id`'s results reports, in projection order.
async fn result_column_types(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> Vec<(String, String)> {
    let body = results(router, case, id).await;
    body["ResultSet"]["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected column metadata, got {body}"))
        .iter()
        .map(|column| {
            (
                column["Name"].as_str().unwrap_or_default().to_string(),
                column["Type"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The DATA rows a read of `id`'s results reports — the label row a DML statement's results
/// open with is the result set's schema rather than a row the query returned.
async fn result_rows(router: &MultiServiceRouter, case: &str, id: &str) -> Vec<Vec<String>> {
    let body = results(router, case, id).await;
    body["ResultSet"]["Rows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect()
}

/// Create the workgroup `name` with the suite's results location and `cutoff` as its data
/// usage limit, enforcing its own configuration.
async fn create_capped_work_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    cutoff: i64,
) {
    let (status, created) = athena(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
                "EnforceWorkGroupConfiguration": true,
                "BytesScannedCutoffPerQuery": cutoff,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the workgroup {name} must be created, got {status} with {created}"
    );
}

/// One row of the cutoff table.
struct CutoffCase {
    /// What the row is about.
    case: &'static str,
    /// The workgroup the query runs in, which carries the cutoff.
    work_group: &'static str,
    /// That workgroup's cutoff.
    cutoff: i64,
    /// The statement submitted.
    sql: &'static str,
    /// The state the query must reach.
    state: &'static str,
    /// The `StateChangeReason` it must carry.
    reason: Option<&'static str>,
    /// The `DataScannedInBytes` it must report.
    data_scanned: i64,
}

/// A query that would scan past its workgroup's cutoff is ABORTED at the cutoff, and one
/// inside the cutoff runs to an end and reports what it really read.
#[tokio::test]
async fn a_scan_past_the_work_group_cutoff_is_aborted_at_the_cutoff() {
    let router = emulator();
    seed(&router).await;
    // The oversized table: records wide enough that a multi-megabyte object holds few rows,
    // so what the cutoff decides is the BYTES and not the row count.
    create_table(
        &router,
        "setup",
        "wide",
        &[("id", "bigint"), ("pad", "string")],
    )
    .await;
    let mut records = String::with_capacity(WIDE_RECORDS * WIDE_RECORD_BYTES);
    for row in 0..WIDE_RECORDS {
        let prefix = format!("{row},");
        let padding = "x".repeat(WIDE_RECORD_BYTES - prefix.len() - 1);
        records.push_str(&prefix);
        records.push_str(&padding);
        records.push('\n');
    }
    let wide_bytes = i64::try_from(records.len()).expect("the object size fits");
    assert!(
        wide_bytes > SMALLEST_CUTOFF && wide_bytes < ROOMY_CUTOFF,
        "the oversized object must sit between the two cutoffs, got {wide_bytes}"
    );
    s3_put(
        &router,
        "setup",
        &format!("/{BUCKET}/wide/rows.csv"),
        &records,
    )
    .await;
    let small_bytes = i64::try_from(
        s3_object_size(&router, &format!("/{BUCKET}/sales/rows.csv")).await,
    )
    .expect("the object size fits");

    // The rows differ along the data the scan would read against the cutoff in force (past
    // it, inside it), the cutoff itself (the smallest the model admits, and one above the
    // same data), and which table the statement reads.
    let cases: Vec<CutoffCase> = vec![
        CutoffCase {
            case: "a scan past the smallest cutoff the model admits",
            work_group: "capped_at_the_minimum",
            cutoff: SMALLEST_CUTOFF,
            sql: "SELECT count(*) AS n FROM wide",
            state: QUERY_STATE_CANCELLED,
            reason: Some("Bytes scanned limit was exceeded"),
            data_scanned: SMALLEST_CUTOFF,
        },
        CutoffCase {
            case: "a scan inside the same cutoff, in the same workgroup",
            work_group: "capped_at_the_minimum",
            cutoff: SMALLEST_CUTOFF,
            sql: "SELECT count(*) AS n FROM sales",
            state: QUERY_STATE_SUCCEEDED,
            reason: None,
            data_scanned: small_bytes,
        },
        CutoffCase {
            case: "the same oversized scan under a cutoff above it",
            work_group: "capped_above_the_data",
            cutoff: ROOMY_CUTOFF,
            sql: "SELECT count(*) AS n FROM wide",
            state: QUERY_STATE_SUCCEEDED,
            reason: None,
            data_scanned: wide_bytes,
        },
    ];

    for work_group in ["capped_at_the_minimum", "capped_above_the_data"] {
        let cutoff = cases
            .iter()
            .find(|case| case.work_group == work_group)
            .map(|case| case.cutoff)
            .expect("every workgroup under test has a cutoff");
        create_capped_work_group(&router, "setup", work_group, cutoff).await;
    }

    let mut started: Vec<(&CutoffCase, String)> = Vec::new();
    for case in &cases {
        let body = json!({
            "QueryString": case.sql,
            "QueryExecutionContext": { "Database": DATABASE },
            "WorkGroup": case.work_group,
        });
        started.push((case, start(&router, case.case, &body).await));
    }

    for (case, id) in &started {
        let name = case.case;
        let execution = settled(&router, name, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(case.state),
            "case {name:?}: expected the state {:?}, got {execution}",
            case.state
        );
        assert_eq!(
            execution["Status"]["StateChangeReason"].as_str(),
            case.reason,
            "case {name:?}: expected the reason {:?}, got {execution}",
            case.reason
        );
        assert_eq!(
            execution["Statistics"]["DataScannedInBytes"].as_i64(),
            Some(case.data_scanned),
            "case {name:?}: expected {} bytes scanned, got {execution}",
            case.data_scanned
        );
        if case.state == QUERY_STATE_CANCELLED {
            // An aborted query produced no results, so a read of them reports their
            // ABSENCE rather than the state the execution is in.
            let (status, body) = athena(
                &router,
                "GetQueryResults",
                &json!({ "QueryExecutionId": id }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {name:?}: a read of an aborted query's results must be refused, got \
                 {status} with {body}"
            );
            assert_eq!(
                body["Message"].as_str().or_else(|| body["message"].as_str()),
                Some("Could not find results"),
                "case {name:?}: unexpected refusal of the results read, got {body}"
            );
        }
    }
}

/// How many bytes the object at `path` holds, read back over Amazon S3's data plane.
async fn s3_object_size(router: &MultiServiceRouter, path: &str) -> usize {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "reading s3://{path} must succeed, got {}",
        response.status()
    );
    response.body().len()
}

/// One row of the expected-owner table.
struct OwnerCase {
    /// What the row is about.
    case: &'static str,
    /// The account the submission expects to own the results bucket.
    expected_owner: Option<String>,
    /// Whether the query must settle SUCCEEDED.
    succeeds: bool,
}

/// A query whose results would be written into a bucket another account owns settles FAILED
/// carrying that refusal and writes nothing, while one naming the owner — or naming no
/// account at all — runs to an end and saves its results.
#[tokio::test]
async fn an_output_bucket_owned_by_another_account_fails_the_query_and_writes_nothing() {
    let router = emulator();
    seed(&router).await;
    let owner = ::engine::emulator_account_id().to_string();

    // The rows differ along the account the submission names: one that does not own the
    // bucket, the account that does, and none at all.
    let cases: Vec<OwnerCase> = vec![
        OwnerCase {
            case: "an account that does not own the results bucket",
            expected_owner: Some("111122223333".to_string()),
            succeeds: false,
        },
        OwnerCase {
            case: "the account that owns it",
            expected_owner: Some(owner.clone()),
            succeeds: true,
        },
        OwnerCase {
            case: "no expected owner at all",
            expected_owner: None,
            succeeds: true,
        },
    ];

    let mut started: Vec<(&OwnerCase, String)> = Vec::new();
    for case in &cases {
        let mut configuration = json!({ "OutputLocation": OUTPUT_LOCATION });
        if let Some(expected) = &case.expected_owner {
            configuration["ExpectedBucketOwner"] = json!(expected);
        }
        let body = json!({
            "QueryString": "SELECT cat, amount FROM sales ORDER BY amount",
            "QueryExecutionContext": { "Database": DATABASE },
            "ResultConfiguration": configuration,
        });
        started.push((case, start(&router, case.case, &body).await));
    }

    for (case, id) in &started {
        let name = case.case;
        let execution = settled(&router, name, id).await;
        let state = execution["Status"]["State"].as_str().unwrap_or_default();
        let object = format!("/{BUCKET}/results/{id}.csv");
        if case.succeeds {
            assert_eq!(
                state, QUERY_STATE_SUCCEEDED,
                "case {name:?}: the query must succeed, got {execution}"
            );
            assert_eq!(
                s3_status(&router, &object).await,
                200,
                "case {name:?}: the results must be saved at s3:/{object}"
            );
            continue;
        }
        assert_eq!(
            state, QUERY_STATE_FAILED,
            "case {name:?}: the query must fail, got {execution}"
        );
        let expected_reason = format!(
            "Access denied when writing output to url: s3://{BUCKET}/results/{id}.csv . Please \
             ensure you are allowed to access the S3 bucket. If specifying an expected bucket \
             owner, confirm the bucket is owned by the expected account. If you are encrypting \
             query results with KMS key, please ensure you are allowed to access your KMS key"
        );
        assert_eq!(
            execution["Status"]["StateChangeReason"].as_str(),
            Some(expected_reason.as_str()),
            "case {name:?}: expected the reason {expected_reason:?}, got {execution}"
        );
        assert_eq!(
            execution["Status"]["AthenaError"]["ErrorCategory"].as_i64(),
            Some(ERROR_CATEGORY_USER),
            "case {name:?}: the refusal is the caller's own to fix, got {execution}"
        );
        assert_eq!(
            s3_status(&router, &object).await,
            404,
            "case {name:?}: nothing must be written at s3:/{object}, got a stored object"
        );
        let (status, body) = athena(
            &router,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: a read of a failed query's results must be refused, got {status} \
             with {body}"
        );
        assert_eq!(
            body["Message"].as_str().or_else(|| body["message"].as_str()),
            Some("Query did not finish successfully. Final query state: FAILED"),
            "case {name:?}: unexpected refusal of the results read, got {body}"
        );
    }
}

/// Every FAILED query reports an `AthenaError` carrying both halves of Athena's
/// classification: the category whose doing the failure was, and the integer error type of
/// the class it belongs to.
#[tokio::test]
async fn every_failed_query_reports_an_error_type_beside_its_category() {
    let router = emulator();
    seed(&router).await;

    // (case, the SQL, the error type its class publishes). The rows differ along the class
    // of failure: a column no relation carries and a function the engine does not have are
    // user errors of no narrower published class, while a table and a database the catalog
    // does not hold are the not-found class, which has one of its own.
    let cases: Vec<(&str, &str, i64)> = vec![
        (
            "a column no relation carries",
            "SELECT missing_column FROM sales",
            GENERIC_USER_ERROR_TYPE,
        ),
        (
            "a table the catalog does not hold",
            "SELECT cat FROM no_such_table",
            ENTITY_NOT_FOUND_ERROR_TYPE,
        ),
        (
            "a database the catalog does not hold",
            "SELECT cat FROM no_such_database.sales",
            ENTITY_NOT_FOUND_ERROR_TYPE,
        ),
        (
            "a function the engine does not have",
            "SELECT no_such_function(amount) FROM sales",
            GENERIC_USER_ERROR_TYPE,
        ),
    ];

    let mut started: Vec<(&str, String, i64)> = Vec::new();
    for (case, sql, error_type) in &cases {
        started.push((case, start(&router, case, &query(sql)).await, *error_type));
    }

    for (case, id, error_type) in &started {
        let execution = settled(&router, case, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: the statement must fail, got {execution}"
        );
        let error = execution["Status"]["AthenaError"].clone();
        assert_eq!(
            error["ErrorType"].as_i64(),
            Some(*error_type),
            "case {case:?}: expected the error type {error_type}, got {error}"
        );
        assert_eq!(
            error["ErrorCategory"].as_i64(),
            Some(ERROR_CATEGORY_USER),
            "case {case:?}: a statement the caller must change is a user error, got {error}"
        );
        assert!(
            error["ErrorMessage"].as_str().is_some_and(|message| !message.is_empty()),
            "case {case:?}: expected a message beside the classification, got {error}"
        );
    }
}

/// A query that really ran reports the engine time it took, and the total execution time is
/// at least that.
#[tokio::test]
async fn a_query_that_ran_reports_the_engine_time_it_took() {
    let router = emulator();
    seed(&router).await;

    // (case, the SQL). The rows differ along the class of statement: a scan of stored data,
    // an aggregate over it, a projection of constants that reads nothing, and a definition
    // the data catalog performs.
    let cases: Vec<(&str, &str)> = vec![
        ("a scan of stored data", "SELECT cat, amount FROM sales"),
        ("an aggregate over stored data", "SELECT count(*) AS n FROM sales"),
        ("a projection of constants", "SELECT 1 AS one"),
        (
            "a definition the data catalog performs",
            "CREATE DATABASE limits_timed",
        ),
    ];

    let mut started: Vec<(&str, String)> = Vec::new();
    for (case, sql) in &cases {
        started.push((case, start(&router, case, &query(sql)).await));
    }

    for (case, id) in &started {
        let execution = settled(&router, case, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {case:?}: the statement must succeed, got {execution}"
        );
        let statistics = execution["Statistics"].clone();
        let engine = statistics["EngineExecutionTimeInMillis"].as_i64();
        assert!(
            engine.is_some_and(|millis| millis > 0),
            "case {case:?}: a query that ran must report engine time, got {statistics}"
        );
        let total = statistics["TotalExecutionTimeInMillis"].as_i64();
        assert!(
            total >= engine,
            "case {case:?}: the total execution time must be at least the engine's, got \
             {statistics}"
        );
    }
}

/// One row of the column-typing table.
struct ColumnTypeCase {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `(name, type)` of each column it must project, in projection order.
    columns: Vec<(&'static str, &'static str)>,
}

/// An aggregate or window function whose result type is its ARGUMENT's reports the type the
/// data catalog declares for the column that argument names, not the storage class the engine
/// held its values in.
#[tokio::test]
async fn an_argument_typed_aggregate_reports_the_declared_type_of_its_column() {
    let router = emulator();
    seed(&router).await;

    // The rows differ along the function applied, the declared type of its argument (both
    // integer widths, an approximate type, character data), whether the call carries a
    // window, and the functions whose type their own signature fixes.
    let cases: Vec<ColumnTypeCase> = vec![
        ColumnTypeCase {
            case: "the aggregates of a grouped read over a 64-bit integer column",
            sql: "SELECT cat, count(*) AS n, sum(amount) AS total, min(amount) AS lo, \
                  max(amount) AS hi FROM sales GROUP BY cat ORDER BY cat",
            columns: vec![
                ("cat", "varchar"),
                ("n", "bigint"),
                ("total", "bigint"),
                ("lo", "bigint"),
                ("hi", "bigint"),
            ],
        },
        ColumnTypeCase {
            case: "a ranking and a total over the window of each row",
            sql: "SELECT cat, amount, row_number() OVER (PARTITION BY cat ORDER BY amount) \
                  AS rn, sum(amount) OVER (PARTITION BY cat) AS cat_total FROM sales \
                  ORDER BY cat, amount",
            columns: vec![
                ("cat", "varchar"),
                ("amount", "bigint"),
                ("rn", "bigint"),
                ("cat_total", "bigint"),
            ],
        },
        ColumnTypeCase {
            case: "a total over a 32-bit integer column, which accumulates wider",
            sql: "SELECT sum(qty) AS total, min(qty) AS lo, max(qty) AS hi FROM sales",
            columns: vec![("total", "bigint"), ("lo", "integer"), ("hi", "integer")],
        },
        ColumnTypeCase {
            case: "the aggregates of an approximate column, which stay approximate",
            sql: "SELECT sum(price) AS total, min(price) AS lo, avg(price) AS mean FROM sales",
            columns: vec![("total", "double"), ("lo", "double"), ("mean", "double")],
        },
        ColumnTypeCase {
            case: "an aggregate over a character column",
            sql: "SELECT max(cat) AS last FROM sales",
            columns: vec![("last", "varchar")],
        },
    ];

    let mut started: Vec<(&ColumnTypeCase, String)> = Vec::new();
    for case in &cases {
        started.push((case, start(&router, case.case, &query(case.sql)).await));
    }

    for (case, id) in &started {
        let name = case.case;
        let execution = settled(&router, name, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {name:?}: the statement must succeed, got {execution}"
        );
        let actual = result_column_types(&router, name, id).await;
        let expected: Vec<(String, String)> = case
            .columns
            .iter()
            .map(|(column, column_type)| ((*column).to_string(), (*column_type).to_string()))
            .collect();
        assert_eq!(
            actual, expected,
            "case {name:?}: expected the columns {expected:?}, got {actual:?}"
        );
    }
}

/// A `ResultReuseConfiguration` body for `sql`: reuse `enabled`, inside `max_age` minutes.
fn reusing_query(sql: &str, enabled: bool, max_age: Option<i32>) -> Value {
    let mut by_age = json!({ "Enabled": enabled });
    if let Some(minutes) = max_age {
        by_age["MaxAgeInMinutes"] = json!(minutes);
    }
    let mut body = query(sql);
    body["ResultReuseConfiguration"] = json!({ "ResultReuseByAgeConfiguration": by_age });
    body
}

/// One row of the reuse table.
struct ReuseCase {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// Whether its submission enables result reuse.
    enabled: bool,
    /// The `MaxAgeInMinutes` it names, where it names one.
    max_age: Option<i32>,
    /// Whether the execution must report that it reused a previous result.
    reused: bool,
}

/// An identical query submitted inside the window its configuration asks for is answered out
/// of the earlier execution's result: it reports the reuse, serves that execution's rows even
/// after the data under the query has changed, and scans nothing.
#[tokio::test]
async fn an_identical_query_inside_the_reuse_window_is_answered_from_the_previous_result() {
    let router = emulator();
    seed(&router).await;
    let reused_sql = "SELECT cat FROM sales ORDER BY cat LIMIT 1";

    // The first run of the statement, which every reuse below is answered out of.
    let first = start(
        &router,
        "the first run",
        &reusing_query(reused_sql, true, Some(60)),
    )
    .await;
    let settled_first = settled(&router, "the first run", &first).await;
    assert_eq!(
        settled_first["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case \"the first run\": the statement must succeed, got {settled_first}"
    );
    assert_eq!(
        settled_first["Statistics"]["ResultReuseInformation"]["ReusedPreviousResult"].as_bool(),
        Some(false),
        "case \"the first run\": the first run of a statement reuses nothing, got \
         {settled_first}"
    );
    let first_rows = result_rows(&router, "the first run", &first).await;

    // The data under the statement CHANGES, so a reused answer is observably the earlier
    // one: a reuse serves the result that was already there.
    s3_put(
        &router,
        "the changed data",
        &format!("/{BUCKET}/sales/rows.csv"),
        "anvils,40,5,40.5\nanvils,50,6,50.25\n",
    )
    .await;

    // The rows differ along what the submission asks for: reuse inside the documented
    // window, reuse with no age of its own, reuse turned off, a window no previous result
    // is inside, and another statement entirely.
    let cases: Vec<ReuseCase> = vec![
        ReuseCase {
            case: "the same statement inside the window it asks for",
            sql: reused_sql,
            enabled: true,
            max_age: Some(60),
            reused: true,
        },
        ReuseCase {
            case: "the same statement with no age of its own, which is the default window",
            sql: reused_sql,
            enabled: true,
            max_age: None,
            reused: true,
        },
        ReuseCase {
            case: "the same statement with reuse turned off",
            sql: reused_sql,
            enabled: false,
            max_age: Some(60),
            reused: false,
        },
        ReuseCase {
            case: "the same statement with a window no previous result is inside",
            sql: reused_sql,
            enabled: true,
            max_age: Some(0),
            reused: false,
        },
        ReuseCase {
            case: "another statement, which no previous result answers",
            sql: "SELECT cat FROM sales ORDER BY cat DESC LIMIT 1",
            enabled: true,
            max_age: Some(60),
            reused: false,
        },
    ];

    let mut started: Vec<(&ReuseCase, String)> = Vec::new();
    for case in &cases {
        let body = reusing_query(case.sql, case.enabled, case.max_age);
        started.push((case, start(&router, case.case, &body).await));
    }

    for (case, id) in &started {
        let name = case.case;
        let execution = settled(&router, name, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {name:?}: the statement must succeed, got {execution}"
        );
        let statistics = execution["Statistics"].clone();
        assert_eq!(
            statistics["ResultReuseInformation"]["ReusedPreviousResult"].as_bool(),
            Some(case.reused),
            "case {name:?}: expected ReusedPreviousResult {}, got {statistics}",
            case.reused
        );
        let rows = result_rows(&router, name, id).await;
        if case.reused {
            // The rows the earlier execution produced, not the ones the changed data would
            // produce now.
            assert_eq!(
                rows, first_rows,
                "case {name:?}: expected the earlier rows {first_rows:?}, got {rows:?}"
            );
            assert_eq!(
                statistics["DataScannedInBytes"].as_i64(),
                Some(0),
                "case {name:?}: a reused answer scanned nothing, got {statistics}"
            );
        } else if case.sql == reused_sql {
            // A run that did NOT reuse read the data as it is now, which the change above
            // made different from the earlier answer.
            assert_ne!(
                rows, first_rows,
                "case {name:?}: expected the rows of the changed data, got the earlier \
                 {rows:?}"
            );
        }
    }
}

/// A previous result answers only the PRINCIPAL it was produced for: what a statement may
/// read is decided per caller, so an identical submission by another principal is run rather
/// than answered out of it — while the same principal's rerun is answered out of it.
#[tokio::test]
async fn a_previous_result_does_not_answer_another_principals_submission() {
    let router = emulator();
    seed(&router).await;
    let sql = "SELECT cat FROM sales ORDER BY cat LIMIT 1";
    // Another principal: a key id naming a different caller, which is all a principal is to
    // this harness (request authentication is off, so nothing is verified).
    let other_principal = "AKIDANOTHERPRINCIPAL";

    // (case, the principal that submits, whether the execution must report a reuse). The
    // first submission has nothing to reuse, the same principal's rerun has, and the other
    // principal's identical submission must not be answered out of it.
    let cases: Vec<(&str, &str, bool)> = vec![
        ("the first submission", ACCOUNT_ACCESS_KEY_ID, false),
        ("the same principal's rerun", ACCOUNT_ACCESS_KEY_ID, true),
        ("another principal's identical submission", other_principal, false),
    ];

    for (case, principal, reused) in &cases {
        let (status, answer) = athena_as(
            &router,
            principal,
            "StartQueryExecution",
            &reusing_query(sql, true, Some(60)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the submission must be accepted, got {status} with {answer}"
        );
        let id = answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {answer}"))
            .to_string();
        let execution = settled(&router, case, &id).await;
        let statistics = execution["Statistics"].clone();
        assert_eq!(
            statistics["ResultReuseInformation"]["ReusedPreviousResult"].as_bool(),
            Some(*reused),
            "case {case:?}: expected ReusedPreviousResult {reused}, got {execution}"
        );
    }
}
