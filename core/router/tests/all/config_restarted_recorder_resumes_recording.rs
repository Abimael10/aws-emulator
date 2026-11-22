// SPDX-License-Identifier: CC0-1.0

//! A configuration recorder that is STARTED again after a stop resumes recording, on the
//! emulator as `aws_emulator_router::build_router` assembles it.
//!
//! The rule under test: a configuration recorder that is recording observes the resources
//! of the types its recording group covers and records the configuration item of the state
//! each one is in, so STARTING a recorder takes an inventory of what it records and every
//! later change to one of those resources is recorded as it happens — and what an operation
//! that reports those items answers is the state the account held when it was asked, not
//! whatever some background task last managed to write. A recorder started again after a
//! stop therefore records the first change made from then on, for a resource that came into
//! existence while it was stopped as much as for one it had already recorded, and its own
//! recording event leaves the in-progress status.
//!
//! The suite lives here because no single crate can see the path: Amazon SQS owns the queue
//! whose change is recorded, IAM holds the recorder's role, Amazon S3 holds the delivery
//! bucket Config reads before it takes the channel, and Config records. Every service is
//! mounted on ONE engine, exactly as the binary mounts them.
//!
//! The rows differ along what the recorder has to catch up with when it is started again
//! (a resource it had already recorded, one that came into existence while it was stopped)
//! and along WHICH attribute changes, so what is asserted is the rule rather than one
//! attribute's value. Nothing here waits for a span to pass: the assertion about the change
//! is made on the FIRST read after it, and every other wait polls its own condition under a
//! generous deadline.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element reader this crate writes ONCE
// (`tests/all/query_wire.rs`): a suite that writes its own drifts from the others.
use super::query_wire::{element, form_encode};

/// The Region every request here is signed for, and so the Region each resource belongs to.
const REGION: &str = "us-east-1";

/// Config's `X-Amz-Target` prefix (its model's service shape, `StarlingDoveService`) and the
/// signing name its credential scope carries (`sigv4Name: config`).
const CONFIG_TARGET_PREFIX: &str = "StarlingDoveService";
/// See [`CONFIG_TARGET_PREFIX`].
const CONFIG_SIGNING_NAME: &str = "config";

/// Amazon SQS' `X-Amz-Target` prefix and signing name.
const SQS_TARGET_PREFIX: &str = "AmazonSQS";
/// See [`SQS_TARGET_PREFIX`].
const SQS_SIGNING_NAME: &str = "sqs";

/// IAM's awsQuery API version, from that crate's `OPERATIONS.md` wire-protocol section.
const IAM_VERSION: &str = "2010-05-08";

/// The service principal Config assumes the recorder's role as.
const CONFIG_PRINCIPAL: &str = "config.amazonaws.com";

/// The resource type Config records an Amazon SQS queue under, a
/// `com.amazonaws.configservice#ResourceType` value.
const QUEUE_RESOURCE_TYPE: &str = "AWS::SQS::Queue";

/// `RecorderStatus` values a recording event moves through
/// (`smithy_query(query="shape", shape="RecorderStatus")`, whose values are `Pending`,
/// `Success`, `Failure` and `NotApplicable`).
const RECORDER_STATUS_SUCCESS: &str = "Success";

/// The error a configuration-item read answers for a resource Config holds no item for.
const NOT_DISCOVERED: &str = "ResourceNotDiscoveredException";

/// How long a poll waits for work the clock owes — a recording pass, a status move. A pass
/// falls due a fraction of a second after the call that makes it due, so this is many
/// passes' worth of slack and not a span any behaviour here depends on: every wait ends on
/// its CONDITION.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a poll waits between two reads.
const LOOK: Duration = Duration::from_millis(25);

