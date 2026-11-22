// SPDX-License-Identifier: CC0-1.0

//! What a job's `retryStrategy` decides when one of its attempts fails — end to end over the
//! wire.
//!
//! RULE under test: `RetryStrategy$attempts` is "the number of times to move a job to the
//! `RUNNABLE` status. You can specify between 1 and 10 attempts. If the value of `attempts` is
//! greater than one, the job is retried on failure the same number of attempts as the value",
//! and `RetryStrategy$evaluateOnExit` is an "array of up to 5 objects that specify the
//! conditions where jobs are retried or failed ... If none of the listed conditions match, then
//! the job is retried". One `EvaluateOnExit` states "the action to take if all of the specified
//! conditions (`onStatusReason`, `onReason`, and `onExitCode`) are met", whose "values aren't
//! case sensitive", and each stated condition is a glob that "can optionally end with an
//! asterisk (*) so that only the start of the string needs to be an exact match". So a failed
//! attempt is read against the list in order: the first entry all of whose stated conditions
//! are met decides (`RETRY` for another attempt, `EXIT` for the end of the job), a list where
//! none is met falls back to the documented default of retrying, and the `attempts` budget is
//! the ceiling either way. Each retry is a REAL further execution, so `JobDetail$attempts` ("a
//! list of job attempts that are associated with this job") reports one `AttemptDetail` per
//! attempt, each naming its own Amazon ECS task
//! (`AttemptContainerDetail$taskArn`: "each container attempt receives a task ARN when they
//! reach the `STARTING` status") and the exit code its own run produced
//! (`AttemptContainerDetail$exitCode`: "the exit code for the job attempt").
//!
//! The rows differ along what the rule generalises over: whether a condition is met at all,
//! which member of a condition is stated (an exit code, the reason, the status reason),
//! whether its glob is exact or a prefix, which action it names, where in the list the match
//! is, how large the budget is, whether the strategy comes from the job definition or from the
//! submit, and the status the container's own program exits with (including zero, which is no
//! failure at all).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{describe_job, error_code, job_queue, Batch};

/// How long a poll below gives the clock before it gives up: a retried job walks its whole
/// lifecycle once per attempt, so this is many times the longest row's work even on a loaded
/// host. Only the loop's CONDITION decides when it stops.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The vCPUs one job of these suites reserves, and the memory with it: the smallest
/// `resourceRequirements` a Fargate container job can carry.
const JOB_VCPUS: &str = "1";
const JOB_MEMORY: &str = "512";

/// A container whose declared program exits with `status`.
///
/// `/bin/sh -c "exit <status>"` — a container's `command` is "the command that's passed to the
/// container", and a shell's `exit` is what decides the status the container's program ends
/// with, which is `ContainerDetail$exitCode` ("the exit code returned upon completion").
fn container_exiting_with(status: i32) -> Value {
    json!({
        "image": "busybox",
        "command": ["/bin/sh", "-c", format!("exit {status}")],
        "resourceRequirements": [
            {"type": "VCPU", "value": JOB_VCPUS},
            {"type": "MEMORY", "value": JOB_MEMORY},
        ],
    })
}

/// Register a job definition called `name` whose container exits with `status`, carrying
/// `retry_strategy` when one is given; answers its ARN.
async fn definition(
    batch: &Batch,
    name: &str,
    status: i32,
    retry_strategy: Option<&Value>,
) -> String {
    let mut body = json!({
        "jobDefinitionName": name,
        "type": "container",
        "containerProperties": container_exiting_with(status),
    });
    if let Some(strategy) = retry_strategy {
        body["retryStrategy"] = strategy.clone();
    }
    let answered = batch.ok("/v1/registerjobdefinition", body).await;
    answered["jobDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case register {name}: expected an ARN, got {answered}"))
        .to_string()
}

