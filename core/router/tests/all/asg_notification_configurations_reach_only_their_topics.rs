// SPDX-License-Identifier: CC0-1.0

//! An Auto Scaling group's notification configurations reach the queues subscribed to the
//! topics they name — over the WHOLE mounted service set, from a group placed in a VPC
//! subnet and launching from the image a published `ssm` alias resolves to, which is the
//! topology a client wires a notification pipe with.
//!
//! The RULE under test: **a group configured with `PutNotificationConfiguration` publishes
//! each event type that configuration names onto the topic it names and onto no other
//! topic** — the configuration itself as `autoscaling:TEST_NOTIFICATION` the moment it is
//! stored, every instance the group launches as `autoscaling:EC2_INSTANCE_LAUNCH` and every
//! instance it terminates as `autoscaling:EC2_INSTANCE_TERMINATE`, each naming the exact
//! instance that moved and the scaling activity that moved it — while
//! `DescribeNotificationConfigurations` reports exactly the (topic, type) pairs stored and
//! `DeleteNotificationConfiguration` removes one pair at a time.
//!
//! [`crate::autoscaling_notification_reaches_its_subscribed_queue`] pins the same rule for a
//! group placed by Availability Zone and created already at capacity, and this suite drives
//! the four services over THAT suite's wire plumbing rather than a second copy of it. What
//! only this one reaches is the wiring a client actually builds — a group at
//! `DesiredCapacity` 0 in a VPC subnet, with a second topic of the same group configured for
//! nothing so the "and no other topic" half of the rule is observable, and the read-back and
//! one-pair-at-a-time delete of the configurations themselves. Its own `drain` carries the
//! SNS ENVELOPE beside the document, which is what lets the confirmation be held to the
//! topic it was published to and not merely to its own contents.
//!
//! The rows of the second test differ along what the rule generalises over: WHICH door moved
//! the capacity. The publication is selected and rendered at the one place a lifecycle step
//! is recorded, so a door that records a termination owes the same notification however the
//! termination was asked for — by a capacity change or by naming the instance.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::autoscaling_notification_reaches_its_subscribed_queue::{
    autoscaling, ec2, form_call, sqs,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{block_of, element, elements, members_of};

/// The three event types this suite configures and reads, spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make the test pass.
const TEST_NOTIFICATION: &str = "autoscaling:TEST_NOTIFICATION";
const INSTANCE_LAUNCH: &str = "autoscaling:EC2_INSTANCE_LAUNCH";
const INSTANCE_TERMINATE: &str = "autoscaling:EC2_INSTANCE_TERMINATE";

/// The published Systems Manager alias a client reads the current Amazon Linux image id
/// from, named by
/// `services/service-com-amazonaws-ec2-amazonec2/src/state/published_amazon_linux.rs`.
const IMAGE_ALIAS: &str = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// How long a leg waits for what it waits for, and how often it looks: a group's instance
/// lifecycle step is clock-owed, so every wait is on the CONDITION under one deadline. The
/// span is generous — the convergence SPEED is not what these suites assert, so a loaded
/// machine is given room to spend rather than failed — while still being the bound past
/// which a notification counts as undelivered.
const SETTLE_DEADLINE: Duration = Duration::from_secs(45);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The long poll each read of a subscribed queue makes, the way a client waiting for a
/// delivery reads one: the wait is what makes the read return as soon as something arrives
/// rather than on the next tick of the loop around it.
const LONG_POLL_SECONDS: u32 = 1;

/// One Amazon SNS call that must answer 200.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    form_call(router, case, region, "sns", "2010-03-31", action, params).await
}

/// The image id the published alias resolves to, read the way a client reads it: through
/// Systems Manager's own `awsJson1_1` wire.
async fn published_image_id(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "AmazonSSM.GetParameter")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/ssm/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(json!({"Name": IMAGE_ALIAS}).to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: GetParameter({IMAGE_ALIAS}) must answer 200, got {status} with \
         {answered}"
    );
    answered["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no published image id in {answered}"))
        .to_string()
}

/// Take every message waiting on the queue at `url`, as `(the SNS envelope, the document it
/// carries)`, deleting what it hands over so a later leg reads only what arrived since.
///
/// A body that is not an envelope, or an envelope whose `Message` is not a document, is
/// answered as it arrived, so a malformed delivery reaches the assertion instead of being
/// silently dropped.
async fn drain(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
) -> Vec<(Value, Value)> {
    let received = sqs(
        router,
        case,
        region,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": LONG_POLL_SECONDS,
            "VisibilityTimeout": 0,
        }),
    )
    .await;
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
                router,
                case,
                region,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    drained
}

