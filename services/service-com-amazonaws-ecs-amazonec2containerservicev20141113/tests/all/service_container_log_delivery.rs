// SPDX-License-Identifier: CC0-1.0

//! What a client waiting for a SERVICE to stabilize observes, end to end: the scheduler's
//! own tasks come up, and the containers they run deliver their output to CloudWatch Logs.
//!
//! RULE: a service whose scheduling strategy is `REPLICA` and whose deployment controller is
//! the ECS one converges, off the clock and with no further call, on holding exactly
//! `desiredCount` tasks in the `RUNNING` state — `Service$runningCount` ("The number of
//! tasks in the cluster that are in the RUNNING state") reaching `desiredCount` with
//! `Service$pendingCount` ("The number of tasks in the cluster that are in the PENDING
//! state") back to zero and the PRIMARY `Deployment$rolloutState` moved to `COMPLETED`
//! ("When a service deployment is started, it begins in an IN_PROGRESS state. When the
//! service reaches a steady state, the deployment transitions to a COMPLETED state") — and
//! every task it placed RUNS its containers' programs, so each line one of them writes
//! becomes one log event in the stream `<awslogs-stream-prefix>/<container name>/<task id>`
//! of the log group that container's `awslogs` `logConfiguration` names, which that group's
//! own `DescribeLogStreams` lists and `GetLogEvents` reads back.
//!
//! The scheduler's path is the one every `aws ecs wait services-stable`, deployment gate and
//! log-driven rollout check is built on, and the one an `AWS::ECS::Service` reaches through:
//! CloudFormation creates such a resource through this same `CreateService`, so a stack's
//! service converges because this rule holds rather than because of anything that knows
//! about stacks.
//!
//! The rows differ along what the rule generalises over: HOW MANY tasks the service wants
//! (one, two), the launch type and network mode its tasks run with (`FARGATE` with `awsvpc`,
//! where the task is allocated its own elastic network interface, and `EC2` on the cluster's
//! registered capacity), how many lines each container writes before it keeps running,
//! whether the revision names a task execution role (which a Fargate revision delivering
//! through `awslogs` must) and whether the log configuration names the Region the group is in
//! at all — a configuration that names none delivers to the Region the task was launched in.
//!
//! Nothing here reaches into a state struct: the services are created through the ECS front
//! door, the execution role through IAM's own `CreateRole`, the groups through CloudWatch
//! Logs' own `CreateLogGroup`, and every value asserted is read back through
//! `DescribeServices`, `ListTasks`, `DescribeTasks`, `DescribeLogStreams` and `GetLogEvents`.
//! The stream naming is spelled out here rather than imported, so a wrong naming in the
//! implementation cannot make a test pass.
//!
//! Every one of those reads is a WAIT FOR ITS CONDITION, re-read until it holds: what this
//! rule is about is owed by the clock, so each reading that could still be one scheduler pass
//! or one delivery behind — the counters, the task list the counters are a projection of, the
//! events a container's run delivers and the streams its group then holds — is polled rather
//! than taken at whatever instant the reading before it settled. The deadlines are derived
//! from the spans the emulator itself documents for that chain plus a margin far wider than
//! any of them ([`LOADED_HOST_MARGIN`]), and nothing here asserts on how long anything took:
//! an outcome that depended on how fast the host ran would hide a real failure on an idle
//! machine and fail a release on a loaded one.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::{
    ROLLOUT_STEP_SPAN, TRANSITION_SPAN,
};

use crate::ecs_wire::{
    awsvpc_network_configuration, cluster_with_capacity, count, iam, list, ok,
    seed_awsvpc_subnets, string, ACCOUNT, AWSVPC_START, SIGNED_REGION,
};

