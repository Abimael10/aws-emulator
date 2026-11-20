// SPDX-License-Identifier: CC0-1.0

//! The ATTEMPT TIMEOUT a job carries, end to end over the wire.
//!
//! RULE under test: `JobTimeout$attemptDurationSeconds` is "the job timeout time (in
//! seconds) that's measured from the job attempt's `startedAt` timestamp. After this time
//! passes, Batch terminates your jobs if they aren't finished"
//! (`smithy_query(query="shape", shape="JobTimeout")`). So a job whose attempt has been
//! running for its whole `attemptDurationSeconds` is terminated BY THE SERVICE at
//! `startedAt + attemptDurationSeconds`: it reaches the terminal `FAILED`
//! (`JobDetail$status` targets `JobStatus`, whose terminal pair is `SUCCEEDED`/`FAILED`),
//! reports that instant as its `stoppedAt` ("when the job transitioned from the `RUNNING`
//! state to a terminal state") and carries the `statusReason` of that failure class — which
//! the attempt it ended reports too. A job still INSIDE its timeout is left alone, and a job
//! that carries no `attemptDurationSeconds` is never terminated for one however long it
//! runs.
//!
//! The rows span what the rule generalises over: the length of the timeout (the documented
//! minimum, a longer one, the longest a day-long job would carry, and none at all) crossed
//! with how long the attempt has been running relative to it — a millisecond short of the
//! deadline, exactly on it, and past it. The deadline is driven by SEEDING the attempt's own
//! `startedAt`, which is what the member is measured from, so each row states the elapsed
//! time it is about instead of waiting for the clock to pass it.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::ops::types::{
    ContainerDetail, JobTimeout,
};
use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{compute_environment, describe_job, job_queue, Batch};

/// One row: how long the job's `timeout` allows its attempt, how long that attempt has
/// already been running, and whether the service must therefore have terminated it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// `timeout.attemptDurationSeconds`, or `None` for a job that states no timeout.
    timeout_seconds: Option<i32>,
    /// How long ago the attempt started, in milliseconds.
    elapsed_millis: i64,
    /// Whether the deadline has passed, i.e. whether the job must be terminal with the
    /// attempt-timeout reason.
    timed_out: bool,
}

/// The table. `60` is the documented floor ("The minimum value for the timeout is 60
/// seconds", `state::MIN_ATTEMPT_DURATION_SECONDS`); the others are longer timeouts a
/// caller may state, and the last row states none.
const ROWS: &[Row] = &[
    Row {
        case: "the minimum timeout, a second past it",
        timeout_seconds: Some(60),
        elapsed_millis: 61_000,
        timed_out: true,
    },
    Row {
        case: "the minimum timeout, exactly on the deadline",
        timeout_seconds: Some(60),
        elapsed_millis: 60_000,
        timed_out: true,
    },
    Row {
        case: "the minimum timeout, a millisecond short of the deadline",
        timeout_seconds: Some(60),
        elapsed_millis: 59_999,
        timed_out: false,
    },
    Row {
        case: "a ten-minute timeout, well past it",
        timeout_seconds: Some(600),
        elapsed_millis: 601_000,
        timed_out: true,
    },
    Row {
        case: "a day-long timeout, an hour into it",
        timeout_seconds: Some(86_400),
        elapsed_millis: 3_600_000,
        timed_out: false,
    },
    Row {
        case: "no timeout at all, a day into the attempt",
        timeout_seconds: None,
        elapsed_millis: 86_400_000,
        timed_out: false,
    },
];

/// One emulator with a queue to submit into.
struct Fixture {
    batch: Batch,
    queue_arn: String,
}

/// A fresh emulator carrying a compute environment and a queue over it.
async fn fixture() -> Fixture {
    let batch = Batch::new();
    let environment_arn = compute_environment(&batch, "ce-timeout").await;
    let queue_arn = job_queue(&batch, "q-timeout", &environment_arn).await;
    Fixture { batch, queue_arn }
}

/// What one seeded row left in the store.
struct Seeded {
    job_id: String,
    /// The instant the seeded attempt started, which the deadline is measured from.
    started_at: i64,
}

