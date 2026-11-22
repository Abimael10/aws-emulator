// SPDX-License-Identifier: CC0-1.0

//! An Auto Scaling group's instance lifecycle events, routed by EventBridge pattern to Amazon
//! SQS targets, driven against the emulator a customer STARTS: the built binary, on its own
//! port, with request authentication on, reached over TLS with SigV4-signed calls.
//!
//! RULE: every instance an Auto Scaling group brings into service, and every instance it takes
//! out of service, is published ONCE onto the account's default event bus in the group's Region
//! under `source` `aws.autoscaling` and the `detail-type` of that step — `EC2 Instance Launch
//! Successful` / `EC2 Instance Terminate Successful` — naming the group and the instance in the
//! `detail`; EventBridge then delivers that one envelope to the targets of every rule whose
//! `EventPattern` agrees with it and to no rule that differs from it in any one field. The
//! pattern a rule is judged by is the one it carries NOW — "if you are updating an existing
//! rule, the rule is replaced with what you specify in this PutRule command" — so a rule
//! rewritten to a pattern the group's next step matches receives that step, with no change to
//! the group and no further Auto Scaling call.
//!
//! [`crate::autoscaling_lifecycle_events_reach_a_corrected_rule`] pins that rule on a router
//! assembled inside the test process, with authentication off, on a runtime the test chose.
//! The emulator a client runs is a different configuration of the same code: the `PutEvents`
//! Auto Scaling makes through the emulator's own callback is signed with the credentials THAT
//! process vends, the queue's own resource policy decides EventBridge's send, and every
//! delivery is made on the binary's multi-thread runtime by a task that outlives the request
//! that armed the clock. This suite is the one that pins the chain THERE, so a lifecycle event
//! that is delivered in process and not on the started emulator is a failure a test reports
//! rather than one only a client sees. The sibling Auto Scaling leg on a started emulator
//! ([`crate::asg_notifications_on_a_started_emulator`]) pins the Amazon SNS half of the same
//! drain; this one pins the event bus.
//!
//! This module carries NO test of its own: starting the binary costs what starting the emulator
//! costs, so it is one LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run on the
//! emulator that suite has already started — the leg beside it
//! ([`crate::asg_notifications_on_a_started_emulator`]) calls it on that same emulator rather
//! than paying for a second one. It drives the whole table: a rule per detail type the service
//! publishes, the rule that names the wrong one of them, and the repair that corrects it. The
//! group is placed in a subnet of the account's OWN default VPC, which is the network a client
//! that creates nothing has. Every wait is on a CONDITION under a generous deadline, and every
//! assertion quotes the emulator's own log, which is where a publication that could not be made
//! says so.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{element, elements, items_of};

/// The Region the started emulator's published configuration names, which is the Region
/// [`Started`] signs and addresses every call for.
const REGION: &str = "us-east-1";

/// The `source` and the two `detail-type`s Auto Scaling publishes an instance lifecycle step
/// under, spelled out here rather than imported from the service crate, so a wrong constant in
/// the implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";

/// The service principal EventBridge delivers to an SQS target as, which is what the queue's
/// resource policy admits.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The group this scenario scales, and the launch template it launches from.
const GROUP: &str = "emu-started-asg-events";
const TEMPLATE: &str = "emu-started-asg-events";

/// The image and instance type the launch template names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// How long any one condition is waited for, and how often it is looked at. Generous next to
/// the spans this emulator's own lifecycle transitions and in-process deliveries take, because
/// what this suite asserts is the DELIVERY and never its speed.
const DEADLINE: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long one waiting read of a queue asks Amazon SQS to hold the call open for, which is how
/// a consumer waits for a delivery rather than spinning on empty reads.
const LONG_POLL_SECONDS: u64 = 1;

/// One Amazon EC2 call over its ec2Query wire, which must answer 200.
fn ec2(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.form_call("ec2", "2016-11-15", action, params);
    assert_eq!(
        status, 200,
        "case {action:?}: the call must be served, got HTTP {status} with {body}"
    );
    body
}

/// One Auto Scaling call over its awsQuery wire, which must answer 200.
fn autoscaling(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.form_call("autoscaling", "2011-01-01", action, params);
    assert_eq!(
        status, 200,
        "case {action:?}: the call must be served, got HTTP {status} with {body}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    body
}

/// One RPC call, which must answer 200, as the JSON it answered.
fn rpc(
    emulator: &Started,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> Value {
    let (status, answered) = emulator.rpc(
        signing_name,
        target,
        &format!("application/x-amz-json-{json_version}"),
        body,
    );
    assert_eq!(
        status, 200,
        "case {target:?}: the call must be served, got HTTP {status} with {answered}"
    );
    serde_json::from_str(&answered)
        .unwrap_or_else(|error| panic!("case {target:?}: {answered} must be JSON: {error}"))
}

/// One EventBridge call.
fn events(emulator: &Started, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        "events",
        &format!("AWSEvents.{operation}"),
        "1.1",
        body,
    )
}

/// One Amazon SQS call.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> Value {
    rpc(emulator, "sqs", &format!("AmazonSQS.{operation}"), "1.0", body)
}