/// How long a row watches while asserting that NOTHING is recorded — several passes' worth
/// of time, so an item that was going to be recorded would have been.
const QUIET: Duration = Duration::from_millis(1_500);

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`].
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The emulator as it is ASSEMBLED, on one engine.
fn build_router() -> MultiServiceRouter {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    // Signing is not the subject here; the credential scope is still carried, because it is
    // what names the Region and routes the request.
    router.set_request_authentication(false);
    router
}

/// One awsQuery call to IAM.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One S3 call on the path-style address.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One RPC call carrying `X-Amz-Target`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Config operation over its awsJson1_1 wire.
async fn config(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    rpc(
        router,
        CONFIG_SIGNING_NAME,
        CONFIG_TARGET_PREFIX,
        "1.1",
        operation,
        request,
    )
    .await
}

/// One Config operation, required to answer 200.
async fn served_config(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, body) = config(router, operation, request).await;
    assert_eq!(
        status, 200,
        "{operation} must be served, got {status} with {body}"
    );
    body
}

/// One Amazon SQS operation over its awsJson1_0 wire, required to answer 200.
async fn sqs(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, body) = rpc(
        router,
        SQS_SIGNING_NAME,
        SQS_TARGET_PREFIX,
        "1.0",
        operation,
        request,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} must be served, got {status} with {body}"
    );
    body
}

/// The wire error code of a refusal: awsJson carries it as the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The trust policy that lets Config assume the recorder's role.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": CONFIG_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The bucket policy AWS documents for a Config delivery channel's bucket.
fn delivery_bucket_policy(bucket: &str) -> String {
    let account = engine::emulator_account_id();
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AWSConfigBucketPermissionsCheck",
                "Effect": "Allow",
                "Principal": { "Service": CONFIG_PRINCIPAL },
                "Action": ["s3:GetBucketAcl", "s3:ListBucket"],
                "Resource": format!("arn:aws:s3:::{bucket}"),
                "Condition": { "StringEquals": { "AWS:SourceAccount": account } },
            },
            {
                "Sid": "AWSConfigBucketDelivery",
                "Effect": "Allow",
                "Principal": { "Service": CONFIG_PRINCIPAL },
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
                "Condition": {
                    "StringEquals": {
                        "s3:x-amz-acl": "bucket-owner-full-control",
                        "AWS:SourceAccount": account,
                    },
                },
            },
        ],
    })
    .to_string()
}

/// Provision the recorder the documented way — the role IAM holds, the delivery bucket
/// Amazon S3 holds and the policy on it, the recorder, the channel — and start it.
async fn recording(router: &MultiServiceRouter, recorder: &str) {
    let bucket = "sim2real-resume-delivery";
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", "sim2real-resume-role"),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole must succeed, got {status} with {created}"
    );
    let role_arn = element(&created, "Arn");
    let (status, answered) = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        status, 200,
        "setup CreateBucket must succeed, got {status} with {answered}"
    );
    let (status, answered) = s3(
        router,
        "PUT",
        &format!("/{bucket}?policy"),
        &delivery_bucket_policy(bucket),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "setup PutBucketPolicy must succeed, got {status} with {answered}"
    );

    served_config(
        router,
        "PutConfigurationRecorder",
        &json!({
            "ConfigurationRecorder": {
                "name": recorder,
                "roleARN": role_arn,
                "recordingGroup": {
                    "allSupported": true,
                    "includeGlobalResourceTypes": false,
                },
            },
        }),
    )
    .await;
    served_config(
        router,
        "PutDeliveryChannel",
        &json!({ "DeliveryChannel": { "s3BucketName": bucket } }),
    )
    .await;
    served_config(
        router,
        "StartConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": recorder }),
    )
    .await;
}

/// The configuration items Config reports for `queue_url`, newest first — empty while Config
/// holds no item for it, which is the refusal the read answers until a pass has seen it.
async fn history(router: &MultiServiceRouter, queue_url: &str) -> Vec<Value> {
    let (status, body) = config(
        router,
        "GetResourceConfigHistory",
        &json!({ "resourceType": QUEUE_RESOURCE_TYPE, "resourceId": queue_url }),
    )
    .await;
    if status == 400 && error_code(&body) == NOT_DISCOVERED {
        return Vec::new();
    }
    assert_eq!(
        status, 200,
        "GetResourceConfigHistory must answer 200, got {status} with {body}"
    );
    body["configurationItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Read `queue_url`'s history until it holds at least `wanted` items, and answer them.
async fn await_items(
    case: &str,
    router: &MultiServiceRouter,
    queue_url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let items = history(router, queue_url).await;
        if items.len() >= wanted {
            return items;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {wanted} configuration item(s) for {queue_url}, \
             got {items:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The recorder's status as Config reports it.
async fn recorder_status(router: &MultiServiceRouter, recorder: &str) -> Value {
    let body = served_config(
        router,
        "DescribeConfigurationRecorderStatus",
        &json!({ "ConfigurationRecorderNames": [recorder] }),
    )
    .await;
    body["ConfigurationRecordersStatus"][0].clone()
}

/// Wait until the recorder reports `recording`.
async fn await_recording(case: &str, router: &MultiServiceRouter, recorder: &str, wanted: bool) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let status = recorder_status(router, recorder).await;
        if status["recording"] == json!(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected recording {wanted}, got {status}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The recorded `configuration` document of `item`, decoded.
fn configuration(case: &str, item: &Value) -> Value {
    let encoded = item["configuration"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a recorded configuration, got {item}"));
    serde_json::from_str(encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: a configuration must be JSON, got {error}"))
}

/// Create a queue through Amazon SQS' own API and answer its url.
async fn create_queue(router: &MultiServiceRouter, name: &str, attributes: &Value) -> String {
    let mut request = json!({ "QueueName": name });
    if !attributes.is_null() {
        request["Attributes"] = attributes.clone();
    }
    let created = sqs(router, "CreateQueue", &request).await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue must answer a QueueUrl, got {created}"))
        .to_string()
}

/// Stop the recorder and wait until it reports that it is not recording.
async fn stop(case: &str, router: &MultiServiceRouter, recorder: &str) {
    served_config(
        router,
        "StopConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": recorder }),
    )
    .await;
    await_recording(case, router, recorder, false).await;
}

/// Start the recorder and wait until it reports that it is recording.
async fn start(case: &str, router: &MultiServiceRouter, recorder: &str) {
    served_config(
        router,
        "StartConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": recorder }),
    )
    .await;
    await_recording(case, router, recorder, true).await;
}

/// Assert that `queue_url`'s history stays empty for as long as [`QUIET`]: a stopped
/// recorder observes nothing, so nothing is recorded for a resource that came into
/// existence while it was stopped.
async fn stays_undiscovered(case: &str, router: &MultiServiceRouter, queue_url: &str) {
    let quiet = Instant::now() + QUIET;
    loop {
        let items = history(router, queue_url).await;
        assert!(
            items.is_empty(),
            "case {case:?}: a stopped recorder records nothing, got {items:?}"
        );
        if Instant::now() >= quiet {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Wait until the recorder's own recording event has been processed.
async fn await_processed(case: &str, router: &MultiServiceRouter, recorder: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let status = recorder_status(router, recorder).await;
        if status["recording"] == json!(true)
            && status["lastStatus"] == json!(RECORDER_STATUS_SUCCESS)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a restarted recorder reaches lastStatus \
             {RECORDER_STATUS_SUCCESS:?} while recording, got {status}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One row: what the recorder has to catch up with when it is started again, and the
/// attribute whose change it then has to record.
struct Row {
    /// What this row is about, named in every assertion.
    case: &'static str,
    /// The queue's name, which is also the last segment of its url.
    queue: &'static str,
    /// Whether the queue existed — and was recorded — BEFORE the recorder was stopped.
    recorded_before: bool,
    /// The attribute the row changes once the recorder is running again, and the value it
    /// changes it to. Both are `com.amazonaws.sqs#QueueAttributeName` values a queue
    /// carries as part of its configuration.
    attribute: &'static str,
    /// See [`Row::attribute`].
    changed_to: &'static str,
}

