// SPDX-License-Identifier: CC0-1.0

//! Which ROLES an EventBridge rule's Amazon ECS target hands Amazon ECS when a matched event
//! runs a task, and the grant each one of them needs for a task to be launched at all.
//!
//! RULE: running the task an ECS target describes HANDS Amazon ECS every role that task acts
//! under — the task role its containers assume and the execution role its agent acts as, taken
//! from the run's own overrides where it names one and from the REGISTERED revision otherwise —
//! and the role the target is invoked as must be allowed `iam:PassRole` on EACH of those roles,
//! judged with the role's own ARN as the resource ("to pass a role to a service, a user must
//! have permission to pass the role to the service"). A run every one of them is granted on
//! places the task, whose container's output reaches the stream its `awslogs` configuration
//! names — which is how a value the rule's input carried into the overridden command becomes a
//! log event a consumer reads. A run one of them is withheld on places NOTHING and parks the
//! event on the queue the target's `DeadLetterConfig` names, under the condition EventBridge
//! reports a delivery no permission allowed and a sentence naming the action and the role that
//! was not passed.
//!
//! The rows differ along what the rule generalises over: WHERE each handed-over role comes from
//! (the revision's `executionRoleArn`, the revision's `taskRoleArn`, a `taskRoleArn` the
//! target's own input overrides) and WHICH role ARNs the grant's `Resource` names (all of them,
//! some of them, another role's, none at all). The expectation is derived from the rule rather
//! than written per row: a row whose grant names every role its run hands over launches, and
//! every other row launches nothing and has a refusal naming a role its grant withheld — one of
//! them where it withholds several, because which of several AWS reports is not documented.
//!
//! The sibling suite `eventbridge_ecs_task_target.rs` pins the FORM the target's input takes and
//! the two actions an ECS target's invocation performs; this one pins the roles the second of
//! those actions is performed on.
//!
//! The suite lives beside the router because the rule spans six services' stores: the rule, its
//! target and the dead letter are EventBridge's, the cluster, the revisions and the tasks are
//! Amazon ECS's, the roles and the documents in force on them are IAM's, the log group and its
//! streams are CloudWatch Logs', the dead-letter queue is Amazon SQS's, and the subnet an
//! `awsvpc` task's interface is created in is Amazon EC2's. Every fact is created through the
//! owning service's own API — bar the VPC, its subnet and its route off the VPC, which the
//! suites that need a LITERAL subnet id hold in Amazon EC2's own store through the shared
//! `ec2_network` helper, because `CreateSubnet` mints an id of its own — and every value
//! asserted is read back through `ListTasks`, `DescribeTasks`, `GetLogEvents` and
//! `ReceiveMessage`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region every request below is signed for, and so the Region of every resource.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The cluster every target below runs its task in.
const CLUSTER: &str = "eventbridgepassedroles";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image that container starts from. Nothing is fetched from it; the reference only has to
/// name a registry the placement has a path to.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:alpine";

/// The log group the container's `awslogs` driver delivers to, and the stream prefix it names:
/// the driver's stream is `<prefix>/<container>/<task id>`.
const GROUP: &str = "/emulator/events/ecs-passed-roles";
const STREAM_PREFIX: &str = "task";

/// The network an `awsvpc` task's own elastic network interface is created in. The ids are
/// fixed because the requests below carry them literally.
const VPC: &str = "vpc-0a1b2c3d4e5f61001";
const SUBNET: &str = "subnet-0a1b2c3d4e5f61002";
const ZONE: &str = "us-west-2a";

/// The service principal EventBridge assumes a target's role as, and the one Amazon ECS assumes
/// a task's roles as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The actions the documents below grant and withhold.
const RUN_TASK: &str = "ecs:RunTask";
const PASS_ROLE: &str = "iam:PassRole";
const ASSUME_ROLE: &str = "sts:AssumeRole";
const SEND_MESSAGE: &str = "sqs:SendMessage";
const GET_CALLER_IDENTITY: &str = "sts:GetCallerIdentity";

