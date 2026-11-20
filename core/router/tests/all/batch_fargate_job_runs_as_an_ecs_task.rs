// SPDX-License-Identifier: CC0-1.0

//! What a PLACED AWS Batch job on Fargate capacity reports, driven end to end across the
//! services that own the facts: Batch, Amazon ECS, Amazon VPC and CloudWatch Logs on one
//! engine.
//!
//! The RULE under test: **a container job placed on the capacity behind its queue RUNS AS a
//! task of the compute environment's Amazon ECS cluster, and everything the job reports of
//! that run is read off that task** — `AttemptContainerDetail$taskArn` is "the Amazon Resource
//! Name (ARN) of the Amazon ECS task that's associated with the job attempt" and
//! `ComputeEnvironmentDetail$ecsClusterArn` "the Amazon Resource Name (ARN) of the underlying
//! Amazon ECS cluster that the compute environment uses", so the job walks the modelled
//! `JobStatus` ladder with the task (`STARTING` while "the relevant container initiation
//! operations are underway", `RUNNING` once its container is), reaches `SUCCEEDED` when that
//! container exits 0 and `FAILED` when it exits with anything else, and its
//! `startedAt`/`stoppedAt`, its attempt's `exitCode`, `reason`, `logStreamName` and
//! `networkInterfaces` and its container's `exitCode` are the task's own. A task that never
//! started a container reports why it did not, and a container that produced no exit status at
//! all leaves the job saying nothing ran it — no status is claimed for work nothing performed.
//!
//! Sources: `smithy_query(query="operation",operation="DescribeJobs")` for `JobDetail`'s
//! `status`, `statusReason`, `startedAt`, `stoppedAt`, `attempts` and `container`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt container's
//! `taskArn`, `exitCode`, `logStreamName` and `networkInterfaces`;
//! `smithy_query(query="shape",shape="JobStatus")` for the ladder's members. On the Amazon ECS
//! side: `smithy_query(query="operation",operation="DescribeTasks")` for the task the ARN has
//! to resolve to (`tasks`/`failures`).
//!
//! The rows differ along what the rule generalises over: WHAT the container runs (an argument
//! vector, a shell program, a program the emulator holds none of), the STATUS that run ends
//! with (zero, non-zero, none at all), and whether the placement's networking lets the task
//! fetch its image at all (a subnet with a route to the registry against one with none) — the
//! boundary at which the job reports the task's own failure to start instead of a run.
//!
//! What is NOT asserted here, because the owning crate's own suite asserts it: the placement
//! itself and the statuses either side of it
//! (`services/service-com-amazonaws-batch-awsbatchv20160810/tests/all/job_lifecycle.rs`,
//! `job_attempt_records.rs`), and the end a job takes when no Amazon ECS is mounted to run its
//! container at all (`batch_ecs_orchestration.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_contains, element, form_encode};

/// The Region every request below is signed for, and so the Region of every ARN minted for
/// them.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the execution role below is named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `CEStatus`/`JQStatus` a compute environment and a job queue come to rest in. Source:
/// `smithy_query(query="shape",shape="CEStatus,JQStatus")`.
const VALID: &str = "VALID";

/// The terminal `JobStatus` members, spelled out here rather than imported from a service
/// crate, so a wrong constant in the implementation cannot make this suite pass. Source:
/// `smithy_query(query="shape",shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The `stoppedReason` Amazon ECS words for a task whose essential container has exited, which
/// is the `statusReason` AWS Batch reports for the job that ran as it. Wholly fixed text: it
/// carries no part of the request.
const ESSENTIAL_CONTAINER_EXITED: &str = "Essential container in task exited";

/// The error CLASS a container runtime words a failed image fetch with, which a task that
/// could not reach its registry stops with.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The fixed text this emulator words a container whose declared program it performs none of
/// with, either side of the program the request named. Spelled out here rather than imported,
/// so a wrong constant in the implementation cannot make this suite pass.
const NOTHING_PERFORMED_PREFIX: &str =
    "This emulator performs a job container's declared command itself and provides no program \
     named";