/// The cluster every row's service runs in. One cluster for all of them, so the rows differ
/// only in what the rule generalises over.
const CLUSTER: &str = "scheduled-logs";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from: a reference naming a registry this emulator answers
/// nothing for, so the image fetch decides nothing in this suite.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The IAM role the Fargate revisions below name as their task execution role — "the task
/// execution role that grants the Amazon ECS container agent permission to make Amazon Web
/// Services API calls on your behalf" (`TaskDefinition$executionRoleArn`), which a Fargate
/// revision delivering through the `awslogs` driver must carry. Created through IAM's own API
/// with a trust policy admitting this service, which is what lets Amazon ECS assume it.
const EXECUTION_ROLE: &str = "scheduledLogsExecutionRole";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to
/// do it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// How long the container's program stays up after it has written its lines, so a task of a
/// converged service is a task whose program is still running — the shape of the workload a
/// service runs.
const STAY_UP_SECONDS: u32 = 300;

/// How often a poll re-reads the emulator. A cadence, not a bound: a poll that finds its
/// condition unmet looks again until its deadline, so this decides how promptly the suite
/// notices and never whether it passes.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How much longer than the spans it waits on every deadline below allows.
///
/// Deliberately far wider than the whole chain a service converges through, because a test
/// host shares its cores with every other suite of this crate and may be orders of magnitude
/// slower than an idle one. Nothing here measures how long anything took, so the only thing
/// this margin costs is the time a BROKEN emulator takes to be reported — and the only thing
/// too small a margin buys is a suite that fails a release on a loaded machine.
const LOADED_HOST_MARGIN: Duration = Duration::from_secs(120);

/// The spans the emulator DOCUMENTS for the chain one row's service converges through: the
/// interface an `awsvpc` task is allocated is created and attached one documented step at a
/// time and only then is that task's own start owed ([`AWSVPC_START`]), the scheduler takes
/// one step per placement of the rollout (`state::ROLLOUT_STEP_SPAN`), and the steady state
/// the rollout settles into is owed one transition later (`state::TRANSITION_SPAN`).
const CONVERGENCE_SPANS: Duration = AWSVPC_START
    .saturating_add(ROLLOUT_STEP_SPAN)
    .saturating_add(TRANSITION_SPAN);

/// How long a wait for one row's service to hold the task set it was asked for keeps trying:
/// the spans that chain is made of, with [`LOADED_HOST_MARGIN`] on top.
const CONVERGENCE_BUDGET: Duration = CONVERGENCE_SPANS.saturating_add(LOADED_HOST_MARGIN);

/// How long a wait for what a container's run delivers keeps trying.
///
/// The delivery is owed no span of its own: the driver creates the stream as the container
/// starts and writes each line the program writes, so this is the margin alone — what a task
/// already reported as `RUNNING` owes is observable from the instant it is reported.
const DELIVERY_BUDGET: Duration = LOADED_HOST_MARGIN;

/// One row: the service the scheduler is given, and what its container writes.
struct Row {
    case: &'static str,
    /// The `desiredCount` the service is created with.
    desired: i64,
    /// The `launchType` its tasks are placed with, and the revision's `networkMode`.
    launch_type: &'static str,
    network_mode: &'static str,
    /// Whether the log configuration names the Region the group is in (`awslogs-region`).
    names_region: bool,
    /// Whether the revision names the task execution role ([`EXECUTION_ROLE`]).
    names_execution_role: bool,
    /// The service, the task-definition family, the log group and the stream prefix this
    /// row uses.
    service_name: &'static str,
    family: &'static str,
    log_group: &'static str,
    stream_prefix: &'static str,
    /// The lines the container writes before it stays up, in order.
    lines: &'static [&'static str],
}

