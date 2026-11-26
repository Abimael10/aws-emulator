// SPDX-License-Identifier: CC0-1.0

//! A configuration recorder that is STARTED again after a stop records the next change to a
//! queue, on the emulator a client STARTS — the binary, over its own listener.
//!
//! The rule under test: Config records no configuration change made while the configuration
//! recorder was stopped, and records the changes it observes once it is recording again — so
//! a queue created while the recorder was stopped acquires exactly ONE configuration item,
//! the one recording the change made after the restart, and none describing it as the
//! stopped window left it.
//!
//! The suite lives here because it needs a listening PROCESS. Every call it makes is
//! addressed the way an SDK configured with this emulator's endpoint url makes it — the
//! `Host` header names the address the client dialled — while Config reads the queue it
//! records through the in-process callback, which addresses Amazon SQS' own endpoint host.
//! The two addressings are the same string only on a started emulator, and the id a
//! configuration item records for a queue is the url its own service reports for it, so a
//! client holding that url has to be able to read the item back by it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{element, form_encode};

/// The resource type Config records an Amazon SQS queue under.
const QUEUE_RESOURCE_TYPE: &str = "AWS::SQS::Queue";

/// The service principal Config assumes the recorder's role as.
const CONFIG_PRINCIPAL: &str = "config.amazonaws.com";

/// IAM's awsQuery API version, from that crate's `OPERATIONS.md` wire-protocol section.
const IAM_VERSION: &str = "2010-05-08";

/// How long a poll waits for work the clock owes, and how often it looks. Every wait here
/// ends on its CONDITION; the span is generous, so a loaded host spends none of it.
const BUDGET: Duration = Duration::from_secs(60);
/// See [`BUDGET`].
const LOOK: Duration = Duration::from_millis(200);

/// How long a client goes on reading after the restart, to pin that the emulator keeps
/// answering: many passes' worth of time, and not a span any behaviour here depends on.
const LIVENESS: Duration = Duration::from_secs(20);

/// One signed call DIALLED at this emulator's own address, which is also the `Host` it
/// carries: the position a client configured with an endpoint url is in.
fn dialled(
    emulator: &Started,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, String) {
    emulator
        .call_at(
            &emulator.authority(),
            signing_name,
            method,
            path,
            headers,
            body,
        )
        .unwrap_or_else(|error| panic!("the emulator answers: {error}"))
}

/// One `X-Amz-Target` RPC call, dialled as [`dialled`] describes.
fn rpc(
    emulator: &Started,
    signing_name: &str,
    target: &str,
    json_version: &str,
    request: &Value,
) -> (u16, String) {
    dialled(
        emulator,
        signing_name,
        "POST",
        "/",
        &[
            (
                "content-type",
                &format!("application/x-amz-json-{json_version}"),
            ),
            ("x-amz-target", target),
        ],
        request.to_string().into_bytes(),
    )
}

/// One Config operation over its awsJson1_1 wire, with the status it answered.
fn config_call(emulator: &Started, operation: &str, request: &Value) -> (u16, String) {
    rpc(
        emulator,
        "config",
        &format!("StarlingDoveService.{operation}"),
        "1.1",
        request,
    )
}