/// The rows: a resource the recorder had already recorded, one that came into existence
/// while it was stopped, and one of each changed in a DIFFERENT attribute — so what is
/// asserted is the rule rather than one attribute's value.
fn rows() -> [Row; 4] {
    [
        Row {
            case: "a queue the recorder had already recorded, changed after the restart",
            queue: "sim2real-resume-known",
            recorded_before: true,
            attribute: "VisibilityTimeout",
            changed_to: "41",
        },
        Row {
            case: "a queue created while the recorder was stopped, changed after the restart",
            queue: "sim2real-resume-created-while-stopped",
            recorded_before: false,
            attribute: "VisibilityTimeout",
            changed_to: "151",
        },
        Row {
            case: "a queue created while the recorder was stopped, delayed after the restart",
            queue: "sim2real-resume-delayed",
            recorded_before: false,
            attribute: "DelaySeconds",
            changed_to: "9",
        },
        Row {
            case: "a queue the recorder had already recorded, retained for longer",
            queue: "sim2real-resume-retained",
            recorded_before: true,
            attribute: "MessageRetentionPeriod",
            changed_to: "600",
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_recorder_records_what_changes_from_then_on() {
    let router = build_router();
    let recorder = "sim2real-resume-recorder";
    recording(&router, recorder).await;

    // The calibration leg: a queue created while the recorder runs is discovered, which is
    // what makes every row below about RESUMING rather than about recording at all.
    let calibration = create_queue(&router, "sim2real-resume-calibration", &Value::Null).await;
    await_items("the calibration queue", &router, &calibration, 1).await;

    for row in rows() {
        let Row {
            case,
            queue,
            recorded_before,
            attribute,
            changed_to,
        } = row;

        // A queue this row's recorder had already recorded is created and observed while
        // it is still running; one it had not comes into existence while it is stopped.
        let mut queue_url = String::new();
        if recorded_before {
            queue_url = create_queue(&router, queue, &json!({ "VisibilityTimeout": "31" })).await;
            await_items(case, &router, &queue_url, 1).await;
        }

        stop(case, &router, recorder).await;
        if !recorded_before {
            queue_url = create_queue(&router, queue, &json!({ "VisibilityTimeout": "31" })).await;
            stays_undiscovered(case, &router, &queue_url).await;
        }

        // Recording resumes, and from then on what changes is recorded: the FIRST read
        // after the change reports the state it left the queue in. No polling here, because
        // what is asserted is that the read itself is answered from the state the account
        // holds rather than from whatever a background task last managed to write.
        start(case, &router, recorder).await;
        sqs(
            &router,
            "SetQueueAttributes",
            &json!({ "QueueUrl": &queue_url, "Attributes": { attribute: changed_to } }),
        )
        .await;
        let items = history(&router, &queue_url).await;
        let newest = items.first().cloned().unwrap_or(Value::Null);
        assert_eq!(
            configuration(case, &newest)[attribute],
            json!(changed_to),
            "case {case:?}: the first read after the restart reports the state the change \
             left, got {items:?}"
        );

        // And the restarted recorder's own recording event is processed: it reports that it
        // is recording, and its last status leaves the in-progress value.
        await_processed(case, &router, recorder).await;
    }
}
