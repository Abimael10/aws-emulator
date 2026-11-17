// SPDX-License-Identifier: CC0-1.0

//! An AMAZON ECS CLUSTER as the target of an EventBridge rule: the Fargate task a matched
//! event launches, the override the target's rendered input becomes, and the two grants the
//! target's role has to carry for the run to be made at all.
//!
//! RULE: a rule target whose `Arn` names an Amazon ECS cluster is invoked by RUNNING the task
//! its `EcsParameters` describe in that cluster — "contains the Amazon ECS task definition
//! and task count to be used, if the event target is an Amazon ECS task"
//! (`Target$EcsParameters`) — acting as the role the target names, with every member of that
//! block a member of the run and with what the target asked to RECEIVE carried as the run's
//! `overrides` ("a list of container overrides in JSON format that specify the name of a
//! container in the specified task definition and the overrides it should receive",
//! `RunTask$overrides`). The run is `startedBy` the rule that made it, so a client tells the
//! tasks one rule launched from every other task of the cluster, and the launched task is an
//! ordinary task of Amazon ECS: it reports its launch type and its overrides through
//! `DescribeTasks`, and its container's output reaches the log group that container's
//! `awslogs` driver names — which is how a rendered event value, carried into the overridden
//! command, becomes a line a consumer of the log group reads.
//!
//! Because the run HANDS Amazon ECS the roles the task assumes, the target's role needs two
//! grants and not one: `ecs:RunTask`, performed on the TASK DEFINITION the run names (the
//! resource type Amazon ECS publishes for that action) with the cluster carried as the
//! `ecs:cluster` condition key, and `iam:PassRole` on each role the run passes, with
//! `iam:PassedToService` carrying `ecs-tasks.amazonaws.com` — "to pass a role to a service, a
//! user must have permission to pass the role to the service". A target whose role is missing
//! either launches NOTHING, and granting exactly the missing one makes the same target launch
//! a task for the next event.
//!
//! The suite lives beside the router because the rule spans five services' stores: the rule
//! and its targets are EventBridge's, the cluster, the revision and the tasks are Amazon
//! ECS's, the roles and the documents in force on them are IAM's, the log group and its
//! streams are CloudWatch Logs', and the subnets an `awsvpc` task's interface is created in
//! are Amazon EC2's. Every fact is created through the owning service's own API and every
//! value asserted is read back through `ListTasks`, `DescribeTasks` and `GetLogEvents`.
//!
//! The rows differ along what the rule generalises over: the FORM the target's input takes (a
//! constant `Input`, an `InputPath` selecting the document out of the event, an
//! `InputTransformer` rendering it from the event's own values), the command that input
//! overrides, how many tasks the parameter block asks for, and — for the authorization table
//! — which of the two actions a grant covers, which resource it names and which condition it
//! is scoped by.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

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

/// The Region every request below is signed for, and so the Region of every resource.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The cluster every target below runs its task in.
const CLUSTER: &str = "eventbridgeecstarget";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image that container starts from. Nothing is fetched from it; the reference only has
/// to name a registry the placement has a path to.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:alpine";

/// The log group the container's `awslogs` driver delivers to, and the stream prefix it
/// names: the driver's stream is `<prefix>/<container>/<task id>`.
const GROUP: &str = "/emulator/events/ecs-target";
const STREAM_PREFIX: &str = "task";

/// The network an `awsvpc` task's own elastic network interface is created in. The ids are
/// fixed because the requests below carry them literally.
const VPC: &str = "vpc-0a1b2c3d4e5f60001";
const SUBNET: &str = "subnet-0a1b2c3d4e5f60002";
const ZONE: &str = "us-west-2a";

/// The service principal EventBridge assumes a target's role as, and the one Amazon ECS
/// assumes a task's roles as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The actions the rows below grant and withhold.
const RUN_TASK: &str = "ecs:RunTask";
const PASS_ROLE: &str = "iam:PassRole";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// How long the container's program stays up after it has written its line, so a launched
/// task is one whose program is still running when it is read.
const STAY_UP_SECONDS: u32 = 600;

/// `Task$launchType` every target below asks for.
const FARGATE: &str = "FARGATE";

