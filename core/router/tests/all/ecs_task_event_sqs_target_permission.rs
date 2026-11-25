// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule's Amazon SQS target is admitted by the TARGET QUEUE'S OWN resource
//! policy, at DELIVERY time, for the `ECS Task State Change` events a task's run publishes.
//!
//! RULE: EventBridge decides nothing about a target's permission when the target is WIRED —
//! `PutTargets` naming a queue that carries no resource policy answers 200 with no failed
//! entry — and makes the delivery itself as its own service principal, which only the target
//! queue's resource-based policy can admit. So a matching envelope reaches a queue whose
//! `Policy` allows that principal `sqs:SendMessage` for THIS delivery, and reaches no queue
//! whose policy does not, while the rule, its targets and the publication are unchanged. The
//! decision is remade for every event, so a policy installed with `SetQueueAttributes` after a
//! delivery was withheld admits the NEXT matching envelope.
//!
//! What is new here, beside
//! `service-com-amazonaws-eventbridge-awsevents/tests/all/queue_target_policy_authorization.rs`
//! — which spans the policy forms themselves against a published entry — is the PUBLISHER: the
//! envelope under test is one Amazon ECS put on the bus itself for a task's own lifecycle, with
//! no `PutEvents` call made by anybody, so the rows here span only whether the target queue's
//! policy admits that delivery rather than restating that suite's table.
//!
//! The event the rule carries is Amazon ECS's own: every status a task moves into is published
//! onto the account's default bus as `source` `aws.ecs` / `detail-type` `ECS Task State
//! Change`, carrying in `detail` the task Amazon ECS holds at that instant — its `taskArn`,
//! the `clusterArn` of the cluster it runs in, and the `lastStatus` it moved into.
//!
//! Sources: `smithy_query(query="operation", operation="RunTask,StopTask")` for the task
//! projection an event's `detail` is read off (`taskArn`, `clusterArn`, `lastStatus`,
//! `desiredStatus`) and for `RunTask$networkConfiguration`;
//! `smithy_query(query="operation", operation="PutTargets")` for `PutTargetsResponse$FailedEntryCount`;
//! `smithy_query(query="shape", shape="ReceiveMessageRequest")` for `WaitTimeSeconds`
//! ("an integer from 0 to 20").
//!
//! This suite lives beside the router because the path is cross-service: IAM owns the task
//! execution role the placement is performed with, Amazon ECS publishes through the in-process
//! callback, EventBridge routes the envelope by its CONTENT and decides each target against
//! what the target's own service says about it, and Amazon SQS owns both the queue policy that
//! decides the send and the messages that arrived. Every fact is created through the owning
//! service's own API and every verdict is read back through Amazon SQS.
//!
//! The rows differ along what the rule generalises over: WHAT the target queue's policy says
//! when the envelope is published — nothing at all, the send granted to the delivering
//! principal, that same grant narrowed to the rule the delivery comes from, the principal
//! granted a different action, and the send granted to a different principal.
//!
//! Every negative half is BOUNDED by a control queue whose own policy admits the delivery and
//! which is targeted by a second rule on the same pattern: a target queue that is still empty
//! once the control holds the same event is a queue the delivery was WITHHELD from rather than
//! one the envelope has not reached yet. Every wait looks again for the CONDITION it waits on
//! until a generous deadline passes, and nothing asserts on how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder this test crate keeps for awsQuery suites (`all/query_wire.rs`).
use crate::query_wire::form_encode;

/// The Region every request below is signed for, and so the Region of every ARN minted for
/// them.
const REGION: &str = "us-west-2";

/// The account every emulator ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `source` and `detail-type` Amazon ECS publishes a task's state changes under, spelled
/// out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.ecs";
const TASK_STATE_CHANGE: &str = "ECS Task State Change";

/// The status every task below comes to rest in, which is the last envelope of its run and so
/// the one the negative halves are bounded by.
const STOPPED: &str = "STOPPED";

/// The statuses a task passes through, spelled out here rather than imported from the service
/// crate so a publisher reporting a status no task has cannot make this suite pass. Source:
/// `smithy_query(query="doc", shape="Task", member="lastStatus")` for the lifecycle a task is
/// reported by.
const TASK_STATUSES: [&str; 5] = [
    "PROVISIONING",
    "PENDING",
    "RUNNING",
    "DEACTIVATING",
    STOPPED,
];

