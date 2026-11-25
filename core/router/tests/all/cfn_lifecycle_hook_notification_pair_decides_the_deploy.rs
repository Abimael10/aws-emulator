// SPDX-License-Identifier: CC0-1.0

//! The `AWS::AutoScaling::LifecycleHook` a stack declares, read back through the wire of the
//! service that OWNS it — and the notification pair that decides whether the deployment
//! completes at all.
//!
//! The RULE under test: **a lifecycle hook a template declares is created in Amazon EC2 Auto
//! Scaling itself through the one call that states it (`PutLifecycleHook`), so that service's
//! own `DescribeLifecycleHooks` answers with it carrying every member the declaration stated;
//! and because that call PROVES the hook's notification pair — "when you specify a notification
//! target, Amazon EC2 Auto Scaling sends it a test message"
//! (`PutLifecycleHookType$NotificationTargetARN`) under "the ARN of the IAM role that allows
//! the Auto Scaling group to publish to the specified notification target" (same shape,
//! `RoleARN`) — a declaration whose role the account's own IAM answers may not publish to that
//! target is refused, so the resource fails, the deployment rolls back and the group carries no
//! hook at all.**
//!
//! Why it matters: a client deploys the hook declaratively and then operates it through the
//! service APIs — completing a lifecycle action, reading the hook, receiving the notification.
//! A hook a stack reports created that `DescribeLifecycleHooks` answers nothing for tells the
//! client the deploy produced nothing while the stack claims success, and a stack that completes
//! over a role that cannot publish leaves a hook whose notifications would silently never
//! arrive.
//!
//! Five crates have to agree for that to hold — CloudFormation makes the call, Amazon EC2 Auto
//! Scaling stores the hook and proves the pair, IAM holds the role the decision is read from,
//! Amazon SQS and Amazon SNS hold the targets — so the suite lives here rather than in any one
//! of them. Every observation is a client's: `DescribeStacks`, `DescribeStackEvents`,
//! `DescribeLifecycleHooks`, `DescribeAutoScalingGroups` and `ReceiveMessage`.
//!
//! The rows differ along what the rule generalises over: WHICH service holds the declared target
//! (an Amazon SQS queue, an Amazon SNS topic, no target at all), what the role's documents say
//! (the publish of that target's own service on that very target, the same action on ANOTHER
//! resource of it, no permissions policy at all, another service's publish action), whether IAM
//! holds the role at all, whether the GROUP the hook is declared on is the stack's own or one
//! that already existed, and the members the declaration states — each transition, and the
//! heartbeat timeout at both of its documented bounds.
//!
//! Nothing here waits out a span: the groups launch no instance, and every wait polls for the
//! CONDITION it is about — a stack that has settled, an event that has been recorded, a message
//! that has arrived — never for a duration.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE form encoder and XML readers this test crate keeps for query suites
// (`tests/all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The zone every group below places its capacity in.
const ZONE: &str = "us-west-2a";

/// The API versions the query wires of the services this suite calls carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";
const EC2_VERSION: &str = "2016-11-15";
const IAM_VERSION: &str = "2010-05-08";
const SNS_VERSION: &str = "2010-03-31";

/// The two terminal `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` a refused resource reaches.
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids the templates below declare.
const HOOK_LOGICAL_ID: &str = "Hook";
const GROUP_LOGICAL_ID: &str = "Asg";
const LAUNCH_TEMPLATE_LOGICAL_ID: &str = "Lt";

/// The two transitions a hook can act on, from the member's own documentation: "to create a
/// lifecycle hook for scale-out events, specify `autoscaling:EC2_INSTANCE_LAUNCHING`… for
/// scale-in events, specify `autoscaling:EC2_INSTANCE_TERMINATING`"
/// (`PutLifecycleHookType$LifecycleTransition`).
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The published bounds of a hook's heartbeat timeout, from that member's own documentation:
/// "the range is from 30 to 7200 seconds" (`PutLifecycleHookType$HeartbeatTimeout`). The rows
/// declare the hook at each of them.
const HEARTBEAT_MINIMUM: u32 = 30;
const HEARTBEAT_MAXIMUM: u32 = 7200;

