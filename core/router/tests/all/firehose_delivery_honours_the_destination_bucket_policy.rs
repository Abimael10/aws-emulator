// SPDX-License-Identifier: CC0-1.0

//! A Firehose delivery is withheld by the DESTINATION BUCKET's own policy, and the refusal is
//! reported through the destination's `CloudWatchLoggingOptions`.
//!
//! The rule under test: a delivered object is written under the role the destination
//! configuration names ("the Amazon Resource Name (ARN) of the Amazon Web Services
//! credentials" — `S3DestinationConfiguration$RoleARN`), so the write is an ordinary
//! `s3:PutObject` by a principal of this account and TWO documents are in force on it — that
//! role's identity-based policies and the destination bucket's own policy. Within one account
//! IAM weighs them together: an explicit `Deny` from either side refuses the write, an `Allow`
//! from either grants it, and a write neither allows falls to the implicit deny. A write the
//! permissions withhold writes no object, leaves the records buffered, and — where the
//! destination's `CloudWatchLoggingOptions` are enabled — is reported as a log event in the
//! `LogGroupName` / `LogStreamName` they name, carrying the stream, the destination, the
//! stream's version and the `errorCode` `S3.AccessDenied`; a destination that took the delivery
//! reports nothing at all, which is what tells a misconfigured stream from a healthy one.
//!
//! Only a cross-service suite can pin this: the delivery is Firehose's, the bucket, its policy
//! and the delivered object are Amazon S3's, the role and the decision about its own documents
//! are IAM's, and the reported event is read back through CloudWatch Logs' own `GetLogEvents` —
//! four services over ONE engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: which side states the permission (the
//! role's own documents, the bucket's policy, both, neither), which effect that statement
//! carries, and what it is about (this write, another action, another bucket's objects, another
//! principal, the role's whole account). Each row is driven by its own test over its own
//! emulator, so no row waits on another and none of them is long-running.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::events_rule_firehose_target_delivery::{given_role, send, REGION};
use super::firehose_buffer_flush_carries_mid_interval_arrivals::{json_call, keys_under};

/// The log group every row's destination reports its failed deliveries into.
const LOG_GROUP: &str = "/aws/kinesisfirehose/bucket-policy";

/// The prefix every row's objects are delivered under.
const PREFIX: &str = "data/";

/// The role carrying an identity policy that allows the object write.
const GRANTED_ROLE: &str = "firehose-bucket-policy-granted";

/// The role carrying no identity policy at all, so its own documents state nothing about the
/// write.
const SILENT_ROLE: &str = "firehose-bucket-policy-silent";

/// A bucket no row delivers into, so a row can deny the write on a bucket that is not its
/// destination.
const OTHER_BUCKET: &str = "firehose-bucket-policy-elsewhere";

/// How long a row waits for the object or the event it expects: far longer than the emulator's
/// own delivery span, so a loaded host never fails a row the clock would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How often the destination, the log stream and a stream's own status are looked at while
/// waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The status a stream that can take a record reports.
const ACTIVE: &str = "ACTIVE";

/// Which role a row's destination delivers under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The role whose own identity policy allows the object write.
    Granted,
    /// The role carrying no identity policy, so only the bucket can state anything.
    Silent,
}

/// One row: who delivers, what the destination bucket's policy says, and what the destination
/// then holds and reports.
struct Row {
    case: &'static str,
    /// Names this row's bucket, stream and log stream, so rows cannot collide.
    name: &'static str,
    role: Role,
    /// The statement this row's destination bucket carries, as `(effect, principal, action,
    /// resource)` rendered against the row's own names, or `None` for a bucket carrying no
    /// policy at all.
    policy: Option<fn(&Row, &str, &str) -> Value>,
    /// Whether the delivery reaches the destination bucket.
    delivers: bool,
    /// The `errorCode` the destination's logging reports, or `None` where it reports nothing.
    reported: Option<&'static str>,
}

