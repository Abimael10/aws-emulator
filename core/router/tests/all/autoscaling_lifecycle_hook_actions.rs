// SPDX-License-Identifier: CC0-1.0

//! A lifecycle hook HOLDS the instance taking its transition, and the hold reaches the queue
//! an EventBridge rule delivers it to — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **when an Auto Scaling group holds a lifecycle hook for the
//! transition one of its instances is taking, that instance is put into the transition's WAIT
//! state and the held lifecycle action is published once onto the account's DEFAULT
//! EventBridge bus as `source` `aws.autoscaling` / `detail-type` `EC2 Instance-launch
//! Lifecycle Action` or `EC2 Instance-terminate Lifecycle Action`, carrying in the `detail`
//! the group, the instance, the hook, the `LifecycleTransition`, the hook's own
//! `NotificationMetadata` and a 36-character `LifecycleActionToken`; the instance stays in
//! that wait state until the token (or the instance id) is passed back through
//! `CompleteLifecycleAction` — `RecordLifecycleActionHeartbeat` keeps it waiting — or the
//! hook's heartbeat runs out and the hook's own `DefaultResult` is applied; a launch that
//! carries on then comes into service, a launch that is abandoned leaves the group, and a
//! termination resumes either way.**
//!
//! Four services have to agree for that to hold — Auto Scaling publishes and holds, Amazon
//! EC2 holds the instances it names, EventBridge routes, Amazon SQS holds what arrived — and
//! the publication Auto Scaling makes is an in-process call addressed the way a client's own
//! `events`-scoped call is addressed, so WHICH mounted model claims it is only decidable with
//! every service mounted.
//!
//! The rows differ along what the rule generalises over: WHICH transition is held (a launch,
//! a termination), how many hooks of the group hold the same instance, HOW the hold ends (the
//! token, the instance id, a heartbeat and then the token, or the hook's own deadline), WHICH
//! result ends it (`CONTINUE`, `ABANDON`), and whether the hook carries notification metadata.
//! The last test is the negative half of the same rule: a rule pinned to the other
//! transition's detail-type receives nothing, and a token that has already been completed
//! resolves nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::query_wire::{element, elements, form_encode, pairs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The zone every group below places its instances in.
const ZONE: &str = "us-west-2a";

/// The `source`, the two `detail-type`s and the two `LifecycleTransition` values Auto Scaling
/// announces a held lifecycle action under, spelled out here rather than imported from the
/// service crate, so a wrong constant in the implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_ACTION: &str = "EC2 Instance-launch Lifecycle Action";
const TERMINATE_ACTION: &str = "EC2 Instance-terminate Lifecycle Action";
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The two `detail-type`s the STEP itself is announced under once it has happened — what the
/// hold defers, and what an abandoned launch never produces.
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";

/// The two wait states a held instance is reported in, and the state a launch that carried on
/// reaches — the model's own `LifecycleState` values, written out for the same reason.
const PENDING_WAIT: &str = "Pending:Wait";
const TERMINATING_WAIT: &str = "Terminating:Wait";
const IN_SERVICE: &str = "InService";

/// The two results a callback may carry.
const CONTINUE: &str = "CONTINUE";
const ABANDON: &str = "ABANDON";

/// How long a `LifecycleActionToken` is: `@length(min: 36, max: 36)` on the model's own
/// `LifecycleActionToken` shape.
const TOKEN_LENGTH: usize = 36;

/// The bus an Amazon Web Services service's own events go to.
const DEFAULT_BUS: &str = "default";

/// The service principal EventBridge delivers to an SQS target as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The launch template every group below launches from.
const TEMPLATE: &str = "lifecycle-hook-template";

/// How long a leg waits for what it is waiting on: far above the work one capacity change
/// makes, and driven off what has ARRIVED rather than off a fixed sleep, so a loaded machine
/// delays a leg instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(60);

/// One look inside [`DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it for
/// the binary; answers the router and the engine its stores live on.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One form-wire (`awsQuery` / `ec2Query`) call, with whatever status it answered.
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Auto Scaling call that must answer 200.
async fn asg(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = form_call(router, "autoscaling", "2011-01-01", action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from autoscaling {action}, got {status} with {text}"
    );
    text
}