/// The metadata every declaration below states, so the member a hook merely carries is read back
/// as declared.
const METADATA: &str = "declared by the stack";

/// The event a test message carries, spelled out here rather than imported from the service
/// crate, so a wrong constant in the implementation cannot make a row pass
/// (`PutLifecycleHookType$NotificationTargetARN`: "test messages contain the following
/// additional key-value pair: "Event": "autoscaling:TEST_NOTIFICATION"").
const TEST_NOTIFICATION: &str = "autoscaling:TEST_NOTIFICATION";

/// The actions the delivery of a notification to each of the two targets is authorized by, from
/// those services' own action lists ("Actions defined by Amazon SQS", "Actions defined by Amazon
/// SNS").
const QUEUE_PUBLISH: &str = "sqs:SendMessage";
const TOPIC_PUBLISH: &str = "sns:Publish";

/// The AMI the launch data names, in Amazon EC2's own id form.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The launch template the groups this suite creates itself launch from.
const LAUNCH_TEMPLATE: &str = "cfn-hook-pair-lt";

/// How long a wait gives the condition it is about, and how often it looks again. Generous: what
/// is waited FOR is a condition on the emulator's own clock, and every wait ends as soon as it
/// holds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The members of a reported hook this suite asserts over — every one of them a member the
/// declaration states, so the comparison is between what was declared and what the owning
/// service reports.
const ASSERTED_MEMBERS: &[&str] = &[
    "AutoScalingGroupName",
    "LifecycleHookName",
    "LifecycleTransition",
    "HeartbeatTimeout",
    "DefaultResult",
    "NotificationMetadata",
    "NotificationTargetARN",
    "RoleARN",
];

/// The whole emulator over one engine, exactly as the binary assembles it: the call
/// CloudFormation makes to Amazon EC2 Auto Scaling, and the ones that service makes to IAM and
/// to the target's own service, are dispatched back into this same router.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request and what each handler reads its Region from.
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
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation call.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "cloudformation", CFN_VERSION, action, members).await
}

/// One form-wire call of `signing_name` that must answer HTTP 200.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = form_call(router, signing_name, version, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with {body}"
    );
    body
}

/// One Amazon SQS call (`awsJson1_0`) that must answer HTTP 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from sqs {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One queue of this emulator: the url a client reads it by and the ARN a hook names it by,
/// both as Amazon SQS reports them.
#[derive(Debug, Clone)]
struct Queue {
    url: String,
    arn: String,
}

/// Create the queue `name` through Amazon SQS's own wire.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> Queue {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    Queue { url, arn }
}

/// Create the topic `name` through Amazon SNS's own wire and answer the ARN it reports.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = served(
        router,
        case,
        "sns",
        SNS_VERSION,
        "CreateTopic",
        &[("Name", name)],
    )
    .await;
    let arn = opt_element(&created, "TopicArn").unwrap_or_default();
    assert!(
        !arn.is_empty(),
        "case {case:?}: CreateTopic must report the topic's ARN, got {created}"
    );
    arn
}

/// Create the IAM role `role_name`, trusted by this service's own principal, carrying `document`
/// as its one inline permissions policy when it carries one at all; answers the ARN IAM ITSELF
/// reports for it, which is the value a declaration names it by.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    document: Option<&str>,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "autoscaling.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = served(
        router,
        case,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let arn = opt_element(&created, "Arn").unwrap_or_default();
    assert!(
        !arn.is_empty(),
        "case {case:?}: CreateRole must report the role's ARN, got {created}"
    );
    if let Some(document) = document {
        served(
            router,
            case,
            "iam",
            IAM_VERSION,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "publish"),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    arn
}