/// The condition EventBridge reports on a dead letter for a delivery no permission allowed,
/// which is what tells such a refusal from every other reason a delivery failed.
const NO_PERMISSIONS: &str = "NO_PERMISSIONS";

/// `Task$launchType` every target below asks for.
const FARGATE: &str = "FARGATE";

/// How long the container's program stays up after it has written its line, so a launched task
/// is one whose program is still running when it is read.
const STAY_UP_SECONDS: u32 = 600;

/// How often a poll re-reads the emulator, and how long it keeps trying. The budget is
/// generous: a launched task's start is owed by the clock, and this suite runs beside every
/// other test of the workspace.
const POLL_INTERVAL: Duration = Duration::from_millis(60);
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// The task-definition families the rows pin: one declaring only an execution role, one
/// declaring a task role beside it.
const EXECUTION_ONLY_FAMILY: &str = "ecspassedexeconly";
const BOTH_ROLES_FAMILY: &str = "ecspassedbothroles";

/// One role of this account, named by the part it plays in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// `TaskDefinition$executionRoleArn`: "the task execution role that grants the Amazon ECS
    /// container agent permission to make Amazon Web Services API calls on your behalf", which
    /// every revision below declares.
    Execution,
    /// `TaskDefinition$taskRoleArn` of the revision: the role the task's containers assume.
    DeclaredTask,
    /// `TaskOverride$taskRoleArn`: the task role the RUN names, which the target's own input
    /// carries and which replaces the one the revision declares.
    OverriddenTask,
    /// A role of this account no run below hands over, which a mis-scoped grant names instead.
    Unrelated,
}

impl Role {
    /// The name IAM holds the role under.
    fn name(self) -> &'static str {
        match self {
            Self::Execution => "ecsPassedExecution",
            Self::DeclaredTask => "ecsPassedDeclaredTask",
            Self::OverriddenTask => "ecsPassedOverriddenTask",
            Self::Unrelated => "ecsPassedUnrelated",
        }
    }

    /// The ARN a policy statement about the role names, and the resource `iam:PassRole` on it
    /// is judged against.
    fn arn(self) -> String {
        format!("arn:aws:iam::{ACCOUNT}:role/{}", self.name())
    }
}

/// Which revision a row's target pins, which is what decides the roles the run takes off the
/// REGISTERED revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Definition {
    /// A revision declaring an execution role and no task role.
    ExecutionRoleOnly,
    /// A revision declaring a task role beside its execution role.
    BothRoles,
}

impl Definition {
    /// The family the revision belongs to.
    fn family(self) -> &'static str {
        match self {
            Self::ExecutionRoleOnly => EXECUTION_ONLY_FAMILY,
            Self::BothRoles => BOTH_ROLES_FAMILY,
        }
    }
}

