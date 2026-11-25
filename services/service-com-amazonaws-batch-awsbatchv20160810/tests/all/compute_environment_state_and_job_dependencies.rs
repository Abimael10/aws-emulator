// SPDX-License-Identifier: CC0-1.0

//! What GATES the placement of a submitted job — the state of the compute environment behind
//! its queue, and the jobs its own `dependsOn` names — on the capacity an Amazon ECS TASK runs
//! on, driven end to end over the wire against the services that own the resources involved.
//!
//! Two rules are under test, both on a `MANAGED` Fargate environment, whose placements are tasks
//! of the Amazon ECS cluster backing it:
//!
//! 1. **A compute environment's `state` decides whether the scheduler places jobs on it, and the
//!    call that changes that state is what withholds or releases the jobs already submitted.**
//!    `UpdateComputeEnvironmentRequest$state`: "If the state is `ENABLED`, then the Batch
//!    scheduler can attempt to place jobs from an associated job queue on the compute resources
//!    within the environment"; "If the state is `DISABLED`, then the Batch scheduler doesn't
//!    attempt to place jobs within the environment." So a job on an `ENABLED` environment is
//!    placed — it leaves `RUNNABLE`, records the one attempt its placement started carrying the
//!    `taskArn` of the Amazon ECS task it runs as, reports a `startedAt` and settles on the
//!    status its container exited with — while a job whose only environment is `DISABLED` waits
//!    in `RUNNABLE` with `attempts` empty and no `startedAt`, and is released by
//!    `UpdateComputeEnvironment(state=ENABLED)` with no resubmission. `state` and `status` are
//!    independent: a `DISABLED` environment still comes to rest in `CEStatus$VALID`.
//!
//! 2. **A job is held in `PENDING` until every job its `dependsOn` names is terminal, then
//!    released by their success or ended by their failure.** `SubmitJobRequest$dependsOn` is "a
//!    list of dependencies for the job", echoed by `JobDetail$dependsOn`, and `JobStatus$PENDING`
//!    is where "the job is in your job queue but your job isn't ready to run" — so an unsatisfied
//!    dependency leaves the job there with no attempt and no `startedAt`; once they have all
//!    SUCCEEDED the job is placed like any other and starts no earlier than the last of them
//!    stopped; and a dependency that FAILED ends the job `FAILED` with nothing of a run on it —
//!    no attempt, no `startedAt`, no exit code — and a `statusReason` ("a short, human-readable
//!    string to provide more details for the current status of the job") naming that failure.
//!
//! The rows of the first test differ along the `CEState` the environment holds and along HOW it
//! came to hold it (stated on the create, or written by a later update). The rows of the second
//! differ along how many jobs the dependent names and along what those jobs did — all succeeded,
//! one failed — which is what the rule generalises over.
//!
//! WHERE this sits beside the suites that assert the same two rules on OTHER capacity.
//! `array_jobs_and_placement.rs` and `compute_capacity_dispatch.rs` drive the state gate, and
//! `job_lifecycle.rs` the `PENDING` hold, against a harness that mounts Batch WITHOUT Amazon ECS:
//! there no cluster backs the environment, so the job's program is performed in this process and
//! no task is ever made. This suite is the same two rules where the placement is an Amazon ECS
//! task — the capacity a `MANAGED` Fargate environment actually offers — which is what makes the
//! `taskArn` of each attempt, the cross-check that Amazon ECS answers for that task, and a
//! dependency that failed because its OWN container exited non-zero observable at all.
//!
//! Sources: `smithy_query(query="doc",shape="UpdateComputeEnvironmentRequest",member="state")`
//! and `smithy_query(query="shape",shape="CEState,CEStatus")` for the environment's state and
//! status; `smithy_query(query="doc",shape="SubmitJobRequest",member="dependsOn")` and
//! `smithy_query(query="shape",shape="JobStatus,JobDetail,AttemptDetail")` for the dependency
//! gate and what a held, released or blocked job reports.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

// The harness the sibling Fargate suite holds for every suite of this binary that drives a job
// onto Fargate capacity: the five services on one engine, one call per protocol, the network ids
// Amazon EC2 mints, the execution role IAM holds, and the image, Region and waits they share.
use crate::fargate_job_execution_role_and_task_tags::{
    batch, ec2, element, execution_role, router, rpc, DEADLINE, DEFAULT_ROUTE, FAILED, IMAGE,
    POLL_INTERVAL, PUBLISHED_EXECUTION_POLICY, REGION, SUBNET_CIDR, SUCCEEDED, VALID, VPC_CIDR,
};

