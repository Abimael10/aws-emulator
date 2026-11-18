// SPDX-License-Identifier: CC0-1.0

//! A group's Amazon SNS notification configuration reaches the queue subscribed to its
//! topic on the emulator a client STARTS: the built binary, on its own port, with request
//! authentication on, reached over TLS with SigV4-signed calls.
//!
//! The RULE under test: **an Auto Scaling group configured with
//! `PutNotificationConfiguration` publishes each event type that configuration names onto
//! the topic it names and onto no other topic** — the configuration itself as
//! `autoscaling:TEST_NOTIFICATION` the moment it is stored, and every instance the group
//! launches or terminates as `autoscaling:EC2_INSTANCE_LAUNCH` /
//! `autoscaling:EC2_INSTANCE_TERMINATE`, naming the instance that moved and the activity
//! that moved it.
//!
//! [`crate::asg_notification_configurations_reach_only_their_topics`] pins that rule over a
//! router assembled inside the test process, with authentication off, on a runtime the test
//! chose. The emulator a client runs is a different configuration of the same code: the
//! publish Auto Scaling makes is signed with the credentials THAT process vends and
//! authorized by the topic's own policy, and the deliveries are made on the binary's own
//! multi-thread runtime. This leg is the one that pins the chain THERE, so a notification
//! that is published in-process and not on the started emulator is a failure a test reports
//! rather than one only a client sees.
//!
//! This module carries NO test of its own: it is one LEG of
//! [`crate::cfn_deployed_pipeline_on_a_started_emulator`], which calls it on the emulator
//! that suite has already started. Starting the binary costs what starting the emulator
//! costs, so the legs share one start rather than paying it twice.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{block_of, element, elements, members_of};

/// The three event types this suite configures and reads, spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make the test pass.
const TEST_NOTIFICATION: &str = "autoscaling:TEST_NOTIFICATION";
const INSTANCE_LAUNCH: &str = "autoscaling:EC2_INSTANCE_LAUNCH";
const INSTANCE_TERMINATE: &str = "autoscaling:EC2_INSTANCE_TERMINATE";

/// The published Systems Manager alias a client reads the current Amazon Linux image id
/// from.
const IMAGE_ALIAS: &str = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// How long a leg waits for what it waits for, and how often it looks: every wait is on a
/// CONDITION. The span is generous — the convergence SPEED is not what this leg asserts, so
/// a loaded host is given room to spend rather than failed — while still being the bound
/// past which a notification counts as undelivered.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// One form-wire (`awsQuery` / `ec2Query`) call that must answer 200, over the started
/// emulator's own signed-call plumbing.
fn form_call(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = emulator.form_call(signing_name, version, action, params);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with \
         {text}; the log:\n{}",
        emulator.log()
    );
    text
}

/// One Auto Scaling call that must answer 200.
fn autoscaling(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    form_call(emulator, case, "autoscaling", "2011-01-01", action, params)
}

/// One Amazon EC2 call that must answer 200.
fn ec2(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    form_call(emulator, case, "ec2", "2016-11-15", action, params)
}

/// One Amazon SNS call that must answer 200.
fn sns(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    form_call(emulator, case, "sns", "2010-03-31", action, params)
}

/// One `X-Amz-Target` RPC call that must answer 200, over the same plumbing.
fn rpc(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let (status, text) = emulator.rpc(signing_name, target, content_type, body);
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// One Amazon SQS call that must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// The image id the published alias resolves to, read the way a client reads it.
fn published_image_id(emulator: &Started, case: &str) -> String {
    let answered = rpc(
        emulator,
        case,
        "ssm",
        "AmazonSSM.GetParameter",
        "application/x-amz-json-1.1",
        &json!({"Name": IMAGE_ALIAS}),
    );
    answered["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no published image id in {answered}"))
        .to_string()
}

