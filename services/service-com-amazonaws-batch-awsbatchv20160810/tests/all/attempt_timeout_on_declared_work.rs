// SPDX-License-Identifier: CC0-1.0

//! The ATTEMPT TIMEOUT measured against the WORK a job's container program declares, end to
//! end over the wire.
//!
//! RULE under test: `JobTimeout$attemptDurationSeconds` is "the job timeout time (in seconds)
//! that's measured from the job attempt's `startedAt` timestamp. After this time passes, Batch
//! terminates your jobs if they aren't finished. The minimum value for the timeout is 60
//! seconds" (`smithy_query(query="shape", shape="JobTimeout")`). So what the timeout asks of
//! an attempt is whether its WORK FINISHED inside the duration the request allowed it: an
//! attempt whose work takes longer is TERMINATED BY THE SERVICE — the job reaches the terminal
//! `FAILED` (`JobDetail$status` targets `JobStatus`, whose terminal pair is
//! `SUCCEEDED`/`FAILED`), carries the `statusReason` of that failure class on the job and on
//! the attempt it ended, reports `isTerminated` ("indicates whether the job is terminated")
//! and reports NO `ContainerDetail$exitCode` ("the exit code returned upon completion"),
//! because the container it was running never exited. An attempt whose work fits inside its
//! allowance ends on its own outcome instead, and a job that states no timeout is never
//! terminated for one however long its work takes.
//!
//! How long the work takes is the duration the program itself STATES: the seconds a `sleep`
//! names are that program's own account of how long it took and no clock of this emulator is
//! spent on them (`job_settles_whatever_wait_its_container_states.rs` pins that reading), so
//! the allowance is compared against that account — wherever the program is performed, in this
//! process or as the Amazon ECS task a Fargate job's container runs as.
//!
//! The rows differ along what the rule generalises over: HOW LONG the work states (from a
//! minute and a half to ten minutes, and none at all), WHERE the wait stands in the program
//! (the whole program, after a line it wrote, before a step that would exit non-zero, spread
//! over two waits), HOW the seconds are spelled (whole, fractional, with a unit suffix), and
//! WHAT the request allows the attempt (the documented minimum, ten minutes, a day, and no
//! timeout at all) — so each row's verdict follows from the comparison and not from one
//! recorded pair of numbers.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{describe_job, job_definition, job_queue, Batch};

/// `JobStatus` members this suite names, spelled out rather than imported so a wrong constant
/// in the implementation cannot make it pass. Source:
/// `smithy_query(query="shape", shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// How long one job of this suite is given to settle, polled for the CONDITION rather than
/// waited out. Far above the spans this service gives a job's moves
/// (`state::JOB_PHASE_MILLIS`) and far below the shortest wait any row's program states, so a
/// loaded host delays a row instead of breaking it.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// What one row's job must come to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The attempt's work outlasted its allowance, so the SERVICE terminated it.
    TimedOut,
    /// The work fits inside the allowance (or nothing bounds it), so the job ends on the
    /// status its program exited with, carrying that exit code.
    OnItsOwn(&'static str, i64),
}

/// One row: the program the job's container declares, what the submit allows its attempt, and
/// what the job must therefore report.
struct Row {
    case: &'static str,
    /// The `sh -c` text the container runs.
    program: &'static str,
    /// `timeout.attemptDurationSeconds`, or `None` for a submit that states none.
    allowance: Option<i64>,
    ends: Ends,
}

const ROWS: &[Row] = &[
    Row {
        case: "ten minutes of work under the documented minimum allowance",
        program: "sleep 600",
        allowance: Some(60),
        ends: Ends::TimedOut,
    },
    Row {
        case: "work that wrote a line and then waited past its allowance",
        program: "echo working; sleep 150",
        allowance: Some(60),
        ends: Ends::TimedOut,
    },
    Row {
        case: "work that would have exited non-zero after waiting past its allowance",
        program: "sleep 300; exit 7",
        allowance: Some(60),
        ends: Ends::TimedOut,
    },
    Row {
        case: "two waits that add up past the allowance",
        program: "sleep 400; echo half; sleep 400",
        allowance: Some(600),
        ends: Ends::TimedOut,
    },
    Row {
        case: "fractional seconds past the allowance",
        program: "sleep 90.5",
        allowance: Some(60),
        ends: Ends::TimedOut,
    },
    Row {
        case: "a wait spelled in minutes, past the allowance",
        program: "sleep 5m",
        allowance: Some(60),
        ends: Ends::TimedOut,
    },
    Row {
        case: "work well inside a ten-minute allowance",
        program: "sleep 90; exit 0",
        allowance: Some(600),
        ends: Ends::OnItsOwn(SUCCEEDED, 0),
    },
    Row {
        case: "failing work inside a day-long allowance keeps its own exit status",
        program: "sleep 120; exit 7",
        allowance: Some(86_400),
        ends: Ends::OnItsOwn(FAILED, 7),
    },
    Row {
        case: "work that waits for nothing at all under the minimum allowance",
        program: "echo quick; exit 0",
        allowance: Some(60),
        ends: Ends::OnItsOwn(SUCCEEDED, 0),
    },
    Row {
        case: "ten minutes of work and no timeout at all",
        program: "sleep 600; exit 0",
        allowance: None,
        ends: Ends::OnItsOwn(SUCCEEDED, 0),
    },
    Row {
        case: "ten minutes of failing work and no timeout at all",
        program: "sleep 600; exit 3",
        allowance: None,
        ends: Ends::OnItsOwn(FAILED, 3),
    },
];