/// One row: the revision its target pins, whether its input hands over a task role of its own,
/// and which roles the target's own role may pass.
struct Row {
    case: &'static str,
    rule: &'static str,
    source: &'static str,
    /// The IAM role the target is invoked as, which the row's own document is written onto.
    target_role: &'static str,
    definition: Definition,
    /// Whether the target's input overrides the task role (`TaskOverride$taskRoleArn`).
    overrides_task_role: bool,
    /// Which roles the target role's `iam:PassRole` statement names as its `Resource`.
    granted: &'static [Role],
    /// The value the event carries into the overridden command, which the container writes.
    token: &'static str,
    /// Whether a task is expected to be placed, which the rule decides: a run is made only
    /// when every role it hands over is one the target's role may pass.
    launches: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the execution role the revision declares, granted on that role's own ARN",
        rule: "passed-exec-granted",
        source: "passed.execgranted",
        target_role: "eventsPassedExecGranted",
        definition: Definition::ExecutionRoleOnly,
        overrides_task_role: false,
        granted: &[Role::Execution],
        token: "tok-exec-granted",
        launches: true,
    },
    Row {
        case: "the same revision, with the hand-over granted on another role's ARN",
        rule: "passed-exec-elsewhere",
        source: "passed.execelsewhere",
        target_role: "eventsPassedExecElsewhere",
        definition: Definition::ExecutionRoleOnly,
        overrides_task_role: false,
        granted: &[Role::Unrelated],
        token: "tok-exec-elsewhere",
        launches: false,
    },
    Row {
        case: "both roles the revision declares, each granted on its own ARN",
        rule: "passed-both-granted",
        source: "passed.bothgranted",
        target_role: "eventsPassedBothGranted",
        definition: Definition::BothRoles,
        overrides_task_role: false,
        granted: &[Role::Execution, Role::DeclaredTask],
        token: "tok-both-granted",
        launches: true,
    },
    Row {
        case: "both roles the revision declares, with only the execution role granted",
        rule: "passed-both-partial",
        source: "passed.bothpartial",
        target_role: "eventsPassedBothPartial",
        definition: Definition::BothRoles,
        overrides_task_role: false,
        granted: &[Role::Execution],
        token: "tok-both-partial",
        launches: false,
    },
    Row {
        case: "a task role the target's input hands over, granted beside the execution role",
        rule: "passed-override-granted",
        source: "passed.overridegranted",
        target_role: "eventsPassedOverrideGranted",
        definition: Definition::ExecutionRoleOnly,
        overrides_task_role: true,
        granted: &[Role::Execution, Role::OverriddenTask],
        token: "tok-override-granted",
        launches: true,
    },
    Row {
        case: "the same input, with only the revision's own role granted",
        rule: "passed-override-partial",
        source: "passed.overridepartial",
        target_role: "eventsPassedOverridePartial",
        definition: Definition::ExecutionRoleOnly,
        overrides_task_role: true,
        granted: &[Role::Execution],
        token: "tok-override-partial",
        launches: false,
    },
    // Two roles withheld at once, so a refusal has to be about a role the grant really
    // withheld rather than about whichever role the run happened to hand over first.
    Row {
        case: "both roles the revision declares, with no hand-over granted at all",
        rule: "passed-both-withheld",
        source: "passed.bothwithheld",
        target_role: "eventsPassedBothWithheld",
        definition: Definition::BothRoles,
        overrides_task_role: false,
        granted: &[],
        token: "tok-both-withheld",
        launches: false,
    },
];

/// The roles the run one row makes HANDS Amazon ECS: the task role its containers assume — the
/// one the run's own overrides name where it names one, and the one the revision declares
/// otherwise — and the execution role its agent acts as.
fn handed_over(row: &Row) -> Vec<Role> {
    let mut handed: Vec<Role> = Vec::new();
    if row.overrides_task_role {
        handed.push(Role::OverriddenTask);
    } else if row.definition == Definition::BothRoles {
        handed.push(Role::DeclaredTask);
    }
    handed.push(Role::Execution);
    handed
}

/// The roles of the hand-over the row's grant does NOT name, which are the roles the refusal is
/// about — empty for a row whose grant names every role its run hands over, which is the row
/// that launches.
///
/// A refusal names ONE withheld role: a run the policies refuse places nothing, so there is one
/// role it was refused over. WHICH of several AWS reports is not documented, so a row withholding
/// more than one asks only that the sentence name one of them.
fn withheld(row: &Row) -> Vec<Role> {
    handed_over(row)
        .into_iter()
        .filter(|role| !row.granted.contains(role))
        .collect()
}