/// One Amazon EC2 call that must answer 200.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = form_call(router, "ec2", "2016-11-15", action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ec2 {action}, got {status} with {text}"
    );
    text
}

/// One `X-Amz-Target` RPC call that must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One EventBridge call.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// A string member of a JSON response, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// An SQS queue called `name`, created through SQS itself and admitting EventBridge's service
/// principal; answers its url and the ARN a rule names it by.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule called `name` on the default bus carrying `pattern`, with the queue
/// `queue_arn` as its one target.
async fn rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
    queue_arn: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{"Id": "t1", "Arn": queue_arn}],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target must be accepted, got {targeted}"
    );
}

/// Every envelope now waiting on the queue at `url`, taken off it.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::new();
    for message in messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        let raw = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
    }
    envelopes
}

/// Look at the queue at `url` until it has delivered `wanted` envelopes carrying
/// `detail_type`, or [`DEADLINE`] passes; answer EVERYTHING that arrived, so a leg can
/// assert what was published as well as what was not.
///
/// Nothing in this loop calls Auto Scaling, which is the point: a client that reacts to a
/// lifecycle hook through EventBridge makes no further call to the service that published.
async fn drained_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, url).await);
        let found = arrived
            .iter()
            .filter(|envelope| envelope["detail-type"] == json!(detail_type))
            .count();
        if found >= wanted || Instant::now() >= deadline {
            return arrived;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The envelopes of [`drained_until`] that carry `detail_type`.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    wanted: usize,
) -> Vec<Value> {
    drained_until(router, case, url, detail_type, wanted)
        .await
        .into_iter()
        .filter(|envelope| envelope["detail-type"] == json!(detail_type))
        .collect()
}

/// The `<Code>` and `<Message>` of a query error envelope, with the serializer's escaping
/// undone — the message states what the caller has to correct, and this service's refusals
/// quote member names in single quotes.
fn refusal_of(body: &str) -> (String, String) {
    (
        elements(body, "Code").into_iter().next().unwrap_or_default(),
        elements(body, "Message")
            .into_iter()
            .next()
            .unwrap_or_default(),
    )
}