/// `containerProperties` running `program` under `sh -c`.
fn container_running(program: &str) -> Value {
    json!({
        "image": "busybox",
        "command": ["/bin/sh", "-c", program],
        "resourceRequirements": [
            {"type": "VCPU", "value": "1"},
            {"type": "MEMORY", "value": "512"},
        ],
    })
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer its detail.
async fn until_terminal(batch: &Batch, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let detail = describe_job(batch, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default();
        if status == SUCCEEDED || status == FAILED {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected a terminal status inside {SETTLES_WITHIN:?}, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: an attempt whose work states more than its `attemptDurationSeconds` allows is
/// terminated by the service — `FAILED`, with the attempt-timeout reason on the job and on the
/// attempt, `isTerminated`, and no exit code — while an attempt whose work fits inside its
/// allowance, and one bounded by no timeout at all, ends on the status its program exited with.
#[tokio::test]
async fn an_attempt_is_terminated_when_its_work_outlasts_the_time_it_was_allowed() {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, "ce-stated-work", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-stated-work", &environment).await;

    // Every job is submitted BEFORE any is waited on, so the spans the clock owes them run
    // together rather than one after another.
    let mut submitted: Vec<(&Row, String)> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        let (definition, _) = job_definition(
            &batch,
            &format!("jd-stated-work-{index}"),
            container_running(row.program),
        )
        .await;
        let mut request = json!({
            "jobName": format!("stated-work-{index}"),
            "jobQueue": &queue_arn,
            "jobDefinition": definition,
        });
        if let Some(seconds) = row.allowance {
            request["timeout"] = json!({"attemptDurationSeconds": seconds});
        }
        let answered = batch.ok("/v1/submitjob", request).await;
        let job_id = answered["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {}: expected a jobId in {answered}", row.case))
            .to_string();
        submitted.push((row, job_id));
    }

    for (row, job_id) in &submitted {
        let case = row.case;
        let detail = until_terminal(&batch, case, job_id).await;
        match row.ends {
            Ends::TimedOut => {
                assert_eq!(
                    detail["status"].as_str(),
                    Some(FAILED),
                    "case {case}: work past its allowance is terminated {FAILED}, got {detail}"
                );
                assert_eq!(
                    detail["statusReason"].as_str(),
                    Some(state::ATTEMPT_TIMEOUT_REASON),
                    "case {case}: expected the attempt-timeout reason, got {detail}"
                );
                assert_eq!(
                    detail["attempts"][0]["statusReason"].as_str(),
                    Some(state::ATTEMPT_TIMEOUT_REASON),
                    "case {case}: the attempt reports why it ended, got {detail}"
                );
                assert_eq!(
                    detail["isTerminated"].as_bool(),
                    Some(true),
                    "case {case}: the SERVICE terminated the job, got {detail}"
                );
                assert_eq!(
                    detail["isCancelled"].as_bool(),
                    Some(false),
                    "case {case}: nothing cancelled the job, got {detail}"
                );
                assert!(
                    detail["container"]["exitCode"].is_null(),
                    "case {case}: no exit code is invented for a container the service \
                     stopped, got {detail}"
                );
                assert!(
                    detail["attempts"][0]["container"]["exitCode"].is_null(),
                    "case {case}: the attempt reports no exit code either, got {detail}"
                );
            }
            Ends::OnItsOwn(status, exit) => {
                assert_eq!(
                    detail["status"].as_str(),
                    Some(status),
                    "case {case}: expected the job to end {status} on its own work, got {detail}"
                );
                assert_ne!(
                    detail["statusReason"].as_str(),
                    Some(state::ATTEMPT_TIMEOUT_REASON),
                    "case {case}: nothing timed this attempt out, got {detail}"
                );
                assert_eq!(
                    detail["container"]["exitCode"].as_i64(),
                    Some(exit),
                    "case {case}: expected the status its program exited with, got {detail}"
                );
            }
        }
        // A terminal job is FROZEN: a later read reports the same document, so what the
        // timeout decided does not drift with the clock.
        let again = describe_job(&batch, job_id).await;
        assert_eq!(
            again, detail,
            "case {case}: a second read must report the settled job unchanged, got {again}"
        );
    }
}

/// The network, the execution role, the Fargate compute environment and the queue the task
/// rows below are submitted to; answers the router, the queue name and the role ARN.
///
/// Built the way this crate's other Fargate suites build it: through Amazon EC2's own API, so
/// every id the placement names is one that service minted and holds, and with a way off the
/// VPC because the fetch of the image the containers run has to reach the registry serving it.
async fn fargate_fixture() -> (MultiServiceRouter, String, String) {
    use crate::fargate_job_execution_role_and_task_tags::{
        ec2, element, execution_role, router, DEFAULT_ROUTE, PUBLISHED_EXECUTION_POLICY, REGION,
        SUBNET_CIDR, VPC_CIDR,
    };
    let router = router();
    let vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", SUBNET_CIDR),
                ("AvailabilityZone", &format!("{REGION}a")),
            ],
        )
        .await,
        "subnetId",
    );
    let security_group = element(
        &ec2(
            &router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "allowance-group"),
                ("GroupDescription", "the-group-this-suites-tasks-run-under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    let gateway = element(
        &ec2(&router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        &router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let table = element(
        &ec2(&router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        &router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
    )
    .await;
    ec2(
        &router,
        "CreateRoute",
        &[
            ("RouteTableId", table.as_str()),
            ("DestinationCidrBlock", DEFAULT_ROUTE),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    let role = execution_role(&router, "allowanceExec", &[PUBLISHED_EXECUTION_POLICY]).await;
    let environment = "ce-task-allowance";
    let queue = "q-task-allowance";
    crate::fargate_job_execution_role_and_task_tags::batch(
        &router,
        "/v1/createcomputeenvironment",
        json!({
            "computeEnvironmentName": environment,
            "type": "MANAGED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": [&subnet],
                "securityGroupIds": [&security_group],
            },
        }),
    )
    .await;
    crate::fargate_job_execution_role_and_task_tags::batch(
        &router,
        "/v1/createjobqueue",
        json!({
            "jobQueueName": queue,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
        }),
    )
    .await;
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let body = crate::fargate_job_execution_role_and_task_tags::batch(
            &router,
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [environment]}),
        )
        .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some(state::STATUS_VALID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the compute environment never reached {}, last {body}",
            state::STATUS_VALID
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    (router, queue.to_string(), role)
}

/// RULE: the same comparison decides a job whose container runs as an Amazon ECS task — the
/// work's own account of its duration against the attempt's allowance — so a Fargate job whose
/// program states more work than its `attemptDurationSeconds` allows is terminated by the
/// service, while one inside its allowance ends on the status its task's container exited with.
///
/// The task path is where a Fargate job's program is really performed (`state::ecs_task`), so
/// the rule is asserted there as well as in this process: the services that own the facts are
/// mounted on one engine and the network the placement names is Amazon EC2's.
#[tokio::test]
async fn the_allowance_also_terminates_a_job_whose_container_runs_as_a_task() {
    let (router, queue, role) = fargate_fixture().await;
    // The rows differ along how long the work states and what the attempt is allowed, which is
    // what the verdict follows from on this path too.
    let rows: &[(&str, &[&str], i64, Option<i64>)] = &[
        (
            "ten minutes of work under the minimum allowance",
            &["/bin/sh", "-c", "sleep 600"],
            60,
            None,
        ),
        (
            "work that wrote a line and waited past its allowance",
            &["/bin/sh", "-c", "echo working; sleep 300"],
            120,
            None,
        ),
        (
            "work well inside a day-long allowance",
            &["/bin/sh", "-c", "sleep 120; exit 5"],
            86_400,
            Some(5),
        ),
    ];

    let mut submitted: Vec<(&str, Option<i64>, String)> = Vec::new();
    for (index, (case, command, allowance, own_exit)) in rows.iter().enumerate() {
        let definition = format!("jd-task-allowance-{index}");
        crate::fargate_job_execution_role_and_task_tags::batch(
            &router,
            "/v1/registerjobdefinition",
            json!({
                "jobDefinitionName": &definition,
                "type": "container",
                "platformCapabilities": ["FARGATE"],
                "containerProperties": {
                    "image": crate::fargate_job_execution_role_and_task_tags::IMAGE,
                    "command": command,
                    "executionRoleArn": &role,
                    "networkConfiguration": {"assignPublicIp": "ENABLED"},
                    "resourceRequirements": [
                        {"type": "VCPU", "value": "0.25"},
                        {"type": "MEMORY", "value": "512"},
                    ],
                },
            }),
        )
        .await;
        let answered = crate::fargate_job_execution_role_and_task_tags::batch(
            &router,
            "/v1/submitjob",
            json!({
                "jobName": format!("task-allowance-{index}"),
                "jobQueue": &queue,
                "jobDefinition": &definition,
                "timeout": {"attemptDurationSeconds": allowance},
            }),
        )
        .await;
        let job_id = answered["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a jobId in {answered}"))
            .to_string();
        submitted.push((case, *own_exit, job_id));
    }

    for (case, own_exit, job_id) in &submitted {
        let deadline = Instant::now() + SETTLES_WITHIN;
        let detail = loop {
            let body = crate::fargate_job_execution_role_and_task_tags::batch(
                &router,
                "/v1/describejobs",
                json!({"jobs": [job_id]}),
            )
            .await;
            let detail = body["jobs"][0].clone();
            let status = detail["status"].as_str().unwrap_or_default();
            if status == SUCCEEDED || status == FAILED {
                break detail;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: expected a terminal status inside {SETTLES_WITHIN:?}, last {detail}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        match own_exit {
            None => {
                assert_eq!(
                    (
                        detail["status"].as_str(),
                        detail["statusReason"].as_str(),
                        detail["isTerminated"].as_bool(),
                    ),
                    (
                        Some(FAILED),
                        Some(state::ATTEMPT_TIMEOUT_REASON),
                        Some(true),
                    ),
                    "case {case}: work past its allowance is terminated by the service, \
                     got {detail}"
                );
                assert!(
                    detail["container"]["exitCode"].is_null(),
                    "case {case}: no exit code is invented for a container the service \
                     stopped, got {detail}"
                );
            }
            Some(exit) => {
                assert_eq!(
                    detail["container"]["exitCode"].as_i64(),
                    Some(*exit),
                    "case {case}: expected the status its task's container exited with, \
                     got {detail}"
                );
                assert_ne!(
                    detail["statusReason"].as_str(),
                    Some(state::ATTEMPT_TIMEOUT_REASON),
                    "case {case}: nothing timed this attempt out, got {detail}"
                );
            }
        }
    }
}

/// RULE: the allowance is measured against the work, so a terminated attempt reports the
/// DEADLINE that allowance names as its stop — `attemptDurationSeconds` after the attempt
/// started, on the job and on the attempt it ended, whatever instant the service noticed it —
/// and the job still replays the `timeout` it was submitted with.
#[tokio::test]
async fn a_terminated_attempt_reports_its_submitted_timeout_and_its_deadline() {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, "ce-timeout-record", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-timeout-record", &environment).await;
    // The rows differ along the allowance a timed-out attempt was given, which is what the
    // deadline below is measured with.
    for (index, allowance) in [60_i64, 120, 600].into_iter().enumerate() {
        let case = format!("an attempt allowed {allowance}s");
        let (definition, _) = job_definition(
            &batch,
            &format!("jd-timeout-record-{index}"),
            container_running("sleep 4h"),
        )
        .await;
        let answered = batch
            .ok(
                "/v1/submitjob",
                json!({
                    "jobName": format!("timeout-record-{index}"),
                    "jobQueue": &queue_arn,
                    "jobDefinition": definition,
                    "timeout": {"attemptDurationSeconds": allowance},
                }),
            )
            .await;
        let job_id = answered["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a jobId in {answered}"))
            .to_string();
        let detail = until_terminal(&batch, &case, &job_id).await;
        assert_eq!(
            detail["timeout"],
            json!({"attemptDurationSeconds": allowance}),
            "case {case}: expected the submitted timeout to be replayed, got {detail}"
        );
        let started_at = detail["startedAt"].as_i64().unwrap_or_else(|| {
            panic!("case {case}: a job whose attempt ran reports startedAt, got {detail}")
        });
        let stopped_at = detail["stoppedAt"].as_i64().unwrap_or_else(|| {
            panic!("case {case}: a job that stopped reports stoppedAt, got {detail}")
        });
        assert_eq!(
            stopped_at,
            started_at + allowance * 1_000,
            "case {case}: expected the stop to be the attempt's own deadline \
             ({started_at} + {allowance}s) — the instant the allowance ran out, not the one \
             the service noticed — got {detail}"
        );
        assert_eq!(
            detail["attempts"][0]["stoppedAt"].as_i64(),
            Some(started_at + allowance * 1_000),
            "case {case}: the attempt the allowance ended reports the same deadline, \
             got {detail}"
        );
    }
}