/// The bus an Amazon Web Services service's own events go to.
const DEFAULT_BUS: &str = "default";

/// The service principal EventBridge makes an SQS delivery as, which is what the target
/// queue's own resource policy admits, and the action such a delivery is.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The `X-Amz-Target` prefix of the Amazon ECS model, whose `sigv4Name` is `ecs`.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The cluster every task below runs in.
const CLUSTER: &str = "taskeventperm";

/// The task execution role every placement below is performed with: "the task execution role
/// that grants the Amazon ECS container agent permission to make Amazon Web Services API calls
/// on your behalf" (`TaskDefinition$executionRoleArn`). It is created through IAM's own API and
/// carries the policy AWS publishes for one.
const EXECUTION_ROLE: &str = "ecsTaskExecutionRole";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The service principal a task execution role trusts.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The network a placement below names. The ids are fixed, because the requests and the records
/// have to name the same ones, so they are written into the account's Amazon EC2 records rather
/// than minted ([`crate::ec2_network`]).
const VPC: &str = "vpc-0ec50000000000002";
const SUBNET: &str = "subnet-0ec50000000000002";

/// The security group a placement names beside its subnet. Nothing resolves it — what these
/// rows are about is the delivery, not the group — so one value does.
const SECURITY_GROUP: &str = "sg-0ec50000000000002";

/// The image every container names, and what the one essential container of each task runs: a
/// wait the task's own stop ends, which is the shape of a short-lived standalone task. The VPC
/// the tasks are placed in has a route to the registry
/// ([`crate::ec2_network::register_internet_route`]) and the placements take a public address,
/// so the fetch is reachable.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";
const CONTAINER_COMMAND: [&str; 2] = ["sleep", "20"];

/// How long a receive waits for a message before answering empty, inside the documented
/// `ReceiveMessageRequest$WaitTimeSeconds` range ("an integer from 0 to 20"): the receives here
/// WAIT rather than sampling and returning at once, so the deliveries under test are made while
/// a consumer is parked on the queue. One second is as much of that wait as these rows need,
/// not the maximum the member allows.
const RECEIVE_WAIT_SECONDS: i64 = 1;

/// How long a poll keeps looking before it gives up, and how often it looks again. Generous by
/// well over an order of magnitude over the spans involved — a task crosses its launch steps,
/// its start and its stop — because the deadline is only here so a failure is a failure rather
/// than a hang, and a run sharing a loaded machine with the rest of the workspace must not be
/// read as one.
const POLL_BUDGET: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Amazon ECS, EventBridge, Amazon SQS and IAM over ONE shared engine, as `router/src/lib.rs`
/// mounts them, with the network the placements name in the account's Amazon EC2 records.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::ec2_network::register_network(&engine, VPC, &[SUBNET], &format!("{REGION}a"));
    crate::ec2_network::register_internet_route(&engine, VPC);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One `X-Amz-Target` RPC call, answered with its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must succeed, answered with its body.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("{ECS_TARGET_PREFIX}.{operation}");
    let (status, parsed) = json_call(router, "ecs", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call that must succeed.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AWSEvents.{operation}");
    let (status, parsed) = json_call(router, "events", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must succeed.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let (status, parsed) = json_call(router, "sqs", &target, "1.0", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM that must succeed: the execution role and the policy in force on it
/// are created through IAM's own API.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, String)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
}

/// A string member at `pointer`, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The ARN of this suite's cluster in this account and Region.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{CLUSTER}")
}

/// The pattern of a rule matching the task state changes of this suite's cluster: the identity
/// Amazon ECS publishes them under, narrowed through the event's own `detail`.
fn task_events_of_the_cluster() -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [TASK_STATE_CHANGE],
        "detail": { "clusterArn": [cluster_arn()] },
    })
}

/// Create, through IAM's own API, the execution role every placement is performed with: it
/// trusts the Amazon ECS tasks service principal and carries the policy AWS publishes for a
/// task execution role. Answers its ARN.
async fn given_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    iam(
        router,
        case,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyArn", EXECUTION_ROLE_POLICY.to_string()),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// An SQS queue called `name`, created through Amazon SQS itself and carrying NO resource
/// policy; answers its url and the ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    (url, arn)
}

