// SPDX-License-Identifier: CC0-1.0

//! Amazon EC2 Auto Scaling publishes an instance lifecycle event when a group scales.
//!
//! The RULE under test: every instance an Auto Scaling group LAUNCHES is published onto
//! the account's DEFAULT EventBridge bus as `source` `aws.autoscaling` / `detail-type`
//! `EC2 Instance Launch Successful`, and every instance it TERMINATES as
//! `EC2 Instance Terminate Successful`, whose `detail` carries the scaling activity that
//! moved it (the group's name, the activity's id, description and cause, the instance id
//! and the placement) and whose `resources` names the group and that instance; EventBridge
//! then routes it, so a rule whose `EventPattern` matches delivers the envelope to that
//! rule's targets and a rule that cannot match receives nothing. The events the clock owes
//! are published as they fall due, with NO further Auto Scaling call: a client watching a
//! group through EventBridge makes none.
//!
//! The rows span what the rule generalises over: WHICH door moved the capacity (the create
//! itself, a later `SetDesiredCapacity` up or down, a forced delete of the group), how many
//! instances one change moves, which of the two lifecycle steps it is, and whether the
//! target queue has granted EventBridge the `sqs:SendMessage` a delivery needs. The last
//! test is the negative half of the contract and the recorded scenario behind it: a rule
//! matching only the terminate detail-type receives nothing from a launch, and correcting
//! that SAME rule's pattern delivers the next one.
//!
//! EventBridge and Amazon SQS are mounted BESIDE Auto Scaling and Amazon EC2 on the SAME
//! engine, so a publication travels the real in-process callback, through the real bus, to
//! a real SQS target: nothing here reads the publisher's own queue.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;

use crate::support::{borrowed, ec2_query, p, query, seed_subnets, tag_value, tag_values, REGION};

/// The `source` and the two `detail-type`s Auto Scaling publishes its instance lifecycle
/// events under, spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a test pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";

/// The rule that matches this service's events, and the rule that cannot.
const MATCHING_RULE: &str = "autoscaling-instance-lifecycle";
const OTHER_RULE: &str = "unrelated-events";

/// The launch template every group below launches from.
const TEMPLATE: &str = "lifecycle-template";

/// How often a poll re-reads the target queue.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle span the service owes a
/// transition, with ample margin for a loaded test machine — the whole workspace's suites
/// run in parallel, so the margin is generous rather than tight. A poll waits for the
/// CONDITION and returns as soon as it holds, so the margin costs nothing when the
/// publication is prompt.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(60)
}

/// How long a row waits before asserting that NOTHING was delivered: long enough that a
/// delivery that was going to happen would have.
fn quiet_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(1)
}

/// One emulator: Auto Scaling, Amazon EC2, EventBridge and Amazon SQS over ONE shared
/// engine — the four services a published lifecycle event travels through, mounted the way
/// the production router mounts them.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// One `awsJson` RPC call against a service mounted beside Auto Scaling, through the one
/// copy of that call this crate's suites share ([`crate::support::json_rpc`]).
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    crate::support::json_rpc(
        router,
        signing_name,
        target_prefix,
        json_version,
        operation,
        body,
    )
    .await
}

/// One EventBridge call.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    rpc(router, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// An SQS queue called `name`, created through Amazon SQS itself; answers its url and the
/// ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, body) = sqs(router, "CreateQueue", json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {body}"
    );
    let url = body["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {body}"))
        .to_string();
    let (status, body) = sqs(
        router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {body}"
    );
    let queue_arn = body["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {body}"))
        .to_string();
    (url, queue_arn)
}

/// Grant `events.amazonaws.com` the `sqs:SendMessage` a rule's target needs on the queue at
/// `url` — EventBridge makes that call as its own service principal and relies on the
/// queue's resource-based policy for it, so a queue that has granted it nothing receives
/// nothing.
async fn grant_delivery(router: &MultiServiceRouter, case: &str, url: &str, queue_arn: &str) {
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
    let (status, body) = sqs(
        router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {body}"
    );
}

