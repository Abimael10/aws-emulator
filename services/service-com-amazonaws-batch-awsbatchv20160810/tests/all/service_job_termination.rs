// SPDX-License-Identifier: CC0-1.0

//! `TerminateServiceJob` and `DescribeServiceJob`, end to end over the wire.
//!
//! The RULES under test:
//!
//! * `TerminateServiceJob` ("Terminates a service job in a job queue") is accepted for
//!   ANY `jobId` — it models no per-job outcome, `TerminateServiceJobResponse` having no
//!   members — and what it does is decided only by the record the store holds for that
//!   id: a service job that has not reached a terminal `ServiceJobStatus` ends `FAILED`
//!   at the call, carrying the caller's `reason` as its `statusReason` and
//!   `isTerminated` ("Indicates whether the service job has been terminated"), and
//!   NOTHING else about it changes; one already in a terminal status is left exactly as
//!   it is; an id the service holds no service job for changes nothing and stays
//!   unresolvable.
//! * `DescribeServiceJob` ("The details of a service job") reports the stored record
//!   member for member, and answers the modelled `ClientException` (HTTP 400) for a
//!   `jobId` the service holds no service job for.
//! * `TerminateServiceJobs` ("a bulk version of `TerminateServiceJob`") ends every id it
//!   lists through that same machinery and reports the outcome PER JOB — 200 with every id
//!   given back in `successful`, an empty `errors`, and an id no service job answers to
//!   "treated as successfully processed" — while a request past the documented maximum of
//!   ids is refused whole and ends nothing.
//!
//! Each test asserts its rule over a SET of inputs: every non-terminal and every
//! terminal `ServiceJobStatus` the model declares, reasons differing in size and
//! alphabet, the six forms an unknown id can take, and a record carrying every optional
//! member beside one carrying only the required ones.
//!
//! A seeded record is how a row reaches a status `SubmitServiceJob` cannot mint: that
//! operation accepts a service job in `SUBMITTED` only, so a test that needs one of the
//! other modelled `ServiceJobStatus` values writes the record through the same state API
//! the handler uses (`ComAmazonawsBatchAwsbatchv20160810State::insert_service_job`),
//! which is the same store both handlers under test read. The submit path itself is
//! covered end to end in `tests/all/service_job_submission.rs`.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::ops::types::{
    ServiceJobEvaluateOnExit, ServiceJobPreemptionConfiguration, ServiceJobRetryStrategy,
    ServiceJobTimeout, SERVICE_JOB_STATUS_VALUES,
};
use service_com_amazonaws_batch_awsbatchv20160810::state::{self, ServiceJob};

use crate::cancel_job::flip_last_hex_digit;
use crate::job_lifecycle::{error_code, Batch, REGION};

/// The only `ServiceJobType` the model declares
/// (`smithy_query(query="shape", shape="ServiceJobType")`: SAGEMAKER_TRAINING).
const SERVICE_JOB_TYPE: &str = "SAGEMAKER_TRAINING";

/// The members `TerminateServiceJob` is allowed to change on the service job it ends.
/// Every other member of the `DescribeServiceJob` answer must read back unchanged.
///
/// `attempts` is among them because the attempt a termination closes carries the ending's
/// own reason (`ServiceJobAttemptDetail$statusReason`: "additional details for the current
/// status of the service job attempt"), which is asserted on its own below.
const TERMINATION_MEMBERS: &[&str] =
    &["status", "statusReason", "stoppedAt", "isTerminated", "attempts"];

/// The `ServiceJobStatus` values a service job never leaves. The model's enum lists
/// FAILED, PENDING, RUNNABLE, RUNNING, SCHEDULED, STARTING, SUBMITTED, SUCCEEDED
/// (`smithy_query(query="shape", shape="ServiceJobStatus")`); the terminal pair is the
/// two completed ones, which is what the handler's state helper keys off.
pub(crate) const TERMINAL_STATUSES: &[&str] =
    &[state::SERVICE_JOB_SUCCEEDED, state::SERVICE_JOB_FAILED];

