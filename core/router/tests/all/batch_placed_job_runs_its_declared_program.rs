// SPDX-License-Identifier: CC0-1.0

//! What a PLACED AWS Batch job does, driven end to end over the wire against the services
//! that own the facts: Batch, CloudFormation, IAM, Amazon ECS and CloudWatch Logs on one
//! engine.
//!
//! The RULE under test: **a job the scheduler places RUNS the program its container declares
//! — `ContainerProperties$command` is "the command that's passed to the container" — so the
//! job reaches `RUNNING` and then the terminal status that program's EXIT STATUS decides:
//! `SUCCEEDED` for a container that exited zero and `FAILED` for any other status, reporting
//! that exit code (`ContainerDetail$exitCode`, "the exit code returned upon completion") and
//! delivering the lines the program wrote to the CloudWatch Logs stream its `awslogs`
//! configuration opened for the task. A container whose declared program this emulator
//! performs none of — the program inside the caller's own image — claims NO run at all: no
//! exit code, no started instant and a `statusReason` naming what was not performed.** And it
//! holds for an estate deployed by CloudFormation exactly as for one created through the Batch
//! API: once the stack reports `CREATE_COMPLETE` its queue is `VALID`, a job submitted to it
//! is reported by `GetJobQueueSnapshot` in `frontOfQueue.jobs` while it waits for the capacity
//! its MANAGED compute environment has to provision, and then it runs.
//!
//! Sources: `smithy_query(query="shape",shape="JobStatus")` (SUBMITTED | PENDING | RUNNABLE |
//! STARTING | RUNNING | SUCCEEDED | FAILED), `smithy_query(query="shape",
//! shape="ContainerDetail")` for `command`, `exitCode`, `reason` and `logStreamName`,
//! `smithy_query(query="shape",shape="AttemptDetail")` for the attempt's `startedAt` and
//! `stoppedAt`, and `smithy_query(query="operation",operation="GetJobQueueSnapshot")` for the
//! front-of-queue projection.
//!
//! Two crates have to agree for the second half to hold — CloudFormation creates the compute
//! environment and the queue in AWS Batch, and Batch places and runs the job on them — so this
//! suite lives here rather than in either crate. The rows of the first half differ along what
//! the rule generalises over: the FORM of the declared program (a utility, a utility named by
//! its absolute path, a shell program, a shell program reading the job's environment), the
//! exit status it ends with (zero and several non-zero ones), how much it writes (nothing, one
//! line, several lines), and whether this emulator performs the declared program at all (no
//! command, and a program its catalog does not hold).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use super::query_wire::form_encode;

/// The Region every request below is signed for, and so the Region of every ARN minted for
/// them.
const REGION: &str = "us-east-1";

/// The log group AWS Batch delivers a job container's output to when its configuration names
/// no other one, spelled out here rather than imported so a wrong constant in the
/// implementation cannot make this suite pass. Source: `AttemptContainerDetail$logStreamName`
/// ("The log group for Batch jobs is `/aws/batch/job`").
const JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The terminal `JobStatus`es and the `CEStatus`/`JQStatus` a compute environment and a job
/// queue come to rest in. Source:
/// `smithy_query(query="shape",shape="JobStatus,CEStatus,JQStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const VALID: &str = "VALID";

/// The `statusReason` AWS Batch reports for a job whose container EXITED, whichever status it
/// exited with — Amazon ECS's own words for a task whose essential container exited. Spelled
/// out here rather than imported, for the same reason as the log group above.
const CONTAINER_EXITED: &str = "Essential container in task exited";

/// How long this suite waits for something the clock owes — a compute environment reaching
/// `VALID`, a stack settling, a job being queued and then running, the lines a container
/// wrote reaching CloudWatch Logs. Far above the spans those take, and every wait looks
/// again for the CONDITION rather than sleeping a fixed stretch, so a loaded machine delays a
/// case instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(180);

/// How often each of those waits looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callbacks these services make sign their own requests either way
/// (the credential scope is what routes them).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one AWS Batch operation over its restJson1 wire.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("batch"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {uri}, got {status} with {payload}"
    );
    payload
}

/// Call one CloudWatch Logs operation over its awsJson1_1 wire.
async fn logs(router: &MultiServiceRouter, action: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{action}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The member at `pointer` of `document`, as a string.
fn member(case: &str, document: &Value, pointer: &str) -> String {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {document}"))
        .to_string()
}

/// Register one container job definition and answer its ARN.
async fn job_definition(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    container: Value,
) -> String {
    let answered = batch(
        router,
        case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": name,
            "type": "container",
            "containerProperties": container,
        }),
    )
    .await;
    member(case, &answered, "/jobDefinitionArn")
}