/// Seed one job RUNNING its attempt, started `row.elapsed_millis` ago, with the row's
/// `timeout`, and with the next instant the clock owes it already reached for a row whose
/// deadline has passed.
///
/// The record is the one the lifecycle writes for a job the scheduler placed and whose
/// attempt then entered `RUNNING` — a job id, the queue it was submitted to, one open
/// attempt carrying the task it was placed as and the instant that attempt started — so what
/// the read below reports is decided by the timeout rule and not by the seeding.
fn seed(fixture: &Fixture, row: &Row) -> Seeded {
    let store = fixture.batch.store();
    let mut store = store.lock();
    let job_id = store.new_uuid();
    let now = state::now_millis();
    let started_at = now - row.elapsed_millis;
    // A job RUNNING its attempt always owes the clock its next step, and that step is never
    // later than its own deadline (`state::attempt_timeout`): a row whose deadline has passed
    // has therefore reached it, and one still inside its timeout has not.
    let next_phase_due_at = if row.timed_out {
        now
    } else {
        now + row.elapsed_millis.max(1_000)
    };
    let job = state::Job {
        job_id: job_id.clone(),
        job_arn: state::job_arn(crate::job_lifecycle::REGION, &job_id),
        job_name: "timeout-subject".to_string(),
        job_queue_arn: fixture.queue_arn.clone(),
        job_definition_arn: "jd-timeout".to_string(),
        status: state::JOB_RUNNING.to_string(),
        created_at: started_at,
        status_since: started_at,
        started_at: Some(started_at),
        next_phase_due_at: Some(next_phase_due_at),
        attempts: vec![state::JobAttempt {
            task_arn: "arn:aws:ecs:us-east-1:000000000000:task/cluster/task".to_string(),
            started_at: Some(started_at),
            stopped_at: None,
            exit_code: None,
            log_stream_name: None,
            status_reason: None,
            network_interfaces: Vec::new(),
            ended_at: None,
        }],
        container: Some(ContainerDetail {
            image: Some("busybox".to_string()),
            command: Some(vec!["sleep".to_string(), "600".to_string()]),
            ..ContainerDetail::default()
        }),
        timeout: row.timeout_seconds.map(|seconds| JobTimeout {
            attempt_duration_seconds: Some(seconds),
        }),
        ..state::Job::default()
    };
    store
        .insert_job(job)
        .unwrap_or_else(|refusal| panic!("case {}: the store took the job: {refusal}", row.case));
    Seeded {
        job_id,
        started_at,
    }
}

/// RULE: the attempt timeout TERMINATES the job at `startedAt + attemptDurationSeconds` and
/// at no other instant — `FAILED`, with that instant as `stoppedAt` and the attempt-timeout
/// `statusReason` on the job and on the attempt it ended — while a job inside its timeout,
/// and one that states none, keeps running.
#[tokio::test]
async fn an_attempt_is_terminated_exactly_when_its_timeout_runs_out() {
    for row in ROWS {
        let case = row.case;
        let fixture = fixture().await;
        let seeded = seed(&fixture, row);
        let detail = describe_job(&fixture.batch, &seeded.job_id).await;

        if !row.timed_out {
            assert_eq!(
                detail["status"].as_str(),
                Some(state::JOB_RUNNING),
                "case {case}: a job inside its timeout keeps running, got {detail}"
            );
            assert_eq!(
                detail["stoppedAt"],
                Value::Null,
                "case {case}: a job that has not stopped reports no stoppedAt, got {detail}"
            );
            assert_eq!(
                detail["statusReason"],
                Value::Null,
                "case {case}: nothing has ended this job, so it carries no statusReason, \
                 got {detail}"
            );
            continue;
        }

        let deadline = seeded.started_at
            + i64::from(row.timeout_seconds.unwrap_or_default()) * 1_000;
        assert_eq!(
            detail["status"].as_str(),
            Some(state::JOB_FAILED),
            "case {case}: an attempt past its timeout is terminated FAILED, got {detail}"
        );
        assert_eq!(
            detail["statusReason"].as_str(),
            Some(state::ATTEMPT_TIMEOUT_REASON),
            "case {case}: expected the attempt-timeout reason, got {detail}"
        );
        assert_eq!(
            detail["stoppedAt"].as_i64(),
            Some(deadline),
            "case {case}: the job stopped at its deadline {deadline}, got {detail}"
        );
        // The SERVICE terminated the job — "Batch terminates your jobs if they aren't
        // finished" — which is what `isTerminated` reports.
        assert_eq!(
            detail["isTerminated"].as_bool(),
            Some(true),
            "case {case}: a job the timeout terminated reports isTerminated, got {detail}"
        );
        assert_eq!(
            detail["isCancelled"].as_bool(),
            Some(false),
            "case {case}: nothing cancelled the job, got {detail}"
        );
        assert_eq!(
            detail["startedAt"].as_i64(),
            Some(seeded.started_at),
            "case {case}: the job still reports when its attempt started, got {detail}"
        );
        let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            attempts.len(),
            1,
            "case {case}: the job reports the one attempt the timeout ended, got {detail}"
        );
        assert_eq!(
            attempts[0]["statusReason"].as_str(),
            Some(state::ATTEMPT_TIMEOUT_REASON),
            "case {case}: the attempt reports why it ended, got {detail}"
        );
        assert_eq!(
            attempts[0]["stoppedAt"].as_i64(),
            Some(deadline),
            "case {case}: the attempt stopped at the same deadline, got {detail}"
        );
        assert!(
            attempts[0]["container"]["exitCode"].is_null(),
            "case {case}: no exit code is invented for a container the service stopped, \
             got {detail}"
        );

        // A terminal job is FROZEN: a later read reports exactly the same document, so the
        // instant the timeout ended it does not drift with the clock.
        let again = describe_job(&fixture.batch, &seeded.job_id).await;
        assert_eq!(
            again, detail,
            "case {case}: a second read must report the timed-out job unchanged, got {again}"
        );
    }
}