/// How much optional detail a seeded service job carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Filled {
    /// Only the members `DescribeServiceJob` models as required.
    Required,
    /// Every member a service job record can carry.
    Everything,
}

/// The `retryStrategy` an `Everything` record carries. `attempts` is inside the 1..10 the
/// member documents, and `action` is one of `ServiceJobRetryAction`'s values (EXIT|RETRY).
fn retry_strategy() -> ServiceJobRetryStrategy {
    ServiceJobRetryStrategy {
        attempts: 3,
        evaluate_on_exit: Some(vec![ServiceJobEvaluateOnExit {
            action: Some("RETRY".to_string()),
            on_status_reason: Some("Internal*".to_string()),
        }]),
    }
}

/// Seed one service job in `status` into the emulator's store and answer the record as
/// the store now holds it.
///
/// The timestamps are seeded consistently with `status`: a job is `scheduledAt` only once
/// it has been dispatched (`SCHEDULED` onwards) and `startedAt` only once it is running
/// (`STARTING` onwards), and a terminal one also carries the instant and reason it ended
/// with, so the "leave a terminal job alone" rows have something to lose.
pub(crate) fn seed(batch: &Batch, status: &str, filled: Filled) -> ServiceJob {
    let store = batch.store();
    let mut store = store.lock();
    let id = store.new_uuid();
    let created_at = state::now_millis();
    let dispatched = !matches!(status, "SUBMITTED" | "PENDING" | "RUNNABLE");
    let running = !matches!(status, "SUBMITTED" | "PENDING" | "RUNNABLE" | "SCHEDULED");
    let terminal = TERMINAL_STATUSES.contains(&status);
    let job = ServiceJob {
        job_id: id.clone(),
        job_arn: state::service_job_arn(REGION, &id),
        job_name: format!("sj-{}", status.to_lowercase()),
        job_queue_arn: state::job_queue_arn(REGION, "q-service-jobs"),
        service_job_type: SERVICE_JOB_TYPE.to_string(),
        status: status.to_string(),
        status_reason: terminal.then(|| "the run's own reason".to_string()),
        created_at,
        scheduled_at: dispatched.then_some(created_at + 10),
        started_at: running.then_some(created_at + 20),
        stopped_at: terminal.then_some(created_at + 30),
        // A seeded record owes the clock nothing: the row under test is the status it
        // was seeded in, which only a call may move.
        next_phase_due_at: None,
        is_terminated: false,
        share_identifier: (filled == Filled::Everything).then(|| "share-a".to_string()),
        quota_share_name: (filled == Filled::Everything).then(|| "quota-a".to_string()),
        scheduling_priority: (filled == Filled::Everything).then_some(42),
        service_request_payload: (filled == Filled::Everything)
            .then(|| r#"{"TrainingJobName":"t-1","InstanceCount":2}"#.to_string()),
        // The seeded payload states no `ResourceConfig`, so it asks for no capacity this
        // service can measure — the demand a submit would derive from it is empty.
        requested_capacity: Vec::new(),
        retry_strategy: (filled == Filled::Everything).then(retry_strategy),
        timeout_config: (filled == Filled::Everything).then_some(ServiceJobTimeout {
            attempt_duration_seconds: Some(600),
        }),
        preemption_configuration: (filled == Filled::Everything).then_some(
            ServiceJobPreemptionConfiguration {
                preemption_retries_before_termination: Some(2),
            },
        ),
        tags: (filled == Filled::Everything).then(|| {
            [
                ("env".to_string(), "test".to_string()),
                ("team".to_string(), "batch".to_string()),
            ]
            .into_iter()
            .collect()
        }),
        // No `SubmitServiceJob` request minted this record, so it carries neither the
        // token nor the digest of one.
        client_token: None,
        request_digest: None,
        // A seeded record stands for a job whose work the downstream service already
        // holds once the row's status says it has been dispatched, so the identifiers a
        // dispatch would have recorded are seeded with it and nothing is owed.
        dispatch_state: if dispatched {
            state::ServiceJobDispatchState::Dispatched
        } else {
            state::ServiceJobDispatchState::Waiting
        },
        dispatch_attempted_at: dispatched.then_some(created_at + 5),
        service_resource_name: dispatched.then(|| "t-1".to_string()),
        service_resource_arn: dispatched
            .then(|| format!("arn:aws:sagemaker:{REGION}:000000000000:training-job/t-1")),
    };
    store
        .insert_service_job(job.clone())
        .expect("a seeded service job fits in the store");
    job
}

/// `DescribeServiceJob` over the wire: the status and the decoded body.
pub(crate) async fn describe(batch: &Batch, job_id: &str) -> (u16, Value) {
    batch
        .call("/v1/describeservicejob", json!({"jobId": job_id}))
        .await
}

/// `DescribeServiceJob` over the wire, requiring the 200.
pub(crate) async fn described(batch: &Batch, case: &str, job_id: &str) -> Value {
    let (status, body) = describe(batch, job_id).await;
    assert_eq!(
        status, 200,
        "case {case}: expected DescribeServiceJob to answer 200, got {status} {body}"
    );
    body
}

/// `TerminateServiceJob` on `job_id` with `reason`, asserting the 200 and the empty body
/// the operation models (`TerminateServiceJobResponse` has no members).
pub(crate) async fn terminate(batch: &Batch, case: &str, job_id: &str, reason: &str) {
    let (status, body) = batch
        .call(
            "/v1/terminateservicejob",
            json!({"jobId": job_id, "reason": reason}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case}: expected TerminateServiceJob to succeed with 200, got {status} {body}"
    );
    assert_eq!(
        body,
        json!({}),
        "case {case}: TerminateServiceJob models no output members, got {body}"
    );
}

/// `TerminateServiceJobs` (the BULK form) over the wire: the status and the decoded body.
async fn terminate_many(batch: &Batch, jobs: &[String], reason: &str) -> (u16, Value) {
    batch
        .call(
            "/v1/terminateservicejobs",
            json!({"jobs": jobs, "reason": reason}),
        )
        .await
}

/// `detail` with the members a termination is allowed to change removed, so two of these
/// compare everything ELSE the answer carries.
fn untouched(detail: &Value) -> Value {
    let mut copy = detail.clone();
    if let Some(object) = copy.as_object_mut() {
        for member in TERMINATION_MEMBERS {
            object.remove(*member);
        }
    }
    copy
}

/// The non-terminal `ServiceJobStatus` values, from the model's own enum.
pub(crate) fn live_statuses() -> Vec<&'static str> {
    SERVICE_JOB_STATUS_VALUES
        .iter()
        .copied()
        .filter(|status| !TERMINAL_STATUSES.contains(status))
        .collect()
}