/// Submit one job to `queue` and answer its `(jobId, jobArn)`.
async fn submit(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue: &str,
    definition: &str,
) -> (String, String) {
    let answered = batch(
        router,
        case,
        "/v1/submitjob",
        &json!({"jobName": name, "jobQueue": queue, "jobDefinition": definition}),
    )
    .await;
    (
        member(case, &answered, "/jobId"),
        member(case, &answered, "/jobArn"),
    )
}

/// Poll `DescribeJobs` until `job_id` reports a terminal status, then answer its whole detail.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let request = json!({"jobs": [job_id]});
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answered = batch(router, case, "/v1/describejobs", &request).await;
        let detail = answered["jobs"][0].clone();
        if matches!(detail["status"].as_str(), Some(SUCCEEDED) | Some(FAILED)) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the job to reach {SUCCEEDED} or {FAILED}, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the stream `name` of `group` until CloudWatch Logs holds exactly the messages
/// `expected`, in that order, then answer them.
///
/// A wait for the CONDITION: the stream is created and its events delivered by the drain that
/// follows the transition they belong to, so a reader that happens to observe the job's
/// terminal status first looks again rather than concluding the container wrote nothing.
async fn until_events(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
    expected: &[&str],
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, events) = logs(
            router,
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": name}),
        )
        .await;
        let written: Vec<String> = events["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        if status == 200 && written == expected {
            return written;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stream {name:?} to hold {expected:?}, last {status} \
             with {events}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one row declares and what the job it submits must then report.
struct ProgramRow {
    case: &'static str,
    /// `ContainerProperties$command`, or `None` for a container that declares none — whose
    /// program is the one inside its own image.
    command: Option<&'static [&'static str]>,
    /// `ContainerProperties$environment`, as `(name, value)` pairs.
    environment: &'static [(&'static str, &'static str)],
    /// The terminal `JobStatus` the program's exit status decides.
    status: &'static str,
    /// `ContainerDetail$exitCode`, and `None` for a job whose container never ran.
    exit_code: Option<i64>,
    /// The lines the program wrote, which its log stream then holds.
    lines: &'static [&'static str],
    /// A fragment the `statusReason` must carry for a program nothing performed, and `None`
    /// for a container that exited (which reports [`CONTAINER_EXITED`]).
    not_performed: Option<&'static str>,
}

const PROGRAM_ROWS: &[ProgramRow] = &[
    ProgramRow {
        case: "a utility that writes one line",
        command: Some(&["echo", "hi"]),
        environment: &[],
        status: SUCCEEDED,
        exit_code: Some(0),
        lines: &["hi"],
        not_performed: None,
    },
    ProgramRow {
        case: "a utility named by its absolute path",
        command: Some(&["/usr/bin/true"]),
        environment: &[],
        status: SUCCEEDED,
        exit_code: Some(0),
        lines: &[],
        not_performed: None,
    },
    ProgramRow {
        case: "a utility that exits non-zero",
        command: Some(&["false"]),
        environment: &[],
        status: FAILED,
        exit_code: Some(1),
        lines: &[],
        not_performed: None,
    },
    ProgramRow {
        case: "a shell program writing two lines before a non-zero exit",
        command: Some(&["/bin/sh", "-c", "echo one; echo two; exit 3"]),
        environment: &[],
        status: FAILED,
        exit_code: Some(3),
        lines: &["one", "two"],
        not_performed: None,
    },
    ProgramRow {
        case: "a shell program branching on the job's own environment",
        command: Some(&[
            "/bin/sh",
            "-c",
            "if [ \"$MODE\" = fail ]; then echo refusing; exit 9; fi",
        ]),
        environment: &[("MODE", "fail")],
        status: FAILED,
        exit_code: Some(9),
        lines: &["refusing"],
        not_performed: None,
    },
    ProgramRow {
        case: "a container that declares no command at all",
        command: None,
        environment: &[],
        status: FAILED,
        exit_code: None,
        lines: &[],
        // The reason names the IMAGE whose own entrypoint was never started.
        not_performed: Some("busybox"),
    },
    ProgramRow {
        case: "a program this emulator performs none of",
        command: Some(&["/opt/app/train.py"]),
        environment: &[],
        status: FAILED,
        exit_code: None,
        lines: &[],
        not_performed: Some("/opt/app/train.py"),
    },
];

