// SPDX-License-Identifier: CC0-1.0

//! The RETENTION a client set on a log group, and what an Amazon ECS task's `awslogs` delivery
//! into that group does to it.
//!
//! RULE under test: **a log group's retention is a setting of the GROUP — CloudWatch Logs'
//! `PutRetentionPolicy` "sets the retention of the specified log group", and every later read of
//! that group reports the `retentionInDays` it was set to until `DeleteRetentionPolicy` takes it
//! away. Ingestion into the group is not a change to that setting: a stream created in it and
//! events put into it leave the retention exactly where the last policy call put it, and a
//! policy call over a group that already holds events replaces the setting without touching the
//! events. The `awslogs` log driver of a started container is one such ingestion — the driver
//! "sends container logs to Amazon CloudWatch Logs", so each line the container's program writes
//! becomes one event of the group its `awslogs-group` option names, readable there with
//! `FilterLogEvents`, in the stream the driver's own `awslogs-stream-prefix` documentation names
//! (`<prefix>/<container name>/<task id>`).**
//!
//! That is the whole of the observability contract a client leans on when CloudWatch Logs is the
//! only evidence a container ran: the marker line the container wrote has to ARRIVE, so a poll
//! for it terminates, and the retention the client configured before the task ran has to still be
//! what the group reports afterwards, so a retention assertion is not undone by the first
//! delivery.
//!
//! The rows differ along what the rule generalises over rather than restating one template:
//!
//! * WHEN the retention is set relative to the ingestion — before the task is launched, and only
//!   after its events have arrived (a group that reports no retention at all until then), so the
//!   rule is exercised in both orders;
//! * the RETENTION itself — the shortest and the longest span the owning service publishes, and
//!   two from inside that published list — and whether the policy that follows the ingestion
//!   raises or lowers it;
//! * HOW MANY lines the container's program writes, so a delivery that clears or re-creates the
//!   group on any line but the first fails its row.
//!
//! No expected value here is a literal from a recording: the retention values are the owning
//! service's own published list (quoted on [`PUBLISHED_RETENTIONS`]), every marker is built by
//! the row that writes it, and the stream name is spelled out from the driver's documented naming
//! rather than imported, so a wrong naming in the implementation cannot make this suite pass.
//!
//! Three crates have to agree for the rule to hold, which is why this suite lives here: Amazon
//! ECS owns the cluster, the revision and the container whose program writes, CloudWatch Logs
//! owns the group, its retention and its events, and IAM holds the execution role the driver is
//! initialised and delivers as — over the network Amazon EC2 owns.
//!
//! Every clock-owed move — the task's start and the delivery of what its container wrote — is
//! POLLED for until it holds, never slept for, and the deadline is only here so a failure is a
//! failure rather than a hang: nothing asserts how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefixes the two awsJson services name an operation under.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The API versions the two Query services' bodies carry.
const EC2_VERSION: &str = "2016-11-15";
const IAM_VERSION: &str = "2010-05-08";

/// The spans Amazon CloudWatch Logs publishes for a log group's retention: "Possible values are:
/// 1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922,
/// 3288, and 3653" (`smithy_query(query="operation",operation="PutRetentionPolicy")`, that input's
/// `retentionInDays`). The rows below take their values from this list — its two ends and two
/// entries inside it.
const PUBLISHED_RETENTIONS: [i64; 22] = [
    1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922,
    3288, 3653,
];

/// The cluster every task below is launched into.
const CLUSTER: &str = "retention-delivery";

/// The one container of every revision below, whose name is the middle segment of the stream its
/// driver delivers to.
const CONTAINER: &str = "app";

/// The image every container names. Nothing is asserted about it: what runs is the `command` the
/// revision carries.
const IMAGE: &str = "public.ecr.aws/docker/library/alpine:3.20";

