// SPDX-License-Identifier: CC0-1.0

//! What a job's ATTEMPTS report, end to end over the wire.
//!
//! RULE under test: AWS Batch records a job attempt each time the scheduler PLACES a job's
//! container on compute capacity, and `JobDetail$attempts` — "a list of job attempts that
//! are associated with this job" — reports one `AttemptDetail` per attempt. The attempt is
//! where the identity of the placed task is reported: `AttemptContainerDetail$taskArn` is
//! "the Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the job attempt", and "each
//! container attempt receives a task ARN when they reach the `STARTING` status" — beside the
//! CloudWatch Logs stream its container writes to, whose log group "for Batch jobs is
//! `/aws/batch/job`" (`AttemptContainerDetail$logStreamName`) and whose name the placement
//! decides from the job definition and the placed task. What an attempt reports of the RUN
//! appears only once its container got that far — `exitCode` is "the exit code for the job
//! attempt", `startedAt` is the `STARTING` -> `RUNNING` transition and `stoppedAt` the
//! `RUNNING` -> terminal one — so an attempt whose container ran the program the job declared
//! reports each of them, beside the reason that attempt ended. A job no placement has taken
//! has had no
//! attempt, so the list it reports is EMPTY and present — "nothing has run yet" is an empty
//! list, not a missing member.
//!
//! The rows differ along what the rule generalises over: whether the job's queue offers the
//! scheduler capacity at all, when that capacity is published, what ends the job, and how
//! many jobs (and array children) are placed.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{
    compute_environment, container_properties, describe_compute_environment, describe_job,
    job_definition, job_queue, until_status, Batch, REGION,
};

/// How AWS Batch names the Amazon ECS cluster backing a compute environment:
/// `AWSBatch-<compute environment name>-<the environment's own id>`. Spelled out here rather
/// than imported, so a wrong name in the implementation cannot make a test pass. The two
/// variable parts are read back off the environment itself
/// (`ComputeEnvironmentDetail$computeEnvironmentName` and `ComputeEnvironmentDetail$uuid`),
/// which is how a client learns them.
async fn backing_cluster(fixture: &Fixture) -> String {
    let described =
        describe_compute_environment(&fixture.batch, &fixture.environment_name).await;
    let name = described["computeEnvironmentName"]
        .as_str()
        .unwrap_or_else(|| panic!("case backing cluster: expected a name, got {described}"));
    let uuid = described["uuid"]
        .as_str()
        .unwrap_or_else(|| panic!("case backing cluster: expected a uuid, got {described}"));
    format!("AWSBatch-{name}-{uuid}")
}

/// What the compute environment behind a queue offers the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    /// `UNMANAGED`: only instances the caller registered, of which this store holds none,
    /// so nothing is ever placed.
    Unmanaged,
    /// `MANAGED` and `ENABLED`: capacity the service itself schedules onto.
    ManagedEnabled,
    /// `MANAGED` but `DISABLED`: Batch places no job on it until a caller re-enables it.
    ManagedDisabled,
}

/// What ends the submitted job in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Nothing: the job comes to rest in `RUNNABLE` and stays there.
    Rests,
    /// The clock: the queue's capacity places the job and the placement ends it.
    Placed,
    /// The caller re-enables the environment, and the placement that follows ends it.
    EnabledThenPlaced,
    /// `CancelJob` on a job that never left `RUNNABLE`.
    Cancelled,
    /// `TerminateJob` on a job that never left `RUNNABLE`.
    Terminated,
}

/// One emulator, with a queue over one compute environment and a definition to submit into.
struct Fixture {
    batch: Batch,
    environment_name: String,
    environment: String,
    queue_arn: String,
    definition: String,
    definition_name: String,
}

/// A fresh emulator whose queue is backed by the capacity `kind` describes.
async fn fixture(prefix: &str, kind: Capacity) -> Fixture {
    let batch = Batch::new();
    let environment_name = format!("ce-{prefix}");
    let environment = match kind {
        Capacity::Unmanaged => compute_environment(&batch, &environment_name).await,
        Capacity::ManagedEnabled => {
            managed_compute_environment(&batch, &environment_name, state::STATE_ENABLED).await
        }
        Capacity::ManagedDisabled => {
            managed_compute_environment(&batch, &environment_name, state::STATE_DISABLED).await
        }
    };
    let queue_arn = job_queue(&batch, &format!("q-{prefix}"), &environment).await;
    let definition_name = format!("jd-{prefix}");
    let (definition, _) = job_definition(&batch, &definition_name, container_properties()).await;
    Fixture {
        batch,
        environment_name,
        environment,
        queue_arn,
        definition,
        definition_name,
    }
}