/// The resource policy a queue carries: `principal` granted `action` on the queue, optionally
/// only for a delivery whose `aws:SourceArn` is `from_rule`.
fn queue_policy(
    principal: &str,
    action: &str,
    queue_arn: &str,
    from_rule: Option<&str>,
) -> String {
    let mut statement = json!({
        "Sid": "TargetDelivery",
        "Effect": "Allow",
        "Principal": { "Service": principal },
        "Action": action,
        "Resource": queue_arn,
    });
    if let Some(rule_arn) = from_rule {
        statement["Condition"] = json!({ "ArnEquals": { "aws:SourceArn": rule_arn } });
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// Install `policy` as the queue's own `Policy`.
async fn set_queue_policy(router: &MultiServiceRouter, case: &str, url: &str, policy: &str) {
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
}

/// An ENABLED rule called `name` on the default bus carrying this suite's pattern, with the
/// queue `queue_arn` as its one target; answers the rule's own ARN — the ARN a target's queue
/// policy names as the source of the delivery.
///
/// `PutTargets` is asserted to accept the target WHATEVER the queue has granted: target-side
/// authorization is a DELIVERY-time question, not a wiring-time one.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue_arn: &str,
) -> String {
    let put = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": task_events_of_the_cluster().to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = member(case, &put, "/RuleArn");
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{ "Id": "1", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets must accept a target whatever its queue has granted, got \
         {targeted}"
    );
    rule_arn
}

/// Create this suite's cluster and register one Fargate revision of `family` performed as
/// `execution_role`; answers nothing — the family is what a placement names.
async fn given_cluster_and_revision(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    execution_role: &str,
) {
    ecs(
        router,
        case,
        "CreateCluster",
        &json!({ "clusterName": CLUSTER }),
    )
    .await;
    ecs(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            // The smallest documented task size pair (`RegisterTaskDefinition$cpu`: "256
            // (.25 vCPU) - Available memory values: 512 (0.5 GB)").
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "essential": true,
                "command": CONTAINER_COMMAND,
            }],
        }),
    )
    .await;
}

/// Place one task of `family` in this suite's cluster and stop it again, the way a client that
/// runs a short-lived task does; answers the ARN of the task that was placed.
async fn run_then_stop_a_task(router: &MultiServiceRouter, case: &str, family: &str) -> String {
    let placed = ecs(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "count": 1,
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [SUBNET],
                    "securityGroups": [SECURITY_GROUP],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    let failures = placed["failures"].as_array().cloned().unwrap_or_default();
    assert!(
        failures.is_empty(),
        "case {case:?}: RunTask must place the task, got failures {failures:?}"
    );
    let task_arn = member(case, &placed, "/tasks/0/taskArn");
    ecs(
        router,
        case,
        "StopTask",
        &json!({ "cluster": CLUSTER, "task": &task_arn, "reason": "the run is over" }),
    )
    .await;
    task_arn
}

/// Every message now waiting on the queue at `url`, as the parsed envelopes their bodies carry
/// — each one DELETED as it is taken, the way a consumer of a queue reads it, so nothing read
/// here can come back when its visibility timeout lapses and be read as a second delivery.
///
/// The receive LONG POLLS (`WaitTimeSeconds`), which is how a consumer of an event queue waits:
/// the deliveries under test are therefore made while a consumer is parked on the queue.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": RECEIVE_WAIT_SECONDS,
        }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes: Vec<Value> = Vec::with_capacity(messages.len());
    for message in &messages {
        let raw = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(raw).unwrap_or_else(|error| {
            panic!("case {case:?}: a delivered event must be JSON, got {raw:?} ({error})")
        }));
        let handle = member(case, message, "/ReceiptHandle");
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
        )
        .await;
    }
    envelopes
}

/// Whether `envelopes` hold the state change of `task_arn` into `status`.
fn holds(envelopes: &[Value], task_arn: &str, status: &str) -> bool {
    envelopes.iter().any(|envelope| {
        envelope["detail"]["taskArn"] == json!(task_arn)
            && envelope["detail"]["lastStatus"] == json!(status)
    })
}

