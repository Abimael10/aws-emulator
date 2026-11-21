// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER may write a query's results where its submission resolved they go.
//!
//! The rule under test: Athena "stores query results and metadata information for each query
//! that runs in a query result location that you can specify", and that location is a bucket
//! of the caller's own account — so the object write that saves a query's results is made
//! with the CALLER's permissions and is decided by exactly the two documents Amazon S3 would
//! decide it by: the identity-based policies in force on the caller, and the destination
//! bucket's own policy. An explicit `Deny` in either refuses the write whatever any grant
//! says; otherwise the write needs an `Allow` in one of them. A write the policies in force
//! refuse is a write that does not happen, so the execution settles FAILED carrying
//! `Access denied when writing to location: <the object it would have written>` and leaves
//! no object behind — rather than reporting a success whose results were never persisted.
//!
//! Athena and Amazon S3 are mounted on ONE shared engine and driven over the wire the
//! production front door uses, with the IAM store carrying each row's own principal, so each
//! row signs as the principal its policies are written for and what is asserted is what an
//! SDK client observes of both services.
//!
//! The suite is a table: the rows differ along where the decision comes from (the identity's
//! documents, the bucket's policy, both, neither) and which way it goes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket whose results the rows write, which carries no policy of its own.
const BUCKET: &str = "athena-write-results";

/// A bucket whose own policy GRANTS one principal the object write.
const GRANTING_BUCKET: &str = "athena-write-granted";

/// A bucket whose own policy DENIES the object write to everybody.
const DENYING_BUCKET: &str = "athena-write-denied";

/// The prefix under each bucket a row's results land at.
const PREFIX: &str = "results";