/// Submit one job called `name`, with `extra` merged into the request; answers its id.
async fn submit(fixture: &Fixture, name: &str, extra: Value) -> String {
    let mut body = json!({
        "jobName": name,
        "jobQueue": &fixture.queue_arn,
        "jobDefinition": &fixture.definition,
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    let answered = fixture.batch.ok("/v1/submitjob", body).await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case submit {name}: expected a jobId, got {answered}"))
        .to_string()
}

/// The `attempts` of a `JobDetail`, or `None` when the member is absent — which no reported
/// job answers, because the member is always there (empty for a job nothing placed).
fn attempts(detail: &Value) -> Option<Vec<Value>> {
    detail
        .get("attempts")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_array()
                .cloned()
                .unwrap_or_else(|| panic!("attempts must be a list, got {value}"))
        })
}

/// Assert that `task_arn` is the ARN of an ECS task placed in the Amazon ECS `cluster`
/// backing the job's compute environment: `arn:aws:ecs:<region>:<account>:task/<cluster>/<task
/// id>`, whose id is the 32 lowercase hex characters an ECS task id is.
fn assert_task_arn(case: &str, cluster: &str, task_arn: &str) {
    let prefix = format!(
        "arn:aws:ecs:{REGION}:{}:task/{cluster}/",
        engine::EMULATOR_ACCOUNT_ID
    );
    let Some(task_id) = task_arn.strip_prefix(&prefix) else {
        panic!("case {case}: expected a task ARN under {prefix}, got {task_arn:?}");
    };
    assert_eq!(
        task_id.len(),
        32,
        "case {case}: expected a 32-character task id, got {task_id:?}"
    );
    assert!(
        task_id.chars().all(|digit| digit.is_ascii_hexdigit()
            && !digit.is_ascii_uppercase()),
        "case {case}: expected a lowercase hex task id, got {task_id:?}"
    );
}

/// Assert everything ONE attempt of a placed job reports: the
/// task the placement made, the log stream that task's container writes to, the reason the
/// attempt ended, and what its container's program produced.
fn assert_placement_attempt(
    case: &str,
    cluster: &str,
    definition_name: &str,
    detail: &Value,
    attempt: &Value,
) {
    let task_arn = attempt["container"]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the attempt must name its task, got {attempt}"));
    assert_task_arn(case, cluster, task_arn);
    // "Each container attempt receives a task ARN": the job's own container reports the
    // task of the attempt it was placed as (`ContainerDetail$taskArn`).
    assert_eq!(
        detail["container"]["taskArn"].as_str(),
        Some(task_arn),
        "case {case}: the job's container must report the attempt's task, got {detail}"
    );
    // The log stream the `awslogs` driver writes this container to:
    // `<job definition name>/default/<ECS task id>`, where `default` is the container name
    // Batch gives a container job's single container.
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    let wanted_stream = format!("{definition_name}/default/{task_id}");
    assert_eq!(
        attempt["container"]["logStreamName"].as_str(),
        Some(wanted_stream.as_str()),
        "case {case}: expected the attempt's log stream {wanted_stream:?}, got {attempt}"
    );
    assert_eq!(
        detail["container"]["logStreamName"].as_str(),
        Some(wanted_stream.as_str()),
        "case {case}: the job's container must report the same log stream, got {detail}"
    );
    let reason = detail["statusReason"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a statusReason, got {detail}"));
    assert_eq!(
        attempt["statusReason"].as_str(),
        Some(reason),
        "case {case}: the attempt reports why it ended, got {attempt}"
    );
    assert_eq!(
        attempt["container"]["reason"].as_str(),
        Some(reason),
        "case {case}: the attempt's container reports why it ended, got {attempt}"
    );
    // The attempt's container RAN the program the job's container declares, so the attempt
    // reports the instant it started, the instant the task it was made for stopped and the
    // status that program exited with — the same exit code the job's own container reports.
    assert!(
        attempt["startedAt"].as_i64().unwrap_or_default() > 0,
        "case {case}: an attempt whose container ran reports when it started, got {attempt}"
    );
    assert!(
        attempt["stoppedAt"].as_i64().is_some(),
        "case {case}: an attempt that has ended reports when its task stopped, got {attempt}"
    );
    assert_eq!(
        attempt["container"]["exitCode"].as_i64(),
        detail["container"]["exitCode"].as_i64(),
        "case {case}: the attempt and the job report one exit code, got {attempt}"
    );
    assert!(
        attempt["container"]["containerInstanceArn"].is_null(),
        "case {case}: no container instance hosts the attempt, got {attempt}"
    );
}

/// Re-enable `fixture`'s compute environment, which is what publishes the capacity its
/// waiting jobs are placed on.
async fn enable_environment(fixture: &Fixture) {
    fixture
        .batch
        .ok(
            "/v1/updatecomputeenvironment",
            json!({"computeEnvironment": &fixture.environment, "state": state::STATE_ENABLED}),
        )
        .await;
}

/// One row of the table: what the queue offers, what ends the job, and how many attempts
/// the job must then report.
struct Row {
    case: &'static str,
    capacity: Capacity,
    ending: Ending,
    /// How many `AttemptDetail`s the job has had — one per placement, and none at all for a
    /// job nothing placed.
    attempts: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "placed by the clock on managed capacity",
        capacity: Capacity::ManagedEnabled,
        ending: Ending::Placed,
        attempts: 1,
    },
    Row {
        case: "placed once the environment is re-enabled",
        capacity: Capacity::ManagedDisabled,
        ending: Ending::EnabledThenPlaced,
        attempts: 1,
    },
    Row {
        case: "resting in RUNNABLE on unmanaged capacity",
        capacity: Capacity::Unmanaged,
        ending: Ending::Rests,
        attempts: 0,
    },
    Row {
        case: "held in RUNNABLE by a disabled environment",
        capacity: Capacity::ManagedDisabled,
        ending: Ending::Rests,
        attempts: 0,
    },
    Row {
        case: "cancelled before anything placed it",
        capacity: Capacity::Unmanaged,
        ending: Ending::Cancelled,
        attempts: 0,
    },
    Row {
        case: "terminated before anything placed it",
        capacity: Capacity::Unmanaged,
        ending: Ending::Terminated,
        attempts: 0,
    },
];