/// The `LifecycleState` the group `group` reports for each instance it holds.
async fn reported_states(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> BTreeMap<String, String> {
    let described = asg(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    pairs_of(&described, "InstanceId", "LifecycleState")
}

/// Look at the group until it reports `expected` for `instance_id` — `None` meaning it no
/// longer holds that instance at all — or [`DEADLINE`] passes.
async fn settles_at(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    instance_id: &str,
    expected: Option<&str>,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let states = reported_states(router, case, group).await;
        let seen = states.get(instance_id).cloned();
        if seen.as_deref() == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group must report {expected:?} for {instance_id}, got \
             {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Backdate every hold the group has taken, so each one's own deadline has passed.
///
/// The shortest `HeartbeatTimeout` the service accepts is far longer than a test may wait, so
/// the row drives the CLOCK rather than waiting for it: the deadline the hold stored is moved
/// into the past and the next read of the store materialises what that instant made due,
/// exactly as it would have on its own.
fn time_out_every_hold(engine: &Arc<engine::InMemoryEngine>, group: &str) {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let Some(stored) = store.groups.get_mut(group) else {
        panic!("the group {group} must be stored for its holds to be timed out");
    };
    for instance in &mut stored.instances {
        for action in &mut instance.pending_lifecycle_actions {
            action.heartbeat_due = SystemTime::UNIX_EPOCH;
            action.global_due = SystemTime::UNIX_EPOCH;
        }
    }
}

/// One lifecycle hook a row's group carries: its name, its `DefaultResult` and the
/// `NotificationMetadata` it was created with.
type Hook = (&'static str, &'static str, Option<&'static str>);

/// How a row ends the hold its hooks took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// `CompleteLifecycleAction` naming the action's own token, with this result.
    ByToken(&'static str),
    /// `CompleteLifecycleAction` naming the instance instead, with this result.
    ByInstance(&'static str),
    /// `RecordLifecycleActionHeartbeat` first, and then the token with `CONTINUE`.
    HeartbeatThenContinue,
    /// Nothing: the hook's own deadline resolves the hold with its `DefaultResult`.
    Timeout,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The hooks the group carries for the transition under test.
    hooks: &'static [Hook],
    /// How the row ends the hold.
    ending: Ending,
    /// What the group must report for the instance once the hold has ended and settled:
    /// `None` for an instance that left the group.
    settles_at: Option<&'static str>,
    /// Whether the step the hook held is announced as having happened once the hold ends: a
    /// launch the hook abandoned never came into service, so it is not.
    announces_step: bool,
}

/// Create the launch template this suite's groups launch from.
async fn given_launch_template(router: &MultiServiceRouter, case: &str) {
    ec2(
        router,
        case,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", "ami-0abcdef1234567890"),
            ("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
}

/// Create the group `group` with no capacity, and put `hooks` on it for `transition`; answers
/// the ARN the service reports for it.
async fn given_group_with_hooks(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    transition: &str,
    hooks: &[Hook],
) -> String {
    asg(
        router,
        case,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("AvailabilityZones.member.1", ZONE),
        ],
    )
    .await;
    for (name, default_result, metadata) in hooks {
        let mut params: Vec<(&str, &str)> = vec![
            ("AutoScalingGroupName", group),
            ("LifecycleHookName", name),
            ("LifecycleTransition", transition),
            ("DefaultResult", default_result),
        ];
        if let Some(metadata) = metadata {
            params.push(("NotificationMetadata", metadata));
        }
        asg(router, case, "PutLifecycleHook", &params).await;
    }
    let described = asg(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    element(&described, "AutoScalingGroupARN")
}

/// Ask for `desired` capacity on the group.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    desired: i32,
) {
    asg(
        router,
        case,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &desired.to_string()),
        ],
    )
    .await;
}

/// Assert one delivered envelope announces the hold of `instance_id` by `hook` at
/// `transition`, and answer the token it carries.
fn assert_announcement(
    case: &str,
    envelope: &Value,
    detail_type: &str,
    transition: &str,
    group: &str,
    group_arn: &str,
    hook: &Hook,
) -> String {
    assert_eq!(
        envelope["source"], json!(EVENT_SOURCE),
        "case {case:?}: expected the source {EVENT_SOURCE}, got {envelope}"
    );
    assert_eq!(
        envelope["detail-type"], json!(detail_type),
        "case {case:?}: expected the detail-type {detail_type}, got {envelope}"
    );
    assert_eq!(
        envelope["detail"]["AutoScalingGroupName"], json!(group),
        "case {case:?}: expected the group {group} in the detail, got {envelope}"
    );
    assert_eq!(
        envelope["detail"]["LifecycleHookName"], json!(hook.0),
        "case {case:?}: expected the hook {} in the detail, got {envelope}", hook.0
    );
    assert_eq!(
        envelope["detail"]["LifecycleTransition"], json!(transition),
        "case {case:?}: expected the transition {transition} in the detail, got {envelope}"
    );
    assert_eq!(
        envelope["detail"]["NotificationMetadata"].as_str(), hook.2,
        "case {case:?}: expected the hook's notification metadata {:?}, got {envelope}", hook.2
    );
    let instance_id = envelope["detail"]["EC2InstanceId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        instance_id.starts_with("i-"),
        "case {case:?}: the detail must name the instance being held, got {envelope}"
    );
    // The event names every resource it is about: the group, and the instance being held.
    assert_eq!(
        envelope["resources"],
        json!([
            group_arn,
            format!(
                "arn:aws:ec2:{REGION}:{}:instance/{instance_id}",
                engine::emulator_account_id()
            )
        ]),
        "case {case:?}: expected the group's and the instance's ARNs in resources, got \
         {envelope}"
    );
    let token = envelope["detail"]["LifecycleActionToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        token.chars().count(),
        TOKEN_LENGTH,
        "case {case:?}: the token must be {TOKEN_LENGTH} characters, got {token:?}"
    );
    token
}

/// Complete the hold `hook` took on `instance_id`, as the row's ending says to.
async fn end_one_hold(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    instance_id: &str,
    token: &str,
    ending: Ending,
) {
    match ending {
        Ending::ByToken(result) => {
            asg(
                router,
                case,
                "CompleteLifecycleAction",
                &[
                    ("AutoScalingGroupName", group),
                    ("LifecycleHookName", hook),
                    ("LifecycleActionToken", token),
                    ("LifecycleActionResult", result),
                ],
            )
            .await;
        }
        Ending::ByInstance(result) => {
            asg(
                router,
                case,
                "CompleteLifecycleAction",
                &[
                    ("AutoScalingGroupName", group),
                    ("LifecycleHookName", hook),
                    ("InstanceId", instance_id),
                    ("LifecycleActionResult", result),
                ],
            )
            .await;
        }
        Ending::HeartbeatThenContinue => {
            asg(
                router,
                case,
                "RecordLifecycleActionHeartbeat",
                &[
                    ("AutoScalingGroupName", group),
                    ("LifecycleHookName", hook),
                    ("LifecycleActionToken", token),
                ],
            )
            .await;
            asg(
                router,
                case,
                "CompleteLifecycleAction",
                &[
                    ("AutoScalingGroupName", group),
                    ("LifecycleHookName", hook),
                    ("LifecycleActionToken", token),
                    ("LifecycleActionResult", CONTINUE),
                ],
            )
            .await;
        }
        Ending::Timeout => {}
    }
}

/// What one leg of the table is about: the transition it holds and where its announcements
/// are delivered.
///
/// Bundled so [`drive_row`] takes the leg rather than eight loose values, and so a launch leg
/// and a termination leg are described the same way.
struct Leg<'a> {
    /// The group under test.
    group: &'a str,
    /// That group's ARN, which the envelope names it by.
    group_arn: &'a str,
    /// The `LifecycleTransition` its hooks hold.
    transition: &'a str,
    /// The `detail-type` the hold is announced under.
    detail_type: &'a str,
    /// The `LifecycleState` the held instance is reported in.
    wait_state: &'a str,
    /// The url of the queue the matching rule delivers to.
    url: &'a str,
    /// The url of the queue a rule on the two STEP detail-types delivers to: what the hold
    /// defers until it resolves.
    step_url: &'a str,
    /// The `detail-type` the step itself is announced under once it has happened.
    step_detail_type: &'a str,
}

/// Drive one row from the hold to its end, over the leg it is about.
async fn drive_row(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Row,
    leg: &Leg<'_>,
) {
    let case = row.case;
    let (group, detail_type, wait_state) = (leg.group, leg.detail_type, leg.wait_state);
    let announcements = delivered(router, case, leg.url, detail_type, row.hooks.len()).await;
    assert_eq!(
        announcements.len(),
        row.hooks.len(),
        "case {case:?}: every hook holding the instance must be announced once, got \
         {announcements:?}"
    );

    // One announcement per hook, each carrying that hook's own token for the one instance
    // the group is holding.
    let mut held: Vec<(Hook, String, String)> = Vec::new();
    for hook in row.hooks {
        let envelope = announcements
            .iter()
            .find(|envelope| envelope["detail"]["LifecycleHookName"] == json!(hook.0))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no announcement for the hook {}: {announcements:?}",
                    hook.0
                )
            });
        let token = assert_announcement(
            case,
            envelope,
            detail_type,
            leg.transition,
            group,
            leg.group_arn,
            hook,
        );
        let instance_id = envelope["detail"]["EC2InstanceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        held.push((*hook, instance_id, token));
    }
    let instance_id = held[0].1.clone();
    assert!(
        held.iter().all(|(_, id, _)| *id == instance_id),
        "case {case:?}: every hook of one transition holds the SAME instance, got {held:?}"
    );
    let tokens: BTreeSet<&str> = held.iter().map(|(_, _, token)| token.as_str()).collect();
    assert_eq!(
        tokens.len(),
        held.len(),
        "case {case:?}: each hold is announced with its own token, got {held:?}"
    );

    // The instance the group reports is the one being held, in the wait state of its
    // transition — and the group's own read is what says so.
    settles_at(router, case, group, &instance_id, Some(wait_state)).await;

    // The STEP the hook is holding is not announced while the hold is on: the launch has
    // not finished and the termination has not happened, so neither is on the bus yet.
    let early = drain(router, case, leg.step_url).await;
    assert!(
        early.is_empty(),
        "case {case:?}: a step a hook is still holding is not announced, got {early:?}"
    );

    if row.ending == Ending::Timeout {
        time_out_every_hold(engine, group);
        // The next read materialises what that instant made due, exactly as it would have
        // on its own.
        reported_states(router, case, group).await;
    } else {
        for (index, (hook, _, token)) in held.iter().enumerate() {
            end_one_hold(router, case, group, hook.0, &instance_id, token, row.ending).await;
            let continued = matches!(
                row.ending,
                Ending::HeartbeatThenContinue
                    | Ending::ByToken(CONTINUE)
                    | Ending::ByInstance(CONTINUE)
            );
            if index + 1 < held.len() && continued {
                // A group holding several hooks for one transition holds the instance at
                // each of them: one that carried on does not release it while another is
                // still holding it.
                let states = reported_states(router, case, group).await;
                assert_eq!(
                    states.get(&instance_id).map(String::as_str),
                    Some(wait_state),
                    "case {case:?}: the instance waits while another hook still holds it, got \
                     {states:?}"
                );
            }
        }
    }

    settles_at(router, case, group, &instance_id, row.settles_at).await;

    // And the step is announced once the hold has released it — or, for a launch the hook
    // abandoned, never announced as successful at all: what the bus carries for that one is
    // the termination that gave the instance back to Amazon EC2.
    if row.announces_step {
        let announced = delivered(router, case, leg.step_url, leg.step_detail_type, 1).await;
        assert_eq!(
            announced.len(),
            1,
            "case {case:?}: the released step must be announced once, got {announced:?}"
        );
        assert_eq!(
            announced[0]["detail"]["EC2InstanceId"],
            json!(instance_id),
            "case {case:?}: the step must name the instance the hook held, got {}",
            announced[0]
        );
    } else {
        let settled = drained_until(router, case, leg.step_url, TERMINATE_SUCCESSFUL, 1).await;
        assert!(
            settled
                .iter()
                .all(|envelope| envelope["detail-type"] != json!(LAUNCH_SUCCESSFUL)),
            "case {case:?}: a launch the hook abandoned is never announced as successful, got \
             {settled:?}"
        );
        assert!(
            settled
                .iter()
                .any(|envelope| envelope["detail-type"] == json!(TERMINATE_SUCCESSFUL)),
            "case {case:?}: the instance it gave back is announced as terminated, got \
             {settled:?}"
        );
    }
}

/// THE RULE over a held LAUNCH: the announcement, the wait, and each way the hold ends.
#[tokio::test]
async fn a_launch_lifecycle_hook_announces_its_hold_and_releases_the_instance() {
    let rows: &[Row] = &[
        Row {
            case: "one launch hook, continued by its token",
            hooks: &[("hold-launch", ABANDON, None)],
            ending: Ending::ByToken(CONTINUE),
            settles_at: Some(IN_SERVICE),
            announces_step: true,
        },
        Row {
            case: "one launch hook, continued by the instance id",
            hooks: &[("hold-launch", ABANDON, None)],
            ending: Ending::ByInstance(CONTINUE),
            settles_at: Some(IN_SERVICE),
            announces_step: true,
        },
        Row {
            case: "one launch hook carrying notification metadata, heartbeated then continued",
            hooks: &[("hold-launch", ABANDON, Some("{\"stage\":\"bootstrap\"}"))],
            ending: Ending::HeartbeatThenContinue,
            settles_at: Some(IN_SERVICE),
            announces_step: true,
        },
        Row {
            case: "one launch hook, abandoned by its token",
            hooks: &[("hold-launch", CONTINUE, None)],
            ending: Ending::ByToken(ABANDON),
            settles_at: None,
            announces_step: false,
        },
        Row {
            case: "two launch hooks of the same group, each continued",
            hooks: &[("hold-launch", ABANDON, None), ("hold-launch-too", ABANDON, None)],
            ending: Ending::ByToken(CONTINUE),
            settles_at: Some(IN_SERVICE),
            announces_step: true,
        },
        Row {
            case: "one launch hook left to its own deadline, defaulting to CONTINUE",
            hooks: &[("hold-launch", CONTINUE, None)],
            ending: Ending::Timeout,
            settles_at: Some(IN_SERVICE),
            announces_step: true,
        },
        Row {
            case: "one launch hook left to its own deadline, defaulting to ABANDON",
            hooks: &[("hold-launch", ABANDON, None)],
            ending: Ending::Timeout,
            settles_at: None,
            announces_step: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's group, hooks and rule cannot decide
        // another's.
        let (router, engine) = emulator();
        given_launch_template(&router, case).await;
        let (url, queue_arn) = queue(&router, case, "autoscaling-launch-actions").await;
        rule(
            &router,
            case,
            "autoscaling-launch-actions",
            &json!({"source": [EVENT_SOURCE], "detail-type": [LAUNCH_ACTION]}),
            &queue_arn,
        )
        .await;

        let group = format!("held-launch-{index}");
        let group_arn =
            given_group_with_hooks(&router, case, &group, LAUNCHING, row.hooks).await;
        // A second rule, on the two detail-types the STEPS themselves carry: what the hold
        // defers until it resolves.
        let (step_url, step_arn) = queue(&router, case, "autoscaling-launch-steps").await;
        rule(
            &router,
            case,
            "autoscaling-launch-steps",
            &json!({
                "source": [EVENT_SOURCE],
                "detail-type": [LAUNCH_SUCCESSFUL, TERMINATE_SUCCESSFUL],
            }),
            &step_arn,
        )
        .await;
        set_desired_capacity(&router, case, &group, 1).await;

        drive_row(
            &router,
            &engine,
            row,
            &Leg {
                group: &group,
                group_arn: &group_arn,
                transition: LAUNCHING,
                detail_type: LAUNCH_ACTION,
                wait_state: PENDING_WAIT,
                url: &url,
                step_url: &step_url,
                step_detail_type: LAUNCH_SUCCESSFUL,
            },
        )
        .await;
    }
}

/// THE RULE over a held TERMINATION: a hook can delay a scale-in, not cancel it.
#[tokio::test]
async fn a_termination_lifecycle_hook_announces_its_hold_and_resumes_the_scale_in() {
    let rows: &[Row] = &[
        Row {
            case: "one termination hook, continued by its token",
            hooks: &[("hold-terminate", ABANDON, None)],
            ending: Ending::ByToken(CONTINUE),
            settles_at: None,
            announces_step: true,
        },
        Row {
            case: "one termination hook, abandoned by the instance id",
            hooks: &[("hold-terminate", CONTINUE, Some("drain=true"))],
            ending: Ending::ByInstance(ABANDON),
            settles_at: None,
            announces_step: true,
        },
        Row {
            case: "one termination hook left to its own deadline",
            hooks: &[("hold-terminate", ABANDON, None)],
            ending: Ending::Timeout,
            settles_at: None,
            announces_step: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (router, engine) = emulator();
        given_launch_template(&router, case).await;
        let (url, queue_arn) = queue(&router, case, "autoscaling-terminate-actions").await;
        rule(
            &router,
            case,
            "autoscaling-terminate-actions",
            &json!({"source": [EVENT_SOURCE], "detail-type": [TERMINATE_ACTION]}),
            &queue_arn,
        )
        .await;

        let group = format!("held-terminate-{index}");
        let group_arn =
            given_group_with_hooks(&router, case, &group, TERMINATING, row.hooks).await;
        // A second rule, on the two detail-types the STEPS themselves carry.
        let (step_url, step_arn) = queue(&router, case, "autoscaling-terminate-steps").await;
        rule(
            &router,
            case,
            "autoscaling-terminate-steps",
            &json!({
                "source": [EVENT_SOURCE],
                "detail-type": [LAUNCH_SUCCESSFUL, TERMINATE_SUCCESSFUL],
            }),
            &step_arn,
        )
        .await;
        // The group holds no launch hook, so its one instance comes into service by itself;
        // the scale-in below is what the row's hooks hold.
        set_desired_capacity(&router, case, &group, 1).await;
        let launched = loop {
            let states = reported_states(&router, case, &group).await;
            if let Some((instance_id, _)) = states
                .iter()
                .find(|(_, state)| state.as_str() == IN_SERVICE)
            {
                break instance_id.clone();
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        // That launch is announced at once \u2014 nothing held it \u2014 so it is taken off the step
        // queue before the scale-in, and what reaches it afterwards is the TERMINATION the
        // row's hook holds.
        let announced = delivered(&router, case, &step_url, LAUNCH_SUCCESSFUL, 1).await;
        assert_eq!(
            announced.len(),
            1,
            "case {case:?}: a launch no hook held is announced at once, got {announced:?}"
        );
        set_desired_capacity(&router, case, &group, 0).await;

        drive_row(
            &router,
            &engine,
            row,
            &Leg {
                group: &group,
                group_arn: &group_arn,
                transition: TERMINATING,
                detail_type: TERMINATE_ACTION,
                wait_state: TERMINATING_WAIT,
                url: &url,
                step_url: &step_url,
                step_detail_type: TERMINATE_SUCCESSFUL,
            },
        )
        .await;
        assert!(
            !reported_states(&router, case, &group)
                .await
                .contains_key(&launched),
            "case {case:?}: the instance {launched} the hook held must have left the group"
        );
    }
}

/// One row of the refusal table: the callback a client sends, and the code and message this
/// service answers it with.
struct Refusal {
    case: &'static str,
    /// The members the callback carries, each as the wire names it.
    params: Vec<(&'static str, String)>,
    code: &'static str,
    message: String,
}

/// The negative half of the same rule: nothing else is announced, and nothing but a token the
/// store is still holding completes an action.
#[tokio::test]
async fn only_a_held_action_is_announced_and_only_its_token_completes_it() {
    let case = "the hold's own token and nothing else";
    let (router, _engine) = emulator();
    given_launch_template(&router, case).await;

    // Two rules: one for the launch announcements this group makes, one pinned to the OTHER
    // transition's detail-type, which nothing here can match.
    let (url, queue_arn) = queue(&router, case, "launch-actions").await;
    rule(
        &router,
        case,
        "launch-actions",
        &json!({"source": [EVENT_SOURCE], "detail-type": [LAUNCH_ACTION]}),
        &queue_arn,
    )
    .await;
    let (other_url, other_arn) = queue(&router, case, "terminate-actions").await;
    rule(
        &router,
        case,
        "terminate-actions",
        &json!({"source": [EVENT_SOURCE], "detail-type": [TERMINATE_ACTION]}),
        &other_arn,
    )
    .await;

    let group = "token-bound-group";
    let hook: Hook = ("hold-launch", ABANDON, None);
    let group_arn =
        given_group_with_hooks(&router, case, group, LAUNCHING, &[hook]).await;
    // And a rule on the detail-type the LAUNCH itself carries: the hold defers it, so it
    // reaches this queue only once the hold has been completed.
    let (step_url, step_arn) = queue(&router, case, "launch-steps").await;
    rule(
        &router,
        case,
        "launch-steps",
        &json!({"source": [EVENT_SOURCE], "detail-type": [LAUNCH_SUCCESSFUL]}),
        &step_arn,
    )
    .await;
    set_desired_capacity(&router, case, group, 1).await;

    let announcements = delivered(&router, case, &url, LAUNCH_ACTION, 1).await;
    assert_eq!(
        announcements.len(),
        1,
        "case {case:?}: the hold must be announced once, got {announcements:?}"
    );
    let token = assert_announcement(
        case,
        &announcements[0],
        LAUNCH_ACTION,
        LAUNCHING,
        group,
        &group_arn,
        &hook,
    );
    let instance_id = announcements[0]["detail"]["EC2InstanceId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // A rule pinned to the terminating transition takes none of it: the routing is evidence
    // of what the envelope carried.
    let other = drain(&router, case, &other_url).await;
    assert!(
        other.is_empty(),
        "case {case:?}: a rule on the terminate detail-type takes no launch announcement, got \
         {other:?}"
    );
    // And the launch the hook is holding is not announced as successful while it waits.
    let early = drain(&router, case, &step_url).await;
    assert!(
        early.is_empty(),
        "case {case:?}: a launch a hook is still holding is not announced, got {early:?}"
    );

    // Every refusal is about the ACTION the request addresses, so the hold is still held
    // after each of them.
    let refusals: Vec<Refusal> = vec![
        Refusal {
            case: "a result outside the pair the member admits",
            params: vec![
                ("AutoScalingGroupName", group.to_string()),
                ("LifecycleHookName", hook.0.to_string()),
                ("LifecycleActionToken", token.clone()),
                ("LifecycleActionResult", "PROCEED".to_string()),
            ],
            code: "ValidationError",
            message: "'LifecycleActionResult' must be one of: CONTINUE, ABANDON".to_string(),
        },
        Refusal {
            case: "neither the token nor the instance",
            params: vec![
                ("AutoScalingGroupName", group.to_string()),
                ("LifecycleHookName", hook.0.to_string()),
                ("LifecycleActionResult", CONTINUE.to_string()),
            ],
            code: "ValidationError",
            message: "Either the lifecycle action token or the instance ID must be specified."
                .to_string(),
        },
        Refusal {
            case: "a token no action of the group is held under",
            params: vec![
                ("AutoScalingGroupName", group.to_string()),
                ("LifecycleHookName", hook.0.to_string()),
                (
                    "LifecycleActionToken",
                    "00000000-0000-4000-8000-000000000000".to_string(),
                ),
                ("LifecycleActionResult", CONTINUE.to_string()),
            ],
            code: "ValidationError",
            message:
                "No active Lifecycle Action found with token 00000000-0000-4000-8000-000000000000"
                    .to_string(),
        },
        Refusal {
            case: "an instance the group is not holding",
            params: vec![
                ("AutoScalingGroupName", group.to_string()),
                ("LifecycleHookName", hook.0.to_string()),
                ("InstanceId", "i-0123456789abcdef0".to_string()),
                ("LifecycleActionResult", CONTINUE.to_string()),
            ],
            code: "ValidationError",
            message: "No active Lifecycle Action found with instance ID i-0123456789abcdef0"
                .to_string(),
        },
        Refusal {
            case: "a hook of the group that is holding nothing",
            params: vec![
                ("AutoScalingGroupName", group.to_string()),
                ("LifecycleHookName", "not-a-hook".to_string()),
                ("InstanceId", instance_id.clone()),
                ("LifecycleActionResult", CONTINUE.to_string()),
            ],
            code: "ValidationError",
            message: format!("No active Lifecycle Action found with instance ID {instance_id}"),
        },
    ];

    for refusal in &refusals {
        let refusal_case = refusal.case;
        let borrowed: Vec<(&str, &str)> = refusal
            .params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let (status, body) = form_call(
            &router,
            "autoscaling",
            "2011-01-01",
            "CompleteLifecycleAction",
            &borrowed,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {refusal_case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            refusal_of(&body),
            (refusal.code.to_string(), refusal.message.clone()),
            "case {refusal_case:?}: got {body}"
        );
        let states = reported_states(&router, case, group).await;
        assert_eq!(
            states.get(&instance_id).map(String::as_str),
            Some(PENDING_WAIT),
            "case {refusal_case:?}: a refused callback leaves the hold exactly as it was, got \
             {states:?}"
        );
    }

    // The token completes the action ONCE: the hold it released is no longer held, so the
    // same token resolves nothing afterwards.
    asg(
        &router,
        case,
        "CompleteLifecycleAction",
        &[
            ("AutoScalingGroupName", group),
            ("LifecycleHookName", hook.0),
            ("LifecycleActionToken", &token),
            ("LifecycleActionResult", CONTINUE),
        ],
    )
    .await;
    settles_at(&router, case, group, &instance_id, Some(IN_SERVICE)).await;
    // The launch the hold deferred is announced now that it has carried on, naming the
    // instance that came into service.
    let announced = delivered(&router, case, &step_url, LAUNCH_SUCCESSFUL, 1).await;
    assert_eq!(
        announced.len(),
        1,
        "case {case:?}: the released launch must be announced once, got {announced:?}"
    );
    assert_eq!(
        announced[0]["detail"]["EC2InstanceId"],
        json!(instance_id),
        "case {case:?}: the launch must name the instance the hook held, got {}",
        announced[0]
    );
    let (status, body) = form_call(
        &router,
        "autoscaling",
        "2011-01-01",
        "CompleteLifecycleAction",
        &[
            ("AutoScalingGroupName", group),
            ("LifecycleHookName", hook.0),
            ("LifecycleActionToken", &token),
            ("LifecycleActionResult", CONTINUE),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a token already completed must be refused, got {status} with {body}"
    );
    assert_eq!(
        refusal_of(&body).1,
        format!("No active Lifecycle Action found with token {token}"),
        "case {case:?}: got {body}"
    );
}
