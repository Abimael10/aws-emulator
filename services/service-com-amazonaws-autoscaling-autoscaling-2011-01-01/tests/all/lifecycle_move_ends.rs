// SPDX-License-Identifier: CC0-1.0

//! WHICH two places a lifecycle event says an instance is moving between.
//!
//! The rule under test: the `detail` of every event this service publishes about one
//! instance — the event announcing a hook's HOLD ('EC2 Instance-launch Lifecycle Action',
//! 'EC2 Instance-terminate Lifecycle Action') and the event reporting the settled step ('EC2
//! Instance Launch Successful', 'EC2 Instance Terminate Successful') — carries the two ENDS
//! of the move that instance is on under the fixed member names `Origin` and `Destination`,
//! and those two are read from the group's OWN state for that instance rather than from the
//! name of the transition: a group that takes an instance into service from cold is bringing
//! it in from Amazon EC2 (`EC2` -> `AutoScalingGroup`), a group that draws on its own warm
//! pool is bringing the same instance in from that pool (`WarmPool` ->
//! `AutoScalingGroup`), and a group that takes a member out of service hands it back to
//! Amazon EC2 (`AutoScalingGroup` -> `EC2`). A consumer branches on those members — an
//! EventBridge rule whose pattern filters `detail.Origin` is the documented way to treat a
//! warm-pool launch differently from a plain scale-out — so they have to name the move that
//! was actually made.
//!
//! The rows span what the rule generalises over: which transition the group is performing,
//! WHERE the instance comes from (Amazon EC2, and the group's warm pool), and whether the
//! group holds a warm pool at all. Each row asserts the pair on BOTH events of the same
//! move, and the warm-pool row asserts the event names the very instance the pool had
//! prepared, so it cannot pass on a fresh launch that merely reached the right count.
//!
//! EventBridge and Amazon SQS are mounted BESIDE Auto Scaling and Amazon EC2 on the SAME
//! engine, so a publication travels the real in-process callback, through the real bus, to a
//! real SQS target.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;

use crate::support::{borrowed, create_launch_template, p, query, seed_subnets, tag_values};

/// The `source` every event of this service carries, and the detail types this suite reads —
/// written out here rather than imported, so a wrong constant in the implementation cannot
/// make a row pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_LIFECYCLE_ACTION: &str = "EC2 Instance-launch Lifecycle Action";
const TERMINATE_LIFECYCLE_ACTION: &str = "EC2 Instance-terminate Lifecycle Action";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";

/// The two transitions a hook can be created for, from the model's own documentation for
/// `PutLifecycleHookType$LifecycleTransition`.
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The result a callback carries to let the held step carry on ("Valid values: CONTINUE |
/// ABANDON").
const CONTINUE: &str = "CONTINUE";

/// The three places an instance of a group can be, as an event's `Origin` and `Destination`
/// name them: Amazon EC2 itself, in service in the group, and prepared in the group's warm
/// pool.
const ENDPOINT_EC2: &str = "EC2";
const ENDPOINT_GROUP: &str = "AutoScalingGroup";
const ENDPOINT_WARM_POOL: &str = "WarmPool";

/// A `HeartbeatTimeout` far above what a row driven by its callback needs, so no hold can be
/// resolved by its own deadline underneath a row.
const UNREACHED_HEARTBEAT_TIMEOUT: i32 = 3600;

/// The `PoolState` each row's warm pool is put in, from the model's own `WarmPoolState` enum
/// (`smithy_query(query="shape",shape="WarmPoolState")`): the state the pool keeps a prepared
/// instance in, which the pool reports it as under the `Warmed:` prefix of the model's own
/// `LifecycleState` enum.
const POOL_STATE: &str = "Running";

/// The launch source every group here launches from, created over Amazon EC2's own wire
/// because an instance is only launched from a template that can produce one.
const TEMPLATE: &str = "lt-move-ends";

/// The rule that delivers every event of this service to the queue.
const RULE: &str = "autoscaling-move-ends";

/// How often a poll looks again.
const POLL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle spans this service owes a
/// transition, with ample margin for a loaded test machine. A poll waits for the CONDITION
/// and returns as soon as it holds, so the margin costs nothing when the service is prompt.
fn budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(90)
}

/// One emulator: Auto Scaling, Amazon EC2, EventBridge and Amazon SQS over ONE shared engine
/// — the four services a published event travels through.
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