/// RULE: an attempt exists for each PLACEMENT and for nothing else — a placed job reports
/// exactly one `AttemptDetail` naming the task it was placed as and the reason that attempt
/// ended, and a job nothing placed reports no attempts however it ends.
#[tokio::test]
async fn a_job_reports_one_attempt_per_placement_and_none_without_one() {
    const REASON: &str = "ended by the caller";

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let fixture = fixture(&format!("att{index}"), row.capacity).await;
        let job_id = submit(&fixture, &format!("j{index}"), json!({})).await;

        let detail = match row.ending {
            Ending::Placed => {
                until_status(&fixture.batch, case, &job_id, state::JOB_SUCCEEDED).await
            }
            Ending::EnabledThenPlaced => {
                until_status(&fixture.batch, case, &job_id, state::JOB_RUNNABLE).await;
                enable_environment(&fixture).await;
                until_status(&fixture.batch, case, &job_id, state::JOB_SUCCEEDED).await
            }
            Ending::Rests => {
                let resting =
                    until_status(&fixture.batch, case, &job_id, state::JOB_RUNNABLE).await;
                // Resting means staying: several further spans place nothing, so no attempt
                // appears for it either. The job is read again on each of them, inside the
                // loop that waits, so what is asserted is the record holding read after read.
                let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
                for round in 0..8 {
                    tokio::time::sleep(Duration::from_millis(span / 2)).await;
                    assert_eq!(
                        describe_job(&fixture.batch, &job_id).await,
                        resting,
                        "case {case} on read {round} past its rest: a resting job must be left \
                         exactly as it is"
                    );
                }
                resting
            }
            Ending::Cancelled | Ending::Terminated => {
                until_status(&fixture.batch, case, &job_id, state::JOB_RUNNABLE).await;
                let uri = if row.ending == Ending::Cancelled {
                    "/v1/canceljob"
                } else {
                    "/v1/terminatejob"
                };
                fixture
                    .batch
                    .ok(uri, json!({"jobId": &job_id, "reason": REASON}))
                    .await;
                let ended = until_status(&fixture.batch, case, &job_id, state::JOB_FAILED).await;
                assert_eq!(
                    ended["statusReason"].as_str(),
                    Some(REASON),
                    "case {case}: the caller's reason is what ended the job, got {ended}"
                );
                ended
            }
        };

        let listed = attempts(&detail).unwrap_or_else(|| {
            panic!(
                "case {case}: `attempts` is always reported, empty for a job no placement \
                 took, got {detail}"
            )
        });
        assert_eq!(
            listed.len(),
            row.attempts,
            "case {case}: expected {} attempt(s), got {detail}",
            row.attempts
        );
        if row.attempts > 0 {
            assert_eq!(
                detail["statusReason"].as_str(),
                Some(state::ESSENTIAL_CONTAINER_EXITED_REASON),
                "case {case}: a placed job whose container exited says so, got {detail}"
            );
            assert_placement_attempt(
                case,
                &backing_cluster(&fixture).await,
                &fixture.definition_name,
                &detail,
                &listed[0],
            );
        }
    }
}