/// This row's bucket, stream and log stream, all named after it.
fn bucket_of(row: &Row) -> String {
    format!("firehose-bucket-policy-{}", row.name)
}

/// A bucket policy with one statement.
fn statement(effect: &str, principal: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "AWS": principal },
            "Action": action,
            "Resource": resource,
        }],
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a deny of the write naming the delivery role",
            name: "denied-role",
            role: Role::Granted,
            policy: Some(|row, granted, _silent| {
                statement(
                    "Deny",
                    granted,
                    "s3:PutObject",
                    &format!("arn:aws:s3:::{}/*", bucket_of(row)),
                )
            }),
            delivers: false,
            reported: Some("S3.AccessDenied"),
        },
        Row {
            case: "a deny of the write naming the delivery role's account",
            name: "denied-account",
            role: Role::Granted,
            policy: Some(|row, _granted, _silent| {
                statement(
                    "Deny",
                    &format!("arn:aws:iam::{}:root", engine::emulator_account_id()),
                    "s3:PutObject",
                    &format!("arn:aws:s3:::{}/*", bucket_of(row)),
                )
            }),
            delivers: false,
            reported: Some("S3.AccessDenied"),
        },
        Row {
            case: "a bucket policy granting a role whose own documents state nothing",
            name: "granted-by-bucket",
            role: Role::Silent,
            policy: Some(|row, _granted, silent| {
                statement(
                    "Allow",
                    silent,
                    "s3:PutObject",
                    &format!("arn:aws:s3:::{}/*", bucket_of(row)),
                )
            }),
            delivers: true,
            reported: None,
        },
        Row {
            case: "a bucket policy and an identity policy that both allow the write",
            name: "granted-by-both",
            role: Role::Granted,
            policy: Some(|row, granted, _silent| {
                statement(
                    "Allow",
                    granted,
                    "s3:PutObject",
                    &format!("arn:aws:s3:::{}/*", bucket_of(row)),
                )
            }),
            delivers: true,
            reported: None,
        },
        Row {
            case: "a deny of another action",
            name: "denied-other-action",
            role: Role::Granted,
            policy: Some(|row, granted, _silent| {
                statement(
                    "Deny",
                    granted,
                    "s3:GetObject",
                    &format!("arn:aws:s3:::{}/*", bucket_of(row)),
                )
            }),
            delivers: true,
            reported: None,
        },
        Row {
            case: "a deny scoped to another bucket's objects",
            name: "denied-other-bucket",
            role: Role::Granted,
            policy: Some(|_row, granted, _silent| {
                statement(
                    "Deny",
                    granted,
                    "s3:PutObject",
                    &format!("arn:aws:s3:::{OTHER_BUCKET}/*"),
                )
            }),
            delivers: true,
            reported: None,
        },
        Row {
            case: "a deny naming another principal",
            name: "denied-other-role",
            role: Role::Granted,
            policy: Some(|row, _granted, silent| {
                statement(
                    "Deny",
                    silent,
                    "s3:PutObject",
                    &format!("arn:aws:s3:::{}/*", bucket_of(row)),
                )
            }),
            delivers: true,
            reported: None,
        },
        Row {
            case: "neither side states anything about the write",
            name: "stated-nowhere",
            role: Role::Silent,
            policy: None,
            delivers: false,
            reported: Some("S3.AccessDenied"),
        },
    ]
}

/// `PUT /<bucket>` — the destination bucket of a row, in the Region this suite signs for.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: the bucket {bucket:?} must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// `PUT /<bucket>?policy` — `PutBucketPolicy` with `document` as the body.
async fn put_bucket_policy(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    document: &Value,
) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}?policy"),
        &[("content-type", "application/json")],
        document.to_string().into_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: the policy of {bucket:?} must be attached, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// The events `LOG_GROUP`/`stream` holds, read through CloudWatch Logs' own `GetLogEvents`.