/// How often a poll re-reads the emulator, and how long it keeps trying. The budget is
/// generous: every transition a launched task makes is owed by the clock, and this suite runs
/// beside every other test of the workspace.
const POLL_INTERVAL: Duration = Duration::from_millis(60);
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// One emulator carrying the five services the rule spans, over ONE shared engine, as the
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsJson1_1` call, answered with its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
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
    let (status, parsed) = json_call(router, "events", &format!("AWSEvents.{operation}"), body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected events {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, answered with its HTTP status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(router, "logs", &format!("Logs_20140328.{operation}"), body).await
}

/// One awsQuery call to IAM that must succeed. Every role and every document in force on one
/// is created through IAM's OWN API, never written into its store.
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
    put_role_policy(router, case, name, "granted", document).await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Write one inline document onto a role.
async fn put_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    policy: &str,
    document: Value,
) {
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role.to_string()),
            ("PolicyName", policy.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
        case,
    )
    .await;
}

/// The execution role every revision below names: the one the container agent acts as, which
/// is what initialises the `awslogs` driver's stream and writes into it.
async fn given_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    given_role(
        router,
        case,
        "ecsTaskExecution",
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
    .await
}

/// Register one Fargate `awsvpc` revision of `family` whose single container writes the line
/// its command names and delivers it through the `awslogs` driver; answers the revision ARN.
async fn given_task_definition(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    execution_role: &str,
) -> String {
    let registered = ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "cpu": "256",
            "memory": "512",
            "networkMode": "awsvpc",
            "requiresCompatibilities": [FARGATE],
            "executionRoleArn": execution_role,
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
        }),
        case,
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no revision ARN in {registered}"))
        .to_string()
}

/// The `EcsParameters` a target carries: the revision to run, how many tasks, and the network
/// its interface is created in.
fn ecs_parameters(task_definition: &str, count: i64) -> Value {
    json!({
        "TaskDefinitionArn": task_definition,
        "TaskCount": count,
        "LaunchType": FARGATE,
        "NetworkConfiguration": {
            "awsvpcConfiguration": {
                "Subnets": [SUBNET],
                // The subnet routes through an internet gateway, which a task reaches only
                // with a public address of its own — the path the image fetch needs.
                "AssignPublicIp": "ENABLED",
            },
        },
    })
}

/// Write a rule matching `source` with the targets `targets`.
async fn given_rule(router: &MultiServiceRouter, case: &str, rule: &str, source: &str, targets: Value) {
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
        json!({ "Rule": rule, "Targets": targets }),
        case,
    )
    .await;
    assert_eq!(
        added["FailedEntryCount"], json!(0),
        "case {case:?}: every target must be stored, got {added}"
    );
}

/// Publish one event of `source` carrying `detail`.
async fn publish(router: &MultiServiceRouter, case: &str, source: &str, detail: Value) {
    let published = events(
        router,
        "PutEvents",
        json!({
            "Entries": [{
                "Source": source,
                "DetailType": "ecs-target-suite",
                "Detail": detail.to_string(),
            }],
        }),
        case,
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"], json!(0),
        "case {case:?}: the event must be accepted, got {published}"
    );
}

/// What `Task$startedBy` carries for a task the rule `rule` launched.
fn started_by(rule: &str) -> String {
    format!("events-rule/{rule}")
}

/// The task ARNs of the cluster that match `filters`, across both desired statuses — a task
/// whose launch failed is still a task that was PLACED, which is what these counts are about.
async fn placed_tasks(router: &MultiServiceRouter, case: &str, filters: Value) -> Vec<String> {
    let mut arns: Vec<String> = Vec::new();
    for desired in ["RUNNING", "STOPPED"] {
        let mut request = filters.clone();
        request["cluster"] = json!(CLUSTER);
        request["desiredStatus"] = json!(desired);
        let listed = ecs(router, "ListTasks", request, case).await;
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

/// One task of the cluster, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
        case,
    )
    .await;
    described["tasks"][0].clone()
}

/// The task id one task ARN names: its last segment.
fn task_id_of(task_arn: &str) -> String {
    task_arn.rsplit('/').next().unwrap_or(task_arn).to_string()
}

/// The stream the driver delivers one task's container to.
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

/// Poll `probe` until it answers `Some`, or the budget runs out. The wait is for the
/// CONDITION: a launched task's start is owed by the clock, not by a call.
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

/// The cluster, the network, the log group and the execution role every test below shares;
/// answers the execution role's ARN.
async fn seed(router: &MultiServiceRouter, engine: &engine::InMemoryEngine, case: &str) -> String {
    // An `awsvpc` task's interface is created in one of the subnets the placement names, and
    // Amazon ECS reads the path to the image's registry off the same records, so the network
    // is held before the first run.
    crate::ec2_network::register_network(engine, VPC, &[SUBNET], ZONE);
    crate::ec2_network::register_internet_route(engine, VPC);
    ecs(router, "CreateCluster", json!({ "clusterName": CLUSTER }), case).await;
    let (status, created) = logs(router, "CreateLogGroup", json!({ "logGroupName": GROUP })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group the driver delivers into must be created, got {status} with \
         {created}"
    );
    given_execution_role(router, case).await
}

/// A policy allowing both actions an ECS target's run performs, on every resource.
fn full_grant() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": [RUN_TASK], "Resource": "*" },
            { "Effect": "Allow", "Action": [PASS_ROLE], "Resource": "*" },
        ],
    })
}

/// One row of the input table: how the target asks for its overrides, and what the overridden
/// command then is.
struct InputRow {
    case: &'static str,
    rule: &'static str,
    source: &'static str,
    /// The value the event's `detail` carries, which the transformer row renders from.
    token: &'static str,
    /// How many tasks the parameter block asks for.
    count: i64,
    /// Which member of the target carries the override document.
    form: InputForm,
}

/// Which of the three members a target reshapes its input with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputForm {
    /// `Target$Input`: "valid JSON text passed to the target. In this case, nothing from the
    /// event itself is passed to the target."
    Constant,
    /// `Target$InputPath`: the part of the event the path selects — here a `detail` member
    /// that IS the override document.
    Path,
    /// `Target$InputTransformer`: the template with the event's own value rendered into it.
    Transformer,
}

const INPUT_ROWS: &[InputRow] = &[
    InputRow {
        case: "an InputTransformer rendering the event's own token into the command",
        rule: "ecs-target-transformed",
        source: "suite.transformer",
        token: "tok-a1",
        count: 1,
        form: InputForm::Transformer,
    },
    InputRow {
        case: "a constant Input carrying the whole override document",
        rule: "ecs-target-constant",
        source: "suite.constant",
        token: "tok-b2",
        count: 1,
        form: InputForm::Constant,
    },
    InputRow {
        case: "an InputPath selecting the override document out of the event",
        rule: "ecs-target-selected",
        source: "suite.selected",
        token: "tok-c3",
        count: 1,
        form: InputForm::Path,
    },
    InputRow {
        case: "a transformer whose parameter block asks for several tasks",
        rule: "ecs-target-counted",
        source: "suite.counted",
        token: "tok-d4",
        count: 3,
        form: InputForm::Transformer,
    },
];

/// The command one row's override sets on the container.
fn overridden_command(token: &str) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("echo {token}; sleep {STAY_UP_SECONDS}"),
    ]
}

/// The override document one row's target hands over.
fn override_document(token: &str) -> Value {
    json!({
        "containerOverrides": [{ "name": CONTAINER, "command": overridden_command(token) }],
    })
}

/// The target one row writes: its parameter block, its role, and the member that carries the
/// override document.
fn input_target(row: &InputRow, task_definition: &str, role: &str) -> Value {
    let mut target = json!({
        "Id": "ecs-task",
        "Arn": cluster_arn(),
        "RoleArn": role,
        "EcsParameters": ecs_parameters(task_definition, row.count),
    });
    match row.form {
        InputForm::Constant => {
            target["Input"] = json!(override_document(row.token).to_string());
        }
        InputForm::Path => {
            target["InputPath"] = json!("$.detail.overrides");
        }
        InputForm::Transformer => {
            // The placeholder stands INSIDE the template's own string, so it carries the
            // characters the selected value holds — which is what puts the event's token in
            // the overridden command.
            target["InputTransformer"] = json!({
                "InputPathsMap": { "token": "$.detail.token" },
                "InputTemplate": json!({
                    "containerOverrides": [{
                        "name": CONTAINER,
                        "command": ["sh", "-c", format!("echo <token>; sleep {STAY_UP_SECONDS}")],
                    }],
                })
                .to_string(),
            });
        }
    }
    target
}

/// The ARN a target names the cluster by.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{CLUSTER}")
}

/// The event one row publishes: the token the transformer renders, and the override document
/// the path row selects.
fn input_detail(row: &InputRow) -> Value {
    json!({
        "token": row.token,
        "overrides": override_document(row.token),
    })
}

#[tokio::test]
async fn an_ecs_target_runs_the_task_its_parameters_describe_with_the_rendered_override() {
    let (router, engine) = emulator();
    let execution_role = seed(&router, &engine, "the shared estate").await;
    let task_definition =
        given_task_definition(&router, "the shared estate", "ecstargetfamily", &execution_role)
            .await;
    let role = given_role(
        &router,
        "the shared estate",
        "eventsRunTask",
        EVENTS_PRINCIPAL,
        full_grant(),
    )
    .await;

    for row in INPUT_ROWS {
        let case = row.case;
        given_rule(
            &router,
            case,
            row.rule,
            row.source,
            json!([input_target(row, &task_definition, &role)]),
        )
        .await;
        publish(&router, case, row.source, input_detail(row)).await;

        // The run was made by the publication itself, so the tasks it placed are listed
        // without waiting: what the clock still owes them is their START.
        let wanted = usize::try_from(row.count).expect("a small task count");
        let arns = placed_tasks(&router, case, json!({ "startedBy": started_by(row.rule) })).await;
        assert_eq!(
            arns.len(),
            wanted,
            "case {case:?}: expected the rule to launch {wanted} task(s), got {arns:?} ({:?})",
            invocations(&engine)
        );

        for arn in &arns {
            let task = describe_task(&router, case, arn).await;
            assert_eq!(
                task["startedBy"].as_str(),
                Some(started_by(row.rule).as_str()),
                "case {case:?}: the task names the rule that started it, got {task}"
            );
            assert_eq!(
                task["launchType"].as_str(),
                Some(FARGATE),
                "case {case:?}: the task runs on the launch type the target named, got {task}"
            );
            assert_eq!(
                task["taskDefinitionArn"].as_str(),
                Some(task_definition.as_str()),
                "case {case:?}: the task runs the revision the target named, got {task}"
            );
            // The override the target asked for reached the run: the container the document
            // names carries the command it set.
            let overrides = task["overrides"]["containerOverrides"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let overridden = overrides
                .iter()
                .find(|entry| entry["name"].as_str() == Some(CONTAINER))
                .unwrap_or_else(|| panic!("case {case:?}: no override for {CONTAINER} in {task}"));
            assert_eq!(
                overridden["command"],
                json!(overridden_command(row.token)),
                "case {case:?}: expected the overridden command, got {overridden}"
            );

            // And the line that command writes reaches the log group the container's driver
            // names, carrying the token the input rendered.
            let stream = stream_of(&task_id_of(arn));
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
                "case {case:?}: expected the rendered token in {stream}, got {messages:?}"
            );
        }
    }
}

/// What one row of the authorization table gives the target's role, and whether the run is
/// then made.
struct GrantRow {
    case: &'static str,
    rule: &'static str,
    source: &'static str,
    role: &'static str,
    /// The service principal the role trusts, which is what EventBridge has to assume it.
    principal: &'static str,
    /// Whether the role's document grants the two actions, and how.
    grant: Grant,
    /// Whether a task is expected to be placed.
    launches: bool,
}

/// How one row's document is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Both actions, on every resource.
    Both,
    /// `ecs:RunTask` alone: the run passes the revision's execution role, which this role may
    /// not hand over.
    RunTaskOnly,
    /// `iam:PassRole` alone: nothing allows the run itself.
    PassRoleOnly,
    /// Both, with `ecs:RunTask` named on the TASK DEFINITION and scoped to the cluster the
    /// target names — the resource and the condition key Amazon ECS publishes for it.
    ScopedToThisCluster,
    /// The same, scoped to ANOTHER cluster.
    ScopedToAnotherCluster,
    /// Both, with `iam:PassRole` scoped to the service the roles are handed to.
    PassedToEcsTasks,
    /// The same, scoped to another service.
    PassedToAnotherService,
    /// Both allowed and `ecs:RunTask` explicitly denied.
    DeniedRunTask,
}

const GRANT_ROWS: &[GrantRow] = &[
    GrantRow {
        case: "a role allowing both actions on every resource",
        rule: "grant-both",
        source: "grant.both",
        role: "eventsGrantBoth",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::Both,
        launches: true,
    },
    GrantRow {
        case: "a role that may run the task but may not pass the roles it hands over",
        rule: "grant-run-only",
        source: "grant.runonly",
        role: "eventsGrantRunOnly",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::RunTaskOnly,
        launches: false,
    },
    GrantRow {
        case: "a role that may pass the roles but may not run the task",
        rule: "grant-pass-only",
        source: "grant.passonly",
        role: "eventsGrantPassOnly",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::PassRoleOnly,
        launches: false,
    },
    GrantRow {
        case: "a grant on the task definition scoped to the cluster the target names",
        rule: "grant-this-cluster",
        source: "grant.thiscluster",
        role: "eventsGrantThisCluster",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::ScopedToThisCluster,
        launches: true,
    },
    GrantRow {
        case: "the same grant scoped to another cluster",
        rule: "grant-other-cluster",
        source: "grant.othercluster",
        role: "eventsGrantOtherCluster",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::ScopedToAnotherCluster,
        launches: false,
    },
    GrantRow {
        case: "a hand-over scoped to the service the task's roles are passed to",
        rule: "grant-passed-ecs",
        source: "grant.passedecs",
        role: "eventsGrantPassedEcs",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::PassedToEcsTasks,
        launches: true,
    },
    GrantRow {
        case: "the same hand-over scoped to another service",
        rule: "grant-passed-other",
        source: "grant.passedother",
        role: "eventsGrantPassedOther",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::PassedToAnotherService,
        launches: false,
    },
    GrantRow {
        case: "a role whose document explicitly denies the run",
        rule: "grant-denied",
        source: "grant.denied",
        role: "eventsGrantDenied",
        principal: EVENTS_PRINCIPAL,
        grant: Grant::DeniedRunTask,
        launches: false,
    },
    GrantRow {
        case: "a role EventBridge cannot assume at all",
        rule: "grant-untrusted",
        source: "grant.untrusted",
        role: "eventsGrantUntrusted",
        principal: ECS_TASKS_PRINCIPAL,
        grant: Grant::Both,
        launches: false,
    },
];

/// One row's document.
fn grant_document(row: &GrantRow, task_definition: &str) -> Value {
    let run_task_everywhere = json!({ "Effect": "Allow", "Action": [RUN_TASK], "Resource": "*" });
    let pass_role_everywhere = json!({ "Effect": "Allow", "Action": [PASS_ROLE], "Resource": "*" });
    let statements = match row.grant {
        Grant::Both => vec![run_task_everywhere, pass_role_everywhere],
        Grant::RunTaskOnly => vec![run_task_everywhere],
        Grant::PassRoleOnly => vec![pass_role_everywhere],
        Grant::ScopedToThisCluster => vec![
            json!({
                "Effect": "Allow",
                "Action": [RUN_TASK],
                "Resource": task_definition,
                "Condition": { "ArnLike": { "ecs:cluster": cluster_arn() } },
            }),
            pass_role_everywhere,
        ],
        Grant::ScopedToAnotherCluster => vec![
            json!({
                "Effect": "Allow",
                "Action": [RUN_TASK],
                "Resource": task_definition,
                "Condition": {
                    "ArnLike": {
                        "ecs:cluster": format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/elsewhere"),
                    },
                },
            }),
            pass_role_everywhere,
        ],
        Grant::PassedToEcsTasks => vec![
            run_task_everywhere,
            json!({
                "Effect": "Allow",
                "Action": [PASS_ROLE],
                "Resource": "*",
                "Condition": { "StringEquals": { "iam:PassedToService": ECS_TASKS_PRINCIPAL } },
            }),
        ],
        Grant::PassedToAnotherService => vec![
            run_task_everywhere,
            json!({
                "Effect": "Allow",
                "Action": [PASS_ROLE],
                "Resource": "*",
                "Condition": {
                    "StringEquals": { "iam:PassedToService": "elasticmapreduce.amazonaws.com" },
                },
            }),
        ],
        Grant::DeniedRunTask => vec![
            run_task_everywhere,
            pass_role_everywhere,
            json!({ "Effect": "Deny", "Action": [RUN_TASK], "Resource": "*" }),
        ],
    };
    json!({ "Version": "2012-10-17", "Statement": statements })
}

#[tokio::test]
async fn an_ecs_targets_run_needs_both_the_run_and_the_hand_over_its_role_performs() {
    let (router, engine) = emulator();
    let execution_role = seed(&router, &engine, "the shared estate").await;
    let task_definition =
        given_task_definition(&router, "the shared estate", "ecsgrantfamily", &execution_role)
            .await;

    for row in GRANT_ROWS {
        let case = row.case;
        let role = given_role(
            &router,
            case,
            row.role,
            row.principal,
            grant_document(row, &task_definition),
        )
        .await;
        given_rule(
            &router,
            case,
            row.rule,
            row.source,
            json!([{
                "Id": "ecs-task",
                "Arn": cluster_arn(),
                "RoleArn": role,
                "EcsParameters": ecs_parameters(&task_definition, 1),
            }]),
        )
        .await;
        publish(&router, case, row.source, json!({ "token": "grant" })).await;

        let arns = placed_tasks(&router, case, json!({ "startedBy": started_by(row.rule) })).await;
        let expected = usize::from(row.launches);
        assert_eq!(
            arns.len(),
            expected,
            "case {case:?}: expected {expected} task(s) to be launched, got {arns:?} ({:?})",
            invocations(&engine)
        );
    }
}

/// The two targets of the repair test, each on its own revision so the tasks one launched are
/// told from the other's.
const CONTROL_FAMILY: &str = "ecscontrolfamily";
const GATED_FAMILY: &str = "ecsgatedfamily";

#[tokio::test]
async fn granting_pass_role_makes_the_gated_target_launch_what_the_control_already_launches() {
    let case = "two targets of one rule differing only in iam:PassRole";
    let (router, engine) = emulator();
    let execution_role = seed(&router, &engine, case).await;
    let control_definition =
        given_task_definition(&router, case, CONTROL_FAMILY, &execution_role).await;
    let gated_definition = given_task_definition(&router, case, GATED_FAMILY, &execution_role).await;

    // The two roles differ in ONE statement: both may run the task, and only the control's may
    // hand Amazon ECS the roles the run passes.
    let control_role = given_role(&router, case, "eventsControl", EVENTS_PRINCIPAL, full_grant()).await;
    let gated_role = given_role(
        &router,
        case,
        "eventsGated",
        EVENTS_PRINCIPAL,
        json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Allow", "Action": [RUN_TASK], "Resource": "*" },
            ],
        }),
    )
    .await;

    let source = "repair.passrole";
    given_rule(
        &router,
        case,
        "ecs-pass-role-repair",
        source,
        json!([
            {
                "Id": "control",
                "Arn": cluster_arn(),
                "RoleArn": control_role,
                "EcsParameters": ecs_parameters(&control_definition, 1),
            },
            {
                "Id": "gated",
                "Arn": cluster_arn(),
                "RoleArn": gated_role,
                "EcsParameters": ecs_parameters(&gated_definition, 1),
            },
        ]),
    )
    .await;

    // (how many events have been published, the control's tasks, the gated target's tasks)
    publish(&router, case, source, json!({ "token": "before-1" })).await;
    assert_launched(&router, &engine, case, "before the repair, event 1", CONTROL_FAMILY, 1).await;
    assert_launched(&router, &engine, case, "before the repair, event 1", GATED_FAMILY, 0).await;

    // A second event changes nothing: the gate is a property of the role, not of the event.
    publish(&router, case, source, json!({ "token": "before-2" })).await;
    assert_launched(&router, &engine, case, "before the repair, event 2", CONTROL_FAMILY, 2).await;
    assert_launched(&router, &engine, case, "before the repair, event 2", GATED_FAMILY, 0).await;

    // Grant EXACTLY the missing action, and nothing else.
    put_role_policy(
        &router,
        case,
        "eventsGated",
        "repair",
        json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": [PASS_ROLE], "Resource": "*" }],
        }),
    )
    .await;

    publish(&router, case, source, json!({ "token": "after" })).await;
    assert_launched(&router, &engine, case, "after the repair", CONTROL_FAMILY, 3).await;
    assert_launched(&router, &engine, case, "after the repair", GATED_FAMILY, 1).await;
}

/// Assert that `family` has exactly `expected` tasks placed in the cluster.
async fn assert_launched(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
    stage: &str,
    family: &str,
    expected: usize,
) {
    let arns = placed_tasks(router, case, json!({ "family": family })).await;
    assert_eq!(
        arns.len(),
        expected,
        "case {case:?} ({stage}): expected {family} to hold {expected} task(s), got {arns:?} \
         ({:?})",
        invocations(engine)
    );
}