/// One emulator carrying the six services the rule spans, over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// What EventBridge RECORDED about each handover it made, which is what a failed assertion
/// below names: a run that was refused says so here, where a count of launched tasks cannot.
fn invocations(engine: &engine::InMemoryEngine) -> Vec<String> {
    engine
        .state::<service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .invocations
        .iter()
        .map(|invocation| {
            format!(
                "{}/{} -> {:?}",
                invocation.rule, invocation.target_id, invocation.outcome
            )
        })
        .collect()
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One JSON-RPC call, answered with its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must succeed, answered with its body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = json_call(
        router,
        "ecs",
        "application/x-amz-json-1.1",
        &format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call that must succeed, answered with its body.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = json_call(
        router,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected events {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, answered with its HTTP status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(
        router,
        "logs",
        "application/x-amz-json-1.1",
        &format!("Logs_20140328.{operation}"),
        body,
    )
    .await
}

/// One Amazon SQS call that must succeed, answered with its body.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = json_call(
        router,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected sqs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM that must succeed. Every role and every document in force on one is
/// created through IAM's OWN API, never written into its store.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, String)], case: &str) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
}

/// A trust policy letting `principal` assume the role.
fn trust(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [principal] },
            "Action": [ASSUME_ROLE],
        }],
    })
    .to_string()
}

/// Create a role trusting `principal` with the inline document `document`, and answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
    document: Value,
) -> String {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust(principal)),
        ],
        case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyName", "granted".to_string()),
            ("PolicyDocument", document.to_string()),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Create the execution role every revision below names: the one Amazon ECS acts as, which is
/// what initialises the `awslogs` driver's stream and writes into it.
async fn given_execution_role(router: &MultiServiceRouter, case: &str) {
    given_role(
        router,
        case,
        Role::Execution.name(),
        ECS_TASKS_PRINCIPAL,
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            }],
        }),
    )
    .await;
}

/// Create one role a task's CONTAINERS assume. The containers below make no call of their own,
/// so the document grants only an identity read; what matters about such a role here is that
/// Amazon ECS may assume it and that the run has to be allowed to hand it over.
async fn given_task_role(router: &MultiServiceRouter, case: &str, role: Role) {
    given_role(
        router,
        case,
        role.name(),
        ECS_TASKS_PRINCIPAL,
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": [GET_CALLER_IDENTITY],
                "Resource": "*",
            }],
        }),
    )
    .await;
}

/// Register one Fargate `awsvpc` revision of `definition`'s family whose single container
/// delivers its output through the `awslogs` driver, and answer the revision's ARN.
async fn given_task_definition(
    router: &MultiServiceRouter,
    case: &str,
    definition: Definition,
) -> String {
    let mut request = json!({
        "family": definition.family(),
        "cpu": "256",
        "memory": "512",
        "networkMode": "awsvpc",
        "requiresCompatibilities": [FARGATE],
        "executionRoleArn": Role::Execution.arn(),
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "essential": true,
            "command": ["sh", "-c", format!("echo registered-default; sleep {STAY_UP_SECONDS}")],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": GROUP,
                    "awslogs-region": REGION,
                    "awslogs-stream-prefix": STREAM_PREFIX,
                },
            },
        }],
    });
    if definition == Definition::BothRoles {
        request["taskRoleArn"] = json!(Role::DeclaredTask.arn());
    }
    let registered = ecs(router, "RegisterTaskDefinition", request, case).await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no revision ARN in {registered}"))
        .to_string()
}

/// A dead-letter queue whose own policy admits the dead-letter write EventBridge makes as its
/// own service principal ("for … Amazon SQS queues, EventBridge relies on resource-based
/// policies"), so what reaches it is decided by the delivery and not by the queue. Answers its
/// url, and its ARN as the queue itself reports it.
async fn given_dead_letter_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(router, "CreateQueue", json!({ "QueueName": name }), case).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
        case,
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": SEND_MESSAGE,
            "Resource": arn,
        }],
    });
    sqs(
        router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
        case,
    )
    .await;
    (url, arn)
}