///
/// A stream the account does not hold holds no event, which is what a group nothing was ever
/// reported into looks like.
async fn log_events(router: &MultiServiceRouter, stream: &str) -> Vec<Value> {
    let (status, body) = send(
        router,
        "logs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "Logs_20140328.GetLogEvents"),
        ],
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        })
        .to_string()
        .into_bytes(),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    let answered: Value =
        serde_json::from_slice(&body).expect("GetLogEvents answers a JSON document");
    answered["events"].as_array().cloned().unwrap_or_default()
}

/// The event document one reported delivery error carries, parsed out of the event's message.
fn reported_document(case: &str, event: &Value) -> Value {
    let message = event["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a reported event carries a message"));
    serde_json::from_str(message).unwrap_or_else(|error| {
        panic!("case {case:?}: the event is a JSON document: {error} in {message:?}")
    })
}

/// The row of the table named `name` — one row is one test below, so a row's own emulator,
/// bucket and stream are its own and no row waits on another.
fn row(name: &str) -> Row {
    rows()
        .into_iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("the table holds a row named {name:?}"))
}

#[tokio::test]
async fn a_bucket_policy_denying_the_delivery_role_withholds_the_write() {
    drive(&row("denied-role")).await;
}

#[tokio::test]
async fn a_bucket_policy_denying_the_roles_account_withholds_the_write() {
    drive(&row("denied-account")).await;
}

#[tokio::test]
async fn a_bucket_policy_granting_the_role_delivers_where_its_own_documents_are_silent() {
    drive(&row("granted-by-bucket")).await;
}

#[tokio::test]
async fn a_write_both_sides_allow_is_delivered() {
    drive(&row("granted-by-both")).await;
}

#[tokio::test]
async fn a_bucket_policy_denying_another_action_withholds_nothing() {
    drive(&row("denied-other-action")).await;
}

#[tokio::test]
async fn a_bucket_policy_denying_another_buckets_objects_withholds_nothing() {
    drive(&row("denied-other-bucket")).await;
}

#[tokio::test]
async fn a_bucket_policy_denying_another_principal_withholds_nothing() {
    drive(&row("denied-other-role")).await;
}

#[tokio::test]
async fn a_write_neither_side_states_anything_about_is_withheld() {
    drive(&row("stated-nowhere")).await;
}