/// A topic of this emulator with a queue of this emulator subscribed to it, the queue
/// granting the SNS service principal the send AWS documents for a subscribed queue.
/// Answers the topic's ARN and the queue's url.
fn given_topic_with_subscriber(emulator: &Started, case: &str, name: &str) -> (String, String) {
    let created = sqs(emulator, case, "CreateQueue", &json!({"QueueName": name}));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let topic = element(&sns(emulator, case, "CreateTopic", &[("Name", name)]), "TopicArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": {"ArnEquals": {"aws:SourceArn": topic}},
        }],
    })
    .to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    );
    sns(
        emulator,
        case,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    );
    (topic, url)
}

/// Take every message waiting on the queue at `url`, as `(the SNS envelope, the document it
/// carries)`, deleting what it hands over so a later leg reads only what arrived since.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<(Value, Value)> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 1}),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut drained = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let inner = envelope["Message"].as_str().unwrap_or_default();
        let document = serde_json::from_str(inner).unwrap_or_else(|_| envelope.clone());
        drained.push((envelope, document));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            );
        }
    }
    drained
}

/// Drain until `wanted` messages carrying `event` have arrived, or the deadline passes.
fn drain_until(
    emulator: &Started,
    case: &str,
    url: &str,
    event: &str,
    wanted: usize,
) -> Vec<(Value, Value)> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut arrived: Vec<(Value, Value)> = Vec::new();
    while Instant::now() < giving_up_at {
        arrived.extend(
            drain(emulator, case, url)
                .into_iter()
                .filter(|(_, document)| document["Event"] == json!(event)),
        );
        if arrived.len() >= wanted {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    arrived
}

/// The instance ids the group `name` reports in `LifecycleState` `state` right now.
fn group_instances_in(emulator: &Started, case: &str, name: &str, state: &str) -> Vec<String> {
    let described = autoscaling(
        emulator,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    );
    let list = block_of(&described, "Instances").unwrap_or_default();
    members_of(list)
        .into_iter()
        .filter_map(|member| {
            let id = elements(member, "InstanceId").into_iter().next()?;
            let lifecycle = elements(member, "LifecycleState").into_iter().next()?;
            (lifecycle == state).then_some(id)
        })
        .collect()
}

/// The whole notification surface of one group on the started emulator: the configuration
/// confirms itself on the topic that asked for it and on no other, and a capacity move up
/// and back down publishes the instance it moved.
///
/// A LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run on the emulator that
/// suite started rather than on a second one.
pub(crate) fn an_asg_notification_reaches_its_subscribed_queue(emulator: &Started) {
    let case = "a group's notification configurations on the emulator a client starts";

    let (lit, lit_queue) = given_topic_with_subscriber(emulator, case, "asg-notify-lit");
    let (dark, dark_queue) = given_topic_with_subscriber(emulator, case, "asg-notify-dark");

    // Both pipes are proved live before anything is asserted about Auto Scaling, so an
    // absence afterwards is the publisher's and not the pipe's.
    for (topic, queue) in [(&lit, &lit_queue), (&dark, &dark_queue)] {
        sns(
            emulator,
            case,
            "Publish",
            &[("TopicArn", topic), ("Message", "canary")],
        );
        let giving_up_at = Instant::now() + SETTLE_DEADLINE;
        let mut delivered = Vec::new();
        while Instant::now() < giving_up_at {
            delivered = drain(emulator, case, queue);
            if !delivered.is_empty() {
                break;
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the canary publish to {topic} must reach its queue, got \
             {delivered:?}; the log:\n{}",
            emulator.log()
        );
    }

    let vpc = element(
        &ec2(emulator, case, "CreateVpc", &[("CidrBlock", "10.0.0.0/16")]),
        "vpcId",
    );
    let subnet = element(
        &ec2(
            emulator,
            case,
            "CreateSubnet",
            &[("VpcId", &vpc), ("CidrBlock", "10.0.1.0/24")],
        ),
        "subnetId",
    );
    let image = published_image_id(emulator, case);
    let template = "asg-notify-template";
    ec2(
        emulator,
        case,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", template),
            ("LaunchTemplateData.ImageId", &image),
            ("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    );

    let group = "asg-notify-group";
    autoscaling(
        emulator,
        case,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", template),
            ("VPCZoneIdentifier", &subnet),
        ],
    );
    let arn = element(
        &autoscaling(
            emulator,
            case,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        ),
        "AutoScalingGroupARN",
    );

    autoscaling(
        emulator,
        case,
        "PutNotificationConfiguration",
        &[
            ("AutoScalingGroupName", group),
            ("TopicARN", &lit),
            ("NotificationTypes.member.1", TEST_NOTIFICATION),
            ("NotificationTypes.member.2", INSTANCE_LAUNCH),
            ("NotificationTypes.member.3", INSTANCE_TERMINATE),
        ],
    );

    let confirmations = drain_until(emulator, case, &lit_queue, TEST_NOTIFICATION, 1);
    assert_eq!(
        confirmations.len(),
        1,
        "case {case:?}: {lit} must receive one {TEST_NOTIFICATION}, got {confirmations:?}; the \
         log:\n{}",
        emulator.log()
    );
    let (envelope, document) = &confirmations[0];
    assert_eq!(
        envelope["TopicArn"],
        json!(lit),
        "case {case:?}: the envelope must name the topic it was published to, got {envelope}"
    );
    assert_eq!(
        document["AutoScalingGroupName"],
        json!(group),
        "case {case:?}: the confirmation must name the group, got {document}"
    );
    assert_eq!(
        document["AutoScalingGroupARN"],
        json!(arn),
        "case {case:?}: the confirmation must name the group's ARN, got {document}"
    );
    let withheld: Vec<Value> = drain(emulator, case, &dark_queue)
        .into_iter()
        .map(|(_, document)| document)
        .collect();
    assert!(
        withheld.is_empty(),
        "case {case:?}: {dark} is configured for nothing and must receive nothing, got \
         {withheld:?}"
    );

    autoscaling(
        emulator,
        case,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut in_service = Vec::new();
    while Instant::now() < giving_up_at {
        in_service = group_instances_in(emulator, case, group, "InService");
        if in_service.len() == 1 {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert_eq!(
        in_service.len(),
        1,
        "case {case:?}: the group must report one InService instance, got {in_service:?}; the \
         log:\n{}",
        emulator.log()
    );
    let launched = drain_until(emulator, case, &lit_queue, INSTANCE_LAUNCH, 1);
    assert_eq!(
        launched.len(),
        1,
        "case {case:?}: one {INSTANCE_LAUNCH} must be delivered, got {launched:?}; the \
         log:\n{}",
        emulator.log()
    );
    assert_eq!(
        launched[0].1["EC2InstanceId"],
        json!(in_service[0]),
        "case {case:?}: the launch must name the instance the group holds, got {}",
        launched[0].1
    );

    autoscaling(
        emulator,
        case,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "0")],
    );
    let terminated = drain_until(emulator, case, &lit_queue, INSTANCE_TERMINATE, 1);
    assert_eq!(
        terminated.len(),
        1,
        "case {case:?}: one {INSTANCE_TERMINATE} must be delivered, got {terminated:?}; the \
         log:\n{}",
        emulator.log()
    );
    assert_eq!(
        terminated[0].1["EC2InstanceId"],
        json!(in_service[0]),
        "case {case:?}: the termination must name the instance that left, got {}",
        terminated[0].1
    );
    let launch_activity = launched[0].1["ActivityId"].as_str().unwrap_or_default();
    let terminate_activity = terminated[0].1["ActivityId"].as_str().unwrap_or_default();
    assert_ne!(
        launch_activity, terminate_activity,
        "case {case:?}: the launch and the termination are different activities, got \
         {launch_activity} twice"
    );
    let activities = elements(
        &autoscaling(
            emulator,
            case,
            "DescribeScalingActivities",
            &[("AutoScalingGroupName", group)],
        ),
        "ActivityId",
    );
    for activity in [launch_activity, terminate_activity] {
        assert!(
            activities.contains(&activity.to_string()),
            "case {case:?}: {activity} must be one of the group's scaling activities, got \
             {activities:?}"
        );
    }
}