/// The `CEState` values a compute environment holds, which is also the `jobQueue` state every
/// queue below is created in and the `assignPublicIp` the tasks are given. Source:
/// `smithy_query(query="shape",shape="CEState,JQState,AssignPublicIp")`.
const ENABLED: &str = "ENABLED";
const DISABLED: &str = "DISABLED";

/// The `JobStatus` values read below that the sibling suite does not already name. Source:
/// `smithy_query(query="shape",shape="JobStatus")`.
const SUBMITTED: &str = "SUBMITTED";
const PENDING: &str = "PENDING";
const RUNNABLE: &str = "RUNNABLE";

/// The `statusReason` AWS Batch gives a job its dependency failed. Spelled out here rather than
/// imported from the implementation, so a wrong constant in the implementation cannot make this
/// suite pass.
const DEPENDENCY_FAILED_REASON: &str = "Dependent Job failed";

/// How long the program of a job another job DEPENDS on declares it is busy for, in seconds: long
/// enough that its dependent is observably held while it runs. Nothing here depends on how fast
/// the host is — the hold is read by polling, not by waiting this out.
const PARENT_WORK_SECONDS: u32 = 3;

/// The subnet and security group a Fargate compute environment's tasks are placed in, built
/// through Amazon EC2's own API — with a way off the VPC, because the fetch of the image the
/// containers run has to reach the registry that serves it.
struct Network {
    subnet: String,
    security_group: String,
}

/// Stand that network up, with the security group named after `prefix`.
async fn network(router: &MultiServiceRouter, prefix: &str) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
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
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("{prefix}-group")),
                ("GroupDescription", "the-group-this-suites-tasks-run-under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
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
    let table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
    )
    .await;
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
    Network {
        subnet,
        security_group,
    }
}

/// A `MANAGED` Fargate compute environment named `<prefix>-ce` in `ce_state` — stated on the
/// create when `ce_state` is `Some` — and an `ENABLED` queue over it, both polled to `VALID`.
/// Answers the two names.
async fn environment_and_queue(
    router: &MultiServiceRouter,
    prefix: &str,
    network: &Network,
    ce_state: Option<&str>,
) -> (String, String) {
    let environment = format!("{prefix}-ce");
    let queue = format!("{prefix}-queue");
    let mut request = json!({
        "computeEnvironmentName": &environment,
        "type": "MANAGED",
        "computeResources": {
            "type": "FARGATE",
            "maxvCpus": 8,
            "subnets": [&network.subnet],
            "securityGroupIds": [&network.security_group],
        },
    });
    if let Some(ce_state) = ce_state {
        request["state"] = json!(ce_state);
    }
    batch(router, "/v1/createcomputeenvironment", request).await;
    batch(
        router,
        "/v1/createjobqueue",
        json!({
            "jobQueueName": &queue,
            "state": ENABLED,
            "priority": 10,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment}],
        }),
    )
    .await;
    until_environment(router, prefix, &environment, ce_state.unwrap_or(ENABLED)).await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            "/v1/describejobqueues",
            json!({"jobQueues": [&queue]}),
        )
        .await;
        if body["jobQueues"][0]["status"].as_str() == Some(VALID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup {prefix}: the job queue never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    (environment, queue)
}