/// Poll the queue at `url` — and only the queue — until it has delivered the move of `task_arn`
/// into `status`, and answer everything that arrived on it.
///
/// Nothing in this loop calls Amazon ECS, which is the point: a client watching a task through
/// EventBridge makes no further call to the service that publishes.
async fn collected_until_stopped(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    url: &str,
    task_arn: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, case, url).await);
        if holds(&collected, task_arn, STOPPED) {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what} to receive the {STOPPED} state change of \
             {task_arn}, got {collected:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert the envelope's own identity and the task it is about.
fn assert_envelope(case: &str, envelope: &Value, task_arn: &str) {
    assert_eq!(
        envelope["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: expected source {EVENT_SOURCE}, got {envelope}"
    );
    assert_eq!(
        envelope["detail-type"],
        json!(TASK_STATE_CHANGE),
        "case {case:?}: expected detail-type {TASK_STATE_CHANGE}, got {envelope}"
    );
    assert_eq!(
        envelope["detail"]["clusterArn"],
        json!(cluster_arn()),
        "case {case:?}: expected detail.clusterArn {}, got {envelope}",
        cluster_arn()
    );
    assert_eq!(
        envelope["detail"]["taskArn"],
        json!(task_arn),
        "case {case:?}: expected detail.taskArn {task_arn}, got {envelope}"
    );
    // The member names of a task state change are fixed, and both halves of the pair a task
    // reports its position by travel with every envelope; their values are statuses of the
    // task lifecycle. The envelope of the move into `STOPPED` is one of a task heading there.
    for name in ["lastStatus", "desiredStatus"] {
        let reported = envelope["detail"][name].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: expected detail.{name} in {envelope}")
        });
        assert!(
            TASK_STATUSES.contains(&reported),
            "case {case:?}: expected detail.{name} to be a task status, got {reported:?} in \
             {envelope}"
        );
    }
    if envelope["detail"]["lastStatus"] == json!(STOPPED) {
        assert_eq!(
            envelope["detail"]["desiredStatus"],
            json!(STOPPED),
            "case {case:?}: a task that has stopped is heading for {STOPPED}, got {envelope}"
        );
    }
}

/// Which `aws:SourceArn` the row's grant is narrowed to, when it is narrowed at all: a
/// statement scoped to the rule the delivery comes FROM admits it, and one scoped to another
/// rule of the same bus is a policy that is present and does not.
#[derive(Debug, Clone, Copy)]
enum Narrowing {
    /// No condition: the grant covers every delivery.
    Unconditional,
    /// The rule whose target this queue is.
    TheDeliveringRule,
    /// Another rule of the same bus.
    AnotherRule,
}

/// One row: what the target queue's own `Policy` says, and whether the delivery is admitted.
struct Row {
    case: &'static str,
    /// How the grant is narrowed, and `None` for a queue that carries no policy at all.
    grant: Option<Narrowing>,
    delivered: bool,
}

/// The table spans what decides THIS delivery: a target queue with no policy, one whose grant
/// covers it, and one whose grant is present but names another rule as the source.
const ROWS: &[Row] = &[
    Row {
        case: "a target queue with no resource policy at all",
        grant: None,
        delivered: false,
    },
    Row {
        case: "the delivering principal granted the send on the queue",
        grant: Some(Narrowing::Unconditional),
        delivered: true,
    },
    Row {
        case: "that grant narrowed to the rule the delivery comes from",
        grant: Some(Narrowing::TheDeliveringRule),
        delivered: true,
    },
    Row {
        case: "that grant narrowed to another rule of the same bus",
        grant: Some(Narrowing::AnotherRule),
        delivered: false,
    },
];