/// Put the rule `name` with `pattern`, and point it at the SQS queue `queue_arn`.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
    queue_arn: &str,
) {
    put_rule(router, case, name, pattern).await;
    let (status, body) = events(
        router,
        "PutTargets",
        json!({ "Rule": name, "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}/{name}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}/{name}: PutTargets reported a failed entry: {body}"
    );
}

/// Put (or re-put) the rule `name` with `pattern`, leaving its targets alone.
async fn put_rule(router: &MultiServiceRouter, case: &str, name: &str, pattern: &Value) {
    let (status, body) = events(
        router,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}/{name}: PutRule answered {status} {body}"
    );
}

/// The pattern a rule carries to match one of this service's detail types on the default
/// bus.
fn lifecycle_pattern(detail_type: &str) -> Value {
    json!({ "source": [EVENT_SOURCE], "detail-type": [detail_type] })
}

/// Every message waiting on the queue at `url` right now, as parsed event envelopes.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, body) = sqs(
        router,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {body}"
    );
    body["Messages"]
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

/// Poll the queue at `url` — and ONLY the queue — until it has delivered `wanted` events
/// carrying `detail_type`, or the budget runs out.
///
/// Nothing in this loop calls Auto Scaling, which is the point: a client that watches a
/// group through EventBridge makes no further call to the service that publishes.
async fn collect_events(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    wanted: usize,
) -> Vec<Value> {
    // ONE polling loop in this suite ([`poll_for_detail_type`]); this caller keeps nothing of
    // the other detail type, so its buffer is its own and dropped with the call.
    let mut buffer: Vec<Value> = Vec::new();
    poll_for_detail_type(router, case, url, detail_type, wanted, &mut buffer).await
}

/// Create the launch template and the subnet every group of this suite launches into, and
/// answer the subnet id.
async fn given_launch_source(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
) -> String {
    let subnet = seed_subnets(engine, 1)
        .first()
        .expect("one seeded subnet")
        .id
        .clone();
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", TEMPLATE),
            p("LaunchTemplateData.ImageId", "ami-0abcdef1234567890"),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate expected 200, got {status}: {body}"
    );
    subnet
}

/// Create one group with `desired` capacity, and answer its ARN.
async fn given_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    subnet: &str,
    desired: i32,
) -> String {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "0"),
        p("MaxSize", "3"),
        p("DesiredCapacity", desired.to_string()),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("VPCZoneIdentifier", subnet),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    group_arn(router, case, name).await
}

/// The ARN the service reports for the group `name`.
async fn group_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "AutoScalingGroupARN");
    assert!(
        !arn.is_empty(),
        "case {case:?}: the group must report its ARN, got {body}"
    );
    arn
}

/// Ask for `desired` capacity on the group `name`.
async fn set_desired_capacity(router: &MultiServiceRouter, case: &str, name: &str, desired: i32) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", name),
            ("DesiredCapacity", &desired.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity expected 200, got {status}: {body}"
    );
}

/// Every activity id the group's own activity log holds, which is what an event's
/// `detail.ActivityId` must name.
///
/// Read off the shared store rather than over the wire because
/// `DescribeScalingActivities` is still a scaffolded stub in this crate (its own issue
/// owns it): the store is where the handler wrote the activity, so this is the same fact
/// that reader will report.
fn activity_ids(engine: &Arc<engine::InMemoryEngine>) -> Vec<String> {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let ids = state
        .lock()
        .activities
        .iter()
        .map(|activity| activity.activity_id.clone())
        .collect();
    ids
}