/// Look again until `environment` reports `VALID` with the `state` the caller expects, and answer
/// its detail: `state` and `status` are independent members, so a `DISABLED` environment is
/// waited for in exactly the same `VALID` status an `ENABLED` one comes to rest in.
async fn until_environment(
    router: &MultiServiceRouter,
    case: &str,
    environment: &str,
    expected_state: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [environment]}),
        )
        .await;
        let detail = body["computeEnvironments"][0].clone();
        if detail["status"].as_str() == Some(VALID)
            && detail["state"].as_str() == Some(expected_state)
        {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the environment {VALID} and {expected_state}, got {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Register a Fargate container job definition called `name` running `command` as `role`.
async fn job_definition(router: &MultiServiceRouter, name: &str, role: &str, command: Value) {
    batch(
        router,
        "/v1/registerjobdefinition",
        json!({
            "jobDefinitionName": name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": {
                "image": IMAGE,
                "command": command,
                // "For jobs that run on Fargate resources, you must provide an execution role"
                // (`ContainerProperties$executionRoleArn`).
                "executionRoleArn": role,
                // The public address an internet gateway carries traffic for, which is what lets
                // the task's interface reach the registry the image names.
                "networkConfiguration": {"assignPublicIp": ENABLED},
                // The one supported Fargate pair this suite runs under
                // (`ResourceRequirement$value`).
                "resourceRequirements": [
                    {"type": "VCPU", "value": "0.25"},
                    {"type": "MEMORY", "value": "512"},
                ],
            },
        }),
    )
    .await;
}

/// Submit one job and answer the id the service minted for it.
async fn submit(
    router: &MultiServiceRouter,
    name: &str,
    queue: &str,
    definition: &str,
    depends_on: &[String],
) -> String {
    let mut request = json!({
        "jobName": name,
        "jobQueue": queue,
        "jobDefinition": definition,
    });
    if !depends_on.is_empty() {
        request["dependsOn"] = Value::Array(
            depends_on
                .iter()
                .map(|job_id| json!({"jobId": job_id}))
                .collect(),
        );
    }
    let answered = batch(router, "/v1/submitjob", request).await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: expected a jobId in {answered}"))
        .to_string()
}

/// The `JobDetail` of one job.
async fn describe(router: &MultiServiceRouter, job_id: &str) -> Value {
    let body = batch(router, "/v1/describejobs", json!({"jobs": [job_id]})).await;
    body["jobs"][0].clone()
}

/// Look again until `job_id`'s detail satisfies `settled`, and answer that detail.
async fn until_job<F>(router: &MultiServiceRouter, case: &str, job_id: &str, settled: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = describe(router, job_id).await;
        if settled(&detail) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached what this row waits for, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a detail reports one of the two terminal `JobStatus` values.
fn is_terminal(detail: &Value) -> bool {
    matches!(detail["status"].as_str(), Some(SUCCEEDED) | Some(FAILED))
}

/// Assert that `detail` reports NOTHING of a run: no attempt, no `startedAt`, no `stoppedAt`, no
/// task and no exit code — which is what a job no placement ever took looks like.
fn assert_nothing_ran(case: &str, detail: &Value) {
    assert_eq!(
        detail["attempts"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected no attempt for a job nothing placed, got {detail}"
    );
    assert_eq!(
        detail["startedAt"],
        Value::Null,
        "case {case:?}: expected no startedAt on a job nothing started, got {detail}"
    );
    assert_eq!(
        detail["stoppedAt"],
        Value::Null,
        "case {case:?}: expected no stoppedAt on a job nothing ran, got {detail}"
    );
    assert_eq!(
        detail["container"]["taskArn"],
        Value::Null,
        "case {case:?}: expected no task on a job nothing placed, got {detail}"
    );
    assert_eq!(
        detail["container"]["exitCode"],
        Value::Null,
        "case {case:?}: expected no exit code on a job nothing ran, got {detail}"
    );
}

/// Assert that `detail` reports the ONE attempt a placement starts, carrying the Amazon ECS task
/// the job ran as — and that Amazon ECS answers for that task — and answer its ARN.
async fn assert_placed_as_one_task(
    router: &MultiServiceRouter,
    case: &str,
    detail: &Value,
) -> String {
    assert_eq!(
        detail["attempts"].as_array().map(Vec::len),
        Some(1),
        "case {case:?}: expected exactly the one attempt its placement started, got {detail}"
    );
    let task_arn = detail["attempts"][0]["container"]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task the job ran as, got {detail}"))
        .to_string();
    assert_eq!(
        detail["container"]["taskArn"].as_str(),
        Some(task_arn.as_str()),
        "case {case:?}: expected the job's container to name the same task, got {detail}"
    );
    let cluster = task_arn
        .split('/')
        .nth(1)
        .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {task_arn}"))
        .to_string();
    let answered = rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        "DescribeTasks",
        json!({"cluster": &cluster, "tasks": [&task_arn]}),
    )
    .await;
    assert_eq!(
        answered["failures"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected Amazon ECS to answer for {task_arn}, got {answered}"
    );
    assert_eq!(
        answered["tasks"][0]["taskArn"].as_str(),
        Some(task_arn.as_str()),
        "case {case:?}: expected the task the attempt names, got {answered}"
    );
    task_arn
}

/// What one row of the withholding table declares: how the environment behind the job's queue
/// came to be `DISABLED`.
struct WithheldRow {
    case: &'static str,
    prefix: &'static str,
    /// The `state` the create states, and `None` for a create that states none at all (the
    /// service's own default) which a later `UpdateComputeEnvironment` then changes.
    created_state: Option<&'static str>,
    /// Whether the environment is DISABLED by an update after it was created.
    disabled_by_update: bool,
}

/// The rows: an environment `DISABLED` from its create, and one a later update disabled — the two
/// ways the state the rule turns on can be written.
const WITHHELD_ROWS: &[WithheldRow] = &[
    WithheldRow {
        case: "an environment DISABLED on its create",
        prefix: "cestate-born",
        created_state: Some(DISABLED),
        disabled_by_update: false,
    },
    WithheldRow {
        case: "an environment a later update disabled",
        prefix: "cestate-upd",
        created_state: None,
        disabled_by_update: true,
    },
];

/// RULE: a compute environment's `state` decides whether the scheduler places jobs from its
/// queues on it — an `ENABLED` one takes the placement, records the one attempt carrying the
/// Amazon ECS task the job runs as and settles on the status its container exited with, while a
/// `DISABLED` one takes none and leaves the job waiting in `RUNNABLE` with no attempt and no
/// `startedAt` — and `UpdateComputeEnvironment(state=ENABLED)` releases the job ALREADY
/// submitted, with no resubmission.
#[tokio::test]
async fn a_compute_environments_state_decides_whether_its_queues_jobs_are_placed() {
    let router = router();
    let network = network(&router, "cestate").await;
    let role = execution_role(&router, "CestateExec", &[PUBLISHED_EXECUTION_POLICY]).await;
    job_definition(
        &router,
        "cestate-jd",
        &role,
        json!(["/bin/sh", "-c", "exit 0"]),
    )
    .await;

    // The POSITIVE CONTROL: an environment that is ENABLED is one the scheduler places on, so a
    // job submitted to its queue runs and settles. Without it a withheld job cannot be told from
    // a job this emulator simply never places.
    let (_, control_queue) =
        environment_and_queue(&router, "cestate-on", &network, Some(ENABLED)).await;
    let control_id = submit(&router, "cestate-control", &control_queue, "cestate-jd", &[]).await;

    // One environment per withholding row, each with its own queue, and the job submitted to it
    // BEFORE anything is waited on — the release the rule is about is of a job that was already
    // submitted.
    let mut withheld: Vec<(String, String)> = Vec::with_capacity(WITHHELD_ROWS.len());
    for row in WITHHELD_ROWS {
        let (environment, queue) =
            environment_and_queue(&router, row.prefix, &network, row.created_state).await;
        if row.disabled_by_update {
            batch(
                &router,
                "/v1/updatecomputeenvironment",
                json!({"computeEnvironment": &environment, "state": DISABLED}),
            )
            .await;
        }
        // `state` and `status` are independent: a DISABLED environment is still VALID.
        let described = until_environment(&router, row.case, &environment, DISABLED).await;
        assert_eq!(
            described["status"].as_str(),
            Some(VALID),
            "case {:?}: a DISABLED environment still reports {VALID}, got {described}",
            row.case
        );
        let job_id = submit(&router, row.prefix, &queue, "cestate-jd", &[]).await;
        withheld.push((environment, job_id));
    }

    let control = until_job(&router, "the ENABLED control", &control_id, is_terminal).await;
    assert_eq!(
        control["status"].as_str(),
        Some(SUCCEEDED),
        "case the ENABLED control: a placed job whose container exits 0 ends {SUCCEEDED}, got \
         {control}"
    );
    assert_placed_as_one_task(&router, "the ENABLED control", &control).await;
    let control_started = control["startedAt"].as_i64().unwrap_or_else(|| {
        panic!("case the ENABLED control: expected the instant it started, got {control}")
    });
    let control_stopped = control["stoppedAt"].as_i64().unwrap_or_else(|| {
        panic!("case the ENABLED control: expected the instant it stopped, got {control}")
    });
    assert!(
        control_stopped >= control_started,
        "case the ENABLED control: expected stoppedAt at or after startedAt, got \
         {control_started} and {control_stopped}"
    );

    // The withheld jobs: each waits in RUNNABLE, and WAITING MEANS STAYING — read again over
    // several further spans of this service's own clock, each read inside the loop that waits.
    let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
    for (_, job_id) in &withheld {
        until_job(&router, "withheld", job_id, |detail| {
            detail["status"].as_str() == Some(RUNNABLE)
        })
        .await;
    }
    for round in 0..6 {
        tokio::time::sleep(Duration::from_millis(span)).await;
        for (row, (_, job_id)) in WITHHELD_ROWS.iter().zip(withheld.iter()) {
            let detail = describe(&router, job_id).await;
            assert_eq!(
                detail["status"].as_str(),
                Some(RUNNABLE),
                "case {:?} on read {round}: a withheld job stays {RUNNABLE}, got {detail}",
                row.case
            );
            assert_nothing_ran(row.case, &detail);
        }
    }

    // Re-ENABLING is what releases them, with no resubmission.
    for (row, (environment, _)) in WITHHELD_ROWS.iter().zip(withheld.iter()) {
        batch(
            &router,
            "/v1/updatecomputeenvironment",
            json!({"computeEnvironment": environment, "state": ENABLED}),
        )
        .await;
        until_environment(&router, row.case, environment, ENABLED).await;
    }
    for (row, (_, job_id)) in WITHHELD_ROWS.iter().zip(withheld.iter()) {
        let detail = until_job(&router, row.case, job_id, is_terminal).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(SUCCEEDED),
            "case {:?}: the released job runs and ends {SUCCEEDED}, got {detail}",
            row.case
        );
        assert_placed_as_one_task(&router, row.case, &detail).await;
        let started = detail["startedAt"].as_i64().unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the instant the released job started, got {detail}",
                row.case
            )
        });
        assert!(
            started >= control_stopped,
            "case {:?}: the release happened after the control had stopped, so the released job \
             started after that instant, got {started} against {control_stopped}",
            row.case
        );
        assert_eq!(
            detail["attempts"][0]["startedAt"].as_i64(),
            Some(started),
            "case {:?}: the attempt reports the same start as the job, got {detail}",
            row.case
        );
    }
}