/// RULE: the timeout is measured from the ATTEMPT's `startedAt`, so a job whose attempt has
/// not started has no deadline at all — however long ago it was submitted, and whatever
/// `attemptDurationSeconds` it carries, it is not terminated for the timeout.
#[tokio::test]
async fn a_job_whose_attempt_never_started_has_no_deadline() {
    // The rows differ along the status a job that has not started its attempt can be read
    // in: waiting to be placed, and placed but with its container still being initiated.
    for status in [state::JOB_RUNNABLE, state::JOB_STARTING] {
        let fixture = fixture().await;
        let (job_id, submitted_at) = {
            let store = fixture.batch.store();
            let mut store = store.lock();
            let job_id = store.new_uuid();
            let now = state::now_millis();
            // A day before now: far past any timeout the row could carry.
            let created_at = now - 86_400_000;
            let job = state::Job {
                job_id: job_id.clone(),
                job_arn: state::job_arn(crate::job_lifecycle::REGION, &job_id),
                job_name: "unstarted-subject".to_string(),
                job_queue_arn: fixture.queue_arn.clone(),
                job_definition_arn: "jd-timeout".to_string(),
                status: status.to_string(),
                created_at,
                status_since: created_at,
                next_phase_due_at: Some(now),
                timeout: Some(JobTimeout {
                    attempt_duration_seconds: Some(60),
                }),
                ..state::Job::default()
            };
            store.insert_job(job).expect("the store took the job");
            (job_id, created_at)
        };

        let detail = describe_job(&fixture.batch, &job_id).await;
        assert_ne!(
            detail["statusReason"].as_str(),
            Some(state::ATTEMPT_TIMEOUT_REASON),
            "case {status}: a job whose attempt never started is not timed out, got {detail}"
        );
        assert_eq!(
            detail["startedAt"],
            Value::Null,
            "case {status}: a job whose attempt never started reports no startedAt, \
             got {detail}"
        );
        assert_eq!(
            detail["createdAt"].as_i64(),
            Some(submitted_at),
            "case {status}: the job still reports when it was created, got {detail}"
        );
    }
}

/// RULE: the `timeout` a job was submitted with is REPLAYED by the reader whatever the job
/// then did with it — a timed-out job still reports the duration that ended it.
#[tokio::test]
async fn the_submitted_timeout_is_replayed_by_the_reader() {
    let fixture = fixture().await;
    let row = ROWS[0];
    let seeded = seed(&fixture, &row);
    let detail = describe_job(&fixture.batch, &seeded.job_id).await;
    assert_eq!(
        detail["timeout"],
        json!({"attemptDurationSeconds": row.timeout_seconds}),
        "case {}: expected the submitted timeout to be replayed, got {detail}",
        row.case
    );
}