/// Drive ONE row end to end on its own emulator: the roles, the destination bucket and its
/// policy, the stream, one put, and then what the destination holds and what its logging
/// reports.
async fn drive(row: &Row) {
    let case = row.case;
    let router = {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        router
    };
    // Both roles exist in IAM before any policy names one: a bucket policy naming a principal
    // this account does not hold is one S3 does not take. The group the destination reports
    // into is created here too, so the row reads it back through its owner's own reader.
    let granted = given_role(
        &router,
        case,
        GRANTED_ROLE,
        "firehose.amazonaws.com",
        Some("s3:PutObject"),
    )
    .await;
    let silent = given_role(&router, case, SILENT_ROLE, "firehose.amazonaws.com", None).await;
    json_call(
        &router,
        case,
        "logs",
        "Logs_20140328.CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    // A bucket no row delivers into, so a statement can be scoped to a bucket that is not this
    // destination and so that nothing may reach it.
    create_bucket(&router, case, OTHER_BUCKET).await;

    let bucket = bucket_of(row);
    create_bucket(&router, case, &bucket).await;
    if let Some(policy) = row.policy {
        put_bucket_policy(&router, case, &bucket, &policy(row, &granted, &silent)).await;
    }
    let role_arn = match row.role {
        Role::Granted => &granted,
        Role::Silent => &silent,
    };
    let created = json_call(
        &router,
        case,
        "firehose",
        "Firehose_20150804.CreateDeliveryStream",
        json!({
            "DeliveryStreamName": row.name,
            "DeliveryStreamType": "DirectPut",
            "ExtendedS3DestinationConfiguration": {
                "RoleARN": role_arn,
                "BucketARN": format!("arn:aws:s3:::{bucket}"),
                "Prefix": PREFIX,
                // The shortest buffering the hints admit, so one put is one delivery attempt.
                "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                "CloudWatchLoggingOptions": {
                    "Enabled": true,
                    "LogGroupName": LOG_GROUP,
                    "LogStreamName": row.name,
                },
            },
        }),
    )
    .await;
    let stream_arn = created["DeliveryStreamARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create answers the stream's ARN"))
        .to_string();
    // The stream is waited for by its own reported STATUS, never by the clock: a stream still
    // `CREATING` takes no record.
    active(&router, case, row.name).await;

    json_call(
        &router,
        case,
        "firehose",
        "Firehose_20150804.PutRecord",
        json!({
            "DeliveryStreamName": row.name,
            "Record": {
                "Data": aws_smithy_types::base64::encode(
                    format!("a record of {}", row.name).as_bytes(),
                ),
            },
        }),
    )
    .await;

    // What this row expects is waited for: the object a permitted write creates, or the event a
    // withheld one reports. Both are CONTENT, so nothing here is timed.
    if row.delivers {
        let keys = delivered(&router, case, &bucket).await;
        assert!(
            keys.iter().all(|key| key.starts_with(PREFIX)),
            "case {case:?}: every delivered object goes under {PREFIX:?}, got {keys:?}"
        );
    }
    if let Some(error_code) = row.reported {
        let events = reported(&router, case, row.name).await;
        let document = reported_document(case, &events[0]);
        assert_eq!(
            document["errorCode"],
            json!(error_code),
            "case {case:?}: expected errorCode {error_code:?}, got {document}"
        );
        assert_eq!(
            document["deliveryStreamARN"],
            json!(stream_arn),
            "case {case:?}: the event names the stream it belongs to, got {document}"
        );
        assert_eq!(
            document["destination"],
            json!(format!("arn:aws:s3:::{bucket}")),
            "case {case:?}: the event names the destination, got {document}"
        );
        assert_eq!(
            document["deliveryStreamVersionId"],
            json!(1),
            "case {case:?}: the event names the stream's version, got {document}"
        );
        assert!(
            document["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty() && message != error_code),
            "case {case:?}: the event states what the code means, got {document}"
        );
    }

    // Then what this row must NOT hold, read once what it expects has arrived — so an empty
    // destination here is a write that was withheld rather than one still owed, and an empty log
    // stream is the absence of a report rather than one not yet written.
    if !row.delivers {
        let keys = keys_under(&router, case, &bucket, PREFIX).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: a withheld delivery writes no object, got {keys:?}"
        );
    }
    if row.reported.is_none() {
        let events = log_events(&router, row.name).await;
        assert!(
            events.is_empty(),
            "case {case:?}: expected nothing reported, got {events:?}"
        );
    }
    // And nothing of this row reached the bucket it does not deliver into.
    let elsewhere = keys_under(&router, case, OTHER_BUCKET, PREFIX).await;
    assert!(
        elsewhere.is_empty(),
        "case {case:?}: the bucket this row does not deliver into must hold nothing, got \
         {elsewhere:?}"
    );
}

/// Wait until the stream `name` reports `ACTIVE`, or fail naming the row once
/// [`DELIVERY_DEADLINE`] has passed.
async fn active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = json_call(
            router,
            case,
            "firehose",
            "Firehose_20150804.DescribeDeliveryStream",
            json!({ "DeliveryStreamName": name }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!(ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stream {name:?} must reach {ACTIVE}, got {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The keys `bucket` holds under [`PREFIX`] once at least one is there, or a failure naming the
/// row once [`DELIVERY_DEADLINE`] has passed.
async fn delivered(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = keys_under(router, case, bucket, PREFIX).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivered object under {PREFIX:?} of {bucket:?}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The events this row's log stream holds once at least one is there, or a failure naming the
/// row once [`DELIVERY_DEADLINE`] has passed.
async fn reported(router: &MultiServiceRouter, case: &str, stream: &str) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let events = log_events(router, stream).await;
        if !events.is_empty() {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a reported delivery error in {stream:?}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