/// Submit one job called `name` against `definition` on `queue`, carrying `retry_strategy`
/// when one is given; answers its id.
async fn submit(
    batch: &Batch,
    queue: &str,
    definition: &str,
    name: &str,
    retry_strategy: Option<&Value>,
) -> String {
    let mut body = json!({
        "jobName": name,
        "jobQueue": queue,
        "jobDefinition": definition,
    });
    if let Some(strategy) = retry_strategy {
        body["retryStrategy"] = strategy.clone();
    }
    let answered = batch.ok("/v1/submitjob", body).await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case submit {name}: expected a jobId, got {answered}"))
        .to_string()
}

/// Poll `DescribeJobs` for `job_id` until it reports a terminal `JobStatus`, and answer that
/// `JobDetail`.
///
/// Driven off the OBSERVED record rather than off any wait: the loop looks again until the
/// condition holds or the generous deadline above passes.
async fn until_terminal(batch: &Batch, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = describe_job(batch, job_id).await;
        let status = last["status"].as_str().unwrap_or_default();
        if status == state::JOB_SUCCEEDED || status == state::JOB_FAILED {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case}: expected the job to reach a terminal status, got {last}");
}

/// The `attempts` of a `JobDetail`; the member is always reported, so an absent one is a
/// failure of the projection rather than an empty list.
fn attempts(case: &str, detail: &Value) -> Vec<Value> {
    detail["attempts"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: `attempts` must be a list, got {detail}"))
}

/// Assert everything the attempts of a job whose container RAN must report: one entry per
/// execution, each carrying the exit status that execution produced, its own task and log
/// stream, and the instants it started and stopped.
fn assert_attempts_are_executions(case: &str, detail: &Value, exit_code: i32, wanted: usize) {
    let listed = attempts(case, detail);
    assert_eq!(
        listed.len(),
        wanted,
        "case {case}: expected {wanted} attempt(s), got {detail}"
    );
    let mut task_arns: Vec<String> = Vec::new();
    let mut streams: Vec<String> = Vec::new();
    for (index, attempt) in listed.iter().enumerate() {
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            Some(i64::from(exit_code)),
            "case {case} attempt {index}: every attempt reports the status its own run \
             exited with, got {attempt}"
        );
        assert!(
            attempt["startedAt"].as_i64().unwrap_or_default() > 0,
            "case {case} attempt {index}: an attempt whose container ran reports when it \
             started, got {attempt}"
        );
        assert!(
            attempt["stoppedAt"].as_i64().unwrap_or_default() > 0,
            "case {case} attempt {index}: an attempt that has ended reports when it stopped, \
             got {attempt}"
        );
        assert_eq!(
            attempt["statusReason"].as_str(),
            detail["statusReason"].as_str(),
            "case {case} attempt {index}: a container that exited reports why, got {attempt}"
        );
        assert_eq!(
            attempt["container"]["reason"].as_str(),
            detail["statusReason"].as_str(),
            "case {case} attempt {index}: the attempt's container reports the same reason, \
             got {attempt}"
        );
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case} attempt {index}: an attempt names its task, got {attempt}")
            })
            .to_string();
        let stream = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case} attempt {index}: an attempt names its stream, got {attempt}")
            })
            .to_string();
        task_arns.push(task_arn);
        streams.push(stream);
    }
    // Each attempt is its own execution, so each one was placed as its own task and its
    // container wrote to the stream named for that task.
    for (what, mut values) in [("task ARN", task_arns), ("log stream", streams)] {
        let total = values.len();
        values.sort_unstable();
        values.dedup();
        assert_eq!(
            values.len(),
            total,
            "case {case}: every attempt must report its own {what}, got {values:?}"
        );
    }
    // The job's own container reports the LATEST attempt's run.
    assert_eq!(
        detail["container"]["exitCode"].as_i64(),
        Some(i64::from(exit_code)),
        "case {case}: the job's container reports the exit status of its run, got {detail}"
    );
}

