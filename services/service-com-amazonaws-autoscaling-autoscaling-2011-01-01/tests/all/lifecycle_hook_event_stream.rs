// SPDX-License-Identifier: CC0-1.0

//! What a client watching a group through EventBridge sees while a lifecycle hook HOLDS one
//! of its instances, and what it sees when that hold is over.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded case:
//!
//! 1. The hold a lifecycle hook takes is ANNOUNCED on the account's default bus under the
//!    `detail-type` of that transition, and its `detail` names the two ENDS of the move the
//!    instance is mid-way through — `Origin`, where it came from, and `Destination`, where it
//!    is going — exactly as the event reporting a COMPLETED step does: a launch comes from
//!    Amazon EC2 into the group (`EC2` -> `AutoScalingGroup`) and a termination hands it back
//!    (`AutoScalingGroup` -> `EC2`), so the two are each other's reverse and neither is read
//!    off anything in the request. Beside them the detail carries the group, the instance,
//!    the hook, the transition and the `LifecycleActionToken` the hold is completed with —
//!    the token being the only thing a client can complete the action with.
//! 2. A step the group set out to take is announced EITHER WAY. One it carried through is
//!    published under that step's successful `detail-type`, with the successful status code
//!    and no status message; one it gave up on — a launch a hook abandoned, a termination a
//!    hook abandoned under a group whose retention trigger keeps the instance — is published
//!    under that step's UNSUCCESSFUL `detail-type`, with the terminal status code such a step
//!    ends in and a status message naming the lifecycle action that was abandoned and why.
//!    The rows span both transitions, both results a callback may carry, and both ways a hold
//!    comes to be abandoned (the callback, and the hook's own deadline).
//!
//! EventBridge and Amazon SQS are mounted BESIDE Auto Scaling and Amazon EC2 on the SAME
//! engine, so a publication travels the real in-process callback, through the real bus, to a
//! real SQS target: nothing here reads the publisher's own queue.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;

use crate::support::{borrowed, create_launch_template, p, query, seed_subnets};

/// The `source` every event of this service carries, and the detail types this suite
/// asserts — all written out here rather than imported, so a wrong constant in the
/// implementation cannot make a row pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_LIFECYCLE_ACTION: &str = "EC2 Instance-launch Lifecycle Action";
const TERMINATE_LIFECYCLE_ACTION: &str = "EC2 Instance-terminate Lifecycle Action";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const LAUNCH_UNSUCCESSFUL: &str = "EC2 Instance Launch Unsuccessful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";
const TERMINATE_UNSUCCESSFUL: &str = "EC2 Instance Terminate Unsuccessful";

/// The two transitions a hook can be created for, from the model's own documentation for
/// `PutLifecycleHookType$LifecycleTransition`.
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The results a hook may default to and a callback may carry ("Valid values: CONTINUE |
/// ABANDON").
const CONTINUE: &str = "CONTINUE";
const ABANDON: &str = "ABANDON";

/// The two ends of the move an instance of a group makes.
const ENDPOINT_EC2: &str = "EC2";
const ENDPOINT_GROUP: &str = "AutoScalingGroup";

/// The `ScalingActivityStatusCode` values a settled step is published under, spelled as the
/// model's own `com.amazonaws.autoscaling#ScalingActivityStatusCode` enum does.
const STATUS_SUCCESSFUL: &str = "Successful";
const STATUS_CANCELLED: &str = "Cancelled";

/// How long a `LifecycleActionToken` is: `@length(min: 36, max: 36)` on the model's own
/// `LifecycleActionToken` shape.
const TOKEN_LENGTH: usize = 36;

/// The smallest `HeartbeatTimeout` the service accepts, from the model's own documentation
/// for `PutLifecycleHookType$HeartbeatTimeout` ("The range is from 30 to 7200 seconds") —
/// the shortest deadline a row can let run out.
const MIN_HEARTBEAT_TIMEOUT: i32 = 30;

/// A heartbeat timeout far above what a row driven by its callback needs, so no row that
/// completes its hold itself can have the deadline resolve the hold underneath it.
const UNREACHED_HEARTBEAT_TIMEOUT: i32 = 3600;

/// The launch source every group here launches from, created over Amazon EC2's own wire
/// because an instance is only launched from a template that can produce one.
const TEMPLATE: &str = "lt-hook-event-stream";

/// The rule that delivers every event of this service to the queue.
const RULE: &str = "autoscaling-hook-events";

/// How often a poll re-reads the target queue.
const POLL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up, for a leg whose hold is resolved by a
/// CALLBACK: the lifecycle span the service owes a transition with ample margin for a loaded
/// test machine. A poll waits for the CONDITION and returns as soon as it holds, so the
/// margin costs nothing when the publication is prompt.
fn callback_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(90)
}