/// RULE: `TerminateServiceJob` ends a service job that has not reached a terminal
/// `ServiceJobStatus` — `FAILED`, `isTerminated`, the caller's `reason` as its
/// `statusReason`, `stoppedAt` at the call — and changes nothing else about it. One row
/// per non-terminal status the model declares, with the `reason` differing in size (one
/// character, a short one, 1024 characters) and alphabet (ASCII, non-ASCII) across them.
#[tokio::test]
async fn terminating_a_live_service_job_ends_it_with_the_callers_reason() {
    let reasons: Vec<(&str, String)> = vec![
        ("a plain reason", "stop it".to_string()),
        ("a one-character reason", "x".to_string()),
        ("a 1024-character reason", "r".repeat(1024)),
        ("a non-ASCII reason", "arrêt immédiat ✋ 停止".to_string()),
    ];
    let statuses = live_statuses();
    assert!(
        statuses.len() > 1,
        "case the status catalog: the model declares more than one non-terminal \
         ServiceJobStatus, got {statuses:?}"
    );

    for (index, status) in statuses.iter().enumerate() {
        let (reason_case, reason) = &reasons[index % reasons.len()];
        let case = format!("a {status} service job, {reason_case}");
        let batch = Batch::new();
        let seeded = seed(&batch, status, Filled::Everything);

        let before = described(&batch, &case, &seeded.job_id).await;
        assert_eq!(
            before["status"].as_str(),
            Some(*status),
            "case {case}: the seeded job must report its own status first, got {before}"
        );
        assert_eq!(
            before["isTerminated"].as_bool(),
            Some(false),
            "case {case}: nothing has terminated it yet, got {before}"
        );

        terminate(&batch, &case, &seeded.job_id, reason).await;

        let after = described(&batch, &case, &seeded.job_id).await;
        assert_eq!(
            after["status"].as_str(),
            Some(state::SERVICE_JOB_FAILED),
            "case {case}: a terminated service job ends FAILED, got {after}"
        );
        assert_eq!(
            after["statusReason"].as_str(),
            Some(reason.as_str()),
            "case {case}: the caller's reason is what DescribeServiceJob reports, got {after}"
        );
        assert_eq!(
            after["isTerminated"].as_bool(),
            Some(true),
            "case {case}: a terminated service job reports isTerminated, got {after}"
        );
        assert!(
            after["stoppedAt"].as_i64().is_some_and(|stopped| stopped >= seeded.created_at),
            "case {case}: expected stoppedAt at or after createdAt {}, got {after}",
            seeded.created_at
        );
        assert_eq!(
            untouched(&after),
            untouched(&before),
            "case {case}: terminating must change only {TERMINATION_MEMBERS:?}, got {after} \
             from {before}"
        );
        // The attempt a termination closes carries the ending's own reason, and it is the
        // same one attempt the dispatch started.
        if before["latestAttempt"] != Value::Null {
            assert_eq!(
                after["attempts"][0]["statusReason"].as_str(),
                Some(reason.as_str()),
                "case {case}: the attempt the termination closed carries its reason, got \
                 {after}"
            );
            assert_eq!(
                after["latestAttempt"], before["latestAttempt"],
                "case {case}: the resource the work was dispatched as does not change, got \
                 {after}"
            );
        }
    }
}

