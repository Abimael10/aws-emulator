// SPDX-License-Identifier: CC0-1.0

//! CloudWatch Logs refuses an export task whose destination bucket does not let it make the
//! calls the export makes on that bucket, before it mints a task id.
//!
//! The rule under test: a create that hands work to another service checks, against the owning
//! service, that the permissions that work needs are granted — here the destination bucket's
//! own policy, which is the only document that can grant a service principal anything — and
//! answers the refusal of that class when one of them is not, leaving no export task behind.
//! The class is a PERMISSION refusal: `AccessDeniedException` (smithy_query shape
//! AccessDeniedException: "you don't have sufficient permissions to perform this action",
//! httpError 400), which is the code recorded CloudWatch Logs answers for a destination whose
//! policy withholds one of these calls — the bucket is named correctly and what is missing is a
//! grant in its policy, so it is not the incorrectly-stated-parameter refusal a bucket that is
//! not there takes (that half is the sibling suite's subject).
//! `CreateExportTask` states the requirement itself — "you must use credentials that have
//! permission to write to the S3 bucket that you specify as the destination" — and an export
//! establishes it, as the CloudWatch Logs service principal, by READING the destination
//! bucket's ACL and then WRITING its own check object under the destination prefix; the
//! refusal names whichever of those two calls the policy withheld, because that is the grant a
//! client has to go and write.
//!
//! The rows span what the rule generalises over: whether the bucket carries a policy at all,
//! WHO the policy grants to (this Region's CloudWatch Logs service principal, the
//! partition-wide spelling, another service, the account itself), WHICH of the two calls it
//! grants (each alone, both, an action wildcard over them), WHICH resource (the bucket, the
//! objects in it, a prefix of them, every resource), what it CONDITIONS the grant on (the
//! account the export is made in, another account, the log group it is made for, the canned ACL
//! the export writes with), and whether a `Deny` of one of those calls sits beside the grant.
//! The recorded cases — a bucket whose policy grants CloudWatch Logs nothing, and one that
//! grants it the ACL read alone — are two rows.
//!
//! This suite needs TWO services on one engine — CloudWatch Logs reaches S3 over the in-process
//! callback, and the policy a row writes has to be a policy Amazon S3 itself holds — which is
//! why it lives here rather than in either service's own crate, exactly as its sibling
//! `logs_export_task_destination_bucket.rs` does for the bucket's existence.

use std::sync::Arc;

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
const GROUP: &str = "/export/permission-gate";
const BUCKET: &str = "logs-export-permission-gate";

/// The wording each refusal carries, written out here rather than read from the service, so a
/// change to the implementation's own constants makes these assertions fail. Each names the
/// call the destination's policy withheld.
const ACL_READ_WITHHELD_MESSAGE: &str =
    "GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been \
     granted permission to perform this operation.";
/// See [`ACL_READ_WITHHELD_MESSAGE`].
const OBJECT_WRITE_WITHHELD_MESSAGE: &str =
    "PutObject call on the given bucket failed. Please check if CloudWatch Logs has been \
     granted permission to perform this operation.";

/// The `destinationPrefix` an export takes when the create names none — "if you don't specify
/// a value, the default is exportedlogs" (`CreateExportTask$destinationPrefix`).
const DEFAULT_PREFIX: &str = "exportedlogs";

/// The code a call this account's policies do not permit is refused under — the recorded answer
/// for a destination whose policy withholds one of the export's own calls, written out here
/// rather than read from the service.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region.
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

/// `PUT /<bucket>` — S3's `CreateBucket`, naming the Region this suite is signed for. The
/// bucket carries NO policy: what each row then puts on it is its own subject.
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
async fn put_bucket_policy(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    document: &str,
) {
    let (status, body) = s3(router, "PUT", &format!("{bucket}?policy"), document).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: PutBucketPolicy({bucket:?}) must succeed, got {status} with {body}"
    );
}

/// `DELETE /<bucket>?policy` — S3's `DeleteBucketPolicy`, which leaves the bucket carrying no
/// policy at all.
async fn delete_bucket_policy(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(router, "DELETE", &format!("{bucket}?policy"), "").await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: DeleteBucketPolicy({bucket:?}) must succeed, got {status} with {body}"
    );
}