/// Assert one delivered envelope is a lifecycle event of `group` about an instance, and
/// answer the instance id it names.
fn assert_envelope(case: &str, event: &Value, detail_type: &str, group: &str, arn: &str) -> String {
    assert_eq!(
        event["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: expected source {EVENT_SOURCE}, got {event}"
    );
    assert_eq!(
        event["detail-type"],
        json!(detail_type),
        "case {case:?}: expected detail-type {detail_type}, got {event}"
    );
    assert_eq!(
        event["detail"]["AutoScalingGroupName"],
        json!(group),
        "case {case:?}: expected the group {group} in the detail, got {event}"
    );
    let instance_id = event["detail"]["EC2InstanceId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        instance_id.starts_with("i-"),
        "case {case:?}: expected an EC2 instance id in the detail, got {event}"
    );
    // The event names every resource it is about: the group, and the instance that moved.
    assert_eq!(
        event["resources"],
        json!([arn, format!("arn:aws:ec2:{REGION}:{}:instance/{instance_id}", engine::emulator_account_id())]),
        "case {case:?}: expected the group's and the instance's ARNs in resources, got {event}"
    );
    assert!(
        event["detail"]["Description"]
            .as_str()
            .unwrap_or_default()
            .contains(&instance_id),
        "case {case:?}: the activity's description names the instance it moved, got {event}"
    );
    assert!(
        !event["detail"]["Cause"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "case {case:?}: the activity's cause is reported, got {event}"
    );
    instance_id
}

/// Which door moves the capacity a row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    /// The create's own `DesiredCapacity`.
    Create,
    /// A later `SetDesiredCapacity`, up or down to the row's capacity.
    SetDesiredCapacity,
    /// A forced delete of the group, which terminates what it was holding.
    ForcedDelete,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The capacity the group is created with.
    created_with: i32,
    /// Which door moves it next, when one does.
    door: Door,
    /// The capacity that door asks for.
    moved_to: i32,
    /// The detail type the row's own change publishes.
    detail_type: &'static str,
    /// How many events of that type the change publishes.
    expected: usize,
    /// Whether the target queue grants EventBridge the delivery it needs.
    granted: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a scale-out asked for by the create itself",
        created_with: 1,
        door: Door::Create,
        moved_to: 1,
        detail_type: LAUNCH_SUCCESSFUL,
        expected: 1,
        granted: true,
    },
    Row {
        case: "a scale-out of two instances asked for by the create",
        created_with: 2,
        door: Door::Create,
        moved_to: 2,
        detail_type: LAUNCH_SUCCESSFUL,
        expected: 2,
        granted: true,
    },
    Row {
        case: "a scale-out asked for by a later SetDesiredCapacity",
        created_with: 0,
        door: Door::SetDesiredCapacity,
        moved_to: 1,
        detail_type: LAUNCH_SUCCESSFUL,
        expected: 1,
        granted: true,
    },
    Row {
        case: "a scale-in asked for by a later SetDesiredCapacity",
        created_with: 2,
        door: Door::SetDesiredCapacity,
        moved_to: 0,
        detail_type: TERMINATE_SUCCESSFUL,
        expected: 2,
        granted: true,
    },
    Row {
        case: "the terminations a forced delete of the group makes",
        created_with: 1,
        door: Door::ForcedDelete,
        moved_to: 0,
        detail_type: TERMINATE_SUCCESSFUL,
        expected: 1,
        granted: true,
    },
    Row {
        case: "a target queue that granted no delivery",
        created_with: 1,
        door: Door::Create,
        moved_to: 1,
        detail_type: LAUNCH_SUCCESSFUL,
        expected: 0,
        granted: false,
    },
];