/// RULE: the target queue's own resource policy is what admits an EventBridge delivery, decided
/// when the envelope is delivered rather than when the target was wired.
#[tokio::test]
async fn a_task_events_sqs_target_is_admitted_by_its_own_queue_policy() {
    for row in ROWS {
        let case = row.case;
        // A fresh emulator per row, so one row's policies cannot decide another's.
        let router = emulator();
        let execution_role = given_execution_role(&router, case).await;
        given_cluster_and_revision(&router, case, "taskeventperm", &execution_role).await;

        // The CONTROL: a second rule on the same pattern whose queue admits the delivery. It is
        // what bounds the negative rows — a target queue still empty once the control holds the
        // same task's STOPPED envelope is one the delivery was withheld from.
        let (control_url, control_arn) = given_queue(&router, case, "ecs-task-events-control").await;
        let control_rule = given_rule(&router, case, "task-events-control", &control_arn).await;
        set_queue_policy(
            &router,
            case,
            &control_url,
            &queue_policy(
                EVENTS_PRINCIPAL,
                SEND_MESSAGE,
                &control_arn,
                Some(&control_rule),
            ),
        )
        .await;

        let (target_url, target_arn) = given_queue(&router, case, "ecs-task-events-target").await;
        let target_rule = given_rule(&router, case, "task-events-target", &target_arn).await;
        if let Some(narrowing) = row.grant {
            let from_rule = match narrowing {
                Narrowing::Unconditional => None,
                Narrowing::TheDeliveringRule => Some(target_rule.as_str()),
                Narrowing::AnotherRule => Some(control_rule.as_str()),
            };
            set_queue_policy(
                &router,
                case,
                &target_url,
                &queue_policy(EVENTS_PRINCIPAL, SEND_MESSAGE, &target_arn, from_rule),
            )
            .await;
        }

        let task_arn = run_then_stop_a_task(&router, case, "taskeventperm").await;

        // The control's own deliveries, which are what says the publication pass for this task
        // has run to the end.
        let control = collected_until_stopped(
            &router,
            case,
            "the control queue",
            &control_url,
            &task_arn,
        )
        .await;
        for envelope in &control {
            assert_envelope(case, envelope, &task_arn);
        }

        if row.delivered {
            let delivered = collected_until_stopped(
                &router,
                case,
                "the target queue",
                &target_url,
                &task_arn,
            )
            .await;
            for envelope in &delivered {
                assert_envelope(case, envelope, &task_arn);
            }
        } else {
            // Two long-polling reads rather than one: the two rules decide their own targets,
            // so a second read gives a delivery that was going to happen its chance after the
            // control already holds the same event.
            let mut withheld = drain(&router, case, &target_url).await;
            withheld.extend(drain(&router, case, &target_url).await);
            assert!(
                withheld.is_empty(),
                "case {case:?}: a queue whose policy does not admit the delivery must receive \
                 nothing, got {withheld:?}"
            );
        }
    }
}

/// RULE, the other half: the decision is remade for every event, so a policy installed AFTER a
/// delivery was withheld admits the NEXT matching envelope — on the same rule, the same target
/// and the same queue.
#[tokio::test]
async fn a_queue_policy_installed_later_admits_the_next_task_event() {
    let case = "the policy installed after a withheld delivery";
    let router = emulator();
    let execution_role = given_execution_role(&router, case).await;
    given_cluster_and_revision(&router, case, "taskeventperm", &execution_role).await;

    let (control_url, control_arn) = given_queue(&router, case, "ecs-task-events-control").await;
    let control_rule = given_rule(&router, case, "task-events-control", &control_arn).await;
    set_queue_policy(
        &router,
        case,
        &control_url,
        &queue_policy(
            EVENTS_PRINCIPAL,
            SEND_MESSAGE,
            &control_arn,
            Some(&control_rule),
        ),
    )
    .await;

    // The target: wired while it carries no policy at all, which `PutTargets` accepts.
    let (target_url, target_arn) = given_queue(&router, case, "ecs-task-events-target").await;
    given_rule(&router, case, "task-events-target", &target_arn).await;

    // First task: the policy is absent, so the whole run is withheld from the target.
    let first = run_then_stop_a_task(&router, case, "taskeventperm").await;
    collected_until_stopped(&router, case, "the control queue", &control_url, &first).await;
    let mut withheld = drain(&router, case, &target_url).await;
    withheld.extend(drain(&router, case, &target_url).await);
    assert!(
        withheld.is_empty(),
        "case {case:?}: nothing may reach a target queue with no policy, got {withheld:?}"
    );

    // The grant a client writes for an EventBridge target, installed now.
    set_queue_policy(
        &router,
        case,
        &target_url,
        &queue_policy(EVENTS_PRINCIPAL, SEND_MESSAGE, &target_arn, None),
    )
    .await;

    // Second task: the next matching envelope is delivered, and it is about THIS task.
    let second = run_then_stop_a_task(&router, case, "taskeventperm").await;
    let delivered =
        collected_until_stopped(&router, case, "the target queue", &target_url, &second).await;
    // Every envelope the grant admitted is about the SECOND task: the deliveries the absent
    // policy withheld from the first are not made good once the policy changes, and the wait
    // above returned because the second task's own `STOPPED` move arrived.
    for envelope in &delivered {
        assert_envelope(case, envelope, &second);
    }
}