/// One row: the strategy a job carries, where it carries it, the status its container's
/// program exits with, and what the job must then report.
struct Row {
    case: &'static str,
    /// The `retryStrategy` document, or `Value::Null` for a job that carries none.
    strategy: fn() -> Value,
    /// Whether the strategy is registered on the job DEFINITION (the other rows carry it on
    /// the submit, which `SubmitJob$retryStrategy` overrides the definition's with).
    on_definition: bool,
    /// The status the container's declared program exits with.
    exit_code: i32,
    /// How many attempts the job must end up having had.
    attempts: usize,
    /// The terminal `JobStatus` it must reach.
    status: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no listed condition is met, so the documented default retries",
            strategy: || json!({"attempts": 3, "evaluateOnExit": [{"onExitCode": "7", "action": "EXIT"}]}),
            on_definition: true,
            exit_code: 3,
            attempts: 3,
            status: state::JOB_FAILED,
        },
        Row {
            case: "the listed condition is met and says EXIT",
            strategy: || json!({"attempts": 3, "evaluateOnExit": [{"onExitCode": "7", "action": "EXIT"}]}),
            on_definition: true,
            exit_code: 7,
            attempts: 1,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a budget with no conditions at all",
            strategy: || json!({"attempts": 2}),
            on_definition: false,
            exit_code: 1,
            attempts: 2,
            status: state::JOB_FAILED,
        },
        Row {
            case: "no retry strategy at all",
            strategy: || Value::Null,
            on_definition: false,
            exit_code: 1,
            attempts: 1,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a budget of one, with a condition that says RETRY",
            strategy: || json!({"attempts": 1, "evaluateOnExit": [{"onExitCode": "*", "action": "RETRY"}]}),
            on_definition: false,
            exit_code: 4,
            attempts: 1,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a condition met on the reason the attempt reports says RETRY",
            strategy: || json!({"attempts": 3, "evaluateOnExit": [{"onReason": "Essential*", "action": "RETRY"}]}),
            on_definition: false,
            exit_code: 5,
            attempts: 3,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a glob matching only the start of the exit status",
            strategy: || json!({"attempts": 3, "evaluateOnExit": [{"onExitCode": "1*", "action": "EXIT"}]}),
            on_definition: true,
            exit_code: 12,
            attempts: 1,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a condition stated over the status reason the attempt reports",
            strategy: || json!({"attempts": 3, "evaluateOnExit": [{"onStatusReason": "Essential*", "action": "EXIT"}]}),
            on_definition: false,
            exit_code: 6,
            attempts: 1,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a condition stating two members, one of which is not met",
            strategy: || json!({"attempts": 2, "evaluateOnExit": [{"onExitCode": "3", "onStatusReason": "Host EC2*", "action": "EXIT"}]}),
            on_definition: false,
            exit_code: 3,
            attempts: 2,
            status: state::JOB_FAILED,
        },
        Row {
            case: "two conditions, of which the first met one decides",
            strategy: || json!({
                "attempts": 3,
                "evaluateOnExit": [
                    {"onExitCode": "9", "action": "EXIT"},
                    {"onExitCode": "2", "action": "RETRY"},
                ],
            }),
            on_definition: false,
            exit_code: 2,
            attempts: 3,
            status: state::JOB_FAILED,
        },
        Row {
            case: "a program that exits zero is no failure, so it is not retried",
            strategy: || json!({"attempts": 3}),
            on_definition: false,
            exit_code: 0,
            attempts: 1,
            status: state::JOB_SUCCEEDED,
        },
    ]
}

/// A fresh emulator with a `MANAGED` Fargate compute environment and an enabled queue over it;
/// answers the harness and the queue's ARN.
async fn fixture(prefix: &str) -> (Batch, String) {
    let batch = Batch::new();
    let environment = managed_compute_environment(
        &batch,
        &format!("ce-{prefix}"),
        state::STATE_ENABLED,
    )
    .await;
    let queue = job_queue(&batch, &format!("q-{prefix}"), &environment).await;
    (batch, queue)
}