/// Drain until `wanted` messages carrying `event` have arrived, or `deadline` passes.
async fn drain_until(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
    event: &str,
    wanted: usize,
) -> Vec<(Value, Value)> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut arrived: Vec<(Value, Value)> = Vec::new();
    while Instant::now() < deadline {
        arrived.extend(
            drain(router, case, region, url)
                .await
                .into_iter()
                .filter(|(_, document)| document["Event"] == json!(event)),
        );
        if arrived.len() >= wanted {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    arrived
}

/// A VPC subnet of this account in `region`, which is where a group places its instances.
async fn given_subnet(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let vpc = element(
        &ec2(
            router,
            case,
            region,
            "CreateVpc",
            &[("CidrBlock", "10.0.0.0/16")],
        )
        .await,
        "vpcId",
    );
    let zone = format!("{region}a");
    element(
        &ec2(
            router,
            case,
            region,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", "10.0.1.0/24"),
                ("AvailabilityZone", &zone),
            ],
        )
        .await,
        "subnetId",
    )
}

/// The launch template the groups below launch from, created in `region`.
async fn given_launch_template(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    let image = published_image_id(router, case, region).await;
    ec2(
        router,
        case,
        region,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", name),
            ("LaunchTemplateData.ImageId", &image),
            ("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
}

/// The (topic, type) pairs the group `name` is configured with, as a client reads them back.
async fn configured_pairs(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
) -> Vec<(String, String)> {
    let described = autoscaling(
        router,
        case,
        region,
        "DescribeNotificationConfigurations",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    let list = block_of(&described, "NotificationConfigurations").unwrap_or_default();
    let mut pairs: Vec<(String, String)> = members_of(list)
        .into_iter()
        .filter_map(|member| {
            let topic = elements(member, "TopicARN").into_iter().next()?;
            let event = elements(member, "NotificationType").into_iter().next()?;
            Some((topic, event))
        })
        .collect();
    pairs.sort();
    pairs
}

/// The instance ids the group `name` reports in `LifecycleState` `state` right now.
async fn group_instances_in(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    state: &str,
) -> Vec<String> {
    let described = autoscaling(
        router,
        case,
        region,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
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

/// Poll the group until it reports exactly `wanted` instances in `state`, or the deadline
/// passes.
async fn await_instances_in(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    state: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut held = group_instances_in(router, case, region, name, state).await;
    while held.len() != wanted && Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        held = group_instances_in(router, case, region, name, state).await;
    }
    held
}

/// Configuring a group's notifications confirms itself on the topic it names and on no
/// other topic, and the configurations a client stores are what it reads back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_configured_topic_is_confirmed_and_an_unconfigured_one_is_untouched() {
    let case = "a group at capacity 0 in a VPC subnet, with one configured and one \
                unconfigured topic";
    let region = "us-west-2";
    let router = router();

    let (lit, lit_queue) =
        given_topic_with_subscriber(&router, case, region, "asg-lit", Grant::ScopedToTopic).await;
    let (dark, dark_queue) =
        given_topic_with_subscriber(&router, case, region, "asg-dark", Grant::ScopedToTopic).await;

    // Both pipes are proved live before anything is asserted about Auto Scaling, so an
    // absence afterwards is the publisher's and not the pipe's.
    for (topic, queue) in [(&lit, &lit_queue), (&dark, &dark_queue)] {
        sns(
            &router,
            case,
            region,
            "Publish",
            &[("TopicArn", topic), ("Message", "canary")],
        )
        .await;
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let mut delivered = drain(&router, case, region, queue).await;
        while delivered.is_empty() && Instant::now() < deadline {
            tokio::time::sleep(LOOK_INTERVAL).await;
            delivered = drain(&router, case, region, queue).await;
        }
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the canary publish to {topic} must reach its queue, got {delivered:?}"
        );
    }

    let subnet = given_subnet(&router, case, region).await;
    let template = "asg-notification-template";
    given_launch_template(&router, case, region, template).await;

    let group = "asg-notified-group";
    autoscaling(
        &router,
        case,
        region,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", template),
            ("VPCZoneIdentifier", &subnet),
        ],
    )
    .await;
    let arn = element(
        &autoscaling(
            &router,
            case,
            region,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await,
        "AutoScalingGroupARN",
    );

    autoscaling(
        &router,
        case,
        region,
        "PutNotificationConfiguration",
        &[
            ("AutoScalingGroupName", group),
            ("TopicARN", &lit),
            ("NotificationTypes.member.1", TEST_NOTIFICATION),
        ],
    )
    .await;

    let confirmations = drain_until(&router, case, region, &lit_queue, TEST_NOTIFICATION, 1).await;
    assert_eq!(
        confirmations.len(),
        1,
        "case {case:?}: {lit} must receive one {TEST_NOTIFICATION}, got {confirmations:?}"
    );
    let (envelope, document) = &confirmations[0];
    assert_eq!(
        envelope["TopicArn"],
        json!(lit),
        "case {case:?}: the envelope must name the topic it was published to, got {envelope}"
    );
    assert_eq!(
        envelope["Type"],
        json!("Notification"),
        "case {case:?}: the envelope must be an SNS notification, got {envelope}"
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
    // Read against a pipe already known to have delivered: the lit topic's confirmation is
    // in hand, so an empty dark queue is the selection and not a delivery still in flight.
    let withheld: Vec<Value> = drain(&router, case, region, &dark_queue)
        .await
        .into_iter()
        .map(|(_, document)| document)
        .collect();
    assert!(
        withheld.is_empty(),
        "case {case:?}: {dark} is configured for nothing and must receive nothing, got \
         {withheld:?}"
    );

    // The second topic becomes live the moment it is configured, and only then.
    autoscaling(
        &router,
        case,
        region,
        "PutNotificationConfiguration",
        &[
            ("AutoScalingGroupName", group),
            ("TopicARN", &dark),
            ("NotificationTypes.member.1", TEST_NOTIFICATION),
        ],
    )
    .await;
    let second = drain_until(&router, case, region, &dark_queue, TEST_NOTIFICATION, 1).await;
    assert_eq!(
        second.len(),
        1,
        "case {case:?}: {dark} must receive one {TEST_NOTIFICATION} once configured, got \
         {second:?}"
    );

    let mut expected = vec![
        (dark.clone(), TEST_NOTIFICATION.to_string()),
        (lit.clone(), TEST_NOTIFICATION.to_string()),
    ];
    expected.sort();
    let read = configured_pairs(&router, case, region, group).await;
    assert_eq!(
        read, expected,
        "case {case:?}: expected the two configured pairs {expected:?}, got {read:?}"
    );

    autoscaling(
        &router,
        case,
        region,
        "DeleteNotificationConfiguration",
        &[("AutoScalingGroupName", group), ("TopicARN", &lit)],
    )
    .await;
    let read = configured_pairs(&router, case, region, group).await;
    assert_eq!(
        read,
        vec![(dark.clone(), TEST_NOTIFICATION.to_string())],
        "case {case:?}: deleting one pair must leave exactly the other, got {read:?}"
    );
    autoscaling(
        &router,
        case,
        region,
        "DeleteNotificationConfiguration",
        &[("AutoScalingGroupName", group), ("TopicARN", &dark)],
    )
    .await;
    let read = configured_pairs(&router, case, region, group).await;
    assert!(
        read.is_empty(),
        "case {case:?}: deleting both pairs must leave none, got {read:?}"
    );
}

/// WHICH door took the instance out of the group, which the rule does not distinguish: the
/// publication is selected and rendered where the lifecycle step is recorded, not where the
/// request arrived.
#[derive(Debug, Clone, Copy)]
enum Door {
    /// The capacity change that leaves the group wanting one instance fewer.
    SetDesiredCapacity,
    /// The caller naming the instance to take out, decrementing the desired capacity with it.
    TerminateInstanceInAutoScalingGroup,
}

/// Moving a group's capacity up and then taking the instance back out publishes the launch
/// and the termination, each naming the instance that moved and the activity that moved it
/// — whichever door performed the termination.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_door_that_moves_an_instance_publishes_the_instance_it_moved() {
    // (case, the door the termination is asked for through)
    let rows: &[(&str, Door)] = &[
        ("a capacity change back to 0", Door::SetDesiredCapacity),
        (
            "the caller naming the instance to take out",
            Door::TerminateInstanceInAutoScalingGroup,
        ),
    ];

    for (case, door) in rows {
        let region = "us-west-2";
        let router = router();
        let (topic, queue) =
            given_topic_with_subscriber(&router, case, region, "asg-moves", Grant::Unscoped).await;
        let subnet = given_subnet(&router, case, region).await;
        let template = "asg-move-template";
        given_launch_template(&router, case, region, template).await;

        let group = "asg-moved-group";
        autoscaling(
            &router,
            case,
            region,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", group),
                ("MinSize", "0"),
                ("MaxSize", "1"),
                ("DesiredCapacity", "0"),
                ("LaunchTemplate.LaunchTemplateName", template),
                ("VPCZoneIdentifier", &subnet),
            ],
        )
        .await;
        autoscaling(
            &router,
            case,
            region,
            "PutNotificationConfiguration",
            &[
                ("AutoScalingGroupName", group),
                ("TopicARN", &topic),
                ("NotificationTypes.member.1", INSTANCE_LAUNCH),
                ("NotificationTypes.member.2", INSTANCE_TERMINATE),
            ],
        )
        .await;

        autoscaling(
            &router,
            case,
            region,
            "SetDesiredCapacity",
            &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
        )
        .await;
        let in_service = await_instances_in(&router, case, region, group, "InService", 1).await;
        assert_eq!(
            in_service.len(),
            1,
            "case {case:?}: the group must report one InService instance, got {in_service:?}"
        );
        let launched = drain_until(&router, case, region, &queue, INSTANCE_LAUNCH, 1).await;
        assert_eq!(
            launched.len(),
            1,
            "case {case:?}: one {INSTANCE_LAUNCH} must be delivered, got {launched:?}"
        );
        assert_eq!(
            launched[0].1["EC2InstanceId"],
            json!(in_service[0]),
            "case {case:?}: the launch must name the instance the group holds, got {}",
            launched[0].1
        );
        assert_eq!(
            launched[0].1["AutoScalingGroupName"],
            json!(group),
            "case {case:?}: the launch must name the group, got {}",
            launched[0].1
        );

        match door {
            Door::SetDesiredCapacity => {
                autoscaling(
                    &router,
                    case,
                    region,
                    "SetDesiredCapacity",
                    &[("AutoScalingGroupName", group), ("DesiredCapacity", "0")],
                )
                .await;
            }
            Door::TerminateInstanceInAutoScalingGroup => {
                autoscaling(
                    &router,
                    case,
                    region,
                    "TerminateInstanceInAutoScalingGroup",
                    &[
                        ("InstanceId", &in_service[0]),
                        ("ShouldDecrementDesiredCapacity", "true"),
                    ],
                )
                .await;
            }
        }

        let terminated = drain_until(&router, case, region, &queue, INSTANCE_TERMINATE, 1).await;
        assert_eq!(
            terminated.len(),
            1,
            "case {case:?}: one {INSTANCE_TERMINATE} must be delivered, got {terminated:?}"
        );
        assert_eq!(
            terminated[0].1["EC2InstanceId"],
            json!(in_service[0]),
            "case {case:?}: the termination must name the instance that left, got {}",
            terminated[0].1
        );
        assert_eq!(
            terminated[0].1["AutoScalingGroupName"],
            json!(group),
            "case {case:?}: the termination must name the group, got {}",
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
                &router,
                case,
                region,
                "DescribeScalingActivities",
                &[("AutoScalingGroupName", group)],
            )
            .await,
            "ActivityId",
        );
        for activity in [launch_activity, terminate_activity] {
            assert!(
                activities.contains(&activity.to_string()),
                "case {case:?}: {activity} must be one of the group's scaling activities, got \
                 {activities:?}"
            );
        }

        // The instance the termination named is really going away in Amazon EC2, read out of
        // that instance's OWN state element rather than off the first `name` of the answer.
        let described = ec2(
            &router,
            case,
            region,
            "DescribeInstances",
            &[("InstanceId.1", &in_service[0])],
        )
        .await;
        let state_block = block_of(&described, "instanceState").unwrap_or_default();
        let state = elements(state_block, "name");
        assert!(
            state
                .iter()
                .any(|value| value == "shutting-down" || value == "terminated"),
            "case {case:?}: Amazon EC2 must report {} shutting down or terminated, got \
             {state:?} in {described}",
            in_service[0]
        );
    }
}
