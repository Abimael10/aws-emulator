// SPDX-License-Identifier: CC0-1.0

//! An Auto Scaling group's instance lifecycle events reach the EventBridge rules that match
//! them — through the WHOLE mounted service set, the one `core/router/src/lib.rs` assembles
//! for the binary.
//!
//! RULE: every instance an Auto Scaling group LAUNCHES is published onto the account's
//! default event bus as `source` `aws.autoscaling` / `detail-type`
//! `EC2 Instance Launch Successful`, naming the group and the instance, and EventBridge then
//! routes it by PATTERN alone: a rule whose `EventPattern` matches delivers the envelope to
//! its targets, and a rule that cannot match receives nothing from the same publication. The
//! pattern a rule is judged by is the one it carries NOW — "if you are updating an existing
//! rule, the rule is replaced with what you specify in this PutRule command" — so correcting
//! a rule that could not match delivers the NEXT launch to it, with no change to the group
//! and no further Auto Scaling call.
//!
//! Four crates have to agree for that to hold, which is why this leg lives here: Auto Scaling
//! publishes, Amazon EC2 owns the instances it launches, EventBridge matches and delivers, and
//! Amazon SQS holds the target queue the envelope is read off. Nothing below reads the
//! publisher's own store.
//!
//! The negative half needs no fixed wait: a CONTROL rule that does match the same publication
//! is what proves the event was published at all, so the mismatched rule's queue is asserted
//! empty only after the control's envelope has arrived, and over a run of reads rather than
//! one.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The `source` and the two `detail-type`s Auto Scaling publishes its instance lifecycle
/// events under, spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make this test pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";

/// The AMI and instance type the launch template names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// How long a leg waits for the clock-owed moves it depends on — the launch, and the
/// publication that reports it — and how often it looks. Generous on purpose: the waits look
/// for their CONDITION, so machine load decides when the leg finishes and never whether it
/// passes.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// How many times a leg re-reads a queue that must stay EMPTY: the absence is asserted over a
/// run of reads rather than one, so a delivery that arrives late still fails the leg.
const HELD_READS: usize = 20;

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson RPC call to a service mounted beside Auto Scaling, the way a client makes it.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.body().clone();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One EventBridge call that must succeed.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "setup: EventBridge {operation} must succeed, got {status} with {answer}"
    );
    answer
}

/// One Amazon SQS call that must succeed.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "setup: Amazon SQS {operation} must succeed, got {status} with {answer}"
    );
    answer
}

/// A queue created through Amazon SQS itself, granting `events.amazonaws.com` the
/// `sqs:SendMessage` a rule's target needs: EventBridge makes that call as its own service
/// principal and relies on the queue's resource-based policy for it.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, queue_arn)
}

/// The pattern a rule carries to match one of this service's detail types on the default bus.
fn lifecycle_pattern(detail_type: &str) -> String {
    json!({ "source": [EVENT_SOURCE], "detail-type": [detail_type] }).to_string()
}

/// The same, narrowed to ONE group by the member the event's `detail` names it in — the
/// pattern a client that watches a single group writes.
fn group_pattern(detail_type: &str, group: &str) -> String {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [detail_type],
        "detail": { "AutoScalingGroupName": [group] },
    })
    .to_string()
}