/// The access key id of a caller neither STS nor IAM holds a record for, which is the
/// position the account's own published credential is in.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The statement every row runs: one the catalog resolves nothing for, so what decides each
/// row is the result write alone.
const SQL: &str = "SELECT 1 AS n";

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` header naming `service` in its credential scope, signed with
/// `access_key_id` — which is what names the principal the request speaks for. Nothing is
/// verified: this harness turns request authentication off.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (engine, router)
}

/// One Athena call signed with `access_key_id`.
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
        .header("authorization", authorization("athena", access_key_id))
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

/// One Amazon S3 request as the account, and its status and body as text.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3", ACCOUNT_KEY_ID))
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The ARN of the IAM user `name`, as this account holds it.
fn user_arn(name: &str) -> String {
    format!(
        "arn:aws:iam::{}:user/{name}",
        engine::emulator_account_id()
    )
}

/// One principal under test: the key it signs with, the documents in force on it, the bucket
/// it writes its results into, and whether the write is permitted.
struct Writer {
    /// What the row is.
    case: &'static str,
    /// The access key id the row signs with.
    key: &'static str,
    /// The IAM user the key belongs to, or `None` for the account's own published
    /// credential, which the IAM store holds no record for.
    name: Option<&'static str>,
    /// The statements of the user's inline policy, beside the `athena:*` grant every row
    /// needs to submit at all.
    statements: Vec<Value>,
    /// The bucket the row's results go to.
    bucket: &'static str,
    /// Whether the policies in force permit the result write.
    permitted: bool,
}

/// A query's results are written with the CALLER's permissions: the identity-based documents
/// in force on it and the destination bucket's own policy decide the write, and a query whose
/// result object the caller may not write settles FAILED naming that object and leaves
/// nothing behind.
#[tokio::test]
async fn a_query_whose_result_object_the_caller_may_not_write_fails_and_saves_nothing() {
    let (engine, router) = emulator();
    for bucket in [BUCKET, GRANTING_BUCKET, DENYING_BUCKET] {
        let (status, body) = s3(&router, "PUT", &format!("/{bucket}"), b"").await;
        assert!(
            (200..300).contains(&status),
            "setup: creating s3://{bucket} must succeed, got {status} with {body}"
        );
    }

    let writers: Vec<Writer> = vec![
        Writer {
            case: "the account's own published credential",
            key: ACCOUNT_KEY_ID,
            name: None,
            statements: Vec::new(),
            bucket: BUCKET,
            permitted: true,
        },
        Writer {
            case: "a principal allowed the object write",
            key: "AKIAEXAMPLEWRITEALLOWED",
            name: Some("results-allowed"),
            statements: vec![
                json!({ "Effect": "Allow", "Action": "s3:PutObject", "Resource": "*" }),
            ],
            bucket: BUCKET,
            permitted: true,
        },
        Writer {
            case: "a principal holding no object write at all",
            key: "AKIAEXAMPLEWRITENONE",
            name: Some("results-none"),
            statements: Vec::new(),
            bucket: BUCKET,
            permitted: false,
        },
        Writer {
            case: "a principal whose policy denies the object write explicitly",
            key: "AKIAEXAMPLEWRITEDENIED",
            name: Some("results-denied"),
            statements: vec![
                json!({ "Effect": "Allow", "Action": "s3:*", "Resource": "*" }),
                json!({ "Effect": "Deny", "Action": "s3:PutObject", "Resource": "*" }),
            ],
            bucket: BUCKET,
            permitted: false,
        },
        Writer {
            case: "a principal the destination bucket's own policy allows",
            key: "AKIAEXAMPLEWRITEGRANTED",
            name: Some("results-granted"),
            statements: Vec::new(),
            bucket: GRANTING_BUCKET,
            permitted: true,
        },
        Writer {
            case: "a destination bucket whose policy denies the write to everybody",
            key: "AKIAEXAMPLEWRITEBLOCKED",
            name: Some("results-blocked"),
            statements: vec![
                json!({ "Effect": "Allow", "Action": "s3:PutObject", "Resource": "*" }),
            ],
            bucket: DENYING_BUCKET,
            permitted: false,
        },
    ];

    // Each row signs as its OWN principal, so one emulator carries every document under
    // test. The IAM records are written before any bucket policy is installed, because a
    // policy naming a principal is stored only for one this account holds.
    {
        let minted = ::aws_smithy_types::DateTime::from_secs(0);
        let store = iam::service_state(&*engine).expect("the IAM store");
        let mut store = store.lock();
        for writer in &writers {
            let Some(name) = writer.name else { continue };
            store.users.insert(
                iam::name_key(name),
                ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
                    path: "/".to_string(),
                    user_name: name.to_string(),
                    user_id: format!(
                        "AIDAEXAMPLE{}",
                        name.to_ascii_uppercase().replace('-', "")
                    ),
                    arn: user_arn(name),
                    create_date: minted,
                    password_last_used: None,
                    permissions_boundary: None,
                    tags: None,
                },
            );
            let mut statements =
                vec![json!({ "Effect": "Allow", "Action": "athena:*", "Resource": "*" })];
            statements.extend(writer.statements.iter().cloned());
            store.user_inline_policies.insert(
                iam::name_key(name),
                BTreeMap::from([(
                    iam::name_key("inline"),
                    iam::InlinePolicy {
                        policy_name: "inline".to_string(),
                        policy_document: json!({
                            "Version": "2012-10-17",
                            "Statement": statements,
                        })
                        .to_string(),
                    },
                )]),
            );
            store.access_keys.insert(
                writer.key.to_string(),
                iam::AccessKeyRecord {
                    user_key: iam::name_key(name),
                    status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
                    create_date: minted,
                },
            );
        }
    }

    // The bucket whose policy GRANTS the write names the one principal it grants it to, and
    // the bucket whose policy DENIES it names every principal.
    for (bucket, document) in [
        (
            GRANTING_BUCKET,
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": user_arn("results-granted") },
                    "Action": "s3:PutObject",
                    "Resource": format!("arn:aws:s3:::{GRANTING_BUCKET}/*"),
                }],
            }),
        ),
        (
            DENYING_BUCKET,
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "s3:PutObject",
                    "Resource": format!("arn:aws:s3:::{DENYING_BUCKET}/*"),
                }],
            }),
        ),
    ] {
        let (status, body) = s3(
            &router,
            "PUT",
            &format!("/{bucket}?policy"),
            document.to_string().as_bytes(),
        )
        .await;
        assert!(
            (200..300).contains(&status),
            "setup: the policy of s3://{bucket} must be stored, got {status} with {body}"
        );
    }

    // Every row's query is started before any of them is read back, so the span a
    // completion is owed after elapses once for the whole table.
    let mut started: Vec<(&Writer, String)> = Vec::new();
    for writer in &writers {
        let (status, answer) = athena_as(
            &router,
            writer.key,
            "StartQueryExecution",
            &json!({
                "QueryString": SQL,
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{}/{PREFIX}/", writer.bucket)
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: StartQueryExecution must be accepted, got {status} with {answer}",
            writer.case
        );
        let id = answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {:?}: expected a query execution id, got {answer}", writer.case)
            })
            .to_string();
        started.push((writer, id));
    }

    for (writer, id) in &started {
        let case = writer.case;
        let object = format!("{PREFIX}/{id}.csv");
        let status = settled_status(&router, case, id).await;
        let state = status["State"].as_str().unwrap_or_default();
        let (served, body) =
            s3(&router, "GET", &format!("/{}/{object}", writer.bucket), b"").await;
        if writer.permitted {
            assert_eq!(
                state, QUERY_STATE_SUCCEEDED,
                "case {case:?}: a permitted write must leave the query successful, got {status}"
            );
            assert_eq!(
                served, 200,
                "case {case:?}: the result object s3://{}/{object} must be served, got {served} \
                 with {body}",
                writer.bucket
            );
            continue;
        }
        assert_eq!(
            state, QUERY_STATE_FAILED,
            "case {case:?}: a refused write must fail the query, got {status}"
        );
        let expected = format!(
            "Access denied when writing to location: s3://{}/{object}",
            writer.bucket
        );
        assert_eq!(
            status["StateChangeReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the reason {expected:?}, got {status}"
        );
        assert_eq!(
            status["AthenaError"]["ErrorMessage"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: the error carries that same reason, got {status}"
        );
        assert_eq!(
            served, 404,
            "case {case:?}: a refused write leaves no object at s3://{}/{object}, got {served} \
             with {body}",
            writer.bucket
        );
    }
}

/// The `Status` of `id` once the execution is TERMINAL, polled until it is or
/// [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so what is
/// waited for is the condition and never a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = athena_as(
            router,
            ACCOUNT_KEY_ID,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution must answer 200, got {status} with {body}"
        );
        let reported = body["QueryExecution"]["Status"].clone();
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}