const ROWS: [Row; 3] = [
    Row {
        case: "a Fargate service that wants one task, writing one line",
        desired: 1,
        launch_type: "FARGATE",
        network_mode: "awsvpc",
        names_region: true,
        names_execution_role: true,
        service_name: "one-fargate",
        family: "scheduled-logs-one",
        log_group: "/emulator/ecs/scheduled-one",
        stream_prefix: "svc",
        lines: &["ready-one"],
    },
    Row {
        case: "a Fargate service that wants two tasks, each writing two lines",
        desired: 2,
        launch_type: "FARGATE",
        network_mode: "awsvpc",
        names_region: true,
        names_execution_role: true,
        service_name: "two-fargate",
        family: "scheduled-logs-two",
        log_group: "/emulator/ecs/scheduled-two",
        stream_prefix: "worker",
        lines: &["booting-two", "ready-two"],
    },
    Row {
        case: "an EC2 launch-type service on the cluster's capacity, with no awslogs-region",
        desired: 1,
        launch_type: "EC2",
        network_mode: "bridge",
        names_region: false,
        names_execution_role: false,
        service_name: "one-ec2",
        family: "scheduled-logs-three",
        log_group: "/emulator/ecs/scheduled-three",
        stream_prefix: "batch",
        lines: &["booting-three", "working-three", "ready-three"],
    },
];

/// A router carrying Amazon ECS, IAM — which holds the execution role Amazon ECS assumes
/// before it launches a task — and CloudWatch Logs, which a container's output is delivered
/// to, over ONE shared engine, mounted the way the production router mounts them, with the
/// subnets an `awsvpc` placement names held in the account's Amazon VPC records.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // An `awsvpc` task is allocated its own elastic network interface in a subnet of the
    // account, so the placements below resolve against Amazon VPC's records.
    seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// Create the task execution role the Fargate revisions name, through IAM's OWN API, with a
/// trust policy that lets Amazon ECS assume it and the grant its log delivery needs — the two
/// actions the `awslogs` driver performs under that role: it creates the stream it delivers
/// to and writes the container's lines into it — and answer its ARN.
async fn create_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [ECS_TASKS_PRINCIPAL] },
            "Action": [ASSUME_ROLE],
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", document),
        ],
        case,
    )
    .await;
    let delivery = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyName", "log-delivery".to_string()),
            ("PolicyDocument", delivery),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// One CloudWatch Logs call, the way a client makes it: `POST /` with
/// `X-Amz-Target: Logs_20140328.<operation>` and a credential scoped to `logs`, which is
/// what the router picks the service from.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /logs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One CloudWatch Logs call that must succeed, answered with its body.
async fn logs_ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = logs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CloudWatch Logs {operation} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// Create the log group one row's containers deliver to, through CloudWatch Logs' own API —
/// the way a client (or the `AWS::Logs::LogGroup` of a stack) sets one up before the
/// workload that writes to it.
async fn create_log_group(router: &MultiServiceRouter, row: &Row) {
    logs_ok(
        router,
        "CreateLogGroup",
        json!({ "logGroupName": row.log_group }),
        row.case,
    )
    .await;
}

/// The program one row's container runs: it writes the row's lines and then stays up, which
/// is the shape of a service's workload.
fn program(row: &Row) -> Vec<String> {
    let mut script = String::new();
    for line in row.lines {
        script.push_str(&format!("echo {line}; "));
    }
    script.push_str(&format!("sleep {STAY_UP_SECONDS}"));
    vec!["sh".to_string(), "-c".to_string(), script]
}

/// Register the revision one row's service runs: a single container that runs the row's
/// program and delivers its output through the `awslogs` driver to the row's group.
async fn register_definition(router: &MultiServiceRouter, row: &Row, execution_role: &str) {
    let mut options = json!({
        "awslogs-group": row.log_group,
        "awslogs-stream-prefix": row.stream_prefix,
    });
    if row.names_region {
        options["awslogs-region"] = json!(SIGNED_REGION);
    }
    let mut request = json!({
        "family": row.family,
        "networkMode": row.network_mode,
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "command": program(row),
            "logConfiguration": { "logDriver": "awslogs", "options": options },
        }],
    });
    // A Fargate revision states the task-level size the launch runs with, and the
    // compatibility the launch type needs.
    if row.launch_type == "FARGATE" {
        request["requiresCompatibilities"] = json!(["FARGATE"]);
        request["cpu"] = json!("256");
        request["memory"] = json!("512");
    }
    if row.names_execution_role {
        request["executionRoleArn"] = json!(execution_role);
    }
    ok(router, "RegisterTaskDefinition", request, row.case).await;
}