const NOTHING_PERFORMED_SUFFIX: &str =
    "so the container's program was never started and the job performed no work.";

/// The log group AWS Batch delivers a job container's output to when its configuration names
/// none: "the log group for Batch jobs is `/aws/batch/job`"
/// (`AttemptContainerDetail$logStreamName`).
const JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The image every job definition below runs. It names a registry this emulator holds none of,
/// so nothing is decided against the fetch beyond the PATH to it — which is the dimension the
/// rows vary.
const IMAGE: &str = "registry.example.com/simparity/batch-worker:1";

/// The `/16` this suite's VPC is carved out of, and the two `/24` blocks its subnets take.
const VPC_CIDR: &str = "10.63.0.0/16";
const REACHABLE_SUBNET_CIDR: &str = "10.63.1.0/24";
const UNREACHABLE_SUBNET_CIDR: &str = "10.63.2.0/24";

/// The destination of a default route.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// How long a wait below gives the clock: far above the spans Batch, Amazon ECS and Amazon VPC
/// give the moves a placed job's task makes, and every wait looks again for the CONDITION
/// rather than sleeping a fixed stretch, so a loaded machine delays a row instead of breaking
/// it.
const DEADLINE: Duration = Duration::from_secs(240);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// AWS Batch, Amazon ECS, Amazon EC2 and CloudWatch Logs over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the task a placed job runs as is Amazon ECS's
/// resource, the interface that task is reached on is Amazon VPC's, and the stream its
/// container writes to is CloudWatch Logs'. Each service claims only requests scoped to its own
/// signing name.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: the router above runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and the parsed body of one response.
fn answered(response: http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).into_owned()))
    };
    (status, parsed)
}

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One `awsJson1_1` call to the service signing as `service`, under `prefix.<operation>`.
async fn json_rpc(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(service))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {service} {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    json_rpc(
        router,
        case,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
    )
    .await
}

/// One CloudWatch Logs call.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    json_rpc(router, case, "logs", "Logs_20140328", operation, body).await
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as
/// XML — returned as text, which the element readers above pull the minted ids out of.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={}&Version=2016-11-15", form_encode(action));
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ec2"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// The network this suite's compute environments place their tasks in: one VPC with an attached
/// internet gateway, a subnet whose route table sends the default route to that gateway (so a
/// task given a public address reaches the registry), a subnet with no route off the VPC at all,
/// and the security group every placement names.
struct Network {
    reachable_subnet: String,
    unreachable_subnet: String,
    security_group: String,
}

/// Build that network through Amazon EC2's own API, so every id below is one that service
/// minted and holds.
async fn network_of(router: &MultiServiceRouter) -> Network {
    let vpc = element(&ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await, "vpcId");
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let mut subnets = Vec::new();
    for (index, cidr) in [REACHABLE_SUBNET_CIDR, UNREACHABLE_SUBNET_CIDR]
        .into_iter()
        .enumerate()
    {
        let subnet = element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", &format!("{REGION}a")),
                ],
            )
            .await,
            "subnetId",
        );
        let table = element(
            &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
            "routeTableId",
        );
        ec2(
            router,
            "AssociateRouteTable",
            &[("RouteTableId", table.as_str()), ("SubnetId", subnet.as_str())],
        )
        .await;
        // Only the FIRST subnet is given a way off the VPC; the second keeps its traffic
        // inside it, which is what makes a registry on the internet unreachable from it.
        if index == 0 {
            ec2(
                router,
                "CreateRoute",
                &[
                    ("RouteTableId", table.as_str()),
                    ("DestinationCidrBlock", DEFAULT_ROUTE),
                    ("GatewayId", gateway.as_str()),
                ],
            )
            .await;
        }
        subnets.push(subnet);
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "batch-fargate-jobs"),
                ("GroupDescription", "the group the tasks of this suite run under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        reachable_subnet: subnets[0].clone(),
        unreachable_subnet: subnets[1].clone(),
        security_group,
    }
}