/// The task execution role the Fargate launches below run under, and the trust policy that lets
/// Amazon ECS assume it — "the task execution role that grants the Amazon ECS container agent
/// permission to make Amazon Web Services API calls on your behalf"
/// (`TaskDefinition$executionRoleArn`), which is the principal the `awslogs` driver's own stream
/// creation and delivery are made as.
const EXECUTION_ROLE_NAME: &str = "ecsTaskExecutionRole";
const EXECUTION_ROLE_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ecs-tasks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The managed policy AWS publishes for exactly that role, which is how the console and AWS's own
/// templates grant it the `logs:CreateLogStream` / `logs:PutLogEvents` the driver needs.
const EXECUTION_ROLE_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// `Task$lastStatus` of a task whose containers have started.
const RUNNING: &str = "RUNNING";

/// How long a row waits for the clock-owed moves it depends on, and how often it looks again.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// One row: where the retention comes from, what the container writes, and which group it all
/// happens in.
struct Row {
    case: &'static str,
    /// The log group the driver delivers to, and whose retention is under test.
    log_group: &'static str,
    /// The revision this row registers, and the `awslogs-stream-prefix` its driver names.
    family: &'static str,
    prefix: &'static str,
    /// The retention set BEFORE the task is launched, or `None` for a group that carries no
    /// retention at all until its events have already arrived.
    retention_before: Option<i64>,
    /// The retention set AFTER those events arrived, over a group that already holds them.
    retention_after: i64,
    /// The lines the container's program writes, each of which is one delivered event.
    lines: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        // The shape the recorded round trip takes: a retention set on the group before the task
        // runs, and one marker line read back out of that group afterwards.
        case: "the shortest published retention, set before one marker line is written",
        log_group: "/sim2real/retention-shortest",
        family: "retention-shortest",
        prefix: "ecs",
        retention_before: Some(PUBLISHED_RETENTIONS[0]),
        retention_after: PUBLISHED_RETENTIONS[2],
        lines: &["marker-shortest-first"],
    },
    Row {
        case: "a retention from inside the published list, with several lines delivered",
        log_group: "/sim2real/retention-middle",
        family: "retention-middle",
        prefix: "ecs",
        retention_before: Some(PUBLISHED_RETENTIONS[4]),
        retention_after: PUBLISHED_RETENTIONS[1],
        lines: &[
            "marker-middle-first",
            "marker-middle-second",
            "marker-middle-third",
        ],
    },
    Row {
        case: "the longest published retention, lowered once the events have arrived",
        log_group: "/sim2real/retention-longest",
        family: "retention-longest",
        prefix: "logs",
        retention_before: Some(PUBLISHED_RETENTIONS[21]),
        retention_after: PUBLISHED_RETENTIONS[0],
        lines: &["marker-longest-first"],
    },
    Row {
        // The other order: nothing set the retention before the delivery, so the group reports
        // none until a policy call over a group that already holds its events.
        case: "a group carrying no retention until after its events arrived",
        log_group: "/sim2real/retention-none",
        family: "retention-none",
        prefix: "ecs-tasks",
        retention_before: None,
        retention_after: PUBLISHED_RETENTIONS[11],
        lines: &["marker-none-first", "marker-none-second"],
    },
];

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation on a service whose model declares a Query protocol, answered with its XML
/// body.
async fn query(
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
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one operation on a service whose model declares awsJson1_1.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one Amazon ECS operation that must succeed, and answer its payload.
async fn ecs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, payload) = json_call(router, "ecs", ECS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {payload}"
    );
    payload
}

/// Call one CloudWatch Logs operation that must succeed, and answer its payload.
async fn logs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, payload) = json_call(router, "logs", LOGS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from logs {operation}, got {status} with {payload}"
    );
    payload
}

/// The account's own default subnets, read out of Amazon EC2 rather than invented — the network an
/// `awsvpc` placement of this account resolves against.
async fn default_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(
        router,
        "ec2",
        EC2_VERSION,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from DescribeSubnets, got {status} with body {body}"
    );
    let subnets = elements(&body, "subnetId");
    assert!(
        !subnets.is_empty(),
        "setup: the account must hold default subnets, got {subnets:?}"
    );
    subnets
}