/// RULE: the `evaluateOnExit` list decides each failed attempt — the first met condition's
/// action, and the documented retry when none is met — bounded by the `attempts` budget, and
/// every attempt the job has had is reported as its own execution.
#[tokio::test]
async fn evaluate_on_exit_decides_each_failed_attempt_within_the_attempts_budget() {
    let (batch, queue) = fixture("eoe").await;
    let rows = rows();

    // Every row's job is submitted FIRST and polled after, so the rows' attempts run beside
    // each other rather than one table-length wait after another. The environment's
    // `maxvCpus` holds far more than these jobs reserve, so none of them waits for another.
    let mut submitted: Vec<String> = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let strategy = (row.strategy)();
        let strategy = (!strategy.is_null()).then_some(strategy);
        let on_definition = strategy.as_ref().filter(|_| row.on_definition);
        let definition_arn = definition(
            &batch,
            &format!("jd-eoe{index}"),
            row.exit_code,
            on_definition,
        )
        .await;
        let on_submit = strategy.as_ref().filter(|_| !row.on_definition);
        submitted.push(
            submit(
                &batch,
                &queue,
                &definition_arn,
                &format!("j-eoe{index}"),
                on_submit,
            )
            .await,
        );
    }

    for (row, job_id) in rows.iter().zip(&submitted) {
        let case = row.case;
        let detail = until_terminal(&batch, case, job_id).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(row.status),
            "case {case}: expected the job to end {}, got {detail}",
            row.status
        );
        assert_attempts_are_executions(case, &detail, row.exit_code, row.attempts);
    }
}

/// RULE: the budget is "the number of times to move a job to the `RUNNABLE` status", so a job
/// whose failures no condition ends gets exactly that many attempts — at each published bound
/// of the member and in between — while a request naming a count outside those bounds is
/// refused rather than stored.
///
/// The bounds are the member's own: "You can specify between 1 and 10 attempts"
/// (`RetryStrategy$attempts`).
#[tokio::test]
async fn the_attempts_budget_is_honoured_across_its_documented_range() {
    /// Each published bound, and a value between them.
    const BUDGETS: &[i32] = &[1, 2, 10];
    /// One past each published bound: no job is stored for either.
    const REFUSED: &[i32] = &[0, 11];
    /// The status every job of this test's container exits with: a failure no condition of
    /// its strategy mentions, so every attempt of the budget is spent.
    const FAILING_STATUS: i32 = 1;

    let (batch, queue) = fixture("budget").await;

    let mut submitted: Vec<(i32, String)> = Vec::with_capacity(BUDGETS.len());
    for budget in BUDGETS {
        let definition_arn =
            definition(&batch, &format!("jd-budget{budget}"), FAILING_STATUS, None).await;
        let strategy = json!({"attempts": budget});
        let job_id = submit(
            &batch,
            &queue,
            &definition_arn,
            &format!("j-budget{budget}"),
            Some(&strategy),
        )
        .await;
        submitted.push((*budget, job_id));
    }

    for (budget, job_id) in &submitted {
        let case = format!("a budget of {budget}");
        let detail = until_terminal(&batch, &case, job_id).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(state::JOB_FAILED),
            "case {case}: a job whose every attempt failed ends FAILED, got {detail}"
        );
        assert_attempts_are_executions(
            &case,
            &detail,
            FAILING_STATUS,
            usize::try_from(*budget).unwrap_or_default(),
        );
    }

    let definition_arn = definition(&batch, "jd-budget-bad", FAILING_STATUS, None).await;
    for attempts in REFUSED {
        let case = format!("a budget of {attempts}");
        let (status, body) = batch
            .call(
                "/v1/submitjob",
                json!({
                    "jobName": format!("j-bad{attempts}"),
                    "jobQueue": &queue,
                    "jobDefinition": &definition_arn,
                    "retryStrategy": {"attempts": attempts},
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: a count outside the documented range is refused, got {body}"
        );
        assert_eq!(
            error_code(&body),
            "ClientException",
            "case {case}: expected the modelled refusal, got {body}"
        );
    }
}
