// SPDX-License-Identifier: CC0-1.0

//! Which CLASS CloudWatch Logs refuses an export task under when the destination bucket it
//! names cannot take the export.
//!
//! The rule under test: a create that must act on another service's resource refuses a GRANT
//! that resource's own policy withholds under the AWS-common permission class
//! `AccessDeniedException` (HTTP 400), naming the withheld call, and keeps the
//! incorrectly-stated-parameter class (`InvalidParameterException`, HTTP 400) for a destination
//! that names no such resource at all — so a caller branching on the refusal tells "go and
//! grant the destination's policy" from "the request names something that is not there". Either
//! refusal mints no task, and the same create against a destination that grants both of the
//! export's calls is accepted and the export runs to its terminal status.
//!
//! `CreateExportTask` states the requirement itself — "you must use credentials that have
//! permission to write to the S3 bucket that you specify as the destination" — and an export
//! establishes it, as the CloudWatch Logs service principal, by READING the destination
//! bucket's ACL and then WRITING under the destination prefix; the sentence names whichever of
//! those two calls the policy withheld, because that is the grant a client has to go and write.
//! The operation's own modelled error set carries no permission code (smithy_query: operation
//! CreateExportTask gives InvalidParameterException / LimitExceededException /
//! OperationAbortedException / ResourceAlreadyExistsException / ResourceNotFoundException /
//! ServiceUnavailableException), and CloudWatch Logs answers the common permission code for
//! this condition regardless.
//!
//! The rows span what the rule generalises over: whether the destination exists at all, and
//! WHICH of the two calls its policy grants — neither, each one alone, and both. The recorded
//! cases — a bucket whose policy grants CloudWatch Logs nothing, and one that grants the ACL
//! read alone — are two of them.
//!
//! This suite needs TWO services on one engine: the policy a row writes has to be a policy
//! Amazon S3 itself holds, and CloudWatch Logs reads it over the in-process callback. Which is
//! why it lives here, exactly as its siblings `logs_export_task_destination_permission.rs` and
//! `logs_export_task_destination_bucket.rs` do.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "eu-west-2";

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// Amazon S3's signing name, whose REST calls carry no `X-Amz-Target`.
const S3_SIGNING_NAME: &str = "s3";

/// The log group every export below reads from, and the bucket every export names.
const GROUP: &str = "/export/refusal-class";
const BUCKET: &str = "logs-export-refusal-class";

/// The two codes a create is refused under here, written out rather than read from the service:
/// the AWS-common permission class, and the class a destination that is not there takes.
const ACCESS_DENIED: &str = "AccessDeniedException";
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence each refusal carries, written out here so a change to the implementation's own
/// constants makes these assertions fail. Each names the call the destination's policy withheld,
/// or states that the destination is not there; all three are fixed text.
const ACL_READ_WITHHELD_MESSAGE: &str =
    "GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been \
     granted permission to perform this operation.";
/// See [`ACL_READ_WITHHELD_MESSAGE`].
const OBJECT_WRITE_WITHHELD_MESSAGE: &str =
    "PutObject call on the given bucket failed. Please check if CloudWatch Logs has been \
     granted permission to perform this operation.";
/// See [`ACL_READ_WITHHELD_MESSAGE`].
const BUCKET_MISSING_MESSAGE: &str =
    "The given bucket does not exist. Please make sure the bucket is valid.";

/// The `destinationPrefix` an export takes when the create names none — "if you don't specify a
/// value, the default is exportedlogs" (`CreateExportTask$destinationPrefix`).
const DEFAULT_PREFIX: &str = "exportedlogs";

/// The status an export that delivered its data settles at, and the statuses an export settles
/// at at all (`smithy_query(query="shape",shape="ExportTaskStatusCode")` gives CANCELLED /
/// COMPLETED / FAILED / PENDING / PENDING_CANCEL / RUNNING; the last three are not terminal).
const COMPLETED: &str = "COMPLETED";
const TERMINAL_STATUSES: [&str; 3] = ["COMPLETED", "CANCELLED", "FAILED"];