/// What had already ended the service job a terminate then arrives for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The record was stored in this terminal status, with its own reason.
    Stored(&'static str),
    /// An earlier `TerminateServiceJob` is what ended it (the idempotence row).
    AlreadyTerminated,
}

/// RULE: a service job already in a terminal `ServiceJobStatus` is left EXACTLY as it is
/// — the whole `DescribeServiceJob` answer, so the reason it already carries is not
/// overwritten — and the call still succeeds. One row per terminal status the model
/// declares, plus a job an earlier `TerminateServiceJob` ended.
#[tokio::test]
async fn terminating_a_service_job_that_already_ended_leaves_it_exactly_as_it_is() {
    let mut rows: Vec<Ended> = TERMINAL_STATUSES.iter().copied().map(Ended::Stored).collect();
    rows.push(Ended::AlreadyTerminated);

    for row in rows {
        let case = format!("{row:?}");
        let batch = Batch::new();
        let seeded = match row {
            Ended::Stored(status) => seed(&batch, status, Filled::Everything),
            Ended::AlreadyTerminated => {
                let seeded = seed(&batch, "RUNNING", Filled::Everything);
                terminate(&batch, &case, &seeded.job_id, "the first reason").await;
                seeded
            }
        };

        let before = described(&batch, &case, &seeded.job_id).await;
        assert!(
            before["status"]
                .as_str()
                .is_some_and(state::is_terminal_service_job_status),
            "case {case}: this row must start in a terminal status, got {before}"
        );

        terminate(&batch, &case, &seeded.job_id, "the second reason").await;

        let after = described(&batch, &case, &seeded.job_id).await;
        assert_eq!(
            after, before,
            "case {case}: terminating a job that already ended must change nothing, got {after}"
        );
        assert_ne!(
            after["statusReason"].as_str(),
            Some("the second reason"),
            "case {case}: the reason that ended it is the one it keeps, got {after}"
        );
    }
}