/// One EventBridge call.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    crate::support::json_rpc(router, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    crate::support::json_rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// An emulator whose default bus delivers every Auto Scaling event to one SQS queue, with a
/// launch template to launch from and a subnet to place into; answers the queue's url and
/// that subnet.
async fn fixture(case: &str) -> (MultiServiceRouter, String, String) {
    let (router, engine) = emulator();
    create_launch_template(&router, TEMPLATE).await;
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .id
        .clone();

    let (status, body) = sqs(&router, "CreateQueue", json!({ "QueueName": "move-ends" })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {body}"
    );
    let url = body["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {body}"))
        .to_string();
    let (status, body) = sqs(
        &router,
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

    // EventBridge delivers as its own service principal, so the queue has to grant it the
    // `sqs:SendMessage` a target delivery makes.
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
        &router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {body}"
    );

    // ONE rule for every event of this service: which move an envelope reports is read off
    // the envelope, not off which rule delivered it.
    let (status, body) = events(
        &router,
        "PutRule",
        json!({
            "Name": RULE,
            "EventPattern": json!({ "source": [EVENT_SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: PutRule answered {status} {body}");
    let (status, body) = events(
        &router,
        "PutTargets",
        json!({ "Rule": RULE, "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {body}"
    );
    (router, url, subnet)
}

/// Create one group with `desired` capacity and a maximum of two.
async fn given_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    subnet: &str,
    desired: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", desired.to_string()),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("VPCZoneIdentifier", subnet),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Give `group` a warm pool keeping `min_size` instances prepared.
async fn given_warm_pool(router: &MultiServiceRouter, case: &str, group: &str, min_size: i32) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("MinSize", min_size.to_string()),
        p("PoolState", POOL_STATE),
    ];
    let (status, body) = query(router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutWarmPool expected 200, got {status}: {body}"
    );
}

/// Store one lifecycle hook of `group` for `transition`.
async fn given_hook(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    transition: &str,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleTransition", transition),
        p("DefaultResult", CONTINUE),
        p("HeartbeatTimeout", UNREACHED_HEARTBEAT_TIMEOUT.to_string()),
    ];
    let (status, body) = query(router, "PutLifecycleHook", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutLifecycleHook expected 200, got {status}: {body}"
    );
}

/// Ask `group` for `desired` capacity.
async fn set_desired_capacity(router: &MultiServiceRouter, case: &str, group: &str, desired: i32) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("DesiredCapacity", desired.to_string()),
    ];
    let (status, body) = query(router, "SetDesiredCapacity", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity expected 200, got {status}: {body}"
    );
}

/// Complete the hold `token` of `group`'s hook, letting the step it holds carry on.
async fn complete(router: &MultiServiceRouter, case: &str, group: &str, hook: &str, token: &str) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleActionToken", token),
        p("LifecycleActionResult", CONTINUE),
    ];
    let (status, body) = query(router, "CompleteLifecycleAction", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CompleteLifecycleAction expected 200, got {status}: {body}"
    );
}

