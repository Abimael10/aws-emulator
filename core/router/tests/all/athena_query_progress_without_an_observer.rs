// SPDX-License-Identifier: CC0-1.0

//! WHAT ADVANCES a submitted Athena query: the emulator's clock, and nothing a caller
//! does — driven across the two services the question spans, mounted on ONE engine
//! exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: `StartQueryExecution` is asynchronous and the query it accepted runs to its
//! terminal state on the service's own clock, so READING an execution never moves it
//! along and never has to: a query nobody looks at until long after it was submitted
//! reaches exactly the state, the completion instant, the result rows and the saved
//! result object that a query a client polls throughout reaches. `QueryExecutionState`
//! is `QUEUED | RUNNING | SUCCEEDED | FAILED | CANCELLED`, where "QUEUED indicates that
//! the query has been submitted to the service, and Athena will execute the query as
//! soon as resources are available" and "SUCCEEDED indicates that the query completed
//! without errors" (`smithy_query(query="shape",shape="QueryExecutionState",
//! model_path="services/service-com-amazonaws-athena-amazonathena/smithy-model/
//! com-amazonaws-athena-amazonathena.json")`), and a completed query carries
//! `Status.CompletionDateTime`, "the date and time that the query completed".
//!
//! Two crates have to agree for that to hold (Athena runs the query and reports it,
//! Amazon S3 holds the result object the completed query saved), so the suite lives
//! here rather than in either of them.
//!
//! HOW it is driven without waiting on a wall clock: every unobserved query is
//! submitted FIRST and one OBSERVED query last, and the observed one is then polled
//! until it is terminal. Its completion falls due after every earlier submission's, so
//! by the time it reports a terminal state each unobserved query's own completion is
//! due as well — and each is then read exactly ONCE. Nothing sleeps for a span and
//! nothing asserts how long anything took.
//!
//! The rows differ along what the rule generalises over: the statement submitted (the
//! literal it projects, the column name it projects it under, and how many columns it
//! projects), and the number of queries left unobserved behind the polled one.
//!
//! Nothing here asserts a value this emulator invented: the query id comes off
//! `StartQueryExecution`'s own answer, the object read from Amazon S3 is the one
//! `GetQueryExecution` named in `ResultConfiguration.OutputLocation`, and the rows are
//! the ones `GetQueryResults` reported.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for and addressed in.
const REGION: &str = "us-west-2";

/// How long the observed query is given to settle, and how often it is looked at while
/// it has not. The budget is generous next to the span a submission's completion falls
/// due after, so a loaded machine has room.
const PATIENCE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// The bucket the workgroup's results are written into, and the prefix inside it.
const BUCKET: &str = "athena-unobserved-results";
const PREFIX: &str = "results";

/// The workgroup every query here runs in.
const WORK_GROUP: &str = "athena-unobserved";

/// The states a query has not finished in.
const NOT_FINISHED: [&str; 2] = ["QUEUED", "RUNNING"];

/// Amazon S3 and Athena over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them — so the results Athena saves land in this very object store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket
/// and the key live in the URI and the body is the payload.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One `awsJson1_1` call to Athena, requiring a 200, under the target prefix its
/// model's `service_info` names (`sigv4Name` `athena`).
///
/// `pub(crate)` because `super::sfn_run_a_job_reports_no_sooner_than_its_job_moves` asks
/// a different question of the same service and `tests/all/main.rs` is one crate root:
/// one caller for Athena rather than a second copy of it.
pub(crate) async fn athena(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header("authorization", authorization("athena"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{operation}: expected 200 from Athena, got {status} with {parsed:?}"
    );
    parsed
}

/// The results bucket and the workgroup that writes into it.
async fn given_the_results_location(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "the results bucket must be created, got {status} with {body:?}"
    );
    athena(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/{PREFIX}/") },
                "EnforceWorkGroupConfiguration": true,
            },
        }),
    )
    .await;
}

/// Submit `statement` and answer the `QueryExecutionId` Athena minted for it.
async fn submitted(router: &MultiServiceRouter, statement: &str) -> String {
    let answered = athena(
        router,
        "StartQueryExecution",
        &json!({ "QueryString": statement, "WorkGroup": WORK_GROUP }),
    )
    .await;
    answered["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartQueryExecution answers a QueryExecutionId, got {answered}"))
        .to_string()
}

/// `GetQueryExecution` for `query_id`, once.
async fn read_once(router: &MultiServiceRouter, query_id: &str) -> Value {
    athena(
        router,
        "GetQueryExecution",
        &json!({ "QueryExecutionId": query_id }),
    )
    .await
}