/// How long a settled status is waited for, and how often it is looked for again. The deadline
/// is generous: what is asserted is the CONDITION the export reaches, never how fast the host
/// got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope is
    // still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// The SigV4 header a request carries, naming the signing name the router routes on.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
    )
}

/// One CloudWatch Logs awsJson1_1 call: its HTTP status and parsed body.
async fn logs_call(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{LOGS_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(LOGS_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudWatch Logs call that must succeed.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, parsed) = logs_call(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: logs {operation} {request} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One path-style Amazon S3 request: its HTTP status and body text.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let http_request = http::Request::builder()
        .method(method)
        .uri(format!("/{path}"))
        .header("host", format!("{S3_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(S3_SIGNING_NAME))
        .body(Bytes::copy_from_slice(body.as_bytes()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// `PUT /<bucket>` — S3's `CreateBucket`, naming the Region this suite is signed for. The bucket
/// carries NO policy: what each row then puts on it is its own subject.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(
        router,
        "PUT",
        bucket,
        &format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {body}"
    );
}

/// `PUT /<bucket>?policy` — S3's `PutBucketPolicy`, with `document` as the body.
async fn put_bucket_policy(router: &MultiServiceRouter, case: &str, bucket: &str, document: &str) {
    let (status, body) = s3(router, "PUT", &format!("{bucket}?policy"), document).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: PutBucketPolicy({bucket:?}) must succeed, got {status} with {body}"
    );
}

/// The export tasks `DescribeExportTasks` reports, as `(taskId, statusCode)` pairs.
async fn export_tasks(router: &MultiServiceRouter, case: &str) -> Vec<(String, String)> {
    let page = logs(router, case, "DescribeExportTasks", &json!({})).await;
    page["exportTasks"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeExportTasks reports a list, got {page}"))
        .iter()
        .map(|task| {
            (
                task["taskId"].as_str().unwrap_or_default().to_string(),
                task["status"]["code"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The awsJson error code of a refusal body (`__type`, which may carry a namespace).
fn error_code(body: &Value) -> String {
    let raw = body["__type"].as_str().unwrap_or_default();
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// The sentence a refusal carries, under either spelling of the member an AWS JSON error states
/// it in — which is how a client reads it.
fn refusal_message(body: &Value) -> &str {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
}

/// One `Allow` statement of a destination policy, granting this Region's CloudWatch Logs service
/// principal `action` on `resource`.
fn granting(action: &str, resource: String) -> Value {
    json!({
        "Effect": "Allow",
        "Principal": {"Service": format!("logs.{REGION}.amazonaws.com")},
        "Action": action,
        "Resource": resource,
    })
}

/// A policy document made of `statements`.
fn policy(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The bucket itself, as the ARN the ACL-read statement names.
fn bucket_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}")
}

/// Everything the export writes under the default prefix, as the ARN the write statement names.
fn objects_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}/{DEFAULT_PREFIX}/*")
}

/// The statement granting the ACL read the export makes on the bucket itself, as the CloudWatch
/// Logs User Guide writes a destination policy.
fn acl_read_grant() -> Value {
    granting("s3:GetBucketAcl", bucket_arn())
}

/// The statement granting the write the export makes under the destination prefix.
fn object_write_grant() -> Value {
    granting("s3:PutObject", objects_arn())
}

/// What a create against a destination must be answered with: a refusal, in its class and its
/// sentence, or acceptance.
enum Expected {
    Refused {
        code: &'static str,
        message: &'static str,
    },
    Accepted,
}

/// One row: the destination's state, and what the identical create must then answer.
struct Row {
    case: &'static str,
    /// Whether the destination bucket is created at all.
    bucket_exists: bool,
    /// The policy put on the bucket, or `None` for a bucket carrying no policy.
    policy: Option<String>,
    expected: Expected,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bucket carrying no policy at all",
            bucket_exists: true,
            policy: None,
            // Nothing is granted, so the FIRST call the export makes is the one refused.
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                message: ACL_READ_WITHHELD_MESSAGE,
            },
        },
        Row {
            case: "a policy granting the ACL read alone",
            bucket_exists: true,
            policy: Some(policy(vec![acl_read_grant()])),
            // The ACL read is admitted, so the refusal names the call after it.
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                message: OBJECT_WRITE_WITHHELD_MESSAGE,
            },
        },
        Row {
            case: "a policy granting the object write alone",
            bucket_exists: true,
            policy: Some(policy(vec![object_write_grant()])),
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                message: ACL_READ_WITHHELD_MESSAGE,
            },
        },
        Row {
            case: "a policy granting both of the export's calls",
            bucket_exists: true,
            policy: Some(policy(vec![acl_read_grant(), object_write_grant()])),
            expected: Expected::Accepted,
        },
        Row {
            case: "a destination naming no bucket at all",
            bucket_exists: false,
            policy: None,
            // Nothing exists to carry a grant, so what the request stated is what is wrong.
            expected: Expected::Refused {
                code: INVALID_PARAMETER,
                message: BUCKET_MISSING_MESSAGE,
            },
        },
    ]
}

/// A withheld grant on the destination is refused under the permission class naming the withheld
/// call, a destination that is not there under the incorrectly-stated-parameter class, and the
/// same create is accepted once both of the export's calls are granted.
#[tokio::test]
async fn the_refusal_class_separates_a_withheld_grant_from_a_destination_that_is_not_there() {
    for row in rows() {
        // One emulator per row, so what the create is judged on is the row's destination and
        // never the account's one active export task.
        let router = build_emulator();
        let case = row.case;
        logs(&router, case, "CreateLogGroup", &json!({"logGroupName": GROUP})).await;
        if row.bucket_exists {
            create_bucket(&router, case, BUCKET).await;
        }
        if let Some(document) = &row.policy {
            put_bucket_policy(&router, case, BUCKET, document).await;
        }

        let request = json!({
            "logGroupName": GROUP,
            "destination": BUCKET,
            "from": 0,
            "to": now_ms(),
        });
        let (status, body) = logs_call(&router, "CreateExportTask", &request).await;
        match row.expected {
            Expected::Refused { code, message } => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, code),
                    "case {case:?}: expected the create refused 400 {code}, got {status} with \
                     {body}"
                );
                assert_eq!(
                    refusal_message(&body),
                    message,
                    "case {case:?}: expected the refusal {message:?}, got {body}"
                );
                // A refused create persists nothing: no export task at all is left behind.
                let reported = export_tasks(&router, case).await;
                assert!(
                    reported.is_empty(),
                    "case {case:?}: a refused create must mint no task, got {reported:?}"
                );
            }
            Expected::Accepted => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a destination granting both calls must be accepted, got \
                     {status} with {body}"
                );
                let task_id = body["taskId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an accepted create answers a taskId, got {body}")
                    })
                    .to_string();
                // Round trip: the task is readable back through DescribeExportTasks, and the
                // export it started delivers — so the status it settles at is the one an export
                // that wrote its data reports.
                let settled = settled_status(&router, case, &task_id).await;
                assert_eq!(
                    settled, COMPLETED,
                    "case {case:?}: the accepted export must settle at {COMPLETED}, got \
                     {settled:?}"
                );
            }
        }
    }
}

/// Look at `task_id` again until `DescribeExportTasks` reports it at a status an export settles
/// at, and answer that status. The wait is on that CONDITION and bounded by a deadline no host
/// needs.
async fn settled_status(router: &MultiServiceRouter, case: &str, task_id: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = export_tasks(router, case).await;
        let found = reported
            .iter()
            .find(|(id, _)| id == task_id)
            .map(|(_, status)| status.clone());
        let status = found.unwrap_or_else(|| {
            panic!("case {case:?}: the created task {task_id} must be reported, got {reported:?}")
        });
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the export task {task_id} never settled; it last reported {status:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The window's end, in the epoch milliseconds `CreateExportTask$to` carries.
fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}