/// A string member of a JSON answer, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The pattern a client watching ONE group writes for one of this service's detail types: the
/// source, the detail type, and the group named by the member the event's `detail` names it in.
fn group_pattern(detail_type: &str) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [detail_type],
        "detail": { "AutoScalingGroupName": [GROUP] },
    })
}

/// Put (or re-put) the ENABLED rule `name` on the default bus with `pattern`; answers its ARN.
fn put_rule(emulator: &Started, name: &str, pattern: &Value) -> String {
    let put = events(
        emulator,
        "PutRule",
        &json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    );
    member(name, &put, "/RuleArn")
}

/// The queue `name`, admitting EventBridge's send on behalf of the rule `rule_arn` and targeted
/// by it; answers its URL.
fn queue_for_rule(emulator: &Started, name: &str, rule_arn: &str) -> String {
    let created = sqs(emulator, "CreateQueue", &json!({ "QueueName": name }));
    let url = member(name, &created, "/QueueUrl");
    let attributes = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = member(name, &attributes, "/Attributes/QueueArn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        emulator,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": document.to_string() } }),
    );
    let targeted = events(
        emulator,
        "PutTargets",
        &json!({ "Rule": name, "Targets": [{ "Id": "the-queue", "Arn": queue_arn }] }),
    );
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {name:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// Every envelope waiting on the queue at `url`, read the way a consumer reads one: a batch at
/// a time, waiting `wait_seconds` for one to arrive — zero for a read that asks what is there
/// RIGHT NOW (`ReceiveMessageRequest$WaitTimeSeconds`: "the duration for which the call waits
/// for a message to arrive in the queue before returning") — and DELETING what it hands over, so
/// a later read sees only what arrived since.
fn drain(emulator: &Started, url: &str, wait_seconds: u64) -> Vec<Value> {
    let received = sqs(
        emulator,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": wait_seconds,
        }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut taken = Vec::with_capacity(messages.len());
    for message in &messages {
        let raw = message["Body"].as_str().unwrap_or_default();
        taken.push(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    taken
}

/// Collect envelopes off the queue at `url` until one of `detail_type` naming `instance_id` has
/// arrived, and answer it.
fn envelope_naming(
    emulator: &Started,
    case: &str,
    url: &str,
    detail_type: &str,
    instance_id: &str,
) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(drain(emulator, url, LONG_POLL_SECONDS));
        let found = seen.iter().find(|envelope| {
            envelope["detail-type"] == json!(detail_type)
                && envelope["detail"]["EC2InstanceId"] == json!(instance_id)
        });
        if let Some(envelope) = found {
            return envelope.clone();
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected an envelope of {detail_type} naming {instance_id} inside \
             {DEADLINE:?}, got {seen:?}; the emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Assert the queue holds nothing: both depth attributes report zero, and a read hands over no
/// message at all.
fn assert_holds_nothing(emulator: &Started, case: &str, url: &str) {
    let reported = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    );
    for attribute in [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
    ] {
        assert_eq!(
            reported["Attributes"][attribute],
            json!("0"),
            "case {case:?}: expected {attribute} \"0\", got {reported}"
        );
    }
    let held = drain(emulator, url, 0);
    assert!(
        held.is_empty(),
        "case {case:?}: expected no message on the queue, got {held:?}"
    );
}

/// A subnet of the account's own DEFAULT VPC, discovered the way a client that created no
/// network of its own discovers one.
fn default_vpc_subnet(emulator: &Started) -> String {
    let vpcs = ec2(
        emulator,
        "DescribeVpcs",
        &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
    );
    let vpc = element(&vpcs, "vpcId");
    let subnets = ec2(
        emulator,
        "DescribeSubnets",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
    );
    let first = items_of(&subnets)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the default VPC of {REGION} must have a subnet: {subnets}"));
    elements(first, "subnetId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the default VPC's subnet must have an id: {subnets}"))
}

/// The instance ids the group reports right now.
fn group_instances(emulator: &Started) -> Vec<String> {
    let described = autoscaling(
        emulator,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", GROUP)],
    );
    elements(&described, "InstanceId")
}

/// Set the group's capacity and wait until it reports `wanted` instances; answers them.
fn scaled_to(emulator: &Started, case: &str, capacity: &str, wanted: usize) -> Vec<String> {
    autoscaling(
        emulator,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", GROUP),
            ("DesiredCapacity", capacity),
            ("HonorCooldown", "false"),
        ],
    );
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(emulator);
        if held.len() == wanted {
            return held;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the group to hold {wanted} instance(s) inside \
             {DEADLINE:?}, got {held:?}; the emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Assert `envelope` is the step `detail_type` of `instance_id` in this group, published by this
/// service onto the default bus of this Region.
fn assert_step_envelope(
    case: &str,
    envelope: &Value,
    detail_type: &str,
    instance_id: &str,
    group_arn: &str,
) {
    for (pointer, expected) in [
        ("/source", json!(EVENT_SOURCE)),
        ("/detail-type", json!(detail_type)),
        ("/region", json!(REGION)),
        ("/detail/AutoScalingGroupName", json!(GROUP)),
        ("/detail/EC2InstanceId", json!(instance_id)),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
        );
    }
    let resources = envelope["resources"].as_array().cloned().unwrap_or_default();
    assert!(
        resources.contains(&json!(group_arn)),
        "case {case:?}: the envelope must name the group among its resources, got \
         {resources:?} in {envelope}"
    );
}

/// RULE: on the emulator a client starts, each instance lifecycle step of a group reaches the
/// rule whose pattern names that step's detail type, no rule that names the other one, and a
/// rule corrected to the step's own detail type on the group's NEXT step of that kind.
///
/// A LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run on the emulator that
/// suite started rather than on a second one.
pub(crate) fn asg_lifecycle_steps_reach_the_rules_naming_them(emulator: &Started) {
    // ---- the network and the launch source a client gives the group ----
    let subnet = default_vpc_subnet(emulator);
    ec2(
        emulator,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    );

    // ---- one rule per detail type this service publishes, each with its own queue, plus the
    // rule under repair, which starts out naming the detail type a scale-out does NOT publish
    let launch_rule = put_rule(
        emulator,
        "emu-started-asg-launch",
        &group_pattern(LAUNCH_SUCCESSFUL),
    );
    let launch_queue = queue_for_rule(emulator, "emu-started-asg-launch", &launch_rule);
    let terminate_rule = put_rule(
        emulator,
        "emu-started-asg-terminate",
        &group_pattern(TERMINATE_SUCCESSFUL),
    );
    let terminate_queue = queue_for_rule(emulator, "emu-started-asg-terminate", &terminate_rule);
    let repaired_rule = put_rule(
        emulator,
        "emu-started-asg-repaired",
        &group_pattern(TERMINATE_SUCCESSFUL),
    );
    let repaired_queue = queue_for_rule(emulator, "emu-started-asg-repaired", &repaired_rule);

    // ---- the group, created holding nothing ----
    autoscaling(
        emulator,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", GROUP),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("VPCZoneIdentifier", &subnet),
        ],
    );
    let group_arn = element(
        &autoscaling(
            emulator,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", GROUP)],
        ),
        "AutoScalingGroupARN",
    );

    // ---- the scale-out: the launch rule receives it, and neither terminate rule does ----
    let case = "a scale-out, against a rule naming each detail type";
    let held = scaled_to(emulator, case, "1", 1);
    let first = held
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the group must name its instance"));
    let delivered = envelope_naming(emulator, case, &launch_queue, LAUNCH_SUCCESSFUL, &first);
    assert_step_envelope(case, &delivered, LAUNCH_SUCCESSFUL, &first, &group_arn);
    // The launch has been matched against every rule of the bus by the time it arrived above,
    // so a rule that holds nothing now received nothing FROM IT.
    for (named, url) in [
        ("the terminate rule", &terminate_queue),
        ("the rule naming the wrong detail type", &repaired_queue),
    ] {
        assert_holds_nothing(emulator, &format!("{case}: {named}"), url);
    }

    // ---- the repair, and the walk down and back up the recorded client makes ----
    let case = "the same rule once its pattern names the launch detail-type";
    put_rule(
        emulator,
        "emu-started-asg-repaired",
        &group_pattern(LAUNCH_SUCCESSFUL),
    );

    let scaled_in = "a scale-in, against a rule naming each detail type";
    let emptied = scaled_to(emulator, scaled_in, "0", 0);
    assert!(
        emptied.is_empty(),
        "case {scaled_in:?}: the group must hold nothing at capacity zero, got {emptied:?}"
    );
    let left = envelope_naming(
        emulator,
        scaled_in,
        &terminate_queue,
        TERMINATE_SUCCESSFUL,
        &first,
    );
    assert_step_envelope(scaled_in, &left, TERMINATE_SUCCESSFUL, &first, &group_arn);

    let held = scaled_to(emulator, case, "1", 1);
    let second = held
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the group must name its instance"));
    assert_ne!(
        second, first,
        "case {case:?}: the walk down and back up launches a NEW instance, got the first one \
         again"
    );
    let repaired = envelope_naming(emulator, case, &repaired_queue, LAUNCH_SUCCESSFUL, &second);
    assert_step_envelope(case, &repaired, LAUNCH_SUCCESSFUL, &second, &group_arn);
    // And the rule that matched all along receives this launch too: one publication is
    // delivered to EVERY rule whose pattern agrees with it.
    let again = envelope_naming(emulator, case, &launch_queue, LAUNCH_SUCCESSFUL, &second);
    assert_step_envelope(case, &again, LAUNCH_SUCCESSFUL, &second, &group_arn);

    // And the terminate rule, which named the other detail type throughout, holds nothing of
    // either launch: the only envelope it ever received is the scale-in's, which the leg above
    // read off it.
    let left_over: Vec<Value> = drain(emulator, &terminate_queue, 0)
        .into_iter()
        .filter(|envelope| envelope["detail-type"] == json!(LAUNCH_SUCCESSFUL))
        .collect();
    assert!(
        left_over.is_empty(),
        "case {case:?}: a rule naming {TERMINATE_SUCCESSFUL} receives no launch at all, got \
         {left_over:?}"
    );
}