/// One Config operation, required to answer 200.
fn config(emulator: &Started, operation: &str, request: &Value) -> Value {
    let (status, body) = config_call(emulator, operation, request);
    assert_eq!(
        status, 200,
        "{operation} must be served, got {status} with {body}"
    );
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

/// One Amazon SQS operation over its awsJson1_0 wire, required to answer 200.
fn sqs(emulator: &Started, operation: &str, request: &Value) -> Value {
    let (status, body) = rpc(
        emulator,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "1.0",
        request,
    );
    assert_eq!(
        status, 200,
        "{operation} must be served, got {status} with {body}"
    );
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

/// The configuration items Config holds for `queue_url`, newest first, with the status and
/// the body the read answered.
fn history(emulator: &Started, queue_url: &str) -> (u16, String, Vec<Value>) {
    let (status, body) = config_call(
        emulator,
        "GetResourceConfigHistory",
        &json!({ "resourceType": QUEUE_RESOURCE_TYPE, "resourceId": queue_url }),
    );
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let items = parsed["configurationItems"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (status, body, items)
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
fn delivery_bucket_policy(bucket: &str, account: &str) -> String {
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

/// Wait until the recorder reports `recording`.
fn await_recording(emulator: &Started, recorder: &str, wanted: bool) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = config(
            emulator,
            "DescribeConfigurationRecorderStatus",
            &json!({ "ConfigurationRecorderNames": [recorder] }),
        );
        let status = body["ConfigurationRecordersStatus"][0].clone();
        if status["recording"] == json!(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected recording {wanted}, got {status}"
        );
        std::thread::sleep(LOOK);
    }
}

/// Provision the recorder the documented way — the role IAM holds, the delivery bucket and
/// the policy on it, the recorder, the channel — and start it.
fn recording(emulator: &Started, recorder: &str, bucket: &str) {
    let account = engine::emulator_account_id();
    let mut body = format!("Action=CreateRole&Version={IAM_VERSION}");
    for (name, value) in [
        ("RoleName", "started-restart-role"),
        ("AssumeRolePolicyDocument", &trust_policy()),
    ] {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, created) = dialled(
        emulator,
        "iam",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    );
    assert_eq!(
        status, 200,
        "setup CreateRole must succeed, got {status} with {created}"
    );
    let role_arn = element(&created, "Arn");

    let (status, answered) = dialled(
        emulator,
        "s3",
        "PUT",
        &format!("/{bucket}"),
        &[("content-type", "application/xml")],
        Vec::new(),
    );
    assert_eq!(
        status, 200,
        "setup CreateBucket must succeed, got {status} with {answered}"
    );
    let (status, answered) = dialled(
        emulator,
        "s3",
        "PUT",
        &format!("/{bucket}?policy"),
        &[("content-type", "application/xml")],
        delivery_bucket_policy(bucket, account).into_bytes(),
    );
    assert!(
        (200..300).contains(&status),
        "setup PutBucketPolicy must succeed, got {status} with {answered}"
    );

    config(
        emulator,
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
    );
    config(
        emulator,
        "PutDeliveryChannel",
        &json!({ "DeliveryChannel": { "s3BucketName": bucket } }),
    );
    config(
        emulator,
        "StartConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": recorder }),
    );
    await_recording(emulator, recorder, true);
}

/// Create a queue through Amazon SQS' own API and answer the url it reports for it.
fn create_queue(emulator: &Started, name: &str, attributes: &Value) -> String {
    let mut request = json!({ "QueueName": name });
    if !attributes.is_null() {
        request["Attributes"] = attributes.clone();
    }
    let created = sqs(emulator, "CreateQueue", &request);
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue must answer a QueueUrl, got {created}"))
        .to_string()
}

#[test]
fn a_restarted_recorder_records_the_next_change_on_a_started_emulator() {
    let emulator = Started::start();
    let recorder = "started-restart-recorder";
    recording(&emulator, recorder, "started-restart-delivery");

    // The calibration leg: a queue created while the recorder runs acquires an item, read
    // back by the url Amazon SQS reported for it — which is the id the item records.
    let calibration = create_queue(&emulator, "started-restart-calibration", &Value::Null);
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, body, items) = history(&emulator, &calibration);
        if !items.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the calibration queue {calibration} must be recorded, last answer {status} with \
             {body}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK);
    }

    config(
        &emulator,
        "StopConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": recorder }),
    );
    await_recording(&emulator, recorder, false);

    // A queue that comes into existence while the recorder is stopped is not discovered:
    // Config observes nothing while it is not recording.
    let queue_url = create_queue(
        &emulator,
        "started-restart-queue",
        &json!({ "VisibilityTimeout": "141" }),
    );
    let (status, body, items) = history(&emulator, &queue_url);
    assert_eq!(
        status, 400,
        "a stopped recorder records nothing, got {status} with {body} and {items:?}"
    );

    config(
        &emulator,
        "StartConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": recorder }),
    );
    await_recording(&emulator, recorder, true);

    sqs(
        &emulator,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &queue_url, "Attributes": { "VisibilityTimeout": "151" } }),
    );

    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, body, items) = history(&emulator, &queue_url);
        let newest = items.first().cloned().unwrap_or(Value::Null);
        let configuration: Value = newest["configuration"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(Value::Null);
        if configuration["VisibilityTimeout"] == json!("151") {
            // The change is the queue's ONLY item: nothing records the state the stopped
            // window left it in, which is the state it was created with.
            assert_eq!(
                items.len(),
                1,
                "the change made after the restart is the queue's only configuration item, \
                 got {items:?}"
            );
            assert_eq!(
                newest["resourceName"],
                json!("started-restart-queue"),
                "the item names the queue it records, got {newest:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a restarted recorder records the change to {queue_url}, last answer {status} \
             with {body}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK);
    }

    // And the emulator GOES ON ANSWERING for as long as a client keeps reading after the
    // restart: a recorder that resumed keeps recording, so neither the passes it owes nor
    // the observation each read makes may wedge the reads that follow.
    let watching_until = Instant::now() + LIVENESS;
    while Instant::now() < watching_until {
        let (status, body, _) = history(&emulator, &queue_url);
        assert_eq!(
            status, 200,
            "a read after the restart must keep being answered, got {status} with {body}; \
             the log:\n{}",
            emulator.log()
        );
        let described = config(
            &emulator,
            "DescribeConfigurationRecorderStatus",
            &json!({ "ConfigurationRecorderNames": [recorder] }),
        );
        assert_eq!(
            described["ConfigurationRecordersStatus"][0]["recording"],
            json!(true),
            "the resumed recorder must keep reporting that it is recording, got {described}"
        );
        std::thread::sleep(LOOK);
    }
}