/// What one row of the dependency table declares: the statuses the jobs this job depends on end
/// with, and what the dependent must then report.
struct DependencyRow {
    case: &'static str,
    prefix: &'static str,
    /// Whether each job it depends on exits 0, in the order they are submitted.
    parents_succeed: &'static [bool],
    /// The terminal `JobStatus` the dependent must settle on.
    status: &'static str,
}

/// The rows differ along how many jobs the dependent names and along what those jobs did — all
/// succeeded, one failed — which is what the gate turns on.
const DEPENDENCY_ROWS: &[DependencyRow] = &[
    DependencyRow {
        case: "one dependency that succeeds",
        prefix: "dep-one-ok",
        parents_succeed: &[true],
        status: SUCCEEDED,
    },
    DependencyRow {
        case: "one dependency that fails",
        prefix: "dep-one-bad",
        parents_succeed: &[false],
        status: FAILED,
    },
    DependencyRow {
        case: "two dependencies that both succeed",
        prefix: "dep-two-ok",
        parents_succeed: &[true, true],
        status: SUCCEEDED,
    },
    DependencyRow {
        case: "two dependencies of which the second fails",
        prefix: "dep-two-bad",
        parents_succeed: &[true, false],
        status: FAILED,
    },
];

/// The status the non-zero program below exits with, which a job that ran it reports as its
/// container's `exitCode` and which decides its terminal `JobStatus`.
const REFUSING_EXIT_STATUS: i64 = 9;