/// What ONE row expects of the job once it has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The container exited with this status, so the job carries it and the fixed AWS sentence
    /// for a task whose essential container exited.
    Exited(i64),
    /// The container produced no exit status at all, so nothing ran it.
    NothingRanIt,
    /// The task never started a container, because it could not fetch its image.
    CouldNotFetchTheImage,
}

/// One row of the table: what the container runs, where its task is placed, and how the job
/// ends.
struct Row {
    case: &'static str,
    /// The container's `command`, the argument vector its image is handed.
    command: Vec<&'static str>,
    /// Whether the task is placed in the subnet that reaches the registry.
    reachable: bool,
    ends: Ends,
}

/// The table. Its rows differ along what the rule generalises over — see the module header.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an argument vector that exits 0",
            command: vec!["echo", "the batch job ran this"],
            reachable: true,
            ends: Ends::Exited(0),
        },
        Row {
            case: "a shell program that exits 0",
            command: vec!["sh", "-c", "echo one && echo two"],
            reachable: true,
            ends: Ends::Exited(0),
        },
        Row {
            case: "a shell program that exits non-zero",
            command: vec!["sh", "-c", "exit 7"],
            reachable: true,
            ends: Ends::Exited(7),
        },
        Row {
            // The boundary of the run: the program a container's IMAGE holds is not one this
            // emulator has, so the run produces no exit status and none is reported for it.
            case: "a program the image holds",
            command: vec!["/opt/simparity/run", "--once"],
            reachable: true,
            ends: Ends::NothingRanIt,
        },
        Row {
            // The boundary of the placement: a task whose subnet reaches no registry fetches
            // no image, so no container of it ever starts and the job reports that failure.
            case: "a placement that reaches no registry",
            command: vec!["echo", "never reached"],
            reachable: false,
            ends: Ends::CouldNotFetchTheImage,
        },
    ]
}

/// Register the job definition of one row and answer its name: a FARGATE container job, whose
/// `resourceRequirements` name the smallest Fargate task size and whose
/// `networkConfiguration` asks for the public address an internet gateway carries traffic for.
async fn register(router: &MultiServiceRouter, row: &Row, index: usize) -> String {
    let name = format!("batch-task-run-{index}");
    batch(
        router,
        row.case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": &name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": {
                "image": IMAGE,
                "command": row.command,
                // "For jobs that run on Fargate resources, you must provide an execution role"
                // (`ContainerProperties$executionRoleArn`) — the role the image pull and the
                // log driver are made as.
                "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/batchExecutionRole"),
                "resourceRequirements": [
                    {"type": "VCPU", "value": "0.25"},
                    {"type": "MEMORY", "value": "512"},
                ],
                "networkConfiguration": {"assignPublicIp": "ENABLED"},
            },
        }),
    )
    .await;
    name
}

/// Create one row's compute environment and the queue in front of it, and answer the queue's
/// name. The environment is `MANAGED` over `FARGATE` capacity, in the subnet the row names.
async fn capacity(
    router: &MultiServiceRouter,
    row: &Row,
    network: &Network,
    index: usize,
) -> String {
    let environment = format!("batch-task-ce-{index}");
    let queue = format!("batch-task-queue-{index}");
    let subnet = if row.reachable {
        &network.reachable_subnet
    } else {
        &network.unreachable_subnet
    };
    batch(
        router,
        row.case,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": &environment,
            "type": "MANAGED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": [subnet],
                "securityGroupIds": [&network.security_group],
            },
        }),
    )
    .await;
    batch(
        router,
        row.case,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": &queue,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment}],
        }),
    )
    .await;
    (environment, queue)
        .1
        .clone()
}

/// Look again until the compute environment `name` reports `VALID`.
async fn until_environment_valid(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": [name]}),
        )
        .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some(VALID) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the compute environment never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the job queue `name` reports `VALID`.
async fn until_queue_valid(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describejobqueues",
            &json!({"jobQueues": [name]}),
        )
        .await;
        if body["jobQueues"][0]["status"].as_str() == Some(VALID) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job queue never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one `JobDetail` `DescribeJobs` reports for `job_id`.