/// Put (or re-put) the rule `name` with `pattern`, leaving its targets alone.
async fn put_rule(router: &MultiServiceRouter, name: &str, pattern: &str) {
    events(
        router,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
}

/// Point the rule `name` at the queue `queue_arn`.
async fn put_target(router: &MultiServiceRouter, name: &str, queue_arn: &str) {
    let answer = events(
        router,
        "PutTargets",
        json!({ "Rule": name, "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "setup: PutTargets reported a failed entry: {answer}"
    );
}

/// Every message waiting on the queue at `url` right now, as parsed event envelopes.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Poll the queue at `url` — and ONLY the queue — until one envelope of `detail_type` has been
/// delivered, and answer it.
async fn envelope_of(router: &MultiServiceRouter, case: &str, url: &str, detail_type: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(drain(router, url).await);
        if let Some(found) = seen
            .iter()
            .find(|envelope| envelope["detail-type"].as_str() == Some(detail_type))
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected an envelope of {detail_type} on the queue, got {seen:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The instance ids the group `name` holds right now, as it reports them.
async fn group_instances(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    elements(&body, "InstanceId")
}

/// Assert the envelope reports the launch of an instance of `group`, and answer the instance
/// id it names.
fn assert_launch_envelope(case: &str, envelope: &Value, group: &str) -> String {
    assert_eq!(
        (
            envelope["source"].as_str(),
            envelope["detail-type"].as_str(),
            envelope["detail"]["AutoScalingGroupName"].as_str(),
        ),
        (Some(EVENT_SOURCE), Some(LAUNCH_SUCCESSFUL), Some(group)),
        "case {case:?}: expected the group's own launch envelope, got {envelope}"
    );
    envelope["detail"]["EC2InstanceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the envelope names its instance, got {envelope}"))
        .to_string()
}

#[tokio::test]
async fn correcting_a_rules_pattern_delivers_the_groups_next_launch() {
    let router = router();
    let vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", "10.77.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.77.1.0/24"),
                ("AvailabilityZone", "us-east-1a"),
            ],
        )
        .await,
        "subnetId",
    );
    ec2(
        &router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", "asgevt-template"),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;

    // The CONTROL: a rule that matches the launch this group is about to make, which is what
    // proves the publication happened at all.
    let (control_url, control_arn) = given_queue(&router, "asgevt-control").await;
    put_rule(
        &router,
        "asgevt-control-rule",
        &lifecycle_pattern(LAUNCH_SUCCESSFUL),
    )
    .await;
    put_target(&router, "asgevt-control-rule", &control_arn).await;
    // The rule under test starts out matching ONLY the terminate detail-type, so a launch
    // cannot reach it.
    let (corrected_url, corrected_arn) = given_queue(&router, "asgevt-corrected").await;
    put_rule(
        &router,
        "asgevt-corrected-rule",
        &lifecycle_pattern(TERMINATE_SUCCESSFUL),
    )
    .await;
    put_target(&router, "asgevt-corrected-rule", &corrected_arn).await;

    let group = "asgevt-group";
    asg(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", "asgevt-template"),
            ("VPCZoneIdentifier", subnet.as_str()),
        ],
    )
    .await;

    // ONE scale-out, published once: the matching rule's target receives it...
    asg(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
    )
    .await;
    let case = "a rule matching only the terminate detail-type";
    let delivered = envelope_of(&router, case, &control_url, LAUNCH_SUCCESSFUL).await;
    let first = assert_launch_envelope(case, &delivered, group);
    assert!(
        group_instances(&router, group).await.contains(&first),
        "case {case:?}: the envelope must name an instance the group itself reports, got \
         {first}"
    );
    // ...and the rule that cannot match receives nothing from it, over a run of reads.
    for read in 0..HELD_READS {
        let quiet = drain(&router, &corrected_url).await;
        assert!(
            quiet.is_empty(),
            "case {case:?}, read {read}: a rule matching only {TERMINATE_SUCCESSFUL} receives \
             nothing from a launch, got {quiet:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    // The SAME rule, with its pattern corrected: the NEXT launch arrives at its target. The
    // corrected pattern also narrows to this group by the member the `detail` names it in, so
    // what is asserted is a match on the whole pattern rather than on the detail type alone.
    let case = "the same rule once its pattern names the launch detail-type";
    put_rule(
        &router,
        "asgevt-corrected-rule",
        &group_pattern(LAUNCH_SUCCESSFUL, group),
    )
    .await;
    asg(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "2")],
    )
    .await;
    let delivered = envelope_of(&router, case, &corrected_url, LAUNCH_SUCCESSFUL).await;
    let second = assert_launch_envelope(case, &delivered, group);
    assert_ne!(
        second, first,
        "case {case:?}: the corrected rule receives the NEXT launch, not the one it could not \
         match"
    );
    let held = group_instances(&router, group).await;
    assert!(
        held.contains(&second) && held.len() == 2,
        "case {case:?}: the group must hold both launched instances, got {held:?} beside \
         {second}"
    );
}