/// Read `group`'s warm pool until it holds one instance the pool reports prepared, or a
/// generous deadline passes; answer that instance's id.
///
/// Waits for the CONDITION — the pool having prepared the instance a later scale-out can draw
/// on — rather than for any span of time.
async fn prepared_instance(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let prepared = format!("Warmed:{POOL_STATE}");
    let deadline = Instant::now() + budget();
    loop {
        let (status, body) =
            query(router, "DescribeWarmPool", &[("AutoScalingGroupName", group)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeWarmPool expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        if let (Some(id), true) = (ids.first(), states.contains(&prepared)) {
            return id.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the warm pool to prepare one instance in {prepared}, got \
             {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every envelope waiting on the queue right now.
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

/// Poll the queue — and ONLY the queue — until an envelope of `detail_type` has been
/// delivered, or a generous deadline passes; answer that envelope.
///
/// Everything drained goes into `buffer`, so an envelope that arrives while another is being
/// waited for is kept rather than thrown away.
async fn await_event(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    buffer: &mut Vec<Value>,
) -> Value {
    let deadline = Instant::now() + budget();
    loop {
        buffer.extend(drain(router, case, url).await);
        if let Some(found) = buffer
            .iter()
            .find(|event| event["detail-type"] == json!(detail_type))
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected one {detail_type} envelope, got {buffer:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The text of `member` in an envelope's `detail`.
fn detail_text(case: &str, event: &Value, member: &str) -> String {
    event["detail"][member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no detail.{member} in {event}"))
        .to_string()
}

/// Assert `event` names the move `origin` -> `destination` of `instance`.
fn assert_move(
    case: &str,
    event: &Value,
    expected: (&str, &str),
    instance: Option<&str>,
) -> String {
    assert_eq!(
        event["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: the envelope must be published by this service, got {event}"
    );
    let actual = (
        detail_text(case, event, "Origin"),
        detail_text(case, event, "Destination"),
    );
    assert_eq!(
        actual,
        (expected.0.to_string(), expected.1.to_string()),
        "case {case:?}: expected the move {} -> {}, got {actual:?} in {event}",
        expected.0,
        expected.1
    );
    let moved = detail_text(case, event, "EC2InstanceId");
    if let Some(instance) = instance {
        assert_eq!(
            moved, instance,
            "case {case:?}: expected the envelope to be about {instance}, got {moved} in {event}"
        );
    }
    assert!(
        moved.starts_with("i-"),
        "case {case:?}: the envelope must name the instance that moved, got {event}"
    );
    moved
}

/// One row of the table: what the group holds before the move, which transition it then
/// performs, and the two ends that move has.
struct Row {
    case: &'static str,
    /// The capacity the group is created with, and the capacity the row then asks for.
    created_with: i32,
    moves_to: i32,
    /// Whether the group holds a warm pool keeping one instance prepared, and whether the
    /// row's move is expected to draw on it.
    warm_pool: bool,
    from_the_pool: bool,
    /// The transition the row's hook holds, and the two detail types the move is published
    /// under: the hold, and the settled step.
    transition: &'static str,
    held_detail_type: &'static str,
    settled_detail_type: &'static str,
    /// The two ends the move has.
    origin: &'static str,
    destination: &'static str,
}

/// The rows span where the instance comes from and where it goes: a cold launch, a launch the
/// group's own warm pool covers, a scale-in of a group with no pool, and a scale-in of a group
/// that HAS one (whose pool is not part of that move, since the pool is not configured to take
/// instances back).
const ROWS: &[Row] = &[
    Row {
        case: "a cold launch comes into the group from Amazon EC2",
        created_with: 0,
        moves_to: 1,
        warm_pool: false,
        from_the_pool: false,
        transition: LAUNCHING,
        held_detail_type: LAUNCH_LIFECYCLE_ACTION,
        settled_detail_type: LAUNCH_SUCCESSFUL,
        origin: ENDPOINT_EC2,
        destination: ENDPOINT_GROUP,
    },
    Row {
        case: "a launch the group's warm pool covers comes into the group from that pool",
        created_with: 0,
        moves_to: 1,
        warm_pool: true,
        from_the_pool: true,
        transition: LAUNCHING,
        held_detail_type: LAUNCH_LIFECYCLE_ACTION,
        settled_detail_type: LAUNCH_SUCCESSFUL,
        origin: ENDPOINT_WARM_POOL,
        destination: ENDPOINT_GROUP,
    },
    Row {
        case: "a scale-in hands a member back to Amazon EC2",
        created_with: 1,
        moves_to: 0,
        warm_pool: false,
        from_the_pool: false,
        transition: TERMINATING,
        held_detail_type: TERMINATE_LIFECYCLE_ACTION,
        settled_detail_type: TERMINATE_SUCCESSFUL,
        origin: ENDPOINT_GROUP,
        destination: ENDPOINT_EC2,
    },
    Row {
        case: "a scale-in of a group that holds a warm pool still hands the member back",
        created_with: 1,
        moves_to: 0,
        warm_pool: true,
        from_the_pool: false,
        transition: TERMINATING,
        held_detail_type: TERMINATE_LIFECYCLE_ACTION,
        settled_detail_type: TERMINATE_SUCCESSFUL,
        origin: ENDPOINT_GROUP,
        destination: ENDPOINT_EC2,
    },
];

/// Every row: both events of one move name the two places that move is actually between.
#[tokio::test]
async fn a_lifecycle_event_names_the_two_ends_of_the_move_the_instance_is_on() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's envelopes cannot be read for another.
        let (router, url, subnet) = fixture(case).await;
        let group = format!("move-ends-{index}");
        let hook = "gate";
        let mut buffer: Vec<Value> = Vec::new();

        given_group(&router, case, &group, &subnet, row.created_with).await;
        // A group created with a member waits for that member's own launch to be announced
        // first, so the envelopes the row reads are the ones its own move produced.
        if row.created_with > 0 {
            await_event(&router, case, &url, LAUNCH_SUCCESSFUL, &mut buffer).await;
            buffer.clear();
        }
        // The pool is given its instance to prepare BEFORE the move, because a pool covers a
        // scale-out only with what it has already prepared.
        let pooled = if row.warm_pool {
            given_warm_pool(&router, case, &group, 1).await;
            Some(prepared_instance(&router, case, &group).await)
        } else {
            None
        };

        given_hook(&router, case, &group, hook, row.transition).await;
        set_desired_capacity(&router, case, &group, row.moves_to).await;

        // The hold the hook took, announced while the instance is MID-WAY through the move.
        let held = await_event(&router, case, &url, row.held_detail_type, &mut buffer).await;
        let expected_instance = row.from_the_pool.then(|| pooled.clone()).flatten();
        let instance = assert_move(
            case,
            &held,
            (row.origin, row.destination),
            expected_instance.as_deref(),
        );
        assert_eq!(
            detail_text(case, &held, "LifecycleTransition"),
            row.transition,
            "case {case:?}: the hold must name the transition it is at, got {held}"
        );
        let token = detail_text(case, &held, "LifecycleActionToken");

        // And the SETTLED step, once the hold is released: the same move, so the same two
        // ends and the same instance.
        complete(&router, case, &group, hook, &token).await;
        let settled = await_event(&router, case, &url, row.settled_detail_type, &mut buffer).await;
        assert_move(
            case,
            &settled,
            (row.origin, row.destination),
            Some(&instance),
        );
    }
}