#[tokio::test]
async fn a_group_that_scales_publishes_an_instance_lifecycle_event() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's groups, rules and policies cannot decide
        // another's.
        let (router, engine) = emulator();
        let subnet = given_launch_source(&router, &engine).await;

        let (url, queue_arn) = given_queue(&router, case, "autoscaling-events").await;
        let (other_url, other_arn) = given_queue(&router, case, OTHER_RULE).await;
        given_rule(
            &router,
            case,
            MATCHING_RULE,
            &lifecycle_pattern(row.detail_type),
            &queue_arn,
        )
        .await;
        // Another source's events: nothing this row publishes can match it.
        given_rule(
            &router,
            case,
            OTHER_RULE,
            &json!({ "source": ["aws.ecs"] }),
            &other_arn,
        )
        .await;
        if row.granted {
            grant_delivery(&router, case, &url, &queue_arn).await;
        }
        grant_delivery(&router, case, &other_url, &other_arn).await;

        let group = format!("lifecycle-{index}");
        let arn = given_group(&router, case, &group, &subnet, row.created_with).await;
        match row.door {
            Door::Create => {}
            Door::SetDesiredCapacity => {
                // Past one span, so the instances the create launched are in service
                // before the next change moves them.
                tokio::time::sleep(poll_budget().min(quiet_budget())).await;
                set_desired_capacity(&router, case, &group, row.moved_to).await;
            }
            Door::ForcedDelete => {
                tokio::time::sleep(poll_budget().min(quiet_budget())).await;
                let (status, body) = query(
                    &router,
                    "DeleteAutoScalingGroup",
                    &[
                        ("AutoScalingGroupName", group.as_str()),
                        ("ForceDelete", "true"),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DeleteAutoScalingGroup expected 200, got {status}: {body}"
                );
            }
        }

        let delivered = collect_events(&router, case, &url, row.detail_type, row.expected).await;

        if !row.granted {
            // Nothing was delivered, and `PutTargets` still succeeded: the queue's own
            // policy is what withheld it.
            tokio::time::sleep(quiet_budget()).await;
            let late = drain(&router, case, &url).await;
            assert!(
                delivered.is_empty() && late.is_empty(),
                "case {case:?}: a queue that granted nothing must receive nothing, got \
                 {delivered:?} then {late:?}"
            );
            continue;
        }

        assert_eq!(
            delivered.len(),
            row.expected,
            "case {case:?}: expected {} {} event(s), got {delivered:?}",
            row.expected,
            row.detail_type
        );
        let mut named: Vec<String> = Vec::new();
        for event in &delivered {
            named.push(assert_envelope(case, event, row.detail_type, &group, &arn));
        }
        // Every event is about a different instance: one step of one instance is one
        // event.
        named.sort();
        let distinct = {
            let mut distinct = named.clone();
            distinct.dedup();
            distinct
        };
        assert_eq!(
            named, distinct,
            "case {case:?}: each instance is reported once, got {named:?}"
        );
        // The activity each event names is one the group's own activity log holds: the
        // event reports the activity the service recorded, not a second copy of it.
        let recorded = activity_ids(&engine);
        for event in &delivered {
            let activity_id = event["detail"]["ActivityId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                recorded.contains(&activity_id),
                "case {case:?}: the event's ActivityId {activity_id:?} must be one the \
                 group's activity log holds, got {recorded:?}"
            );
        }

        // The rule that cannot match this service's events received nothing.
        let unrelated = drain(&router, case, &other_url).await;
        assert!(
            unrelated.is_empty(),
            "case {case:?}: a rule whose pattern cannot match must receive nothing, got \
             {unrelated:?}"
        );
    }
}

#[tokio::test]
async fn correcting_a_rules_pattern_delivers_the_next_scale_out() {
    let case = "a rule whose pattern is corrected from terminate to launch";
    let (router, engine) = emulator();
    let subnet = given_launch_source(&router, &engine).await;
    let (url, queue_arn) = given_queue(&router, case, "autoscaling-events").await;
    // The rule starts out matching ONLY the terminate detail-type, so a launch cannot
    // reach it.
    given_rule(
        &router,
        case,
        MATCHING_RULE,
        &lifecycle_pattern(TERMINATE_SUCCESSFUL),
        &queue_arn,
    )
    .await;
    grant_delivery(&router, case, &url, &queue_arn).await;

    let group = "lifecycle-corrected";
    let arn = given_group(&router, case, group, &subnet, 0).await;
    set_desired_capacity(&router, case, group, 1).await;
    tokio::time::sleep(quiet_budget()).await;
    let delivered = drain(&router, case, &url).await;
    assert!(
        delivered.is_empty(),
        "case {case:?}: a rule matching only {TERMINATE_SUCCESSFUL} receives nothing from a \
         launch, got {delivered:?}"
    );

    // The SAME rule, with the pattern corrected to the launch detail-type: the NEXT
    // scale-out arrives at its target.
    put_rule(
        &router,
        case,
        MATCHING_RULE,
        &lifecycle_pattern(LAUNCH_SUCCESSFUL),
    )
    .await;
    set_desired_capacity(&router, case, group, 2).await;
    let delivered = collect_events(&router, case, &url, LAUNCH_SUCCESSFUL, 1).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: the corrected rule's target receives the next launch, got \
         {delivered:?}"
    );
    let instance_id = assert_envelope(case, &delivered[0], LAUNCH_SUCCESSFUL, group, &arn);

    // The instance the event names is one the group itself reports: the envelope is about
    // a real instance of the account, not an id invented for the event.
    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    assert!(
        tag_values(&body, "InstanceId").contains(&instance_id),
        "case {case:?}: the group must report the instance {instance_id} the event named, \
         got {body}"
    );
}

// ---------------------------------------------------------------------------
// The DIRECTION of a lifecycle step.
//
// The RULE: a lifecycle event's `detail` names the two ends of the move the step made —
// `Origin`, where the instance came from, and `Destination`, where it went — as fixed text
// decided by the step's direction alone, never by anything in the request: a group that
// LAUNCHES an instance takes it from Amazon EC2 into the group ("EC2" -> "AutoScalingGroup")
// and a group that SCALES ONE IN hands it back ("AutoScalingGroup" -> "EC2"), so the two are
// each other's reverse. The two spellings are written out here, not imported, so a wrong
// constant in the implementation cannot make these tests pass.
// ---------------------------------------------------------------------------

