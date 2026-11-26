// SPDX-License-Identifier: CC0-1.0

//! A group's Amazon SNS notifications reach the queue subscribed to its topic for a client
//! that addresses the emulator by its PUBLISHED endpoint, which is the `Host` every SDK
//! configured with an `endpoint_url` carries — not the AWS service host a signed call would
//! name against the real service.
//!
//! The RULE under test: **an Auto Scaling group configured with
//! `PutNotificationConfiguration` publishes each event type that configuration names onto the
//! topic it names, and onto no other topic** — the configuration itself as
//! `autoscaling:TEST_NOTIFICATION` the moment it is stored, and every instance the group
//! launches or terminates as `autoscaling:EC2_INSTANCE_LAUNCH` /
//! `autoscaling:EC2_INSTANCE_TERMINATE`, each naming the instance the group moved and the
//! scaling activity that moved it, with the instance's own Amazon EC2 state agreeing.
//!
//! [`crate::asg_notification_configurations_reach_only_their_topics`] pins that rule over a
//! router assembled inside the test process, and
//! [`crate::asg_notifications_on_a_started_emulator`] pins it on the started binary for a
//! client addressing the AWS service HOST of each service. This leg is the third
//! configuration of the same code and the one a client in the field is actually in: it was
//! handed one endpoint url, so every call it makes — Auto Scaling, Amazon EC2, Amazon SNS and
//! Amazon SQS alike — carries that one authority as its `Host` and names its service only in
//! the SigV4 credential scope and the request itself. A notification that is published for a
//! host-addressed client and withheld from an endpoint-addressed one is a failure a test
//! reports rather than one only a client sees.
//!
//! The table this leg spans is the rule's own: the three event types of one configuration, a
//! topic configured for NONE of them beside the configured one, and a capacity move up and
//! back down, each notification checked against the facts the control plane reports for the
//! same instant (the group's own member list, its scaling activities and the instance's
//! Amazon EC2 state). Every wait is on a CONDITION under a generous deadline; nothing here
//! asserts how fast the emulator answers.
//!
//! Every instant a published document states is checked against the GRAMMAR Amazon EC2 Auto
//! Scaling states one in ([`states_a_millisecond_instant`]) rather than against a value of
//! this run: a document whose fraction of a second is finer than the thousandth the service
//! stamps is one a client reading it with a date-time parser bounded at microseconds cannot
//! read at all.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{block_of, element, elements, form_encode, members_of};

/// The three event types this leg configures and reads, spelled out here rather than imported
/// from the service crate, so a wrong constant in the implementation cannot make this leg
/// pass.
const TEST_NOTIFICATION: &str = "autoscaling:TEST_NOTIFICATION";
const INSTANCE_LAUNCH: &str = "autoscaling:EC2_INSTANCE_LAUNCH";
const INSTANCE_TERMINATE: &str = "autoscaling:EC2_INSTANCE_TERMINATE";

/// The `LifecycleState` of a group member that has come into service, and the Amazon EC2
/// instance states a running and a retired instance report.
const IN_SERVICE: &str = "InService";
const RUNNING: &str = "running";
const LEAVING: [&str; 2] = ["shutting-down", "terminated"];

