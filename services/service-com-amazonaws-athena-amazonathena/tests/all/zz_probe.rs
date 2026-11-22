// SPDX-License-Identifier: CC0-1.0

//! What a query that WROTE no objects says about a data manifest.
//!
//! THE RULE UNDER TEST: Athena creates a data manifest only for the statements that WRITE
//! their output as objects — "only the query types CREATE TABLE AS SELECT, UNLOAD, and
//! INSERT can generate a manifest file" (`GetQueryResults$QueryResultType`) — so an
//! execution whose run wrote no output of its own reports no `DataManifestLocation`, saves no
//! manifest object beside its results, and a read of its results as `DATA_MANIFEST` is
//! refused rather than answered with an empty manifest.
//!
//! The rows differ along the class of statement submitted: one whose results are rows, one
//! whose result is the engine's text output, one the data catalog performs, and one that
//! DEFINES a table from a query in a database the catalog does not hold — which the engine
//! performs in its own database, writing no object of the caller's.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_MANIFEST_SUFFIX, QUERY_EXECUTION_COMPLETION, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The bucket these queries write their results into.
const BUCKET: &str = "athena-no-manifest";

/// Where they write them.
const RESULTS: &str = "s3://athena-no-manifest/results/";

/// How long past [`QUERY_EXECUTION_COMPLETION`] a poll waits for a started query to be
/// readable in its terminal state. Generous: what is asserted is the state reached, never
/// how fast the host got there.
const SETTLE_GRACE: Duration = Duration::from_secs(60);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A SigV4 `Authorization` header naming `service` in its credential scope. Nothing is
/// verified: this harness turns request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena and Amazon S3 on ONE shared engine, the way the router mounts every service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One Athena call, and its status and parsed body.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "athena.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header("authorization", authorization("athena"))
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

/// One path-style request to the emulator's Amazon S3 front door.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "s3.us-east-1.amazonaws.com")
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// RULE: a statement that writes no objects reports no data manifest, saves none and
/// refuses a read of one.
#[tokio::test]
async fn a_query_that_wrote_no_objects_has_no_data_manifest() {
    let router = emulator();
    let (status, body) = s3(&router, "PUT", &format!("/{BUCKET}")).await;
    assert!(
        (200..300).contains(&status),
        "setup: creating s3://{BUCKET} must succeed, got {status} with {body}"
    );

    // (case, the statement, the database it runs in, the terminal state it reaches)
    let cases: Vec<(&str, &str, Option<&str>, &str)> = vec![
        ("a projection", "SELECT 1 AS n", None, QUERY_STATE_SUCCEEDED),
        (
            "the engine's text output",
            "SHOW DATABASES",
            None,
            QUERY_STATE_SUCCEEDED,
        ),
        (
            "a statement the catalog refuses",
            "DROP DATABASE nothing_holds_this",
            None,
            QUERY_STATE_FAILED,
        ),
        (
            // A table defined from a query with no location of its own, in a database the
            // catalog does not hold: the engine performs it in its own database, so no
            // object of the caller's was written and there is no manifest of one.
            "a table the engine defined in its own database",
            "CREATE TABLE engine_local AS SELECT 1 AS n",
            Some("held_by_no_catalog"),
            QUERY_STATE_SUCCEEDED,
        ),
    ];

    let mut started: Vec<(&str, &str, String)> = Vec::new();
    for (case, sql, database, state) in &cases {
        let mut request = json!({
            "QueryString": sql,
            "ResultConfiguration": { "OutputLocation": RESULTS },
        });
        if let Some(database) = database {
            request["QueryExecutionContext"] = json!({ "Database": database });
        }
        let (status, answer) = athena(&router, "StartQueryExecution", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: StartQueryExecution must be accepted, got {status} with {answer}"
        );
        let id = answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an execution id, got {answer}"))
            .to_string();
        started.push((case, state, id));
    }

    let attempts = ((QUERY_EXECUTION_COMPLETION + SETTLE_GRACE).as_millis()
        / POLL_INTERVAL.as_millis())
        + 2;
    for (case, state, id) in &started {
        let mut execution = Value::Null;
        for _ in 0..attempts {
            let (status, body) = athena(
                &router,
                "GetQueryExecution",
                &json!({ "QueryExecutionId": id }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetQueryExecution must answer 200, got {status} with {body}"
            );
            execution = body["QueryExecution"].clone();
            let reached = execution["Status"]["State"].as_str().unwrap_or_default();
            if reached == QUERY_STATE_SUCCEEDED || reached == QUERY_STATE_FAILED {
                break;
            }
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(*state),
            "case {case:?}: expected the state {state:?}, got {}",
            execution["Status"]
        );
        assert!(
            execution["Statistics"]["DataManifestLocation"].is_null(),
            "case {case:?}: no manifest may be named, got {}",
            execution["Statistics"]
        );

        // No manifest object was saved beside the results either.
        let (status, body) = s3(
            &router,
            "GET",
            &format!("/{BUCKET}/results/{id}{DATA_MANIFEST_SUFFIX}"),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: no manifest object may be saved, got {status} with {body}"
        );

        // And a read of the results AS a manifest is refused.
        let (status, refusal) = athena(
            &router,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id, "QueryResultType": "DATA_MANIFEST" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a manifest read must be refused, got {status} with {refusal}"
        );
    }
}