/// Poll `query_id` until it reports a state it has finished in.
async fn polled_to_terminal(router: &MultiServiceRouter, query_id: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let reported = read_once(router, query_id).await;
        let state = reported["QueryExecution"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !NOT_FINISHED.contains(&state.as_str()) {
            return reported;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the polled query to settle within {PATIENCE:?}, got \
             {reported:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// Every row of a query's results, each row as the values of its `Datum`s
/// (`GetQueryResults` renders one `Datum` per value, carrying `VarCharValue`), followed
/// through the listing's own `NextToken`.
///
/// `pub(crate)` for the reason [`athena`] is: the suite that reads the same rows of a
/// query a workflow submitted reads them through this.
pub(crate) async fn query_rows(
    router: &MultiServiceRouter,
    query_id: &str,
) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        pages += 1;
        let mut request = json!({ "QueryExecutionId": query_id });
        if let Some(token) = &token {
            request["NextToken"] = Value::String(token.clone());
        }
        let answered = athena(router, "GetQueryResults", &request).await;
        for row in answered["ResultSet"]["Rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            rows.push(
                row["Data"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|datum| {
                        datum["VarCharValue"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect(),
            );
        }
        token = answered["NextToken"].as_str().map(str::to_string);
        if token.is_none() || pages > 16 {
            return rows;
        }
    }
}

/// The bytes the object at the `s3://` `address` holds, read through Amazon S3's own
/// front door.
async fn fetched(router: &MultiServiceRouter, address: &str, case: &str) -> String {
    let inside = address
        .strip_prefix("s3://")
        .unwrap_or_else(|| panic!("case {case:?}: expected an s3:// address, got {address:?}"));
    let (status, body) = s3(router, "GET", &format!("/{inside}"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the result object {address:?} to be readable, got {status} \
         with {body:?}"
    );
    body
}

/// One row: the case, the statement submitted, and the rows a completed run of it
/// reports — the header row the statement's own column names make, and the one row of
/// values it projects.
struct Row {
    case: &'static str,
    statement: &'static str,
    reported: Vec<Vec<&'static str>>,
}

/// Every submitted query runs to completion on the service's own clock: the ones
/// nobody read are as finished, as complete and as readable as the one that was polled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_query_nobody_reads_runs_to_completion_anyway() {
    let router = emulator();
    given_the_results_location(&router).await;

    let rows = vec![
        Row {
            case: "one column under the name the statement gives it",
            statement: "SELECT 'north-1' AS marker",
            reported: vec![vec!["marker"], vec!["north-1"]],
        },
        Row {
            case: "a literal carrying a space, under another name",
            statement: "SELECT 'south 2' AS label",
            reported: vec![vec!["label"], vec!["south 2"]],
        },
        Row {
            case: "two columns, one of them a number",
            statement: "SELECT 'east-3' AS marker, 42 AS answer",
            reported: vec![vec!["marker", "answer"], vec!["east-3", "42"]],
        },
    ];

    // Every row's query is submitted BEFORE the one that will be polled, so each one's
    // completion falls due before the polled query's does.
    let mut unobserved = Vec::new();
    for row in &rows {
        unobserved.push((row, submitted(&router, row.statement).await));
    }
    let polled = submitted(&router, "SELECT 'polled' AS marker").await;
    let reported = polled_to_terminal(&router, &polled, "the polled query").await;
    assert_eq!(
        reported["QueryExecution"]["Status"]["State"].as_str(),
        Some("SUCCEEDED"),
        "the polled query must have completed, got {reported:?}"
    );

    for (row, query_id) in unobserved {
        let case = row.case;
        // The ONE read of this query: nothing has looked at it since it was submitted.
        let reported = read_once(&router, &query_id).await;
        let status = &reported["QueryExecution"]["Status"];
        assert_eq!(
            status["State"].as_str(),
            Some("SUCCEEDED"),
            "case {case:?}: expected the unread query to have completed, got {reported:?}"
        );
        assert!(
            status["CompletionDateTime"].is_number(),
            "case {case:?}: expected a completed query to carry its completion instant, got \
             {status:?}"
        );
        let rows = query_rows(&router, &query_id).await;
        let expected: Vec<Vec<String>> = row
            .reported
            .iter()
            .map(|values| values.iter().map(|value| (*value).to_string()).collect())
            .collect();
        assert_eq!(
            rows, expected,
            "case {case:?}: expected the rows the statement projects, got {rows:?}"
        );
        // The completed run SAVED its results, at the address this read names: the
        // object the workgroup's results location holds for this execution.
        let address = reported["QueryExecution"]["ResultConfiguration"]["OutputLocation"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the read to name the result object, got {reported:?}")
            })
            .to_string();
        assert_eq!(
            address,
            format!("s3://{BUCKET}/{PREFIX}/{query_id}.csv"),
            "case {case:?}: expected the result object of this execution, got {address:?}"
        );
        let saved = fetched(&router, &address, case).await;
        for value in row.reported.iter().flatten() {
            assert!(
                saved.contains(value),
                "case {case:?}: expected the saved result object to carry {value:?}, got \
                 {saved:?}"
            );
        }
    }
}