/// The same, for the leg that waits for the hook's own DEADLINE to run out: the shortest
/// deadline the service accepts, and then the same margin.
fn deadline_budget() -> Duration {
    callback_budget() + Duration::from_secs(u64::from(MIN_HEARTBEAT_TIMEOUT.unsigned_abs()))
}

/// One emulator: Auto Scaling, Amazon EC2, EventBridge and Amazon SQS over ONE shared
/// engine — the four services a published event travels through.
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

    let (status, body) = sqs(&router, "CreateQueue", json!({ "QueueName": "hook-events" })).await;
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

    // ONE rule for every event of this service: which step an envelope reports is read off
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
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {body}"
    );
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

/// Create one group with `desired` capacity, carrying `extra` request members.
async fn given_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    subnet: &str,
    desired: i32,
    extra: &[(String, String)],
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", desired.to_string()),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("VPCZoneIdentifier", subnet),
    ];
    params.extend_from_slice(extra);
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Store one lifecycle hook of `group` for `transition`, defaulting to `default_result` after
/// `heartbeat` seconds.
async fn given_hook(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    transition: &str,
    default_result: &str,
    heartbeat: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleTransition", transition),
        p("DefaultResult", default_result),
        p("HeartbeatTimeout", heartbeat.to_string()),
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

/// Complete the held action `token` of `group`'s hook with `result`.
async fn complete(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    token: &str,
    result: &str,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleActionToken", token),
        p("LifecycleActionResult", result),
    ];
    let (status, body) = query(router, "CompleteLifecycleAction", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CompleteLifecycleAction expected 200, got {status}: {body}"
    );
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

/// Poll the queue — and ONLY the queue — until one envelope of `detail_type` has been
/// delivered, or `budget` runs out; answer that envelope.
///
/// Nothing in this loop calls Auto Scaling, which is the point: a client that watches a group
/// through EventBridge makes no further call to the service that publishes. Everything
/// drained goes into `buffer`, so an envelope that arrives while another is being waited for
/// is kept rather than thrown away.
async fn await_event(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    budget: Duration,
    buffer: &mut Vec<Value>,
) -> Value {
    let deadline = Instant::now() + budget;
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

/// Assert no envelope of `detail_type` is among everything delivered so far.
fn assert_absent(case: &str, buffer: &[Value], detail_type: &str) {
    assert!(
        !buffer
            .iter()
            .any(|event| event["detail-type"] == json!(detail_type)),
        "case {case:?}: no {detail_type} envelope may be published, got {buffer:?}"
    );
}

/// The text of `member` in an envelope's `detail`.
fn detail_text(case: &str, event: &Value, member: &str) -> String {
    event["detail"][member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no detail.{member} in {event}"))
        .to_string()
}

/// The `StatusMessage` the step an abandoned lifecycle action was holding reports: the action
/// the instance did not complete, and why it was abandoned.
///
/// Written out here rather than taken from the crate, so a wrong sentence in the
/// implementation cannot make a row pass. The two reasons are the two ways a hold ends: the
/// callback carried `ABANDON` back, or the hook's own deadline ran out — "if the lifecycle
/// hook times out, Amazon EC2 Auto Scaling performs the action that you specified in the
/// DefaultResult property" (`LifecycleHook$HeartbeatTimeout`).
fn abandoned_message(token: &str, reason: &str) -> String {
    format!(
        "Instance failed to complete user's Lifecycle Action: Lifecycle Action with token \
         {token} was abandoned: {reason}"
    )
}
const ABANDONED_BY_CALLBACK: &str = "Lifecycle Action Completed with ABANDON Result";
const ABANDONED_BY_DEADLINE: &str = "Heartbeat Timeout";

/// Assert the lifecycle-action envelope `event` announces the hold of `hook` on `instance` at
/// `transition`, naming the two ends of the move it stands inside, and answer its token.
fn assert_held_action(
    case: &str,
    event: &Value,
    group: &str,
    hook: &str,
    transition: &str,
    origin: &str,
    destination: &str,
) -> String {
    assert_eq!(
        event["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: the hold must be published by this service, got {event}"
    );
    assert_eq!(
        detail_text(case, event, "AutoScalingGroupName"),
        group,
        "case {case:?}: the hold must name the group holding the instance, got {event}"
    );
    assert_eq!(
        detail_text(case, event, "LifecycleHookName"),
        hook,
        "case {case:?}: the hold must name the hook holding the instance, got {event}"
    );
    assert_eq!(
        detail_text(case, event, "LifecycleTransition"),
        transition,
        "case {case:?}: the hold must name the transition it is at, got {event}"
    );
    assert_eq!(
        (
            detail_text(case, event, "Origin"),
            detail_text(case, event, "Destination"),
        ),
        (origin.to_string(), destination.to_string()),
        "case {case:?}: a hold at {transition} must flow {origin} -> {destination}, got {event}"
    );
    let token = detail_text(case, event, "LifecycleActionToken");
    assert_eq!(
        token.len(),
        TOKEN_LENGTH,
        "case {case:?}: the published token must be a {TOKEN_LENGTH}-character identifier, \
         got {token:?}"
    );
    token
}

/// One row of the DIRECTION table: the transition a hook holds, and the move that hold stands
/// inside.
struct DirectionRow {
    case: &'static str,
    transition: &'static str,
    detail_type: &'static str,
    origin: &'static str,
    destination: &'static str,
}

/// The rows span the two transitions a hook can be created for, which is what the rule
/// generalises over: the move a hold stands inside is read off the transition alone, and the
/// two are each other's reverse.
const DIRECTION_ROWS: &[DirectionRow] = &[
    DirectionRow {
        case: "a launch hook holds an instance coming into the group",
        transition: LAUNCHING,
        detail_type: LAUNCH_LIFECYCLE_ACTION,
        origin: ENDPOINT_EC2,
        destination: ENDPOINT_GROUP,
    },
    DirectionRow {
        case: "a termination hook holds one going back to Amazon EC2",
        transition: TERMINATING,
        detail_type: TERMINATE_LIFECYCLE_ACTION,
        origin: ENDPOINT_GROUP,
        destination: ENDPOINT_EC2,
    },
];

/// RULE 1: the hold a hook takes is announced, and its detail names where the instance it is
/// holding came from and where it is going.
#[tokio::test]
async fn a_held_lifecycle_action_names_the_move_it_stands_inside() {
    for (index, row) in DIRECTION_ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's envelopes cannot be read for another.
        let (router, url, subnet) = fixture(case).await;
        let group = format!("hold-direction-{index}");
        let hook = "gate";
        let mut buffer: Vec<Value> = Vec::new();

        // A launch hook is put in front of a launch the group has still to make; a
        // termination hook in front of a scale-in of an instance that is already in service,
        // which is the only point either transition can be held at.
        if row.transition == LAUNCHING {
            given_group(&router, case, &group, &subnet, 0, &[]).await;
            given_hook(
                &router,
                case,
                &group,
                hook,
                row.transition,
                CONTINUE,
                UNREACHED_HEARTBEAT_TIMEOUT,
            )
            .await;
            set_desired_capacity(&router, case, &group, 1).await;
        } else {
            given_group(&router, case, &group, &subnet, 1, &[]).await;
            await_event(
                &router,
                case,
                &url,
                LAUNCH_SUCCESSFUL,
                callback_budget(),
                &mut buffer,
            )
            .await;
            given_hook(
                &router,
                case,
                &group,
                hook,
                row.transition,
                CONTINUE,
                UNREACHED_HEARTBEAT_TIMEOUT,
            )
            .await;
            set_desired_capacity(&router, case, &group, 0).await;
        }

        let held = await_event(
            &router,
            case,
            &url,
            row.detail_type,
            callback_budget(),
            &mut buffer,
        )
        .await;
        let token = assert_held_action(
            case,
            &held,
            &group,
            hook,
            row.transition,
            row.origin,
            row.destination,
        );
        let instance = detail_text(case, &held, "EC2InstanceId");
        assert!(
            instance.starts_with("i-"),
            "case {case:?}: the hold must name the instance it is holding, got {held}"
        );

        // The token the hold was announced with is the one that completes it: the event
        // carries the whole of what a client needs to release the instance.
        complete(&router, case, &group, hook, &token, CONTINUE).await;
    }
}

/// One row of the OUTCOME table: the hook that holds the step, how its hold ends, and the
/// envelope the settled step is published as.
struct OutcomeRow {
    case: &'static str,
    /// The transition the hook holds, which decides which step is settled.
    transition: &'static str,
    /// The result the callback carries, or `None` to let the hook's own deadline resolve it
    /// with its `DefaultResult`.
    result: Option<&'static str>,
    /// Whether the group's retention trigger keeps an instance whose termination was
    /// abandoned (`RetentionTriggers$TerminateHookAbandon`).
    retains: bool,
    /// The `detail-type` the settled step must be published under, and the one it must NOT be.
    published: &'static str,
    withheld: &'static str,
    /// The status code that step ends in, and the reason it states — empty for a step that
    /// carried through.
    status_code: &'static str,
    reason: &'static str,
}

/// The rows span what the rule generalises over: which transition the hold was at, which
/// result ended it, HOW it was abandoned (the callback or the hook's own deadline), and
/// whether the group's own retention trigger left the step undone.
const OUTCOME_ROWS: &[OutcomeRow] = &[
    OutcomeRow {
        case: "a launch the hook let carry on",
        transition: LAUNCHING,
        result: Some(CONTINUE),
        retains: false,
        published: LAUNCH_SUCCESSFUL,
        withheld: LAUNCH_UNSUCCESSFUL,
        status_code: STATUS_SUCCESSFUL,
        reason: "",
    },
    OutcomeRow {
        case: "a launch the callback abandoned",
        transition: LAUNCHING,
        result: Some(ABANDON),
        retains: false,
        published: LAUNCH_UNSUCCESSFUL,
        withheld: LAUNCH_SUCCESSFUL,
        status_code: STATUS_CANCELLED,
        reason: ABANDONED_BY_CALLBACK,
    },
    OutcomeRow {
        case: "a termination the hook let carry on",
        transition: TERMINATING,
        result: Some(CONTINUE),
        retains: false,
        published: TERMINATE_SUCCESSFUL,
        withheld: TERMINATE_UNSUCCESSFUL,
        status_code: STATUS_SUCCESSFUL,
        reason: "",
    },
    OutcomeRow {
        case: "a termination the callback abandoned under a group that retains the instance",
        transition: TERMINATING,
        result: Some(ABANDON),
        retains: true,
        published: TERMINATE_UNSUCCESSFUL,
        withheld: TERMINATE_SUCCESSFUL,
        status_code: STATUS_CANCELLED,
        reason: ABANDONED_BY_CALLBACK,
    },
];

/// The row whose hold nobody completes: the hook's own deadline runs out and its
/// `DefaultResult` is what resolves the action.
const DEADLINE_ROW: OutcomeRow = OutcomeRow {
    case: "a launch nobody completed, abandoned by the hook's own deadline",
    transition: LAUNCHING,
    result: None,
    retains: false,
    published: LAUNCH_UNSUCCESSFUL,
    withheld: LAUNCH_SUCCESSFUL,
    status_code: STATUS_CANCELLED,
    reason: ABANDONED_BY_DEADLINE,
};

/// RULE 2: a step the group set out to take is announced either way — under its successful
/// detail-type when it carried through, and under its unsuccessful one when the group gave
/// up on it.
#[tokio::test]
async fn a_settled_held_step_is_announced_under_its_own_outcome() {
    for (index, row) in OUTCOME_ROWS.iter().enumerate() {
        drive_outcome_row(row, &format!("outcome-{index}")).await;
    }
}

/// The same rule, for the hold that is abandoned by the hook's own deadline rather than by a
/// callback: its own test, because this is the one leg that waits for a deadline to run out.
#[tokio::test]
async fn a_launch_nobody_completes_is_abandoned_by_the_hooks_deadline() {
    drive_outcome_row(&DEADLINE_ROW, "deadline").await;
}

/// Drive one outcome row end to end: hold the step, end the hold the way the row says, and
/// assert the envelope the settled step is published as.
async fn drive_outcome_row(row: &OutcomeRow, group: &str) {
    let case = row.case;
    let hook = "gate";
    let (router, url, subnet) = fixture(case).await;
    let mut buffer: Vec<Value> = Vec::new();
    // The hold's own deadline must only resolve the action on the row that waits for it.
    let heartbeat = if row.result.is_some() {
        UNREACHED_HEARTBEAT_TIMEOUT
    } else {
        MIN_HEARTBEAT_TIMEOUT
    };
    // A hook whose DefaultResult is what the deadline applies, and which is never reached on
    // a row that completes the hold itself.
    let default_result = row.result.unwrap_or(ABANDON);
    let retention = if row.retains {
        vec![p(
            "InstanceLifecyclePolicy.RetentionTriggers.TerminateHookAbandon",
            "retain",
        )]
    } else {
        Vec::new()
    };

    let (held_type, instance) = if row.transition == LAUNCHING {
        given_group(&router, case, group, &subnet, 0, &retention).await;
        given_hook(
            &router,
            case,
            group,
            hook,
            row.transition,
            default_result,
            heartbeat,
        )
        .await;
        set_desired_capacity(&router, case, group, 1).await;
        (LAUNCH_LIFECYCLE_ACTION, None)
    } else {
        given_group(&router, case, group, &subnet, 1, &retention).await;
        let launched = await_event(
            &router,
            case,
            &url,
            LAUNCH_SUCCESSFUL,
            callback_budget(),
            &mut buffer,
        )
        .await;
        let instance = detail_text(case, &launched, "EC2InstanceId");
        given_hook(
            &router,
            case,
            group,
            hook,
            row.transition,
            default_result,
            heartbeat,
        )
        .await;
        set_desired_capacity(&router, case, group, 0).await;
        (TERMINATE_LIFECYCLE_ACTION, Some(instance))
    };

    let held = await_event(
        &router,
        case,
        &url,
        held_type,
        callback_budget(),
        &mut buffer,
    )
    .await;
    let token = detail_text(case, &held, "LifecycleActionToken");
    let instance = instance.unwrap_or_else(|| detail_text(case, &held, "EC2InstanceId"));

    if let Some(result) = row.result {
        complete(&router, case, group, hook, &token, result).await;
    }

    let settled = await_event(
        &router,
        case,
        &url,
        row.published,
        if row.result.is_some() {
            callback_budget()
        } else {
            deadline_budget()
        },
        &mut buffer,
    )
    .await;
    assert_eq!(
        detail_text(case, &settled, "EC2InstanceId"),
        instance,
        "case {case:?}: the settled step must report the instance that was held, got {settled}"
    );
    assert_eq!(
        detail_text(case, &settled, "StatusCode"),
        row.status_code,
        "case {case:?}: the settled step must end in {:?}, got {settled}",
        row.status_code
    );
    let expected_message = if row.reason.is_empty() {
        String::new()
    } else {
        abandoned_message(&token, row.reason)
    };
    assert_eq!(
        detail_text(case, &settled, "StatusMessage"),
        expected_message,
        "case {case:?}: the settled step must state {expected_message:?}, got {settled}"
    );
    // And the OTHER outcome of the same step is not published at all: the two readings of one
    // step are exclusive.
    assert_absent(case, &buffer, row.withheld);
}