/// The command one row's override sets on the container: it writes the row's token and stays
/// up, so a launched task is one whose program is still running when it is read.
fn overridden_command(token: &str) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("echo {token}; sleep {STAY_UP_SECONDS}"),
    ]
}

/// The override document one row's target hands over: the command its container runs, and the
/// task role the run names where the row overrides it.
fn override_document(row: &Row) -> Value {
    let mut document = json!({
        "containerOverrides": [{ "name": CONTAINER, "command": overridden_command(row.token) }],
    });
    if row.overrides_task_role {
        document["taskRoleArn"] = json!(Role::OverriddenTask.arn());
    }
    document
}

/// The document in force on one row's target role: the run itself on every resource — so the
/// row's expectation is about the HAND-OVER alone — and `iam:PassRole` on exactly the role ARNs
/// the row names.
fn grant_document(row: &Row) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": [RUN_TASK],
        "Resource": "*",
    })];
    if !row.granted.is_empty() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": [PASS_ROLE],
            "Resource": row.granted.iter().map(|role| role.arn()).collect::<Vec<_>>(),
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The ARN a target names the cluster by.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{CLUSTER}")
}

/// One row's target: the revision it pins, the network its interface is created in, the
/// override document its input carries, the role it is invoked as and the queue its
/// undeliverable event is parked on.
fn target(row: &Row, task_definition: &str, role_arn: &str, dead_letter_arn: &str) -> Value {
    json!({
        "Id": "ecs-task",
        "Arn": cluster_arn(),
        "RoleArn": role_arn,
        "Input": override_document(row).to_string(),
        "EcsParameters": {
            "TaskDefinitionArn": task_definition,
            "TaskCount": 1,
            "LaunchType": FARGATE,
            "NetworkConfiguration": {
                "awsvpcConfiguration": {
                    "Subnets": [SUBNET],
                    // The subnet routes through an internet gateway, which a task reaches only
                    // with a public address of its own — the path the image fetch needs.
                    "AssignPublicIp": "ENABLED",
                },
            },
        },
        // A run no permission allowed is refused permanently, so the parked event is the first
        // attempt's and the row waits for no retry allowance to run out.
        "RetryPolicy": { "MaximumRetryAttempts": 0 },
        "DeadLetterConfig": { "Arn": dead_letter_arn },
    })
}

/// Write a rule matching `source` with the one target `target`.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    rule: &str,
    source: &str,
    target: Value,
) {
    events(
        router,
        "PutRule",
        json!({
            "Name": rule,
            "EventPattern": json!({ "source": [source] }).to_string(),
            "State": "ENABLED",
        }),
        case,
    )
    .await;
    let added = events(
        router,
        "PutTargets",
        json!({ "Rule": rule, "Targets": [target] }),
        case,
    )
    .await;
    assert_eq!(
        added["FailedEntryCount"],
        json!(0),
        "case {case:?}: the target must be stored, got {added}"
    );
}

/// Publish one event of `source` carrying the row's token.
async fn publish(router: &MultiServiceRouter, case: &str, source: &str, token: &str) {
    let published = events(
        router,
        "PutEvents",
        json!({
            "Entries": [{
                "Source": source,
                "DetailType": "ecs-passed-roles-suite",
                "Detail": json!({ "token": token }).to_string(),
            }],
        }),
        case,
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: the event must be accepted, got {published}"
    );
}

/// What `Task$startedBy` carries for a task the rule `rule` launched, which is how a client
/// tells the tasks one rule launched from every other task of the cluster.
fn started_by(rule: &str) -> String {
    format!("events-rule/{rule}")
}