/// Create one row's service, asserting that the create itself reports the count the caller
/// asked for with nothing running yet: the convergence is owed by the CLOCK, not done by the
/// call.
async fn create_service(router: &MultiServiceRouter, row: &Row) {
    let mut request = json!({
        "cluster": CLUSTER,
        "serviceName": row.service_name,
        "taskDefinition": row.family,
        "desiredCount": row.desired,
        "launchType": row.launch_type,
    });
    if row.network_mode == "awsvpc" {
        request["networkConfiguration"] = awsvpc_network_configuration();
    }
    let created = ok(router, "CreateService", request, row.case).await;
    let service = &created["service"];
    assert_eq!(
        service["desiredCount"].as_i64(),
        Some(row.desired),
        "case {:?}: the create must keep the count the caller asked for, got {created}",
        row.case
    );
    assert_eq!(
        count(service, "runningCount", row.case),
        0,
        "case {:?}: nothing is running at the instant the service is created, got {created}",
        row.case
    );
    assert_eq!(
        service["deployments"][0]["rolloutState"].as_str(),
        Some("IN_PROGRESS"),
        "case {:?}: the rollout a create starts is under way, got {created}",
        row.case
    );
}

/// Wait until one row's service holds the task set it was asked for with its rollout
/// completed, and answer the service as it is then reported.
async fn converged_service(router: &MultiServiceRouter, row: &Row) -> Value {
    let case = row.case;
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let described = ok(
            router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [row.service_name] }),
            case,
        )
        .await;
        assert!(
            list(&described, "failures", case).is_empty(),
            "case {case:?}: DescribeServices must report no failure, got {described}"
        );
        let service = described["services"][0].clone();
        let running = count(&service, "runningCount", case);
        let pending = count(&service, "pendingCount", case);
        let deployment = service["deployments"][0].clone();
        let rollout = deployment["rolloutState"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let converged = running == row.desired && pending == 0 && rollout == "COMPLETED";
        if converged || Instant::now() >= deadline {
            assert!(
                converged,
                "case {case:?}: expected runningCount {} with pendingCount 0 and a COMPLETED \
                 rollout, got runningCount {running}/pendingCount {pending}/{rollout:?} in \
                 {service}",
                row.desired
            );
            assert_eq!(
                deployment["status"].as_str(),
                Some("PRIMARY"),
                "case {case:?}: the rollout that completed is the primary deployment, got \
                 {deployment}"
            );
            return service;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ARNs of the RUNNING tasks of one row's service, with each task's status asserted on
/// the way.
///
/// WAITED FOR, not read once: the counters [`converged_service`] settled on are the SERVICE's
/// own projection of the task records, while this is the task records' own, so a list that is
/// still one scheduler pass behind those counters is a condition to look again on rather than
/// a failure. The assertions are the same ones either way — they are made on the reading this
/// settles on, or on the last one taken before the deadline, which is what names the actual
/// value when it never settles.
async fn running_task_arns(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let case = row.case;
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let listed = ok(
            router,
            "ListTasks",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name,
                "desiredStatus": "RUNNING",
            }),
            case,
        )
        .await;
        let arns = list(&listed, "taskArns", case);
        // A describe of no task names no task to describe, so an empty list is reported as
        // the empty answer it is rather than put to the emulator as a request.
        let described = if arns.is_empty() {
            json!({ "tasks": [], "failures": [] })
        } else {
            ok(
                router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": arns.clone() }),
                case,
            )
            .await
        };
        let tasks = list(&described, "tasks", case);
        let settled = arns.len() as i64 == row.desired
            && tasks.len() == arns.len()
            && tasks
                .iter()
                .all(|task| task["lastStatus"].as_str() == Some("RUNNING"));
        if !settled && Instant::now() < deadline {
            ::tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        }
        assert_eq!(
            arns.len() as i64,
            row.desired,
            "case {case:?}: expected {} task ARN(s) of the converged service, got {listed}",
            row.desired
        );
        assert_eq!(
            tasks.len() as i64,
            row.desired,
            "case {case:?}: every listed task must be describable, got {described}"
        );
        let mut task_arns = Vec::new();
        for task in &tasks {
            assert_eq!(
                task["lastStatus"].as_str(),
                Some("RUNNING"),
                "case {case:?}: a task counted as running reports RUNNING, got {task}"
            );
            assert_eq!(
                task["launchType"].as_str(),
                Some(row.launch_type),
                "case {case:?}: the task runs the launch type the service was created with, got \
                 {task}"
            );
            assert_eq!(
                task["group"].as_str(),
                Some(format!("service:{}", row.service_name).as_str()),
                "case {case:?}: the task the scheduler placed carries the service's own group, \
                 got {task}"
            );
            task_arns.push(string(task, "taskArn", case));
        }
        task_arns.sort();
        return task_arns;
    }
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming the driver's own
/// `awslogs-stream-prefix` documentation states. Spelled out here rather than imported, so a
/// wrong naming in the implementation cannot make a test pass.
fn stream_of(row: &Row, task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{}/{CONTAINER}/{task_id}", row.stream_prefix)
}