/// How long any one condition is waited for, and how often it is looked at. Generous next to
/// the span this emulator takes to settle a capacity change, because what is waited for is the
/// CONDITION and this is only the backstop that turns a condition which never holds into a
/// failed assertion.
const DEADLINE: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// One form-wire (`awsQuery` / `ec2Query`) call that must answer 200, addressed at the one
/// authority this client was handed.
fn form_call(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in params {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, text) = emulator
        .call_at(
            &emulator.authority(),
            signing_name,
            "POST",
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        )
        .unwrap_or_else(|error| panic!("case {case:?}: {signing_name} {action}: {error}"));
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

/// One Amazon SQS call that must answer 200, over the same one authority.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator
        .call_at(
            &emulator.authority(),
            "sqs",
            "POST",
            "/",
            &[
                ("content-type", "application/x-amz-json-1.0"),
                ("x-amz-target", &format!("AmazonSQS.{operation}")),
            ],
            body.to_string().into_bytes(),
        )
        .unwrap_or_else(|error| panic!("case {case:?}: SQS {operation}: {error}"));
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A topic of this emulator with a queue of this emulator subscribed to it, the queue granting
/// the Amazon SNS service principal the send AWS documents for a subscribed queue. Answers the
/// topic's ARN and the queue's url, each as this emulator published it to this client.
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
    let topic = element(
        &sns(emulator, case, "CreateTopic", &[("Name", name)]),
        "TopicArn",
    );
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
/// carries)`, deleting what it hands over so a later wait reads only what arrived since.
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
    let giving_up_at = Instant::now() + DEADLINE;
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

/// Whether `text` states an instant the way Amazon EC2 Auto Scaling states one inside a
/// document it publishes: RFC 3339 UTC seconds, and a fraction of at most the thousandth it
/// stamps (its own examples carry `…:36.414Z` and `…:11.44Z`, a trailing zero of that
/// fraction left off), with the `Z` zone and nothing else.
fn states_a_millisecond_instant(text: &str) -> bool {
    let Some(body) = text.strip_suffix('Z') else {
        return false;
    };
    let (seconds, fraction) = match body.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (body, None),
    };
    // `YYYY-MM-DDTHH:MM:SS`, digits everywhere the form has one.
    let shaped = seconds.len() == 19
        && seconds.bytes().enumerate().all(|(at, byte)| match at {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            _ => byte.is_ascii_digit(),
        });
    shaped
        && fraction.is_none_or(|fraction| {
            (1..=3).contains(&fraction.len())
                && fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Assert every instant `document` states is stated the way the service states one.
fn assert_instants_are_stated(case: &str, document: &Value, members: &[&str]) {
    for member in members {
        let stated = document[*member].as_str().unwrap_or_default();
        assert!(
            states_a_millisecond_instant(stated),
            "case {case:?}: {member} must state an instant to the thousandth of a second, got \
             {stated:?} in {document}"
        );
    }
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

/// The Amazon EC2 instance state of `instance_id`, as `DescribeInstances` reports it, and the
/// empty string for an instance that service holds no record of.
fn ec2_instance_state(emulator: &Started, case: &str, instance_id: &str) -> String {
    let described = ec2(
        emulator,
        case,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    );
    elements(&described, "name").into_iter().next().unwrap_or_default()
}

/// Wait until `instance_id`'s Amazon EC2 state is one of `wanted`, and answer the state it
/// settled at (or the last one seen).
fn ec2_state_until(
    emulator: &Started,
    case: &str,
    instance_id: &str,
    wanted: &[&str],
) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut state = ec2_instance_state(emulator, case, instance_id);
    while !wanted.contains(&state.as_str()) && Instant::now() < giving_up_at {
        std::thread::sleep(LOOK_INTERVAL);
        state = ec2_instance_state(emulator, case, instance_id);
    }
    state
}

#[test]
fn asg_notifications_reach_an_endpoint_addressed_client() {
    let case = "a group's notification configurations for a client handed one endpoint url";
    let emulator = Started::start();

    let (lit, lit_queue) = given_topic_with_subscriber(&emulator, case, "endpoint-asg-lit");
    let (dark, dark_queue) = given_topic_with_subscriber(&emulator, case, "endpoint-asg-dark");

    // Both pipes are proved live before anything is asserted about Auto Scaling, so an absence
    // afterwards is the publisher's and not the pipe's.
    for (topic, queue) in [(&lit, &lit_queue), (&dark, &dark_queue)] {
        sns(
            &emulator,
            case,
            "Publish",
            &[("TopicArn", topic), ("Message", "canary")],
        );
        let giving_up_at = Instant::now() + DEADLINE;
        let mut delivered = Vec::new();
        while Instant::now() < giving_up_at {
            delivered = drain(&emulator, case, queue);
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
        &ec2(&emulator, case, "CreateVpc", &[("CidrBlock", "10.0.0.0/16")]),
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &emulator,
            case,
            "CreateSubnet",
            &[("VpcId", &vpc), ("CidrBlock", "10.0.1.0/24")],
        ),
        "subnetId",
    );
    let template = "endpoint-asg-template";
    ec2(
        &emulator,
        case,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", template),
            ("LaunchTemplateData.ImageId", "ami-0abcdef1234567890"),
            ("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    );

    let group = "endpoint-asg-group";
    autoscaling(
        &emulator,
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
            &emulator,
            case,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        ),
        "AutoScalingGroupARN",
    );

    autoscaling(
        &emulator,
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

    let confirmations = drain_until(&emulator, case, &lit_queue, TEST_NOTIFICATION, 1);
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
    assert_instants_are_stated(case, document, &["Time"]);

    // The topic no configuration of this group names is told nothing, which is only
    // distinguishable from "nothing was published" because the publication above happened.
    let withheld: Vec<Value> = drain(&emulator, case, &dark_queue)
        .into_iter()
        .map(|(_, document)| document)
        .collect();
    assert!(
        withheld.is_empty(),
        "case {case:?}: {dark} is configured for nothing and must receive nothing, got \
         {withheld:?}"
    );

    autoscaling(
        &emulator,
        case,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
    );
    let giving_up_at = Instant::now() + DEADLINE;
    let mut in_service = Vec::new();
    while Instant::now() < giving_up_at {
        in_service = group_instances_in(&emulator, case, group, IN_SERVICE);
        if in_service.len() == 1 {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert_eq!(
        in_service.len(),
        1,
        "case {case:?}: the group must report one {IN_SERVICE} instance, got {in_service:?}; \
         the log:\n{}",
        emulator.log()
    );
    let moved = in_service[0].clone();

    let launched = drain_until(&emulator, case, &lit_queue, INSTANCE_LAUNCH, 1);
    assert_eq!(
        launched.len(),
        1,
        "case {case:?}: one {INSTANCE_LAUNCH} must be delivered, got {launched:?}; the log:\n{}",
        emulator.log()
    );
    assert_eq!(
        launched[0].1["EC2InstanceId"],
        json!(moved),
        "case {case:?}: the launch must name the instance the group holds, got {}",
        launched[0].1
    );
    assert_eq!(
        launched[0].1["AutoScalingGroupARN"],
        json!(arn),
        "case {case:?}: the launch must name the group's ARN, got {}",
        launched[0].1
    );
    assert_instants_are_stated(
        case,
        &launched[0].1,
        &["Time", "StartTime", "EndTime"],
    );
    let running = ec2_state_until(&emulator, case, &moved, &[RUNNING]);
    assert_eq!(
        running, RUNNING,
        "case {case:?}: {moved} must be {RUNNING} at Amazon EC2, got {running:?}"
    );

    autoscaling(
        &emulator,
        case,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "0")],
    );
    let terminated = drain_until(&emulator, case, &lit_queue, INSTANCE_TERMINATE, 1);
    assert_eq!(
        terminated.len(),
        1,
        "case {case:?}: one {INSTANCE_TERMINATE} must be delivered, got {terminated:?}; the \
         log:\n{}",
        emulator.log()
    );
    assert_eq!(
        terminated[0].1["EC2InstanceId"],
        json!(moved),
        "case {case:?}: the termination must name the instance that left, got {}",
        terminated[0].1
    );
    assert_instants_are_stated(
        case,
        &terminated[0].1,
        &["Time", "StartTime", "EndTime"],
    );
    let leaving = ec2_state_until(&emulator, case, &moved, &LEAVING);
    assert!(
        LEAVING.contains(&leaving.as_str()),
        "case {case:?}: {moved} must be one of {LEAVING:?} at Amazon EC2, got {leaving:?}"
    );

    // The two notifications report the two scaling activities the group's own activity log
    // reports, one per step: the launch and the termination are not the same activity.
    let launch_activity = launched[0].1["ActivityId"].as_str().unwrap_or_default();
    let terminate_activity = terminated[0].1["ActivityId"].as_str().unwrap_or_default();
    assert_ne!(
        launch_activity, terminate_activity,
        "case {case:?}: the launch and the termination are different activities, got \
         {launch_activity} twice"
    );
    let activities = elements(
        &autoscaling(
            &emulator,
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

    // And the unconfigured topic is still told nothing, after a capacity move in each
    // direction on a group whose other topic received both notifications. Read with the
    // LONGEST long poll Amazon SQS admits, which is how a client proves an absence: the poll
    // has to come back, empty, for the reading to be an observation at all.
    let received = sqs(
        &emulator,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": dark_queue, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 20}),
    );
    let still_withheld = received["Messages"].as_array().cloned().unwrap_or_default();
    assert!(
        still_withheld.is_empty(),
        "case {case:?}: {dark} is configured for nothing and must receive nothing, got \
         {still_withheld:?}"
    );
}