async fn described(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let body = batch(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
    let jobs = body["jobs"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        jobs.len(),
        1,
        "case {case:?}: expected exactly one described job for {job_id}, got {body}"
    );
    jobs[0].clone()
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer the detail it reports
/// then. Nothing here sleeps for a span: it waits for the CONDITION.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default().to_string();
        if status == SUCCEEDED || status == FAILED {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a placed Fargate job runs as a task of its compute environment's Amazon ECS cluster,
/// and the status, the instants and the attempt it reports are that task's own.
#[tokio::test]
async fn a_placed_fargate_job_runs_as_a_task_of_its_compute_environments_cluster() {
    let rows = rows();
    let router = router();
    let network = network_of(&router).await;

    // Every row's estate is created, and every job submitted, BEFORE any of them is waited on,
    // so the spans the clocks owe them run together rather than one after another.
    let mut queues = Vec::new();
    let mut definitions = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        definitions.push(register(&router, row, index).await);
        queues.push(capacity(&router, row, &network, index).await);
    }
    for (index, row) in rows.iter().enumerate() {
        until_environment_valid(&router, row.case, &format!("batch-task-ce-{index}")).await;
        until_queue_valid(&router, row.case, &queues[index]).await;
    }
    let mut submitted = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let answer = batch(
            &router,
            row.case,
            "/v1/submitjob",
            &json!({
                "jobName": format!("batch-task-job-{index}"),
                "jobQueue": &queues[index],
                "jobDefinition": &definitions[index],
            }),
        )
        .await;
        submitted.push(
            answer["jobId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {:?}: expected a jobId in {answer}", row.case))
                .to_string(),
        );
    }

    for (row, job_id) in rows.iter().zip(&submitted) {
        let case = row.case;
        let detail = until_terminal(&router, case, job_id).await;

        // ONE attempt per placement, carrying the task the job was placed as.
        let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            attempts.len(),
            1,
            "case {case:?}: expected exactly one attempt for a job placed once, got {detail}"
        );
        let attempt = attempts[0].clone();
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the attempt must name the task it ran as, got {detail}")
            })
            .to_string();
        assert_eq!(
            detail["container"]["taskArn"].as_str(),
            Some(task_arn.as_str()),
            "case {case:?}: the job's container must name the same task as its attempt, got \
             {detail}"
        );

        // The task EXISTS where Amazon ECS holds it: that service answers for the ARN AWS Batch
        // published, with no failure — which is what makes the reported handle usable at all.
        let cluster = task_arn
            .split('/')
            .nth(1)
            .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {task_arn}"))
            .to_string();
        let tasks = ecs(
            &router,
            case,
            "DescribeTasks",
            &json!({"cluster": &cluster, "tasks": [&task_arn]}),
        )
        .await;
        let failures = tasks["failures"].as_array().cloned().unwrap_or_default();
        assert!(
            failures.is_empty(),
            "case {case:?}: Amazon ECS must resolve the task the job reports, got failures \
             {failures:?}"
        );
        assert_eq!(
            tasks["tasks"][0]["launchType"].as_str(),
            Some("FARGATE"),
            "case {case:?}: a job of a Fargate compute environment runs as a Fargate task, got \
             {tasks}"
        );

        let (expected_status, expected_exit) = match row.ends {
            Ends::Exited(0) => (SUCCEEDED, Some(0)),
            Ends::Exited(status) => (FAILED, Some(status)),
            Ends::NothingRanIt | Ends::CouldNotFetchTheImage => (FAILED, None),
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {detail}"
        );
        let reason = detail["statusReason"].as_str().unwrap_or_default().to_string();
        match row.ends {
            Ends::Exited(_) => assert_eq!(
                reason, ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: a job whose essential container exited carries the sentence AWS \
                 words for it, got {reason:?}"
            ),
            Ends::NothingRanIt => {
                // The program the container declares is not one this emulator performs, so the
                // task's container produced no status at all and the job says so — the fixed
                // text this emulator words that with, naming the program the request declared.
                let program = row.command.first().copied().unwrap_or_default();
                assert!(
                    reason.contains(NOTHING_PERFORMED_PREFIX)
                        && reason.contains(program)
                        && reason.contains(NOTHING_PERFORMED_SUFFIX),
                    "case {case:?}: a container nothing performed reports that, naming \
                     {program:?}, got {reason:?}"
                );
            }
            Ends::CouldNotFetchTheImage => assert!(
                reason.contains(CANNOT_PULL) && reason.contains(IMAGE),
                "case {case:?}: a task that could not fetch its image reports the fetch failure \
                 naming the reference, got {reason:?}"
            ),
        }

        // The run's own members: the exit status the container produced, on the attempt and on
        // the job's effective container, and NOTHING where no status was produced.
        for (where_, found) in [
            ("the attempt's container", &attempt["container"]["exitCode"]),
            ("the job's container", &detail["container"]["exitCode"]),
        ] {
            assert_eq!(
                found.as_i64(),
                expected_exit,
                "case {case:?}: expected {expected_exit:?} as {where_}'s exitCode, got {found}"
            );
        }
        // The attempt's statusReason and its container's `reason` are projections of ONE
        // reading of what the attempt ended with, so they agree with the job's.
        for (member, found) in [
            ("statusReason", &attempt["statusReason"]),
            ("container.reason", &attempt["container"]["reason"]),
        ] {
            assert_eq!(
                found.as_str(),
                Some(reason.as_str()),
                "case {case:?}: expected the attempt's {member} to be {reason:?}, got {found}"
            );
        }

        // A job whose task STARTED its container reports the instant it started at; one whose
        // task never started a container at all has none to report, and reports the stop of
        // the task it was placed as either way. A container whose program this emulator holds
        // none of still STARTED — its task reached `RUNNING` — so its job started too; what
        // that run produced no value for is the exit status, which is the member left absent.
        // The start instant is the STARTING -> RUNNING transition on both: a job whose
        // container never started reports no `startedAt` at all, which is the half
        // `job_rests_in_runnable.rs` pins.
        let started = !matches!(row.ends, Ends::CouldNotFetchTheImage);
        assert_eq!(
            attempt["startedAt"].as_i64().is_some(),
            started,
            "case {case:?}: expected the attempt's startedAt present = {started}, got {attempt}"
        );
        assert_eq!(
            detail["startedAt"].as_i64().is_some_and(|at| at > 0),
            started,
            "case {case:?}: expected the job's startedAt present = {started}, got {detail}"
        );
        for (member, found) in [
            ("the job's stoppedAt", &detail["stoppedAt"]),
            ("the attempt's stoppedAt", &attempt["stoppedAt"]),
        ] {
            assert!(
                found.as_i64().is_some(),
                "case {case:?}: expected {member} on a job placed as a task that stopped, got \
                 {detail}"
            );
        }
        if started {
            let at = attempt["startedAt"].as_i64().unwrap_or_default();
            let stopped = attempt["stoppedAt"].as_i64().unwrap_or_default();
            assert!(
                at <= stopped,
                "case {case:?}: expected startedAt {at} at or before stoppedAt {stopped}"
            );
        }

        // The interface the task is reached on, reported on the attempt's container and on the
        // job's: an `awsvpc` task of a Fargate environment runs on one elastic network
        // interface, whose private address comes out of the subnet it was created in.
        if started {
            for (where_, found) in [
                (
                    "the attempt's container",
                    &attempt["container"]["networkInterfaces"],
                ),
                ("the job's container", &detail["container"]["networkInterfaces"]),
            ] {
                let interfaces = found.as_array().cloned().unwrap_or_default();
                assert_eq!(
                    interfaces.len(),
                    1,
                    "case {case:?}: expected one interface on {where_}, got {found}"
                );
                let attachment = interfaces[0]["attachmentId"].as_str().unwrap_or_default();
                assert!(
                    !attachment.is_empty(),
                    "case {case:?}: expected an attachmentId on {where_}, got {found}"
                );
                let address = interfaces[0]["privateIpv4Address"]
                    .as_str()
                    .unwrap_or_default();
                assert!(
                    block_contains(REACHABLE_SUBNET_CIDR, address),
                    "case {case:?}: expected {where_}'s address out of {REACHABLE_SUBNET_CIDR}, \
                     got {address:?}"
                );
            }
        }
    }
}