/// Create the task execution role the launches run under, with AWS's own managed policy for it
/// attached, and answer the ARN IAM itself minted for it.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let (status, created) = query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE_NAME),
            ("AssumeRolePolicyDocument", EXECUTION_ROLE_TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from CreateRole, got {status} with body {created}"
    );
    let arn = element(&created, "Arn");
    let (status, attached) = query(
        router,
        "iam",
        IAM_VERSION,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE_NAME),
            ("PolicyArn", EXECUTION_ROLE_POLICY_ARN),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from AttachRolePolicy, got {status} with body {attached}"
    );
    arn
}

/// The `retentionInDays` the group `group` reports right now, and `None` for a group whose events
/// never expire — read through the owning service's own `DescribeLogGroups` rather than out of any
/// state struct.
async fn reported_retention(router: &MultiServiceRouter, case: &str, group: &str) -> Option<i64> {
    let described = logs_ok(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    let reported = described["logGroups"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|reported| reported["logGroupName"].as_str() == Some(group))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CloudWatch Logs must hold the group {group:?}, got {described}")
        });
    reported["retentionInDays"].as_i64()
}

/// The shell program one row's container runs: it writes each of the row's lines, one per line of
/// standard output, which is one delivered log event each.
fn program(row: &Row) -> Vec<String> {
    let script = row
        .lines
        .iter()
        .map(|line| format!("echo {line}"))
        .collect::<Vec<String>>()
        .join("; ");
    vec!["sh".to_string(), "-c".to_string(), script]
}

/// Register one row's revision and launch one standalone task of it, answering that task's ARN.
async fn launch(
    router: &MultiServiceRouter,
    row: &Row,
    role_arn: &str,
    subnets: &[String],
) -> String {
    let case = row.case;
    ecs_ok(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "command": program(row),
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": row.log_group,
                        "awslogs-region": REGION,
                        "awslogs-stream-prefix": row.prefix,
                    },
                },
            }],
        }),
    )
    .await;
    let launched = ecs_ok(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": CLUSTER,
            "taskDefinition": row.family,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": subnets,
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    launched["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: RunTask reported no task: {launched}"))
        .to_string()
}