/// RULE: a job is held in `PENDING` until every job its `dependsOn` names is terminal — echoing
/// those ids, with no attempt and no `startedAt` while it waits — and is then released by their
/// success, starting no earlier than the last of them stopped, or ended `FAILED` by any one of
/// them failing, in which case nothing of a run is reported for it and its `statusReason` names
/// the dependency failure.
#[tokio::test]
async fn a_jobs_dependencies_hold_it_pending_until_they_are_all_terminal() {
    let router = router();
    let network = network(&router, "jobdep").await;
    let role = execution_role(&router, "JobdepExec", &[PUBLISHED_EXECUTION_POLICY]).await;
    let (_, queue) = environment_and_queue(&router, "jobdep", &network, Some(ENABLED)).await;

    // Two programs: one that is busy for a while and exits 0, and one that is busy for the same
    // while and exits non-zero. The wait is what makes the dependent's hold observable.
    for (name, status) in [("jobdep-jd-ok", 0), ("jobdep-jd-bad", REFUSING_EXIT_STATUS)] {
        job_definition(
            &router,
            name,
            &role,
            json!([
                "/bin/sh",
                "-c",
                format!("sleep {PARENT_WORK_SECONDS}; exit {status}"),
            ]),
        )
        .await;
    }

    // Every row's jobs are submitted before any is waited on, so the spans the clocks owe them
    // run together rather than one after another.
    let mut rows: Vec<(Vec<String>, String)> = Vec::with_capacity(DEPENDENCY_ROWS.len());
    for row in DEPENDENCY_ROWS {
        let mut parents: Vec<String> = Vec::with_capacity(row.parents_succeed.len());
        for (index, succeeds) in row.parents_succeed.iter().enumerate() {
            let definition = if *succeeds {
                "jobdep-jd-ok"
            } else {
                "jobdep-jd-bad"
            };
            parents.push(
                submit(
                    &router,
                    &format!("{}-p{index}", row.prefix),
                    &queue,
                    definition,
                    &[],
                )
                .await,
            );
        }
        let dependent = submit(
            &router,
            &format!("{}-child", row.prefix),
            &queue,
            "jobdep-jd-ok",
            &parents,
        )
        .await;
        rows.push((parents, dependent));
    }

    // HELD: each dependent leaves SUBMITTED for PENDING — not for RUNNABLE — while the jobs it
    // names are still running, echoes their ids, and reports nothing of a run.
    for (row, (parents, dependent)) in DEPENDENCY_ROWS.iter().zip(rows.iter()) {
        let detail = until_job(&router, row.case, dependent, |detail| {
            detail["status"].as_str() != Some(SUBMITTED)
        })
        .await;
        assert_eq!(
            detail["status"].as_str(),
            Some(PENDING),
            "case {:?}: a job whose dependencies are unsatisfied is held {PENDING}, got {detail}",
            row.case
        );
        let echoed: Vec<String> = detail["dependsOn"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["jobId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            &echoed, parents,
            "case {:?}: expected the dependency ids echoed back, got {detail}",
            row.case
        );
        assert_nothing_ran(row.case, &detail);
    }

    for (row, (parents, dependent)) in DEPENDENCY_ROWS.iter().zip(rows.iter()) {
        // The jobs it waits on reach the statuses their own programs decide.
        let mut last_stopped = i64::MIN;
        for (index, succeeds) in row.parents_succeed.iter().enumerate() {
            let case = format!("{} — dependency {index}", row.case);
            let parent = until_job(&router, &case, &parents[index], is_terminal).await;
            let expected = if *succeeds { SUCCEEDED } else { FAILED };
            assert_eq!(
                parent["status"].as_str(),
                Some(expected),
                "case {case:?}: expected the status its program decided, got {parent}"
            );
            assert_eq!(
                parent["container"]["exitCode"].as_i64(),
                Some(if *succeeds { 0 } else { REFUSING_EXIT_STATUS }),
                "case {case:?}: expected the status its container exited with, got {parent}"
            );
            let stopped = parent["stoppedAt"].as_i64().unwrap_or_else(|| {
                panic!("case {case:?}: expected the instant it stopped, got {parent}")
            });
            last_stopped = last_stopped.max(stopped);
        }

        let detail = until_job(&router, row.case, dependent, is_terminal).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(row.status),
            "case {:?}: expected the dependent to settle {}, got {detail}",
            row.case,
            row.status
        );
        if row.status == FAILED {
            // The dependent was never dispatched: no task was made for it, so nothing of a run
            // is reported — and the reason it ends with names the dependency failure.
            assert_nothing_ran(row.case, &detail);
            assert_eq!(
                detail["statusReason"].as_str(),
                Some(DEPENDENCY_FAILED_REASON),
                "case {:?}: expected the reason AWS Batch words a dependency failure with, got \
                 {detail}",
                row.case
            );
            continue;
        }
        // RELEASED: it ran like any other placed job, and it cannot have started before the last
        // job it waited on stopped.
        assert_eq!(
            detail["container"]["exitCode"].as_i64(),
            Some(0),
            "case {:?}: expected the status the released job's container exited with, got {detail}",
            row.case
        );
        assert_placed_as_one_task(&router, row.case, &detail).await;
        let started = detail["startedAt"].as_i64().unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the instant the released job started, got {detail}",
                row.case
            )
        });
        assert!(
            started >= last_stopped,
            "case {:?}: expected the released job to start at or after the last dependency \
             stopped, got {started} against {last_stopped}",
            row.case
        );
    }
}