/// RULE: the output a placed job's container writes goes to the CloudWatch Logs stream the
/// attempt reports, in the log group AWS Batch delivers a job's logs to — so a client that
/// reads the stream the job names finds what the container really wrote.
#[tokio::test]
async fn the_output_a_placed_jobs_container_writes_is_in_the_stream_it_reports() {
    let case = "a container that writes two lines";
    let router = router();
    let network = network_of(&router).await;
    let row = Row {
        case,
        command: vec!["sh", "-c", "echo first line && echo second line"],
        reachable: true,
        ends: Ends::Exited(0),
    };
    let definition = register(&router, &row, 0).await;
    let queue = capacity(&router, &row, &network, 0).await;
    until_environment_valid(&router, case, "batch-task-ce-0").await;
    until_queue_valid(&router, case, &queue).await;
    let answer = batch(
        &router,
        case,
        "/v1/submitjob",
        &json!({
            "jobName": "batch-logged-job",
            "jobQueue": &queue,
            "jobDefinition": &definition,
        }),
    )
    .await;
    let job_id = answer["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a jobId in {answer}"))
        .to_string();

    let detail = until_terminal(&router, case, &job_id).await;
    assert_eq!(
        detail["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: expected {SUCCEEDED}, got {detail}"
    );
    let stream = detail["attempts"][0]["container"]["logStreamName"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the attempt must name its log stream, {detail}"))
        .to_string();
    assert_eq!(
        detail["container"]["logStreamName"].as_str(),
        Some(stream.as_str()),
        "case {case:?}: the job's container must name the same stream as its attempt, {detail}"
    );
    // The stream is named `<prefix>/<container name>/<task id>`, and the task id is the one the
    // attempt's task ARN carries — the two views of one task agree.
    let task_arn = detail["attempts"][0]["container"]["taskArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let task_id = task_arn.rsplit('/').next().unwrap_or_default().to_string();
    assert!(
        stream.ends_with(&task_id) && !task_id.is_empty(),
        "case {case:?}: expected the stream {stream:?} to be named for the task {task_id:?}"
    );

    // What the container WROTE is in that stream, where CloudWatch Logs holds it. The events
    // are delivered as the container writes them, so this looks again until both lines are
    // there rather than assuming one call sees them.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let events = logs(
            &router,
            case,
            "GetLogEvents",
            &json!({
                "logGroupName": JOB_LOG_GROUP,
                "logStreamName": &stream,
                "startFromHead": true,
            }),
        )
        .await;
        let messages: Vec<String> = events["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        if messages == ["first line", "second line"] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the two lines the container wrote in {stream:?}, got \
             {messages:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // And the estate the job ran in can be TORN DOWN once it has: the stream a client holds
    // the name of is deletable, and nothing the run created still holds a place in the network
    // it ran in — the elastic network interface Amazon ECS created for the task is torn down
    // WITH the task (`Attachment$status` reaches `DELETED`), so by the time the job reports a
    // terminal status the address it held is back in its subnet and the subnet itself can go.
    logs(
        &router,
        case,
        "DeleteLogStream",
        &json!({"logGroupName": JOB_LOG_GROUP, "logStreamName": &stream}),
    )
    .await;
    for (resource, action, params) in [
        (
            "the subnet the task ran in",
            "DeleteSubnet",
            vec![("SubnetId", network.reachable_subnet.clone())],
        ),
        (
            "the security group it ran under",
            "DeleteSecurityGroup",
            vec![("GroupId", network.security_group.clone())],
        ),
    ] {
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        // The helper asserts the 200 itself, quoting Amazon EC2's own refusal when a resource
        // of the finished run still holds the one it names.
        let _ = resource;
        ec2(&router, action, &borrowed).await;
    }
}