/// RULE: each placement makes its OWN task, so two jobs placed on one queue report two
/// different task ARNs — and reading a job records nothing, so the attempts a job reports
/// are the placements it had and never one per read.
#[tokio::test]
async fn each_placement_names_its_own_task_and_a_read_records_none() {
    let fixture = fixture("tasks", Capacity::ManagedEnabled).await;
    let mut task_arns: Vec<String> = Vec::new();

    for index in 0..3 {
        let case = format!("job {index}");
        let job_id = submit(&fixture, &format!("t{index}"), json!({})).await;
        let detail = until_status(&fixture.batch, &case, &job_id, state::JOB_SUCCEEDED).await;
        let listed = attempts(&detail)
            .unwrap_or_else(|| panic!("case {case}: expected one attempt, got {detail}"));
        assert_eq!(
            listed.len(),
            1,
            "case {case}: a placed job has had exactly one placement, got {detail}"
        );
        assert_placement_attempt(
            &case,
            &backing_cluster(&fixture).await,
            &fixture.definition_name,
            &detail,
            &listed[0],
        );
        task_arns.push(
            listed[0]["container"]["taskArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );

        // A READ records no attempt: the same job, described again and again and after
        // further spans of the clock, still reports the one placement it had.
        let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
        tokio::time::sleep(Duration::from_millis(span * 2)).await;
        for read in 0..3 {
            let again = describe_job(&fixture.batch, &job_id).await;
            assert_eq!(
                again, detail,
                "case {case}: read {read} must report the same job, got {again}"
            );
        }
    }

    let mut unique = task_arns.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        task_arns.len(),
        "case three placements: every placement must name its own task, got {task_arns:?}"
    );
}

/// RULE: an attempt belongs to the job that was PLACED — so each child of an array job
/// placed on managed capacity reports its own attempt and its own task, while the parent,
/// which runs no container of its own and is never placed, reports none.
#[tokio::test]
async fn array_children_report_their_own_attempts_and_the_parent_reports_none() {
    const SIZE: i32 = 3;
    let fixture = fixture("arrayatt", Capacity::ManagedEnabled).await;
    let parent_id = submit(
        &fixture,
        "shards",
        json!({"arrayProperties": {"size": SIZE}}),
    )
    .await;

    let mut task_arns: Vec<String> = Vec::new();
    for index in 0..SIZE {
        let case = format!("child {index}");
        let child_id = state::array_child_job_id(&parent_id, index);
        let child = until_status(&fixture.batch, &case, &child_id, state::JOB_SUCCEEDED).await;
        let listed = attempts(&child)
            .unwrap_or_else(|| panic!("case {case}: expected one attempt, got {child}"));
        assert_eq!(
            listed.len(),
            1,
            "case {case}: a placed child has had exactly one placement, got {child}"
        );
        assert_placement_attempt(
            &case,
            &backing_cluster(&fixture).await,
            &fixture.definition_name,
            &child,
            &listed[0],
        );
        task_arns.push(
            listed[0]["container"]["taskArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    let mut unique = task_arns.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        task_arns.len(),
        "case the array: each child's placement names its own task, got {task_arns:?}"
    );

    let parent = until_status(&fixture.batch, "the parent", &parent_id, state::JOB_SUCCEEDED).await;
    assert_eq!(
        attempts(&parent).map(|listed| listed.len()),
        Some(0),
        "case the parent: an array parent is never placed, so the attempts it reports are \
         the empty list, got {parent}"
    );
}