/// The ARN of a role of this account IAM holds NO record of: one created and then deleted, so
/// the value is a well-formed ARN of a role of this account that resolves to nothing — which is
/// how a role provisioned out of band looks to this emulator.
async fn given_absent_role(router: &MultiServiceRouter, case: &str, role_name: &str) -> String {
    let arn = given_role(router, case, role_name, None).await;
    served(
        router,
        case,
        "iam",
        IAM_VERSION,
        "DeleteRole",
        &[("RoleName", role_name)],
    )
    .await;
    arn
}

/// One document allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// Create the launch template this suite's own groups launch from.
async fn given_launch_template(router: &MultiServiceRouter, case: &str) {
    served(
        router,
        case,
        "ec2",
        EC2_VERSION,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", LAUNCH_TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
}

/// Create the Auto Scaling group `group` with no capacity, through that service's own wire: the
/// group a row's hook is declared on where the row's stack does not declare one.
async fn given_group(router: &MultiServiceRouter, case: &str, group: &str) {
    served(
        router,
        case,
        "autoscaling",
        ASG_VERSION,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
            ("AvailabilityZones.member.1", ZONE),
        ],
    )
    .await;
}

/// Every lifecycle hook Amazon EC2 Auto Scaling reports for `group` right now, each as the
/// members this suite asserts over that it carries a non-empty value for.
///
/// A group that service no longer holds answers no hook at all, which is reported as the empty
/// list: a rolled-back stack takes the group it created with it, and that is a group carrying no
/// hook either way.
async fn reported_hooks(
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<BTreeMap<String, String>> {
    let (status, body) = form_call(
        router,
        "autoscaling",
        ASG_VERSION,
        "DescribeLifecycleHooks",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    members_of(&body)
        .into_iter()
        .map(|member| {
            ASSERTED_MEMBERS
                .iter()
                .filter_map(|name| {
                    opt_element(member, name)
                        .filter(|value| !value.is_empty())
                        .map(|value| ((*name).to_string(), value))
                })
                .collect()
        })
        .collect()
}

/// Whether Amazon EC2 Auto Scaling still holds a group called `group`.
async fn holds_group(router: &MultiServiceRouter, case: &str, group: &str) -> bool {
    let described = served(
        router,
        case,
        "autoscaling",
        ASG_VERSION,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    members_of(&described)
        .into_iter()
        .any(|member| opt_element(member, "AutoScalingGroupName").as_deref() == Some(group))
}

/// Poll `DescribeStacks` until `stack` first reports a status that is not an operation in
/// flight, and answer it with the body it was read from.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.is_empty() && !reached.ends_with("_IN_PROGRESS") {
            return (reached, body);
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatusReason` of the event in which `logical_id` reached `RESOURCE_CREATE_FAILED`,
/// looked for again until the stack has recorded it or [`SETTLE_DEADLINE`] passes.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together, so a
/// reason is never read off another resource's event.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {body}"
        );
        let recorded = members_of(&body).into_iter().find_map(|member| {
            let named = opt_element(member, "LogicalResourceId").as_deref() == Some(logical_id);
            let failed =
                opt_element(member, "ResourceStatus").as_deref() == Some(RESOURCE_CREATE_FAILED);
            (named && failed)
                .then(|| opt_element(member, "ResourceStatusReason"))
                .flatten()
                .filter(|reason| !reason.is_empty())
        });
        if let Some(reason) = recorded {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} recorded no {RESOURCE_CREATE_FAILED} reason within \
             {SETTLE_DEADLINE:?}: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every document now waiting on the queue at `url`, each read as the notification its publisher
/// sent.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            serde_json::from_str(message["Body"].as_str().unwrap_or_default()).unwrap_or(Value::Null)
        })
        .collect()
}

/// Look at the queue until it has delivered the `autoscaling:TEST_NOTIFICATION` for `group`, or
/// [`SETTLE_DEADLINE`] passes. Nothing in this loop calls CloudFormation or Auto Scaling.
async fn awaited_test_notification(
    router: &MultiServiceRouter,
    case: &str,
    queue: &Queue,
    group: &str,
) -> Option<Value> {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let found = drained(router, case, &queue.url)
            .await
            .into_iter()
            .find(|document| {
                document["Event"] == json!(TEST_NOTIFICATION)
                    && document["AutoScalingGroupName"] == json!(group)
            });
        if found.is_some() || tokio::time::Instant::now() >= giving_up_at {
            return found;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Which service holds the target a row's hook declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// No notification pair at all: the declaration names neither half.
    Unset,
    /// The queue the hook is meant to notify.
    Queue,
    /// A topic of the account.
    Topic,
}

/// What a row's role is granted, and whether IAM holds it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No inline permissions policy at all.
    Nothing,
    /// The publish action of the target's own service, on that very target.
    PublishOnTarget,
    /// That same action, on ANOTHER resource of the same service.
    PublishOnAnother,
    /// The publish action of the OTHER target service, on everything.
    TheOtherServicesPublish,
    /// A role this account's IAM holds no record of.
    NoSuchRole,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// Which target the declaration names.
    target: Target,
    /// What the role it names is granted.
    grant: Grant,
    /// Whether the template declares the GROUP the hook sits on, or the hook is declared on a
    /// group that already existed.
    group_in_stack: bool,
    /// The transition, timeout and default result the declaration states.
    transition: &'static str,
    heartbeat: u32,
    default_result: &'static str,
    /// The status the deployment must settle in.
    settles: &'static str,
}

/// The rows span what the rule generalises over: the target's own service, what the role is
/// granted, whether IAM holds the role, where the group comes from, and the members declared.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a queue target under a role granted the send on it, the group the stack's own",
            target: Target::Queue,
            grant: Grant::PublishOnTarget,
            group_in_stack: true,
            transition: LAUNCHING,
            heartbeat: 300,
            default_result: "CONTINUE",
            settles: CREATE_COMPLETE,
        },
        Row {
            case: "a queue target under a role granted the send on ANOTHER queue",
            target: Target::Queue,
            grant: Grant::PublishOnAnother,
            group_in_stack: true,
            transition: LAUNCHING,
            heartbeat: 300,
            default_result: "CONTINUE",
            settles: ROLLBACK_COMPLETE,
        },
        Row {
            case: "a queue target under a role carrying no permissions policy at all",
            target: Target::Queue,
            grant: Grant::Nothing,
            group_in_stack: false,
            transition: TERMINATING,
            heartbeat: HEARTBEAT_MINIMUM,
            default_result: "ABANDON",
            settles: ROLLBACK_COMPLETE,
        },
        Row {
            case: "a topic target under a role granted the publish on it",
            target: Target::Topic,
            grant: Grant::PublishOnTarget,
            group_in_stack: false,
            transition: TERMINATING,
            heartbeat: HEARTBEAT_MAXIMUM,
            default_result: "ABANDON",
            settles: CREATE_COMPLETE,
        },
        Row {
            case: "a topic target under a role granted only the queue service's action",
            target: Target::Topic,
            grant: Grant::TheOtherServicesPublish,
            group_in_stack: false,
            transition: LAUNCHING,
            heartbeat: HEARTBEAT_MINIMUM,
            default_result: "CONTINUE",
            settles: ROLLBACK_COMPLETE,
        },
        Row {
            case: "a queue target under a role this account's IAM holds no record of",
            target: Target::Queue,
            grant: Grant::NoSuchRole,
            group_in_stack: false,
            transition: LAUNCHING,
            heartbeat: HEARTBEAT_MAXIMUM,
            default_result: "ABANDON",
            settles: CREATE_COMPLETE,
        },
        Row {
            case: "a hook naming neither half of the notification pair",
            target: Target::Unset,
            grant: Grant::Nothing,
            group_in_stack: false,
            transition: TERMINATING,
            heartbeat: 1800,
            default_result: "CONTINUE",
            settles: CREATE_COMPLETE,
        },
    ]
}

/// The template of one row: the hook, and the group it sits on where the row declares it.
fn template(row: &Row, group: &str, hook: &str, target: Option<&str>, role: Option<&str>) -> String {
    let mut properties = json!({
        "AutoScalingGroupName": group,
        "LifecycleHookName": hook,
        "LifecycleTransition": row.transition,
        "HeartbeatTimeout": row.heartbeat,
        "DefaultResult": row.default_result,
        "NotificationMetadata": METADATA,
    });
    if let Some(target) = target {
        properties["NotificationTargetARN"] = json!(target);
    }
    if let Some(role) = role {
        properties["RoleARN"] = json!(role);
    }
    let mut declared = json!({
        HOOK_LOGICAL_ID: {
            "Type": "AWS::AutoScaling::LifecycleHook",
            "Properties": properties,
        },
    });
    if row.group_in_stack {
        declared[LAUNCH_TEMPLATE_LOGICAL_ID] = json!({
            "Type": "AWS::EC2::LaunchTemplate",
            "Properties": {
                "LaunchTemplateData": { "ImageId": IMAGE_ID, "InstanceType": INSTANCE_TYPE },
            },
        });
        declared[GROUP_LOGICAL_ID] = json!({
            "Type": "AWS::AutoScaling::AutoScalingGroup",
            "Properties": {
                "AutoScalingGroupName": group,
                "MinSize": "0",
                "MaxSize": "1",
                "DesiredCapacity": "0",
                "AvailabilityZones": [ZONE],
                "LaunchTemplate": {
                    "LaunchTemplateId": { "Ref": LAUNCH_TEMPLATE_LOGICAL_ID },
                    "Version": { "Fn::GetAtt": [LAUNCH_TEMPLATE_LOGICAL_ID, "LatestVersionNumber"] },
                },
            },
        });
        // The hook addresses its group by NAME, so the order the two are created in is stated
        // rather than inferred from a reference.
        declared[HOOK_LOGICAL_ID]["DependsOn"] = json!(GROUP_LOGICAL_ID);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": declared,
    })
    .to_string()
}

/// The whole message Amazon EC2 Auto Scaling refuses a hook whose role may not publish to its
/// target with: fixed text around the TWO ARNs the request carried. Spelled out here rather than
/// taken from the crate, so the assertion does not take the handler's own word for the sentence.
fn unable_to_publish_message(target_arn: &str, role_arn: &str) -> String {
    format!(
        "Unable to publish test message to notification target {target_arn} using IAM role \
         {role_arn}. Please check your target and role configuration and try to put lifecycle \
         hook again."
    )
}

/// RULE: the hook a stack declares is the hook the owning service holds, and the notification
/// pair the declaration names decides whether the deployment completes at all.
#[tokio::test]
async fn a_declared_lifecycle_hook_is_held_by_auto_scaling_only_where_its_role_may_publish() {
    let router = router();
    let setup = "the shared estate";
    given_launch_template(&router, setup).await;
    // The resource a role may be granted on INSTEAD of the target, which is a target of the same
    // service so only the resource differs.
    let other_queue = given_queue(&router, setup, "cfn-hook-pair-other-q").await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-hook-pair-{index}");
        let group = format!("cfn-hook-pair-group-{index}");
        let hook = format!("cfn-hook-pair-hook-{index}");

        let queue = given_queue(&router, case, &format!("cfn-hook-pair-q-{index}")).await;
        let topic = given_topic(&router, case, &format!("cfn-hook-pair-t-{index}")).await;
        let target = match row.target {
            Target::Unset => None,
            Target::Queue => Some(queue.arn.clone()),
            Target::Topic => Some(topic.clone()),
        };
        let publish = match row.target {
            Target::Topic => TOPIC_PUBLISH,
            Target::Unset | Target::Queue => QUEUE_PUBLISH,
        };
        let document = match row.grant {
            Grant::Nothing | Grant::NoSuchRole => None,
            Grant::PublishOnTarget => {
                Some(allow(publish, target.as_deref().unwrap_or_default()))
            }
            Grant::PublishOnAnother => Some(allow(publish, &other_queue.arn)),
            Grant::TheOtherServicesPublish => Some(allow(
                if publish == QUEUE_PUBLISH {
                    TOPIC_PUBLISH
                } else {
                    QUEUE_PUBLISH
                },
                "*",
            )),
        };
        let role_name = format!("cfn-hook-pair-role-{index}");
        let role = match row.target {
            // A declaration that names no target names no role either: there is no pair.
            Target::Unset => None,
            _ => Some(match row.grant {
                Grant::NoSuchRole => given_absent_role(&router, case, &role_name).await,
                _ => given_role(&router, case, &role_name, document.as_deref()).await,
            }),
        };

        if !row.group_in_stack {
            given_group(&router, case, &group).await;
        }

        let body = template(&row, &group, &hook, target.as_deref(), role.as_deref());
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );

        let (settled, described) = settled_status(&router, case, &stack).await;
        assert_eq!(
            settled, row.settles,
            "case {case:?}: expected the deployment to settle {:?}, got {settled:?} ({described})",
            row.settles
        );

        let hooks = reported_hooks(&router, &group).await;
        if row.settles == CREATE_COMPLETE {
            // The hook the stack reports created is one the owning service HOLDS, carrying every
            // member the declaration stated.
            let mut expected: BTreeMap<String, String> = BTreeMap::new();
            expected.insert("AutoScalingGroupName".to_string(), group.clone());
            expected.insert("LifecycleHookName".to_string(), hook.clone());
            expected.insert("LifecycleTransition".to_string(), row.transition.to_string());
            expected.insert("HeartbeatTimeout".to_string(), row.heartbeat.to_string());
            expected.insert("DefaultResult".to_string(), row.default_result.to_string());
            expected.insert("NotificationMetadata".to_string(), METADATA.to_string());
            if let Some(target) = target.as_deref() {
                expected.insert("NotificationTargetARN".to_string(), target.to_string());
            }
            if let Some(role) = role.as_deref() {
                expected.insert("RoleARN".to_string(), role.to_string());
            }
            assert_eq!(
                hooks,
                vec![expected],
                "case {case:?}: expected {group} to carry the one declared hook"
            );
            // A queue target the role may publish to RECEIVES the test message the put proves the
            // pair with.
            if row.target == Target::Queue {
                let delivered = awaited_test_notification(&router, case, &queue, &group).await;
                assert!(
                    delivered.is_some(),
                    "case {case:?}: the declared target must receive one {TEST_NOTIFICATION} for \
                     {group}, got {:?}",
                    drained(&router, case, &queue.url).await
                );
            }
        } else {
            // A refused declaration leaves NO hook: not on the group that already existed, and
            // not on one the rolled-back stack took with it.
            assert!(
                hooks.is_empty(),
                "case {case:?}: a refused declaration must leave no hook, got {hooks:?}"
            );
            assert_eq!(
                row.group_in_stack,
                !holds_group(&router, case, &group).await,
                "case {case:?}: the rollback must take exactly the group the stack created"
            );
            // The client is told WHY, in the owning service's own words.
            let reason = failure_reason(&router, case, &stack, HOOK_LOGICAL_ID).await;
            let sentence = unable_to_publish_message(
                target.as_deref().unwrap_or_default(),
                role.as_deref().unwrap_or_default(),
            );
            assert!(
                reason.contains(&sentence),
                "case {case:?}: expected the failure to quote {sentence:?}, got {reason:?}"
            );
            // Nothing was published to the target either: the pair was never provable.
            if row.target == Target::Queue {
                let delivered = drained(&router, case, &queue.url).await;
                assert!(
                    delivered.is_empty(),
                    "case {case:?}: a refused declaration must publish nothing, got {delivered:?}"
                );
            }
        }
    }
}