/// RULE: `TerminateServiceJob` for a `jobId` the service holds no service job for
/// succeeds and persists nothing, so the id stays unresolvable — `DescribeServiceJob`
/// still answers the modelled `ClientException` (HTTP 400) for it — and the service jobs
/// the store does hold are untouched. One row per form an unknown id can take, none of
/// which this service minted.
#[tokio::test]
async fn terminating_a_service_job_the_service_does_not_hold_is_accepted_and_persists_nothing() {
    let batch = Batch::new();
    let live = seed(&batch, "SUBMITTED", Filled::Required);

    let unknown: Vec<(&str, String)> = vec![
        ("a word", "does-not-exist".to_string()),
        (
            "a UUID this service never minted",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "a service job ARN",
            state::service_job_arn(REGION, "00000000-0000-4000-8000-000000000001"),
        ),
        ("one character", "x".to_string()),
        ("a 128-character id", "9".repeat(128)),
        (
            "the live job's id with one digit changed",
            flip_last_hex_digit(&live.job_id),
        ),
    ];

    for (case, id) in &unknown {
        assert_ne!(
            id, &live.job_id,
            "case {case}: the row must not name the live service job, got {id:?}"
        );

        terminate(&batch, case, id, "no such service job").await;

        let (status, body) = describe(&batch, id).await;
        assert_eq!(
            status, 400,
            "case {case}: an unknown id stays unresolvable, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ClientException",
            "case {case}: expected the modelled ClientException, got {body}"
        );
    }

    let detail = described(&batch, "the live service job", &live.job_id).await;
    assert_eq!(
        detail["status"].as_str(),
        Some("SUBMITTED"),
        "case the live service job: terminating other ids must not touch it, got {detail}"
    );
    assert_eq!(
        detail["isTerminated"].as_bool(),
        Some(false),
        "case the live service job: it was never terminated, got {detail}"
    );
    assert_eq!(
        detail["statusReason"],
        Value::Null,
        "case the live service job: no reason was recorded on it, got {detail}"
    );
}