/// The export tasks `DescribeExportTasks` reports, as `(taskId, destination)` pairs.
async fn export_tasks(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let page = logs(router, "describe", "DescribeExportTasks", &json!({})).await;
    page["exportTasks"]
        .as_array()
        .unwrap_or_else(|| panic!("DescribeExportTasks reports a list, got {page}"))
        .iter()
        .map(|task| {
            (
                task["taskId"].as_str().unwrap_or_default().to_string(),
                task["destination"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The awsJson error code of a refusal body (`__type`, which may carry a namespace).
fn error_code(body: &Value) -> String {
    let raw = body["__type"].as_str().unwrap_or_default();
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// The sentence a refusal carries, under either spelling of the member an AWS JSON error
/// states it in — which is how a client reads it.
fn refusal_message(body: &Value) -> &str {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
}

/// This account's own id, which a destination policy's `aws:SourceAccount` condition names.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// One statement of a destination policy, as its four elements. `action` and `resource` are
/// each one value or a list of them, the way an IAM statement writes them.
fn statement(principal: Value, action: Value, resource: Value) -> Value {
    json!({
        "Effect": "Allow",
        "Principal": principal,
        "Action": action,
        "Resource": resource,
    })
}

/// A policy document made of `statements`.
fn policy(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The service principal of this Region's CloudWatch Logs, as the CloudWatch Logs User Guide
/// writes a destination bucket's policy.
fn logs_principal() -> Value {
    json!({"Service": format!("logs.{REGION}.amazonaws.com")})
}

/// The bucket itself, and the objects under `prefix` in it, as the ARNs a statement names.
fn bucket_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}")
}

fn objects_arn(prefix: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{prefix}/*")
}

/// The two statements the CloudWatch Logs User Guide writes on an export destination, granting
/// `principal` the ACL read on the bucket and the write of the objects under `prefix`.
fn documented_grant(principal: Value, prefix: &str) -> Vec<Value> {
    vec![
        statement(
            principal.clone(),
            json!("s3:GetBucketAcl"),
            json!(bucket_arn()),
        ),
        statement(principal, json!("s3:PutObject"), json!(objects_arn(prefix))),
    ]
}

/// One statement with a `Condition` block on it.
fn conditioned(mut statement: Value, condition: Value) -> Value {
    statement["Condition"] = condition;
    statement
}

/// One row: the policy the destination bucket carries, the prefix the create names, and what
/// the create must then answer.
struct Row {
    case: &'static str,
    /// The document put on the bucket, or `None` for a bucket carrying no policy at all.
    policy: Option<String>,
    /// The `destinationPrefix` the create names, or `None` to take the documented default.
    prefix: Option<&'static str>,
    /// The sentence the create must be refused with, or `None` when it must be accepted.
    refusal: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    let bucket = json!(bucket_arn());
    let objects = json!(objects_arn(DEFAULT_PREFIX));
    let both_calls = json!(["s3:GetBucketAcl", "s3:PutObject"]);
    vec![
        Row {
            case: "a bucket carrying no policy at all",
            policy: None,
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            // One recorded case: a policy that grants CloudWatch Logs nothing.
            case: "a policy that grants the account itself everything and CloudWatch Logs \
                   nothing",
            policy: Some(policy(vec![statement(
                json!({"AWS": format!("arn:aws:iam::{}:root", account())}),
                json!("s3:*"),
                json!("*"),
            )])),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "the documented grant: the ACL read on the bucket and the write of its \
                   exported objects",
            policy: Some(policy(documented_grant(logs_principal(), DEFAULT_PREFIX))),
            prefix: None,
            refusal: None,
        },
        Row {
            // The other recorded case: the first call is granted and the second is not.
            case: "the ACL read alone, without the write the export makes",
            policy: Some(policy(vec![statement(
                logs_principal(),
                json!("s3:GetBucketAcl"),
                bucket.clone(),
            )])),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
        Row {
            case: "the write alone, without the ACL read the export makes first",
            policy: Some(policy(vec![statement(
                logs_principal(),
                json!("s3:PutObject"),
                objects.clone(),
            )])),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "the partition-wide CloudWatch Logs principal, granted both calls",
            policy: Some(policy(documented_grant(
                json!({"Service": "logs.amazonaws.com"}),
                DEFAULT_PREFIX,
            ))),
            prefix: None,
            refusal: None,
        },
        Row {
            case: "another service principal, granted both calls",
            policy: Some(policy(documented_grant(
                json!({"Service": "delivery.logs.amazonaws.com"}),
                DEFAULT_PREFIX,
            ))),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "the account's own root, granted both calls",
            policy: Some(policy(documented_grant(
                json!({"AWS": format!("arn:aws:iam::{}:root", account())}),
                DEFAULT_PREFIX,
            ))),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "an action wildcard covering both calls",
            policy: Some(policy(vec![statement(
                logs_principal(),
                json!("s3:*"),
                json!("*"),
            )])),
            prefix: None,
            refusal: None,
        },
        Row {
            case: "an action wildcard covering the ACL read alone",
            policy: Some(policy(vec![statement(
                logs_principal(),
                json!("s3:Get*"),
                json!("*"),
            )])),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
        Row {
            case: "both calls granted on every resource",
            policy: Some(policy(vec![statement(
                logs_principal(),
                both_calls.clone(),
                json!("*"),
            )])),
            prefix: None,
            refusal: None,
        },
        Row {
            case: "both calls granted on the objects rather than on the bucket",
            policy: Some(policy(vec![statement(
                logs_principal(),
                both_calls.clone(),
                objects.clone(),
            )])),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "both calls granted on the bucket rather than on its objects",
            policy: Some(policy(vec![statement(
                logs_principal(),
                both_calls.clone(),
                bucket.clone(),
            )])),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
        Row {
            case: "the write granted under the prefix the create names",
            policy: Some(policy(documented_grant(logs_principal(), "data"))),
            prefix: Some("data"),
            refusal: None,
        },
        Row {
            case: "the same prefix-scoped grant, with the create taking the default prefix",
            policy: Some(policy(documented_grant(logs_principal(), "data"))),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
        Row {
            case: "both calls conditioned on the account the export is made in",
            policy: Some(policy(
                documented_grant(logs_principal(), DEFAULT_PREFIX)
                    .into_iter()
                    .map(|one| {
                        conditioned(
                            one,
                            json!({"StringEquals": {"aws:SourceAccount": account()}}),
                        )
                    })
                    .collect(),
            )),
            prefix: None,
            refusal: None,
        },
        Row {
            case: "the ACL read conditioned on another account",
            policy: Some(policy(vec![
                conditioned(
                    statement(logs_principal(), json!("s3:GetBucketAcl"), bucket.clone()),
                    json!({"StringEquals": {"aws:SourceAccount": "999999999999"}}),
                ),
                statement(logs_principal(), json!("s3:PutObject"), objects.clone()),
            ])),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "both calls conditioned on the log groups of this account and Region",
            policy: Some(policy(
                documented_grant(logs_principal(), DEFAULT_PREFIX)
                    .into_iter()
                    .map(|one| {
                        conditioned(
                            one,
                            json!({"ArnLike": {
                                "aws:SourceArn":
                                    format!("arn:aws:logs:{REGION}:{}:log-group:*", account()),
                            }}),
                        )
                    })
                    .collect(),
            )),
            prefix: None,
            refusal: None,
        },
        Row {
            case: "the write conditioned on another log group",
            policy: Some(policy(vec![
                statement(logs_principal(), json!("s3:GetBucketAcl"), bucket.clone()),
                conditioned(
                    statement(logs_principal(), json!("s3:PutObject"), objects.clone()),
                    json!({"ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:logs:{REGION}:{}:log-group:/other/*", account()),
                    }}),
                ),
            ])),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
        Row {
            case: "the write conditioned on the canned ACL the export writes its objects with",
            policy: Some(policy(vec![
                statement(logs_principal(), json!("s3:GetBucketAcl"), bucket.clone()),
                conditioned(
                    statement(logs_principal(), json!("s3:PutObject"), objects.clone()),
                    json!({"StringEquals": {"s3:x-amz-acl": "bucket-owner-full-control"}}),
                ),
            ])),
            prefix: None,
            refusal: None,
        },
        Row {
            case: "the write conditioned on another canned ACL",
            policy: Some(policy(vec![
                statement(logs_principal(), json!("s3:GetBucketAcl"), bucket.clone()),
                conditioned(
                    statement(logs_principal(), json!("s3:PutObject"), objects.clone()),
                    json!({"StringEquals": {"s3:x-amz-acl": "private"}}),
                ),
            ])),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
        Row {
            case: "a deny of the ACL read beside the grant of both calls",
            policy: Some(policy({
                let mut statements = documented_grant(logs_principal(), DEFAULT_PREFIX);
                statements.push(json!({
                    "Effect": "Deny",
                    "Principal": logs_principal(),
                    "Action": "s3:GetBucketAcl",
                    "Resource": bucket.clone(),
                }));
                statements
            })),
            prefix: None,
            refusal: Some(ACL_READ_WITHHELD_MESSAGE),
        },
        Row {
            case: "a deny of the write beside the grant of both calls",
            policy: Some(policy({
                let mut statements = documented_grant(logs_principal(), DEFAULT_PREFIX);
                statements.push(json!({
                    "Effect": "Deny",
                    "Principal": logs_principal(),
                    "Action": "s3:PutObject",
                    "Resource": objects.clone(),
                }));
                statements
            })),
            prefix: None,
            refusal: Some(OBJECT_WRITE_WITHHELD_MESSAGE),
        },
    ]
}

/// An export task is created only for a destination whose own policy lets CloudWatch Logs make
/// both of the calls the export makes on that bucket, and a create it withholds one of is
/// refused naming that call.
#[tokio::test]
async fn an_export_task_is_created_only_for_a_destination_that_grants_the_exports_calls() {
    for row in rows() {
        // One emulator per row, so what the create is judged on is the row's policy and never
        // the account's one active export task.
        let router = build_emulator();
        let case = row.case;
        logs(&router, case, "CreateLogGroup", &json!({"logGroupName": GROUP})).await;
        create_bucket(&router, case, BUCKET).await;
        if let Some(document) = &row.policy {
            put_bucket_policy(&router, case, BUCKET, document).await;
        }

        let mut request = json!({
            "logGroupName": GROUP,
            "destination": BUCKET,
            "from": 0,
            "to": now_ms(),
        });
        if let Some(prefix) = row.prefix {
            request["destinationPrefix"] = json!(prefix);
        }
        let (status, body) = logs_call(&router, "CreateExportTask", &request).await;
        match row.refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a destination that grants both calls must be accepted, got \
                     {status} with {body}"
                );
                let task_id = body["taskId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an accepted create answers a taskId, got {body}")
                    })
                    .to_string();
                // Round trip: the task is readable back through DescribeExportTasks, which
                // answers from the same stored record, carrying the bucket it was created for.
                let reported = export_tasks(&router).await;
                assert!(
                    reported.contains(&(task_id, BUCKET.to_string())),
                    "case {case:?}: the created task must be reported against its bucket, got \
                     {reported:?}"
                );
            }
            Some(expected) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, ACCESS_DENIED),
                    "case {case:?}: a destination that withholds one of the export's calls must \
                     be refused 400 {ACCESS_DENIED}, got {status} with {body}"
                );
                assert_eq!(
                    refusal_message(&body),
                    expected,
                    "case {case:?}: expected the refusal naming the withheld call, got {body}"
                );
                // A refused create persists nothing: no export task at all is left behind.
                let reported = export_tasks(&router).await;
                assert!(
                    reported.is_empty(),
                    "case {case:?}: a refused create must mint no task, got {reported:?}"
                );
            }
        }
    }
}

/// The identical request, sent three times around the destination's policy: refused while the
/// bucket carries none, accepted once the grant is on it, and refused again once the grant is
/// taken off — so what decides is the policy the bucket carries and nothing about the request.
#[tokio::test]
async fn the_same_create_follows_the_destinations_policy() {
    let router = build_emulator();
    let case = "the same create around the destination's policy";
    logs(&router, case, "CreateLogGroup", &json!({"logGroupName": GROUP})).await;
    create_bucket(&router, case, BUCKET).await;
    let request = json!({
        "logGroupName": GROUP,
        "destination": BUCKET,
        "from": 0,
        "to": now_ms(),
    });

    let (status, body) = logs_call(&router, "CreateExportTask", &request).await;
    assert_eq!(
        (status, refusal_message(&body)),
        (400, ACL_READ_WITHHELD_MESSAGE),
        "case {case:?}: before the grant the create must be refused, got {status} with {body}"
    );

    let grant = policy(documented_grant(logs_principal(), DEFAULT_PREFIX));
    put_bucket_policy(&router, case, BUCKET, &grant).await;
    let (status, body) = logs_call(&router, "CreateExportTask", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: with the grant on the bucket the identical create must succeed, got \
         {status} with {body}"
    );
    let task_id = body["taskId"].as_str().unwrap_or_default().to_string();
    assert!(
        export_tasks(&router)
            .await
            .contains(&(task_id.clone(), BUCKET.to_string())),
        "case {case:?}: the accepted create must be reported against the bucket it names"
    );

    delete_bucket_policy(&router, case, BUCKET).await;
    let (status, body) = logs_call(&router, "CreateExportTask", &request).await;
    assert_eq!(
        (status, refusal_message(&body)),
        (400, ACL_READ_WITHHELD_MESSAGE),
        "case {case:?}: with the grant taken off the create must be refused again, got {status} \
         with {body}"
    );
    let reported = export_tasks(&router).await;
    assert_eq!(
        reported,
        vec![(task_id, BUCKET.to_string())],
        "case {case:?}: the refused creates must leave the one accepted task as all there is, \
         got {reported:?}"
    );
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
