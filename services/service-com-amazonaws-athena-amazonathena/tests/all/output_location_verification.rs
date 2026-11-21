// SPDX-License-Identifier: CC0-1.0

//! The results LOCATION a submission resolves, verified before the submission is accepted.
//!
//! The rule under test: Athena writes a query's results into a bucket of the caller's own
//! account, so it VERIFIES the location the submission resolved — the one the request names,
//! or the workgroup's where the request names none — and refuses a submission whose location
//! names a bucket it can neither verify nor create with
//! `InvalidRequestException: Unable to verify/create output bucket <bucket>`, recording no
//! query execution at all. The same submission is accepted once that bucket is there, so
//! what decides it is the bucket the location names and nothing of the statement.
//!
//! Athena and Amazon S3 are mounted on ONE shared engine and driven over the wire the
//! production front door uses, so the bucket the verification consults is the one S3's own
//! `CreateBucket` puts there.
//!
//! The suite is a table: the rows differ along where the resolved location came from, the
//! prefix under its bucket, and whether that bucket is there.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::DEFAULT_WORK_GROUP;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket this account HOLDS, which a location may name.
const HELD_BUCKET: &str = "athena-verified-results";

/// A bucket nothing created, which the rows name before it exists.
const ABSENT_BUCKET: &str = "athena-unverifiable-results";

/// A SigV4 `Authorization` header naming `service` in its credential scope. Nothing is
/// verified: this harness turns request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service.
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

/// One Athena call, and its HTTP status and parsed body.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
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

/// `PUT /<bucket>` — `CreateBucket`, which is what puts a bucket in this account.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "setup: creating s3://{bucket} must succeed, got {} with {}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// How many query executions the account's default workgroup holds.
async fn history_length(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, listed) = athena(
        router,
        "ListQueryExecutions",
        &json!({ "WorkGroup": DEFAULT_WORK_GROUP, "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListQueryExecutions must answer 200, got {status} with {listed}"
    );
    listed["QueryExecutionIds"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// One submission under test: the location it resolves and whether that location's bucket is
/// one this account holds.
struct Submission {
    /// What the row is.
    case: &'static str,
    /// The `StartQueryExecution` request, less the statement.
    request: Value,
    /// The bucket the refusal must name, or `None` for a submission that is accepted.
    refused: Option<&'static str>,
}

/// A submission whose resolved output location names a bucket this account does not hold is
/// refused naming that bucket and records no execution; one whose bucket is there is
/// accepted.
///
/// The rows differ along where the resolved location came from (the request, the workgroup
/// behind a request that names none), the prefix under its bucket, and whether that bucket is
/// there.
#[tokio::test]
async fn a_submission_is_refused_until_the_bucket_its_location_names_is_there() {
    let router = emulator();
    create_bucket(&router, HELD_BUCKET).await;
    // A workgroup whose own results location is in the bucket nothing created, for the row
    // whose location the WORKGROUP supplies.
    let (created, body) = athena(
        &router,
        "CreateWorkGroup",
        &json!({
            "Name": "unverifiable-results",
            "Configuration": {
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{ABSENT_BUCKET}/from-the-work-group/")
                }
            },
        }),
    )
    .await;
    assert_eq!(
        created, 200,
        "setup: the workgroup must be created, got {created} with {body}"
    );

    let rows: Vec<Submission> = vec![
        Submission {
            case: "a location the request names, in a bucket nothing created",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{ABSENT_BUCKET}/results/") }
            }),
            refused: Some(ABSENT_BUCKET),
        },
        Submission {
            case: "the same bucket under a deeper prefix",
            request: json!({
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{ABSENT_BUCKET}/year/2026/month/01/")
                }
            }),
            refused: Some(ABSENT_BUCKET),
        },
        Submission {
            case: "the same bucket named without a trailing separator",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{ABSENT_BUCKET}") }
            }),
            refused: Some(ABSENT_BUCKET),
        },
        Submission {
            case: "a location the workgroup supplies, in a bucket nothing created",
            request: json!({ "WorkGroup": "unverifiable-results" }),
            refused: Some(ABSENT_BUCKET),
        },
        Submission {
            case: "a location in the bucket this account holds",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{HELD_BUCKET}/results/") }
            }),
            refused: None,
        },
    ];

    for row in &rows {
        let before = history_length(&router, row.case).await;
        let mut request = row.request.clone();
        request["QueryString"] = json!("SELECT 1 AS n");
        let (status, answer) = athena(&router, "StartQueryExecution", &request).await;
        match row.refused {
            Some(bucket) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: the submission must be refused, got {status} with {answer}",
                    row.case
                );
                let expected = format!("Unable to verify/create output bucket {bucket}");
                assert_eq!(
                    answer["Message"].as_str(),
                    Some(expected.as_str()),
                    "case {:?}: expected the message {expected:?}, got {answer}",
                    row.case
                );
                assert!(
                    answer["__type"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("InvalidRequestException"),
                    "case {:?}: expected InvalidRequestException, got {answer}",
                    row.case
                );
                let after = history_length(&router, row.case).await;
                assert_eq!(
                    after, before,
                    "case {:?}: a refused submission records no execution, got {after} where \
                     there were {before}",
                    row.case
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: the submission must be accepted, got {status} with {answer}",
                    row.case
                );
                assert!(
                    answer["QueryExecutionId"].is_string(),
                    "case {:?}: an accepted submission answers with an id, got {answer}",
                    row.case
                );
            }
        }
    }

    // The bucket is what the refusal rests on, so the SAME submission is accepted once the
    // client has created it.
    create_bucket(&router, ABSENT_BUCKET).await;
    for row in rows.iter().filter(|row| row.refused.is_some()) {
        let mut request = row.request.clone();
        request["QueryString"] = json!("SELECT 1 AS n");
        let (status, answer) = athena(&router, "StartQueryExecution", &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: the submission must be accepted once the bucket is there, got {status} \
             with {answer}",
            row.case
        );
        assert!(
            answer["QueryExecutionId"].is_string(),
            "case {:?}: an accepted submission answers with an id, got {answer}",
            row.case
        );
    }
}