/// Read one task again until it has reached `RUNNING`, so what is asserted afterwards is what a
/// started container delivered rather than a reading taken before anything of it ran.
async fn await_running(router: &MultiServiceRouter, case: &str, task_arn: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs_ok(
            router,
            case,
            "DescribeTasks",
            &json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        )
        .await;
        let task = &described["tasks"][0];
        // A container whose program finishes stops the task, so a row that was slow to be read
        // may already be past RUNNING: what matters is that it got there.
        if task["lastStatus"].as_str() == Some(RUNNING) || task["startedAt"].as_f64().is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never started, last describe was {described}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The `(message, logStreamName)` of every event `FilterLogEvents` reports for one group, as that
/// reader answers them.
async fn group_events(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String)> {
    let answered = logs_ok(
        router,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    answered["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|event| {
            (
                event["message"].as_str().unwrap_or_default().to_string(),
                event["logStreamName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The events of one group once it holds every line the row's container writes — looked at again
/// until it does, or the deadline passes: the delivery a started container owes is a clock-owed
/// move rather than something the launch answered with.
async fn settled_events(
    router: &MultiServiceRouter,
    row: &Row,
    group: &str,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let events = group_events(router, row.case, group).await;
        let delivered = row
            .lines
            .iter()
            .all(|line| events.iter().any(|(message, _)| message.as_str() == *line));
        if delivered || Instant::now() >= deadline {
            return events;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The stream one row's container delivers to: `<prefix>/<container name>/<task id>`, the naming
/// the driver's own `awslogs-stream-prefix` documentation states. Spelled out here rather than
/// imported, so a wrong naming in the implementation cannot make this suite pass.
fn stream_of(row: &Row, task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or(task_arn);
    format!("{}/{CONTAINER}/{task_id}", row.prefix)
}

#[tokio::test]
async fn an_ecs_tasks_awslogs_delivery_leaves_the_groups_configured_retention_in_force() {
    let router = build_router();
    let subnets = default_subnets(&router).await;
    let role_arn = execution_role(&router).await;
    ecs_ok(
        &router,
        "the cluster every row's task is launched into",
        "CreateCluster",
        &json!({ "clusterName": CLUSTER }),
    )
    .await;

    // Each row's group, the retention its client set on it before anything was ingested, and the
    // task whose container writes into it. Every task is launched up front, so the clock-owed
    // work of all of them falls due at once rather than one row's wait after another's.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for row in ROWS {
        let case = row.case;
        logs_ok(
            &router,
            case,
            "CreateLogGroup",
            &json!({ "logGroupName": row.log_group }),
        )
        .await;
        if let Some(days) = row.retention_before {
            logs_ok(
                &router,
                case,
                "PutRetentionPolicy",
                &json!({ "logGroupName": row.log_group, "retentionInDays": days }),
            )
            .await;
        }
        // 1. What the client configured is what the group reports before any ingestion — and a
        //    group nobody set a retention on reports none, because "to set a log group so that
        //    its log events do not expire, use DeleteRetentionPolicy".
        let before = reported_retention(&router, case, row.log_group).await;
        assert_eq!(
            before, row.retention_before,
            "case {case:?}: expected the group {:?} to report the retention its client set, \
             {:?}, got {before:?}",
            row.log_group, row.retention_before
        );

        launched.push((row, launch(&router, row, &role_arn, &subnets).await));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        await_running(&router, case, task_arn).await;

        // 2. Every line the container wrote arrives as an event of the configured group, so a
        //    read-back of a marker terminates rather than exhausting a budget.
        let events = settled_events(&router, row, row.log_group).await;
        let stream = stream_of(row, task_arn);
        for line in row.lines {
            let found = events.iter().find(|(message, _)| message.as_str() == *line);
            let (_, delivered_to) = found.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the line {line:?} the container wrote in the group \
                     {:?}, got {events:?}",
                    row.log_group
                )
            });
            // ...in the stream the driver's documented naming gives that container of that task.
            assert_eq!(
                delivered_to, &stream,
                "case {case:?}: expected the line {line:?} in the stream {stream:?}, got \
                 {delivered_to:?} among {events:?}"
            );
        }

        // 3. ...and the retention the client set BEFORE that ingestion is still what the group
        //    reports after it: delivering events into a group is not a change to its retention.
        let after = reported_retention(&router, case, row.log_group).await;
        assert_eq!(
            after, row.retention_before,
            "case {case:?}: expected the group {:?} to still report the retention {:?} its \
             client set before the task's container wrote, got {after:?}",
            row.log_group, row.retention_before
        );

        // 4. A policy call over a group that ALREADY holds events replaces the setting — the
        //    other order of the same rule — and leaves every delivered event readable.
        logs_ok(
            &router,
            case,
            "PutRetentionPolicy",
            &json!({ "logGroupName": row.log_group, "retentionInDays": row.retention_after }),
        )
        .await;
        let replaced = reported_retention(&router, case, row.log_group).await;
        assert_eq!(
            replaced,
            Some(row.retention_after),
            "case {case:?}: expected the group {:?} to report the retention set over its events, \
             {}, got {replaced:?}",
            row.log_group,
            row.retention_after
        );
        let still_there = group_events(&router, case, row.log_group).await;
        for line in row.lines {
            assert!(
                still_there
                    .iter()
                    .any(|(message, _)| message.as_str() == *line),
                "case {case:?}: expected the line {line:?} to still be readable in {:?} after \
                 its retention was set, got {still_there:?}",
                row.log_group
            );
        }
    }
}