/// RULE: `DescribeServiceJob` reports the record the store holds, member for member, and
/// invents nothing for the attempts a service job's dispatched work would have had — no
/// runtime here dispatches one. Two rows: a record carrying every optional member, and
/// one carrying only the required members, whose optional members come back ABSENT.
#[tokio::test]
async fn describe_service_job_reports_the_stored_record_member_for_member() {
    for (case, status, filled) in [
        ("every member filled", "RUNNING", Filled::Everything),
        ("only the required members", "SUBMITTED", Filled::Required),
    ] {
        let batch = Batch::new();
        let seeded = seed(&batch, status, filled);
        let detail = described(&batch, case, &seeded.job_id).await;

        assert_eq!(
            detail["jobId"].as_str(),
            Some(seeded.job_id.as_str()),
            "case {case}: expected the stored jobId, got {detail}"
        );
        assert_eq!(
            detail["jobArn"].as_str(),
            Some(seeded.job_arn.as_str()),
            "case {case}: expected the stored jobArn, got {detail}"
        );
        assert_eq!(
            detail["jobName"].as_str(),
            Some(seeded.job_name.as_str()),
            "case {case}: expected the stored jobName, got {detail}"
        );
        assert_eq!(
            detail["jobQueue"].as_str(),
            Some(seeded.job_queue_arn.as_str()),
            "case {case}: jobQueue is the ARN of the queue the job is in, got {detail}"
        );
        assert_eq!(
            detail["serviceJobType"].as_str(),
            Some(SERVICE_JOB_TYPE),
            "case {case}: expected the stored serviceJobType, got {detail}"
        );
        assert_eq!(
            detail["status"].as_str(),
            Some(status),
            "case {case}: expected the stored status, got {detail}"
        );
        assert_eq!(
            detail["createdAt"].as_i64(),
            Some(seeded.created_at),
            "case {case}: expected the stored createdAt, got {detail}"
        );
        assert_eq!(
            detail["startedAt"].as_i64(),
            seeded.started_at,
            "case {case}: startedAt is the instant the run began, absent for a job nothing \
             started, got {detail}"
        );
        assert_eq!(
            detail["isTerminated"].as_bool(),
            Some(false),
            "case {case}: nothing terminated this job, got {detail}"
        );
        // The identifiers a dispatch records are reported only for a job whose work the
        // downstream service was actually handed: a seeded row that has not been dispatched
        // has had no attempt, so it reports none.
        let dispatched = seeded.service_resource_arn.is_some();
        if dispatched {
            assert_eq!(
                detail["latestAttempt"]["serviceResourceId"],
                json!({
                    "name": "TrainingJobArn",
                    "value": seeded.service_resource_arn.clone().unwrap_or_default(),
                }),
                "case {case}: expected the attempt to name the stored resource, got {detail}"
            );
            assert_eq!(
                detail["attempts"][0]["serviceResourceId"]["value"].as_str(),
                seeded.service_resource_arn.as_deref(),
                "case {case}: the one attempt a dispatch started names the same resource, \
                 got {detail}"
            );
        } else {
            for member in ["attempts", "latestAttempt"] {
                assert_eq!(
                    detail[member],
                    Value::Null,
                    "case {case}: nothing dispatched this job's work, so {member} is not \
                     invented, got {detail}"
                );
            }
        }
        // The seeded payload states no `ResourceConfig`, so the job asks for no capacity
        // this service can measure and holds none; nothing here preempts a job either.
        for member in ["capacityUsage", "preemptionSummary"] {
            assert_eq!(
                detail[member],
                Value::Null,
                "case {case}: {member} is not invented, got {detail}"
            );
        }

        match filled {
            Filled::Everything => {
                assert_eq!(
                    detail["scheduledAt"].as_i64(),
                    seeded.scheduled_at,
                    "case {case}: expected the stored scheduledAt, got {detail}"
                );
                assert_eq!(
                    detail["shareIdentifier"].as_str(),
                    Some("share-a"),
                    "case {case}: expected the stored shareIdentifier, got {detail}"
                );
                assert_eq!(
                    detail["quotaShareName"].as_str(),
                    Some("quota-a"),
                    "case {case}: expected the stored quotaShareName, got {detail}"
                );
                assert_eq!(
                    detail["schedulingPriority"].as_i64(),
                    Some(42),
                    "case {case}: expected the stored schedulingPriority, got {detail}"
                );
                assert_eq!(
                    detail["serviceRequestPayload"].as_str(),
                    seeded.service_request_payload.as_deref(),
                    "case {case}: expected the stored serviceRequestPayload, got {detail}"
                );
                assert_eq!(
                    detail["retryStrategy"],
                    json!({
                        "attempts": 3,
                        "evaluateOnExit": [{"action": "RETRY", "onStatusReason": "Internal*"}],
                    }),
                    "case {case}: expected the stored retryStrategy, got {detail}"
                );
                assert_eq!(
                    detail["timeoutConfig"],
                    json!({"attemptDurationSeconds": 600}),
                    "case {case}: expected the stored timeoutConfig, got {detail}"
                );
                assert_eq!(
                    detail["preemptionConfiguration"],
                    json!({"preemptionRetriesBeforeTermination": 2}),
                    "case {case}: expected the stored preemptionConfiguration, got {detail}"
                );
                assert_eq!(
                    detail["tags"],
                    json!({"env": "test", "team": "batch"}),
                    "case {case}: expected the stored tags, got {detail}"
                );
            }
            Filled::Required => {
                for member in [
                    "statusReason",
                    "scheduledAt",
                    "stoppedAt",
                    "shareIdentifier",
                    "quotaShareName",
                    "schedulingPriority",
                    "serviceRequestPayload",
                    "retryStrategy",
                    "timeoutConfig",
                    "preemptionConfiguration",
                ] {
                    assert_eq!(
                        detail[member],
                        Value::Null,
                        "case {case}: the record carries no {member}, so the answer must \
                         not either, got {detail}"
                    );
                }
                // `tags` is the exception: Batch's tag plane MATERIALISES the map, so a
                // record carrying none is reported with an empty one rather than with the
                // member missing.
                assert_eq!(
                    detail["tags"],
                    json!({}),
                    "case {case}: a record with no tags must report an empty map, got \
                     {detail}"
                );
            }
        }
    }
}