/// The task ARNs the rule `rule` placed in the cluster, across both desired statuses — a task
/// whose launch failed is still a task that was PLACED, which is what these counts are about.
async fn placed_tasks(router: &MultiServiceRouter, case: &str, rule: &str) -> Vec<String> {
    let mut arns: Vec<String> = Vec::new();
    for desired in ["RUNNING", "STOPPED"] {
        let listed = ecs(
            router,
            "ListTasks",
            json!({
                "cluster": CLUSTER,
                "startedBy": started_by(rule),
                "desiredStatus": desired,
            }),
            case,
        )
        .await;
        arns.extend(
            listed["taskArns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|arn| arn.as_str().map(str::to_string)),
        );
    }
    arns
}

/// The task id one task ARN names: its last segment.
fn task_id_of(task_arn: &str) -> String {
    task_arn.rsplit('/').next().unwrap_or(task_arn).to_string()
}

/// The stream the driver delivers one task's container to: `<prefix>/<container>/<task id>`,
/// the naming the driver's own `awslogs-stream-prefix` documentation states.
fn stream_of(task_id: &str) -> String {
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// The messages one stream of the group holds, oldest first.
async fn delivered(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (_, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What ONE read of the queue at `url` answers with, left ON the queue for the row's own later
/// reads: a dead letter is never consumed by the look that found it.
async fn on_queue(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answer = sqs(
        router,
        "ReceiveMessage",
        json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "VisibilityTimeout": 0,
            "MessageAttributeNames": ["All"],
        }),
        case,
    )
    .await;
    answer["Messages"].as_array().cloned().unwrap_or_default()
}

/// The messages the queue at `url` holds, polled until there is at least one of them or the
/// budget passes. The wait is for the CONDITION — an undeliverable event reaching the queue —
/// never for a duration.
async fn parked(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = on_queue(router, case, url).await;
        if !held.is_empty() || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The value of the message attribute `name` of a dead letter, or `None` when it carries none.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(str::to_string)
}

/// Poll `probe` until it answers `Some`, or the budget runs out. The wait is for the CONDITION:
/// a launched task's start is owed by the clock, not by a call.
async fn poll_until<T, F, Fut>(probe: F, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        if let Some(answer) = probe().await {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{description}: not observed within {POLL_BUDGET:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn an_ecs_targets_run_needs_pass_role_on_every_role_the_launch_hands_over() {
    let (router, engine) = emulator();
    let shared = "the shared estate";

    // An `awsvpc` task's interface is created in one of the subnets the placement names, and
    // Amazon ECS reads the path to the image's registry off the same records, so the network is
    // held before the first run.
    crate::ec2_network::register_network(&engine, VPC, &[SUBNET], ZONE);
    crate::ec2_network::register_internet_route(&engine, VPC);
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        shared,
    )
    .await;
    let (status, created) = logs(&router, "CreateLogGroup", json!({ "logGroupName": GROUP })).await;
    assert_eq!(
        status, 200,
        "case {shared:?}: the group the driver delivers into must be created, got {status} with \
         {created}"
    );
    given_execution_role(&router, shared).await;
    for role in [Role::DeclaredTask, Role::OverriddenTask, Role::Unrelated] {
        given_task_role(&router, shared, role).await;
    }
    let execution_only = given_task_definition(&router, shared, Definition::ExecutionRoleOnly).await;
    let both_roles = given_task_definition(&router, shared, Definition::BothRoles).await;

    for row in ROWS {
        let case = row.case;
        let withheld = withheld(row);
        assert_eq!(
            withheld.is_empty(),
            row.launches,
            "case {case:?}: the row states that it launches {} while the roles its run hands \
             over are {:?} and its grant names {:?}",
            row.launches,
            handed_over(row),
            row.granted
        );
        let task_definition = match row.definition {
            Definition::ExecutionRoleOnly => &execution_only,
            Definition::BothRoles => &both_roles,
        };
        let (queue_url, queue_arn) = given_dead_letter_queue(&router, case, row.rule).await;
        let role_arn = given_role(
            &router,
            case,
            row.target_role,
            EVENTS_PRINCIPAL,
            grant_document(row),
        )
        .await;
        given_rule(
            &router,
            case,
            row.rule,
            row.source,
            target(row, task_definition, &role_arn, &queue_arn),
        )
        .await;
        publish(&router, case, row.source, row.token).await;

        if withheld.is_empty() {
            // Every role the run hands over is one the target's role may pass, so the rule placed
            // the task its parameters asked for. The look is a poll on the CONDITION — the
            // publication makes the run itself, so the first read already sees it — and the task
            // runs the revision the target pinned, with the line its overridden command writes
            // reaching the stream its own `awslogs` configuration names.
            let arns = poll_until(
                || async {
                    let arns = placed_tasks(&router, case, row.rule).await;
                    (!arns.is_empty()).then_some(arns)
                },
                &format!("case {case:?}: the task the rule launched"),
            )
            .await;
            assert_eq!(
                arns.len(),
                1,
                "case {case:?}: expected the rule to launch one task, got {arns:?} ({:?})",
                invocations(&engine)
            );
            let stream = stream_of(&task_id_of(&arns[0]));
            let messages = poll_until(
                || async {
                    let messages = delivered(&router, &stream).await;
                    (!messages.is_empty()).then_some(messages)
                },
                &format!("case {case:?}: the overridden command's line in {stream}"),
            )
            .await;
            assert_eq!(
                messages,
                vec![row.token.to_string()],
                "case {case:?}: expected the token the event carried in {stream}, got {messages:?}"
            );
            let described = ecs(
                &router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": [&arns[0]] }),
                case,
            )
            .await;
            assert_eq!(
                described["tasks"][0]["taskDefinitionArn"].as_str(),
                Some(task_definition.as_str()),
                "case {case:?}: the task runs the revision the target pinned, got {described}"
            );
            // And nothing was parked: the queue is read after the run's own publication was
            // answered and after the task's line had been delivered, so a dead letter this
            // delivery had made would already be on it.
            let held = on_queue(&router, case, &queue_url).await;
            assert!(
                held.is_empty(),
                "case {case:?}: a run the policies permit parks nothing, got {held:?}"
            );
        } else {
            // A role the run hands over is withheld: the event is on the queue the target names,
            // under the condition EventBridge reports for a delivery no permission allowed and a
            // sentence naming the action and one of the roles that was not passed...
            let held = parked(&router, case, &queue_url).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected the one dead letter the refused run produced, got \
                 {held:?} ({:?})",
                invocations(&engine)
            );
            assert_eq!(
                attribute(&held[0], "ERROR_CODE").as_deref(),
                Some(NO_PERMISSIONS),
                "case {case:?}: expected the {NO_PERMISSIONS:?} condition, got {}",
                held[0]
            );
            let message = attribute(&held[0], "ERROR_MESSAGE").unwrap_or_default();
            assert!(
                message.contains(PASS_ROLE)
                    && withheld.iter().any(|role| message.contains(&role.arn())),
                "case {case:?}: expected the parked refusal to name {PASS_ROLE} on one of {:?}, \
                 got {message:?}",
                withheld.iter().map(|role| role.arn()).collect::<Vec<_>>()
            );
            assert!(
                row.granted
                    .iter()
                    .all(|granted| !message.contains(&granted.arn())),
                "case {case:?}: the refusal is about a role the grant withheld, not one it \
                 names, got {message:?}"
            );
            assert_eq!(
                attribute(&held[0], "TARGET_ARN").as_deref(),
                Some(cluster_arn().as_str()),
                "case {case:?}: expected the target's own ARN on the dead letter, got {}",
                held[0]
            );
            // ...and the refusal placed nothing: read again now the refusal is known to have
            // happened, so an absence asserted before it would not pass for a delivery that had
            // simply not got there yet.
            let arns = placed_tasks(&router, case, row.rule).await;
            assert!(
                arns.is_empty(),
                "case {case:?}: a run the policies refuse places no task, got {arns:?} ({:?})",
                invocations(&engine)
            );
        }
    }
}