/// RULE: the program a placed job's container declares is what the job REPORTS — the status it
/// exited with decides the job's terminal status and is reported as the container's exit code,
/// the lines it wrote are in the stream the driver opened for its task, and a program this
/// emulator performs none of claims no run at all.
#[tokio::test]
async fn the_declared_program_decides_what_a_placed_job_reports() {
    let router = build_router();
    let fixture = "the estate";

    // A MANAGED compute environment MAINTAINING capacity, so every job of the table is placed
    // as soon as the scheduler looks at it rather than waiting for an instance to be launched.
    batch(
        &router,
        fixture,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": "ce-programs",
            "type": "MANAGED",
            "state": "ENABLED",
            "computeResources": {
                "type": "EC2",
                "minvCpus": 4,
                "maxvCpus": 16,
                "instanceTypes": ["optimal"],
                "subnets": ["subnet-00000000000000001"],
                "securityGroupIds": ["sg-00000000000000001"],
            },
        }),
    )
    .await;
    let environment_valid = Instant::now() + DEADLINE;
    loop {
        let described = batch(
            &router,
            fixture,
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": ["ce-programs"]}),
        )
        .await;
        if described["computeEnvironments"][0]["status"].as_str() == Some(VALID) {
            break;
        }
        assert!(
            Instant::now() < environment_valid,
            "case {fixture:?}: expected the compute environment to reach {VALID}, last \
             {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let queue = member(
        fixture,
        &batch(
            &router,
            fixture,
            "/v1/createjobqueue",
            &json!({
                "jobQueueName": "jq-programs",
                "priority": 1,
                "state": "ENABLED",
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": "ce-programs"}],
            }),
        )
        .await,
        "/jobQueueArn",
    );

    // Every job is submitted first and read back afterwards: each settles on its own clock, so
    // the whole table costs one run rather than one per row.
    let mut submitted: Vec<(&ProgramRow, String)> = Vec::new();
    for (index, row) in PROGRAM_ROWS.iter().enumerate() {
        let case = row.case;
        let mut container = json!({
            // A bare name is a reference to a registry this emulator does not hold, so
            // nothing is decided against the image fetch and the program is what decides.
            "image": "busybox",
            "resourceRequirements": [
                {"type": "VCPU", "value": "1"},
                {"type": "MEMORY", "value": "512"},
            ],
        });
        if let Some(command) = row.command {
            container["command"] = json!(command);
        }
        if !row.environment.is_empty() {
            container["environment"] = Value::Array(
                row.environment
                    .iter()
                    .map(|(name, value)| json!({"name": name, "value": value}))
                    .collect(),
            );
        }
        let definition =
            job_definition(&router, case, &format!("jd-program-{index}"), container).await;
        let (job_id, _) = submit(
            &router,
            case,
            &format!("job-program-{index}"),
            &queue,
            &definition,
        )
        .await;
        submitted.push((row, job_id));
    }

    for (row, job_id) in &submitted {
        let case = row.case;
        let detail = until_terminal(&router, case, job_id).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the job to end {:?}, got {detail}",
            row.status
        );
        let reason = member(case, &detail, "/statusReason");
        match row.not_performed {
            None => assert_eq!(
                reason, CONTAINER_EXITED,
                "case {case:?}: a container that exited says so, got {reason:?}"
            ),
            Some(fragment) => {
                assert!(
                    reason.contains(fragment) && !reason.contains(CONTAINER_EXITED),
                    "case {case:?}: expected a reason naming {fragment:?} and no exit, got \
                     {reason:?}"
                );
            }
        }
        assert_eq!(
            detail["container"]["exitCode"].as_i64(),
            row.exit_code,
            "case {case:?}: expected the exit code {:?}, got {detail}",
            row.exit_code
        );

        // The attempt the placement made reports the same reading, and the instants of the run
        // it performed: one placement, one attempt.
        let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            attempts.len(),
            1,
            "case {case:?}: a placed job has had exactly one placement, got {detail}"
        );
        let attempt = attempts[0].clone();
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            row.exit_code,
            "case {case:?}: the attempt reports the same exit code, got {attempt}"
        );
        assert_eq!(
            attempt["startedAt"].is_null(),
            row.exit_code.is_none(),
            "case {case:?}: startedAt belongs to an attempt whose container ran, got {attempt}"
        );
        assert!(
            attempt["stoppedAt"].as_i64().is_some(),
            "case {case:?}: an attempt that has ended reports when its task stopped, got \
             {attempt}"
        );

        // And what the program WROTE is in the stream the `awslogs` driver opened for its
        // task, where CloudWatch Logs holds it.
        let stream = member(case, &attempt, "/container/logStreamName");
        until_events(&router, case, JOB_LOG_GROUP, &stream, row.lines).await;
    }
}