/// RULE: `TerminateServiceJobs` is the bulk `TerminateServiceJob` — every id it lists is
/// ended exactly as one call per id would end it, and the outcome is reported PER JOB
/// rather than as the call's status: the call answers 200 with every id it was given in
/// `successful` ("Service jobs that can't be found are treated as successfully processed")
/// and an empty `errors`, a job already in a terminal status is left exactly as it is, an
/// unknown id changes nothing, and a service job the request does NOT list is untouched.
///
/// The one request spans what the rule generalises over: a live job in every non-terminal
/// `ServiceJobStatus` the model declares, a job in each terminal status, two forms of
/// unknown id, and the same id listed twice.
#[tokio::test]
async fn terminating_service_jobs_in_bulk_ends_each_listed_job_as_the_singular_call_does() {
    const REASON: &str = "end them all";
    let batch = Batch::new();

    let live: Vec<ServiceJob> = live_statuses()
        .iter()
        .map(|status| seed(&batch, status, Filled::Everything))
        .collect();
    let already_ended: Vec<ServiceJob> = TERMINAL_STATUSES
        .iter()
        .map(|status| seed(&batch, status, Filled::Everything))
        .collect();
    // A live service job the request never names: nothing may happen to it.
    let unlisted = seed(&batch, state::SERVICE_JOB_SUBMITTED, Filled::Required);
    let unknown: Vec<String> = vec![
        "does-not-exist".to_string(),
        "00000000-0000-4000-8000-000000000000".to_string(),
    ];

    let mut ended_before: Vec<Value> = Vec::new();
    for job in &already_ended {
        ended_before.push(described(&batch, "already ended", &job.job_id).await);
    }

    let mut listed: Vec<String> = live.iter().map(|job| job.job_id.clone()).collect();
    listed.extend(already_ended.iter().map(|job| job.job_id.clone()));
    listed.extend(unknown.iter().cloned());
    // Every id named so far is a distinct job, and `successful` names the JOBS whose
    // termination the call accepted — so this is the answer expected, whatever the request
    // repeats after it.
    let expected: Vec<String> = listed.clone();
    // The same id once more: one request naming one job twice.
    listed.push(live[0].job_id.clone());

    let (status, body) = terminate_many(&batch, &listed, REASON).await;
    assert_eq!(
        status, 200,
        "case the bulk termination: expected 200, got {status} {body}"
    );
    assert_eq!(
        body["successful"],
        json!(expected),
        "case the bulk termination: every job the request named is successful, and the job it \
         named twice once, got {body}"
    );
    assert_eq!(
        body["errors"],
        json!([]),
        "case the bulk termination: no id this service could address failed, got {body}"
    );

    for job in &live {
        let case = format!("the {} job the bulk call listed", job.status);
        let after = described(&batch, &case, &job.job_id).await;
        assert_eq!(
            after["status"].as_str(),
            Some(state::SERVICE_JOB_FAILED),
            "case {case}: a bulk-terminated service job ends FAILED, got {after}"
        );
        assert_eq!(
            after["statusReason"].as_str(),
            Some(REASON),
            "case {case}: the caller's reason is what DescribeServiceJob reports, got {after}"
        );
        assert_eq!(
            after["isTerminated"].as_bool(),
            Some(true),
            "case {case}: a bulk-terminated service job reports isTerminated, got {after}"
        );
        assert!(
            after["stoppedAt"]
                .as_i64()
                .is_some_and(|stopped| stopped >= job.created_at),
            "case {case}: expected stoppedAt at or after createdAt {}, got {after}",
            job.created_at
        );
    }

    for (job, before) in already_ended.iter().zip(&ended_before) {
        let case = format!("the {} job the bulk call listed", job.status);
        let after = described(&batch, &case, &job.job_id).await;
        assert_eq!(
            &after, before,
            "case {case}: a job that already ended is left exactly as it is, got {after}"
        );
    }

    for id in &unknown {
        let (status, body) = describe(&batch, id).await;
        assert_eq!(
            status, 400,
            "case {id:?}: an id the bulk call could not resolve stays unresolvable, got \
             {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ClientException",
            "case {id:?}: expected the modelled ClientException, got {body}"
        );
    }

    let after = described(&batch, "the unlisted job", &unlisted.job_id).await;
    assert_eq!(
        after["status"].as_str(),
        Some(state::SERVICE_JOB_SUBMITTED),
        "case the unlisted job: a bulk call ends only what it listed, got {after}"
    );
    assert_eq!(
        after["isTerminated"].as_bool(),
        Some(false),
        "case the unlisted job: it was never terminated, got {after}"
    );
    assert_eq!(
        after["statusReason"],
        Value::Null,
        "case the unlisted job: no reason was recorded on it, got {after}"
    );
}