/// The endpoint outside the group: Amazon EC2 itself.
const ENDPOINT_EC2: &str = "EC2";
/// The endpoint inside it: the group.
const ENDPOINT_GROUP: &str = "AutoScalingGroup";

/// The `(Origin, Destination)` pair the event of `detail_type` must carry.
fn expected_direction(detail_type: &str) -> (&'static str, &'static str) {
    if detail_type == LAUNCH_SUCCESSFUL {
        (ENDPOINT_EC2, ENDPOINT_GROUP)
    } else {
        (ENDPOINT_GROUP, ENDPOINT_EC2)
    }
}

/// A rule pattern matching every event of this service, whichever step it reports.
fn any_lifecycle_pattern() -> Value {
    json!({ "source": [EVENT_SOURCE] })
}

/// Poll the queue at `url` until `buffer` holds `wanted` events of `detail_type`, or the
/// budget runs out; answer those events.
///
/// Everything drained goes into `buffer`, so a step whose events arrive while another step's
/// are being waited for is kept rather than thrown away — the queue carries both directions.
async fn poll_for_detail_type(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    wanted: usize,
    buffer: &mut Vec<Value>,
) -> Vec<Value> {
    let deadline = Instant::now() + poll_budget();
    loop {
        buffer.extend(drain(router, case, url).await);
        let found: Vec<Value> = buffer
            .iter()
            .filter(|event| event["detail-type"] == json!(detail_type))
            .cloned()
            .collect();
        if found.len() >= wanted || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert every event of `detail_type` delivered so far carries the direction that step
/// moves in, and that there are `wanted` of them.
async fn assert_direction_of_step(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    wanted: usize,
    buffer: &mut Vec<Value>,
) {
    let delivered =
        poll_for_detail_type(router, case, url, detail_type, wanted, buffer).await;
    assert_eq!(
        delivered.len(),
        wanted,
        "case {case:?}: expected {wanted} {detail_type} event(s), got {delivered:?}"
    );
    let (origin, destination) = expected_direction(detail_type);
    for event in &delivered {
        assert_eq!(
            (event["detail"]["Origin"].clone(), event["detail"]["Destination"].clone()),
            (json!(origin), json!(destination)),
            "case {case:?}: a {detail_type} event must flow {origin} -> {destination}, got \
             {event}"
        );
    }
}

/// One row of the direction table: the capacity the group is created with, the capacities a
/// later `SetDesiredCapacity` asks for, and whether a forced delete finishes it off.
struct DirectionRow {
    case: &'static str,
    created_with: i32,
    steps: &'static [i32],
    forced_delete: bool,
}

/// The rows span what the rule generalises over: which door moved the capacity (the create,
/// a later `SetDesiredCapacity`, a forced delete), how many instances one change moves, and
/// which of the two steps — in both orders, so neither direction is read off the other.
const DIRECTION_ROWS: &[DirectionRow] = &[
    DirectionRow {
        case: "one instance launched by the create, then scaled in to none",
        created_with: 1,
        steps: &[0],
        forced_delete: false,
    },
    DirectionRow {
        case: "two launched by the create, one of them scaled in",
        created_with: 2,
        steps: &[1],
        forced_delete: false,
    },
    DirectionRow {
        case: "an empty group scaled out by SetDesiredCapacity, then back in",
        created_with: 0,
        steps: &[2, 0],
        forced_delete: false,
    },
    DirectionRow {
        case: "two launched by the create and taken out by a forced delete",
        created_with: 2,
        steps: &[],
        forced_delete: true,
    },
];

#[tokio::test]
async fn a_lifecycle_event_names_where_the_instance_came_from_and_went() {
    for (index, row) in DIRECTION_ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's events cannot be counted for another.
        let (router, engine) = emulator();
        let subnet = given_launch_source(&router, &engine).await;
        let (url, queue_arn) = given_queue(&router, case, "autoscaling-events").await;
        // ONE rule for both steps: the direction is read off the event, not off which rule
        // delivered it.
        given_rule(
            &router,
            case,
            MATCHING_RULE,
            &any_lifecycle_pattern(),
            &queue_arn,
        )
        .await;
        grant_delivery(&router, case, &url, &queue_arn).await;

        let group = format!("direction-{index}");
        let _arn = given_group(&router, case, &group, &subnet, row.created_with).await;
        let mut buffer: Vec<Value> = Vec::new();
        let mut launched = usize::try_from(row.created_with).unwrap_or(0);
        let mut terminated = 0usize;
        let mut capacity = row.created_with;
        if launched > 0 {
            assert_direction_of_step(
                &router,
                case,
                &url,
                LAUNCH_SUCCESSFUL,
                launched,
                &mut buffer,
            )
            .await;
        }
        for step in row.steps {
            // The events of the previous step have been delivered by now (they were waited
            // for), so the group has settled before the next door moves it.
            set_desired_capacity(&router, case, &group, *step).await;
            let moved = usize::try_from((*step - capacity).abs()).unwrap_or(0);
            if *step > capacity {
                launched += moved;
                assert_direction_of_step(
                    &router,
                    case,
                    &url,
                    LAUNCH_SUCCESSFUL,
                    launched,
                    &mut buffer,
                )
                .await;
            } else if *step < capacity {
                terminated += moved;
                assert_direction_of_step(
                    &router,
                    case,
                    &url,
                    TERMINATE_SUCCESSFUL,
                    terminated,
                    &mut buffer,
                )
                .await;
            }
            capacity = *step;
        }
        if row.forced_delete {
            let (status, body) = query(
                &router,
                "DeleteAutoScalingGroup",
                &[
                    ("AutoScalingGroupName", group.as_str()),
                    ("ForceDelete", "true"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteAutoScalingGroup expected 200, got {status}: {body}"
            );
            terminated += usize::try_from(capacity).unwrap_or(0);
            assert_direction_of_step(
                &router,
                case,
                &url,
                TERMINATE_SUCCESSFUL,
                terminated,
                &mut buffer,
            )
            .await;
        }
        // Both halves of the row were observed: every row both launches and scales in, so a
        // row that silently skipped one of the two directions fails here instead of passing
        // the half it never reached.
        assert!(
            launched > 0 && terminated > 0,
            "case {case:?}: the row must move instances in both directions, launched \
             {launched} and terminated {terminated}"
        );
    }
}

#[tokio::test]
async fn a_rule_pattern_on_the_direction_separates_scale_in_from_scale_out() {
    // What the fields are FOR: an EventBridge pattern keyed on the direction routes a
    // scale-in apart from a scale-out, without either rule naming a detail-type.
    let case = "rules keyed on detail.Origin alone";
    let (router, engine) = emulator();
    let subnet = given_launch_source(&router, &engine).await;
    let (out_url, out_arn) = given_queue(&router, case, "scale-out-events").await;
    let (in_url, in_arn) = given_queue(&router, case, "scale-in-events").await;
    given_rule(
        &router,
        case,
        "direction-out",
        &json!({ "source": [EVENT_SOURCE], "detail": { "Origin": [ENDPOINT_EC2] } }),
        &out_arn,
    )
    .await;
    given_rule(
        &router,
        case,
        "direction-in",
        &json!({ "source": [EVENT_SOURCE], "detail": { "Origin": [ENDPOINT_GROUP] } }),
        &in_arn,
    )
    .await;
    grant_delivery(&router, case, &out_url, &out_arn).await;
    grant_delivery(&router, case, &in_url, &in_arn).await;

    let group = "direction-routed";
    let arn = given_group(&router, case, group, &subnet, 2).await;
    let mut out_buffer: Vec<Value> = Vec::new();
    let launches =
        poll_for_detail_type(&router, case, &out_url, LAUNCH_SUCCESSFUL, 2, &mut out_buffer).await;
    assert_eq!(
        launches.len(),
        2,
        "case {case:?}: the rule matching Origin {ENDPOINT_EC2} receives the two launches, \
         got {launches:?}"
    );
    for event in &launches {
        assert_envelope(case, event, LAUNCH_SUCCESSFUL, group, &arn);
    }

    set_desired_capacity(&router, case, group, 0).await;
    let mut in_buffer: Vec<Value> = Vec::new();
    let terminations = poll_for_detail_type(
        &router,
        case,
        &in_url,
        TERMINATE_SUCCESSFUL,
        2,
        &mut in_buffer,
    )
    .await;
    assert_eq!(
        terminations.len(),
        2,
        "case {case:?}: the rule matching Origin {ENDPOINT_GROUP} receives the two \
         terminations, got {terminations:?}"
    );
    for event in &terminations {
        assert_envelope(case, event, TERMINATE_SUCCESSFUL, group, &arn);
    }
    // Neither rule received the other direction: every envelope each queue holds is the
    // step its pattern named.
    assert!(
        out_buffer
            .iter()
            .all(|event| event["detail-type"] == json!(LAUNCH_SUCCESSFUL)),
        "case {case:?}: the scale-out rule must receive only launches, got {out_buffer:?}"
    );
    assert!(
        in_buffer
            .iter()
            .all(|event| event["detail-type"] == json!(TERMINATE_SUCCESSFUL)),
        "case {case:?}: the scale-in rule must receive only terminations, got {in_buffer:?}"
    );
}

// ---------------------------------------------------------------------------
// Filtering by the GROUP a step belongs to.
//
// The RULE: a lifecycle event's `detail` names the group whose step it reports in
// `AutoScalingGroupName`, so a rule whose `EventPattern` filters on that member receives
// every step of the group it names — launches and terminations alike, whichever door moved
// the capacity — and no step of any other group. The member carries the group's own name,
// so a pattern naming a group the account does not hold matches nothing.
//
// The rows span what the rule generalises over: WHICH of several groups a pattern names,
// how the pattern names it (the whole name, or a prefix of it), which OTHER envelope fields
// the pattern constrains beside the group (both detail types, neither, the source alone),
// and a name no group answers to at all.

/// The two groups that scale while the rules below watch, and the name no group carries.
const GROUP_ALPHA: &str = "lifecycle-alpha";
const GROUP_BETA: &str = "lifecycle-beta";
const GROUP_ABSENT: &str = "lifecycle-gamma";

/// How the capacity of each group moves: `ALPHA` launches two instances and scales one in,
/// `BETA` launches one and scales it in, so each group owes a different number of events of
/// each step and a leak between the two queues is visible as a count.
const ALPHA_CREATED_WITH: i32 = 2;
const ALPHA_SCALED_TO: i32 = 1;
const BETA_CREATED_WITH: i32 = 1;
const BETA_SCALED_TO: i32 = 0;

/// One row of the group-filter table: the pattern a rule carries, and the steps of which
/// group that rule must therefore receive.
struct GroupFilterRow {
    case: &'static str,
    /// The whole `EventPattern` the rule is put with.
    pattern: fn() -> Value,
    /// The group whose steps the rule must receive, when it receives any.
    group: Option<&'static str>,
}

const GROUP_FILTER_ROWS: &[GroupFilterRow] = &[
    GroupFilterRow {
        case: "a rule naming the whole name of the two-instance group, beside both steps",
        pattern: || {
            json!({
                "source": [EVENT_SOURCE],
                "detail-type": [LAUNCH_SUCCESSFUL, TERMINATE_SUCCESSFUL],
                "detail": { "AutoScalingGroupName": [GROUP_ALPHA] },
            })
        },
        group: Some(GROUP_ALPHA),
    },
    GroupFilterRow {
        case: "a rule naming the whole name of the one-instance group, beside the source",
        pattern: || {
            json!({
                "source": [EVENT_SOURCE],
                "detail": { "AutoScalingGroupName": [GROUP_BETA] },
            })
        },
        group: Some(GROUP_BETA),
    },
    GroupFilterRow {
        case: "a rule naming the group and nothing else",
        pattern: || json!({ "detail": { "AutoScalingGroupName": [GROUP_ALPHA] } }),
        group: Some(GROUP_ALPHA),
    },
    GroupFilterRow {
        case: "a rule naming a prefix only the two-instance group carries",
        pattern: || {
            json!({
                "source": [EVENT_SOURCE],
                "detail": { "AutoScalingGroupName": [{ "prefix": "lifecycle-al" }] },
            })
        },
        group: Some(GROUP_ALPHA),
    },
    GroupFilterRow {
        case: "a rule naming a group the account does not hold",
        pattern: || {
            json!({
                "source": [EVENT_SOURCE],
                "detail": { "AutoScalingGroupName": [GROUP_ABSENT] },
            })
        },
        group: None,
    },
];

/// How many launches and how many terminations the group `name` owes over the whole run.
///
/// A count this table could not state is a mistake in the table, so it panics rather than
/// standing in a zero: an expectation of no event at all would be met by a bus that
/// delivered nothing.
fn owed_steps(name: &str) -> (usize, usize) {
    let (created_with, scaled_to) = match name {
        GROUP_ALPHA => (ALPHA_CREATED_WITH, ALPHA_SCALED_TO),
        GROUP_BETA => (BETA_CREATED_WITH, BETA_SCALED_TO),
        other => panic!("{other} is not one of this table's groups"),
    };
    (
        usize::try_from(created_with).expect("a group is created with a non-negative capacity"),
        usize::try_from(created_with - scaled_to).expect("the row scales the group in, not out"),
    )
}

#[tokio::test]
async fn a_rule_filtered_on_the_group_name_receives_that_groups_steps_only() {
    let case = "rules keyed on detail.AutoScalingGroupName";
    let (router, engine) = emulator();
    let subnet = given_launch_source(&router, &engine).await;

    // One queue per row, each behind a rule whose pattern names its own group, and all of
    // them in place BEFORE any capacity moves: a client that watches a group through
    // EventBridge arms its rule first and then makes no further Auto Scaling call.
    let mut queues: Vec<(String, String)> = Vec::new();
    for (at, row) in GROUP_FILTER_ROWS.iter().enumerate() {
        let (url, queue_arn) = given_queue(&router, row.case, &format!("group-filter-{at}")).await;
        given_rule(
            &router,
            row.case,
            &format!("group-filter-rule-{at}"),
            &(row.pattern)(),
            &queue_arn,
        )
        .await;
        grant_delivery(&router, row.case, &url, &queue_arn).await;
        queues.push((url, queue_arn));
    }

    let alpha_arn = given_group(&router, case, GROUP_ALPHA, &subnet, ALPHA_CREATED_WITH).await;
    let beta_arn = given_group(&router, case, GROUP_BETA, &subnet, BETA_CREATED_WITH).await;
    set_desired_capacity(&router, case, GROUP_ALPHA, ALPHA_SCALED_TO).await;
    set_desired_capacity(&router, case, GROUP_BETA, BETA_SCALED_TO).await;

    for (at, row) in GROUP_FILTER_ROWS.iter().enumerate() {
        let (url, _) = &queues[at];
        let Some(group) = row.group else {
            // The negative half: a pattern naming a group the account does not hold
            // receives nothing, which is only distinguishable from a broken bus because
            // every other row's queue did receive.
            tokio::time::sleep(quiet_budget()).await;
            let late = drain(&router, row.case, url).await;
            assert!(
                late.is_empty(),
                "case {:?}: a rule naming {GROUP_ABSENT} must receive nothing, got {late:?}",
                row.case
            );
            continue;
        };
        let arn = match group {
            GROUP_ALPHA => &alpha_arn,
            GROUP_BETA => &beta_arn,
            other => panic!("{other} is not one of this table's groups"),
        };
        let (launches_owed, terminations_owed) = owed_steps(group);
        let mut buffer: Vec<Value> = Vec::new();
        let launches = poll_for_detail_type(
            &router,
            row.case,
            url,
            LAUNCH_SUCCESSFUL,
            launches_owed,
            &mut buffer,
        )
        .await;
        let terminations = poll_for_detail_type(
            &router,
            row.case,
            url,
            TERMINATE_SUCCESSFUL,
            terminations_owed,
            &mut buffer,
        )
        .await;
        assert_eq!(
            (launches.len(), terminations.len()),
            (launches_owed, terminations_owed),
            "case {:?}: expected {launches_owed} launch and {terminations_owed} terminate \
             event(s) of {group}, got {buffer:?}",
            row.case
        );
        for event in &launches {
            assert_envelope(row.case, event, LAUNCH_SUCCESSFUL, group, arn);
        }
        for event in &terminations {
            assert_envelope(row.case, event, TERMINATE_SUCCESSFUL, group, arn);
        }
        // Nothing of the OTHER group reached this rule: the filter is the group's own name.
        for event in &buffer {
            assert_eq!(
                event["detail"]["AutoScalingGroupName"],
                json!(group),
                "case {:?}: only the steps of {group} may reach this rule, got {event}",
                row.case
            );
        }
    }
}