/// RULE (the CloudFormation-deployed half): a job submitted to the queue a stack declared is
/// reported by `GetJobQueueSnapshot` in `frontOfQueue.jobs` while it waits for the capacity the
/// declared MANAGED compute environment provisions, and then RUNS — the same rule as above, on
/// an estate no Batch API call created.
#[tokio::test]
async fn a_job_on_a_cloudformation_deployed_queue_is_queued_and_then_runs() {
    let router = build_router();
    let case = "a stack-deployed managed EC2 queue";
    let stack_name = "batch-run-estate";

    let template = json!({
        "Resources": {
            "Environment": {
                "Type": "AWS::Batch::ComputeEnvironment",
                "Properties": {
                    "ComputeEnvironmentName": "run-ce",
                    "Type": "MANAGED",
                    "State": "ENABLED",
                    "ComputeResources": {
                        "Type": "EC2",
                        // Maintaining NO vCPUs: the environment has to launch capacity for the
                        // job, which is what makes the job observable in the queue's snapshot.
                        "MinvCpus": 0,
                        "MaxvCpus": 2,
                        "InstanceTypes": ["optimal"],
                        "Subnets": ["subnet-00000000000000001"],
                        "SecurityGroupIds": ["sg-00000000000000001"]
                    }
                }
            },
            "Queue": {
                "Type": "AWS::Batch::JobQueue",
                "Properties": {
                    "JobQueueName": "run-jq",
                    "Priority": 1,
                    "State": "ENABLED",
                    "ComputeEnvironmentOrder": [
                        { "Order": 1, "ComputeEnvironment": { "Ref": "Environment" } }
                    ]
                }
            }
        },
        "Outputs": {
            "ComputeEnvironmentArn": { "Value": { "Ref": "Environment" } },
            "JobQueueArn": { "Value": { "Ref": "Queue" } }
        }
    })
    .to_string();

    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );
    let deadline = Instant::now() + DEADLINE;
    let described = loop {
        let (status, body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to settle, last {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );

    let outputs: BTreeMap<String, String> = outputs_of(&described);
    let queue_arn = outputs
        .get("JobQueueArn")
        .unwrap_or_else(|| panic!("case {case:?}: no JobQueueArn output in {described}"))
        .clone();
    let environment_arn = outputs
        .get("ComputeEnvironmentArn")
        .unwrap_or_else(|| panic!("case {case:?}: no ComputeEnvironmentArn output in {described}"))
        .clone();

    // The queue the stack published is one AWS Batch holds, VALID and backed by the declared
    // environment: that is what a client reads before it submits.
    let queues = batch(
        &router,
        case,
        "/v1/describejobqueues",
        &json!({"jobQueues": [&queue_arn]}),
    )
    .await;
    assert_eq!(
        queues["jobQueues"][0]["status"].as_str(),
        Some(VALID),
        "case {case:?}: expected the stack's queue to be {VALID}, got {queues}"
    );
    assert_eq!(
        queues["jobQueues"][0]["computeEnvironmentOrder"][0]["computeEnvironment"].as_str(),
        Some(environment_arn.as_str()),
        "case {case:?}: expected the queue to order the declared environment, got {queues}"
    );

    let definition = job_definition(
        &router,
        case,
        "jd-run",
        json!({
            "image": "public.ecr.aws/docker/library/busybox:1.36",
            "command": ["echo", "hi"],
            "resourceRequirements": [
                {"type": "VCPU", "value": "1"},
                {"type": "MEMORY", "value": "512"},
            ],
        }),
    )
    .await;
    let (job_id, job_arn) = submit(&router, case, "job-run", &queue_arn, &definition).await;

    // While it waits for the capacity the environment is launching, the job is AT THE FRONT of
    // its queue — the observation `GetJobQueueSnapshot` is published for. The wait is for that
    // CONDITION: a job that has already been placed has left the ordering, so the deadline
    // reports the status it had reached instead of looking forever.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = batch(
            &router,
            case,
            "/v1/getjobqueuesnapshot",
            &json!({"jobQueue": &queue_arn}),
        )
        .await;
        let queued: Vec<String> = snapshot["frontOfQueue"]["jobs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["jobArn"].as_str().map(str::to_string))
            .collect();
        if queued.contains(&job_arn) {
            break;
        }
        let detail = batch(&router, case, "/v1/describejobs", &json!({"jobs": [&job_id]})).await;
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {job_arn} in the queue's snapshot while it waited for \
             capacity, last {queued:?} with {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // And then it runs: the program its container declares exits zero, so the job SUCCEEDS.
    let detail = until_terminal(&router, case, &job_id).await;
    assert_eq!(
        detail["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: expected the stack-deployed queue's job to succeed, got {detail}"
    );
    assert_eq!(
        detail["container"]["exitCode"].as_i64(),
        Some(0),
        "case {case:?}: expected the exit code of the program it ran, got {detail}"
    );
    assert_eq!(
        member(case, &detail, "/statusReason"),
        CONTAINER_EXITED,
        "case {case:?}: a container that exited says so, got {detail}"
    );
    let stream = member(case, &detail, "/container/logStreamName");
    until_events(&router, case, JOB_LOG_GROUP, &stream, &["hi"]).await;
}