/// RULE: a bulk termination naming more service jobs than the documented maximum ("up to 50
/// service job IDs", `state::limits::MAX_BULK_JOBS`) is refused WHOLE with the modelled
/// `ClientException` (HTTP 400) and ends nothing, while a request naming exactly the maximum
/// is served.
///
/// The rows are the bound itself and one past it, each carrying the same live service job,
/// so what decides the answer is the number of ids and nothing else.
#[tokio::test]
async fn a_bulk_service_job_termination_holds_the_documented_maximum() {
    let maximum = state::limits::MAX_BULK_JOBS;
    // (case, how many ids the request lists, the status expected)
    let rows: Vec<(&str, usize, u16)> = vec![
        ("one past the documented maximum", maximum + 1, 400),
        ("exactly the documented maximum", maximum, 200),
    ];

    let batch = Batch::new();
    let live = seed(&batch, "RUNNING", Filled::Required);
    for (case, count, expected) in rows {
        // The live job first, then as many ids this service never minted as the row needs.
        let mut jobs: Vec<String> = vec![live.job_id.clone()];
        jobs.extend((1..count).map(|index| format!("00000000-0000-4000-8000-{index:012}")));
        assert_eq!(
            jobs.len(),
            count,
            "case {case:?}: the row must list {count} ids, got {}",
            jobs.len()
        );

        let (status, body) = terminate_many(&batch, &jobs, "over the bound").await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected}, got {status} {body}"
        );
        let detail = described(&batch, case, &live.job_id).await;
        if expected == 400 {
            assert_eq!(
                error_code(&body),
                "ClientException",
                "case {case:?}: expected the modelled ClientException, got {body}"
            );
            assert_eq!(
                detail["status"].as_str(),
                Some("RUNNING"),
                "case {case:?}: a refused bulk call ends nothing it named, got {detail}"
            );
            assert_eq!(
                detail["isTerminated"].as_bool(),
                Some(false),
                "case {case:?}: a refused bulk call terminates nothing, got {detail}"
            );
        } else {
            assert_eq!(
                detail["status"].as_str(),
                Some(state::SERVICE_JOB_FAILED),
                "case {case:?}: a served bulk call ends the job it named, got {detail}"
            );
        }
    }
}