/// The SET of stream names one row's log group holds, read with the call a rollout check
/// makes — `orderBy` `LastEventTime` and `descending`, which is how a client asks for the
/// most recently written streams first. Which streams the group holds is what this rule is
/// about, so the names are compared as a set; the ordering that call applies is its own
/// surface and is pinned by the log service's own suites.
///
/// WAITED FOR, not read once: the streams are created by the runs of the containers the
/// clock started, so a group that does not hold them all YET is a condition to look again
/// on. `wanted` is the set the caller will compare against, and the reading this answers is
/// the one that matched it or the last one taken before the deadline.
async fn described_streams(
    router: &MultiServiceRouter,
    row: &Row,
    wanted: &BTreeSet<String>,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let described = logs_ok(
            router,
            "DescribeLogStreams",
            json!({
                "logGroupName": row.log_group,
                "orderBy": "LastEventTime",
                "descending": true,
            }),
            row.case,
        )
        .await;
        let streams: BTreeSet<String> = list(&described, "logStreams", row.case)
            .into_iter()
            .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
            .collect();
        if &streams == wanted || Instant::now() >= deadline {
            return streams;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The messages CloudWatch Logs holds in `stream` of one row's group, oldest first. An
/// unknown stream reads as no messages, which is what a stream nothing was delivered to
/// holds.
async fn delivered(router: &MultiServiceRouter, row: &Row, stream: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": row.log_group,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
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

/// Poll `probe` until it answers `Some`, or `budget` runs out — for the clock-owed
/// transitions and the deliveries a container's own run makes.
async fn poll_until<T, F, Fut>(probe: F, budget: Duration, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + budget;
    loop {
        if let Some(answer) = probe().await {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{description}: not observed within {budget:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Everything one row is asserted to converge on and to deliver, read back through the API
/// alone: the counters and the rollout, the tasks the scheduler placed, the events each of
/// their containers wrote, and the streams the group then holds.
///
/// One function, so the two tests below observe one row by exactly the same readings and the
/// same assertions: what differs between them is only WHAT materialises the clock-owed work
/// behind those readings.
async fn observed_row(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let service = converged_service(router, row).await;
    let deployment = &service["deployments"][0];
    assert_eq!(
        count(deployment, "runningCount", case),
        row.desired,
        "case {case:?}: the completed rollout holds the tasks the service runs, got {deployment}"
    );

    let task_arns = running_task_arns(router, row).await;
    let expected: Vec<String> = row.lines.iter().map(|line| line.to_string()).collect();
    let mut streams = BTreeSet::new();
    for task_arn in &task_arns {
        let stream = stream_of(row, task_arn);
        // Every line the container wrote is one event of its own stream, in the order
        // the program wrote them.
        let events = poll_until(
            || async {
                let events = delivered(router, row, &stream).await;
                (events.len() >= expected.len()).then_some(events)
            },
            DELIVERY_BUDGET,
            &format!("case {case:?}: {} line(s) in {stream}", expected.len()),
        )
        .await;
        assert_eq!(
            events, expected,
            "case {case:?}: expected {expected:?} in {stream}, got {events:?}"
        );
        streams.insert(stream);
    }

    // The group lists exactly those streams: one per task of the service, and nothing
    // else was delivered to it.
    let described = described_streams(router, row, &streams).await;
    assert_eq!(
        described, streams,
        "case {case:?}: expected the group {} to hold one stream per running task, got \
         {described:?}",
        row.log_group
    );
}

/// Create the cluster, the execution role, and every row's group, revision and service.
async fn place_services(router: &MultiServiceRouter, rows: &[&Row]) {
    // The cluster holds registered capacity, because one row's launch type runs on it: "The
    // EC2 launch type runs your tasks on Amazon EC2 instances registered to your cluster".
    cluster_with_capacity(router, CLUSTER, "the cluster every row's service runs in").await;
    let execution_role =
        create_execution_role(router, "the role Amazon ECS assumes to launch a task").await;
    // Every row's service is created up front, so the clock-owed work of all of them falls
    // due at once rather than one row's budget after another's.
    for row in rows {
        create_log_group(router, row).await;
        register_definition(router, row, &execution_role).await;
        create_service(router, row).await;
    }
}

#[tokio::test]
async fn a_services_tasks_come_up_and_their_containers_output_reaches_cloudwatch_logs() {
    let router = emulator();
    let rows: Vec<&Row> = ROWS.iter().collect();
    place_services(&router, &rows).await;
    for row in &rows {
        observed_row(&router, row).await;
    }
}

/// The rows the test below places and then observes through READS alone.
///
/// The `awsvpc` ones: the first thing the clock owes such a task is the interface Amazon ECS
/// creates and attaches for it, one documented step at a time
/// (`state::INTERFACE_ATTACH_LIFECYCLE`), and its own start is owed only after that — so the
/// services are placed, and the runtime that placed them dropped, while every task of them is
/// still provisioning and no container of any of them has been started by anything. They
/// still differ along what the rule generalises over: how many tasks the service wants (one,
/// two) and how many lines each of their containers writes (one, two).
fn read_driven_rows() -> Vec<&'static Row> {
    ROWS.iter().filter(|row| row.network_mode == "awsvpc").collect()
}

/// The same rule, where the only thing that materialises it is a client READING.
///
/// RULE: what a task has reached is owed by the CLOCK, so every reading of a task
/// materialises what the clock already owed it — and a task that has reached `RUNNING` is a
/// task whose containers are RUNNING, so the output of their programs follows the status a
/// client reads back rather than the liveness of whichever in-process task of this emulator
/// happened to be driving the clock when the service was created.
///
/// The condition is produced the way a client cannot tell from any other: the services are
/// placed on one runtime, that runtime is DROPPED — which ends every in-process task this
/// emulator started while it was alive, with whatever each of them was holding left behind —
/// and the rows are then observed from a second runtime, which calls nothing that writes a
/// task's status. Every transition those rows reach, and every container program whose output
/// arrives, is therefore owed to a read: it is the readings themselves that materialise the
/// placement, the interface, the task's start and the runs behind its containers.
#[test]
fn a_read_materialises_the_container_runs_of_the_task_it_reports_as_running() {
    let router = emulator();
    let rows = read_driven_rows();

    let placing = ::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime to place the services on");
    placing.block_on(place_services(&router, &rows));
    // Every in-process task this emulator started while the services were being placed ends
    // here, the one driving the clock among them. Nothing of the stores goes with it: what a
    // task has reached is a due INSTANT each record carries, never a wait something is
    // sitting in.
    drop(placing);

    let reading = ::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime to read the emulator from");
    reading.block_on(async {
        for row in &rows {
            observed_row(&router, row).await;
        }
    });
}
