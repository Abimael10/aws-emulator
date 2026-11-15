// SPDX-License-Identifier: CC0-1.0

//! Changing the status of a Macie classification job — `UpdateClassificationJob`
//! — end to end over the wire, read back through `DescribeClassificationJob`.
//!
//! Each case drives the production `MultiServiceRouter` through the harness
//! `macie_session_and_custom_data_identifiers` builds, so what is pinned is the
//! HTTP behaviour and the response BODIES rather than a handler called directly.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * a request moves a job to the status it asks for ONLY when that status is
//!   one the member documents a transition for AND the job's current status is
//!   one that status is valid from; a status the job's current status may not
//!   move to is refused 400 `ValidationException` worded with the status the
//!   job is in and the job's ARN, a status no request may ask for is refused
//!   with the member's own list of valid values and that ARN, and either way
//!   the stored job is left exactly as it was — asserted over the whole matrix
//!   of current status by requested status;
//! * pausing a job records WHEN it was paused and when the paused job or job
//!   run expires if it is not resumed, which a read reports as
//!   `userPausedDetails`, and resuming it clears that and puts the rest of the
//!   run back;
//! * a cancel is terminal: no later request moves the job, whatever it asks
//!   for;
//! * a request naming a job the addressed region holds none of is refused 404
//!   `ResourceNotFoundException`, and one addressing a region Macie is not
//!   enabled in is refused 403 `AccessDeniedException`.

use smithy_server_runtime::MultiServiceRouter;

use super::classification_jobs::{create, describe, one_time, scheduled};
use super::macie_session_and_custom_data_identifiers::{
    call_in, emulator, enable_in, error_code, error_message, reported_instant,
};

/// The region most cases sign for.
const REGION: &str = "us-west-2";

/// A second region, for the case that pins the per-region scope of a job.
const OTHER_REGION: &str = "eu-west-1";

/// The job collection's URI, from the `@http` bindings of this plane.
const JOBS_URI: &str = "/jobs";

/// Every value the modelled `JobStatus` enum declares. Source:
/// `smithy_query(query="shape",shape="JobStatus")` — RUNNING, PAUSED,
/// CANCELLED, COMPLETE, IDLE, USER_PAUSED.
const JOB_STATUSES: [&str; 6] = [
    "RUNNING",
    "PAUSED",
    "CANCELLED",
    "COMPLETE",
    "IDLE",
    "USER_PAUSED",
];

/// The statuses a request may ask a job to move to, each with the current
/// statuses it is valid from. Written out here rather than read from the crate,
/// so this expectation can fail. Source: the `jobStatus` member doc of
/// `UpdateClassificationJobRequest` — "CANCELLED … is valid only if the job's
/// current status is IDLE, PAUSED, RUNNING, or USER_PAUSED", "RUNNING - Resumes
/// the job. This value is valid only if the job's current status is
/// USER_PAUSED", "USER_PAUSED - Pauses the job temporarily. This value is valid
/// only if the job's current status is IDLE, PAUSED, or RUNNING."
const REQUESTABLE: [(&str, &[&str]); 3] = [
    ("CANCELLED", &["IDLE", "PAUSED", "RUNNING", "USER_PAUSED"]),
    ("RUNNING", &["USER_PAUSED"]),
    ("USER_PAUSED", &["IDLE", "PAUSED", "RUNNING"]),
];

/// How long a paused job or job run stays paused before it expires, in seconds.
/// Source: the same member doc — "If you pause a one-time job and you don't
/// resume it within 30 days, the job expires and Macie cancels the job."
const PAUSE_EXPIRES_AFTER_SECONDS: f64 = 30.0 * 24.0 * 60.0 * 60.0;

/// A deadline generous enough to outlast a loaded host: every wait in this
/// suite is for a CONDITION, re-read until it holds or this passes.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// How often a wait re-reads the condition it is waiting for.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Ask for a new status for `job_id`.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    job_id: &str,
    job_status: &str,
) -> (u16, serde_json::Value) {
    call_in(
        router,
        region,
        "PATCH",
        &format!("{JOBS_URI}/{job_id}"),
        &serde_json::json!({ "jobStatus": job_status }).to_string(),
    )
    .await
}

/// The status a read reports for `job_id`.
async fn reported_status(router: &MultiServiceRouter, job_id: &str) -> String {
    let (status, reply) = describe(router, REGION, job_id).await;
    assert_eq!(
        status, 200,
        "the job must be readable, got {status} with {reply:?}"
    );
    reply["jobStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("a read must report a jobStatus, got {reply:?}"))
        .to_string()
}

/// A one-time job whose run has reached its end, which is the only way a job
/// gets to a status the service — rather than a request — moved it to. Waits
/// for the CONDITION, so nothing here depends on how fast the host runs.
async fn completed_job(router: &MultiServiceRouter, token: &str) -> (String, String) {
    let (job_id, job_arn) = create(
        router,
        REGION,
        &one_time(token, serde_json::json!({})),
        "a job whose run reaches its end",
    )
    .await;
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let status = reported_status(router, &job_id).await;
        if status == "COMPLETE" {
            return (job_id, job_arn);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {token:?}: the started run never ended; the last read reported {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One job in `current`, with its ARN, with every status this suite's matrix
/// covers reachable through the API alone.
///
/// `PAUSED` is left out: it is the status Macie itself moves a job to when
/// further processing would exceed an account's discovery quota, and nothing a
/// caller can do reaches it.
async fn job_in(router: &MultiServiceRouter, current: &str, token: &str) -> (String, String) {
    match current {
        // A one-time job's only run starts when the job is created.
        "RUNNING" => {
            create(
                router,
                REGION,
                &one_time(token, serde_json::json!({})),
                current,
            )
            .await
        }
        // A recurring job that was not configured to analyze existing objects
        // has started no run and waits for its first scheduled one.
        "IDLE" => {
            create(
                router,
                REGION,
                &scheduled(token, serde_json::json!({})),
                current,
            )
            .await
        }
        "COMPLETE" => completed_job(router, token).await,
        "USER_PAUSED" | "CANCELLED" => {
            let (job_id, job_arn) = create(
                router,
                REGION,
                &one_time(token, serde_json::json!({})),
                current,
            )
            .await;
            let (status, reply) = update(router, REGION, &job_id, current).await;
            assert_eq!(
                status, 200,
                "case {current:?}: moving a running job to {current} must answer 200, got \
                 {status} with {reply:?}"
            );
            (job_id, job_arn)
        }
        other => panic!("case {other:?}: this suite reaches no job in that status"),
    }
}

/// The current statuses `requested` is a valid new status from, per
/// [`REQUESTABLE`], or `None` when no request may ask for it.
fn valid_from(requested: &str) -> Option<&'static [&'static str]> {
    REQUESTABLE
        .iter()
        .find(|(status, _)| *status == requested)
        .map(|(_, from)| *from)
}

/// Whether a request asking a job in `current` for `requested` is one the
/// member documents.
fn is_documented(requested: &str, current: &str) -> bool {
    valid_from(requested).is_some_and(|from| from.contains(&current))
}

/// The refusal a status the job's CURRENT status may not move to is worded
/// with: the service's own text, naming the status the job is in and the job's
/// ARN.
fn transition_refusal(current: &str, job_arn: &str) -> String {
    format!(
        "Input validation failed, cannot update {} job for job: {job_arn}",
        current.to_lowercase()
    )
}

/// The refusal a status NO request may ask for is worded with — a different
/// refusal, because what is wrong is the status the request asked for rather
/// than the status the job is in: it names the member's own list of valid
/// values and the job's ARN.
fn value_refusal(job_arn: &str) -> String {
    let requestable: Vec<&str> = REQUESTABLE.iter().map(|(status, _)| *status).collect();
    format!(
        "Input validation failed, jobStatus must be one of {} for job: {job_arn}",
        requestable.join(", ")
    )
}

/// The matrix over the statuses a job RESTS in: every status a request may
/// name, asked of a job in each of them. A documented transition moves the job;
/// a status the job's current status may not move to, and a status no request
/// may ask for, are each refused 400 `ValidationException` with their own
/// wording and leave the job's status exactly as it was.
///
/// `RUNNING` is covered by the test below instead: a run in progress is the one
/// status the clock moves on its own, so a case that asked for it to still be
/// in progress would turn on how fast the host runs. `PAUSED` is not reached by
/// any call a caller can make.
#[tokio::test]
async fn a_status_moves_a_resting_job_only_from_the_statuses_it_is_valid_from() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    for (index, current) in ["IDLE", "USER_PAUSED", "CANCELLED", "COMPLETE"]
        .into_iter()
        .enumerate()
    {
        for (offset, requested) in JOB_STATUSES.into_iter().enumerate() {
            let case = format!("{current} -> {requested}");
            // A fresh job per case, so one case's move cannot be read as
            // another's.
            let (job_id, job_arn) = job_in(
                &router,
                current,
                &format!("t-matrix-{index}-{offset}-{requested}"),
            )
            .await;
            let (status, reply) = update(&router, REGION, &job_id, requested).await;

            if is_documented(requested, current) {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a documented transition must answer 200, got {status} with \
                     {reply:?}"
                );
                assert_eq!(
                    reply,
                    serde_json::json!({}),
                    "case {case:?}: UpdateClassificationJobResponse models no members, got \
                     {reply:?}"
                );
                let now = reported_status(&router, &job_id).await;
                // A resumed run may already have reached its end on a slow
                // host, which is the status the clock owes it — never the
                // pause it was resumed from.
                let acceptable: Vec<&str> = if requested == "RUNNING" {
                    vec!["RUNNING", "COMPLETE"]
                } else {
                    vec![requested]
                };
                assert!(
                    acceptable.contains(&now.as_str()),
                    "case {case:?}: expected one of {acceptable:?}, got {now:?}"
                );
            } else {
                let expected = if valid_from(requested).is_some() {
                    transition_refusal(current, &job_arn)
                } else {
                    value_refusal(&job_arn)
                };
                assert_eq!(
                    (status, error_code(&reply), error_message(&reply)),
                    (400, "ValidationException".to_string(), expected.as_str()),
                    "case {case:?}: a request the member does not document must be refused, \
                     got {status} with {reply:?}"
                );
                assert_eq!(
                    reported_status(&router, &job_id).await,
                    current,
                    "case {case:?}: a refused request must leave the job's status alone"
                );
            }
        }
    }
}

/// The same table for a job whose run is in progress, which is the one status
/// the clock moves on its own: each case accepts either the outcome for
/// `RUNNING` or the outcome for the `COMPLETE` the clock owes that run, so the
/// case says the same thing however fast the host runs — and a status no
/// request may ask for is refused with the same wording either way, because
/// that refusal does not depend on the status the job is in.
#[tokio::test]
async fn a_running_jobs_transitions_follow_the_same_table() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    for (offset, requested) in JOB_STATUSES.into_iter().enumerate() {
        let case = format!("RUNNING -> {requested}");
        let (job_id, job_arn) = job_in(&router, "RUNNING", &format!("t-running-{offset}")).await;
        let (status, reply) = update(&router, REGION, &job_id, requested).await;

        if valid_from(requested).is_none() {
            assert_eq!(
                (status, error_code(&reply), error_message(&reply)),
                (
                    400,
                    "ValidationException".to_string(),
                    value_refusal(&job_arn).as_str()
                ),
                "case {case:?}: a status no request may ask for must be refused, got {status} \
                 with {reply:?}"
            );
            continue;
        }

        if is_documented(requested, "RUNNING") {
            // Valid from RUNNING: either the job was still running, and the
            // move took, or its run had already reached its end, and the move
            // is refused out of COMPLETE.
            if status == 200 {
                assert_eq!(
                    reported_status(&router, &job_id).await,
                    requested,
                    "case {case:?}: a documented transition must leave the job in the status it \
                     asked for"
                );
            } else {
                assert_eq!(
                    (status, error_code(&reply), error_message(&reply)),
                    (
                        400,
                        "ValidationException".to_string(),
                        transition_refusal("COMPLETE", &job_arn).as_str()
                    ),
                    "case {case:?}: the only other outcome is the refusal out of the COMPLETE \
                     the clock owes the run, got {status} with {reply:?}"
                );
            }
            continue;
        }

        // Requestable, but not from RUNNING (a resume) — nor from the COMPLETE
        // the clock may owe the run by now.
        let refused_from = reported_status(&router, &job_id).await;
        assert!(
            ["RUNNING", "COMPLETE"].contains(&refused_from.as_str()),
            "case {case:?}: expected the job to be running or complete, got {refused_from:?}"
        );
        assert_eq!(
            (status, error_code(&reply)),
            (400, "ValidationException".to_string()),
            "case {case:?}: a status valid from neither must be refused, got {status} with \
             {reply:?}"
        );
        let acceptable = [
            transition_refusal("RUNNING", &job_arn),
            transition_refusal("COMPLETE", &job_arn),
        ];
        assert!(
            acceptable.iter().any(|text| text == error_message(&reply)),
            "case {case:?}: expected one of {acceptable:?}, got {:?}",
            error_message(&reply)
        );
    }
}

/// Pausing a job records the pause: a read reports `userPausedDetails` with
/// when the job was paused and when the paused job expires if it is not
/// resumed, the pause outlasts the run it interrupted, and resuming the job
/// clears the pause and puts the rest of that run back — so the resumed run
/// reaches its own end.
#[tokio::test]
async fn a_pause_is_recorded_outlasts_the_run_and_is_cleared_by_a_resume() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let (job_id, _) = create(
        &router,
        REGION,
        &one_time("t-pause-cycle", serde_json::json!({})),
        "a running one-time job",
    )
    .await;
    let (_, before) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        (&before["jobStatus"], &before["userPausedDetails"]),
        (
            &serde_json::json!("RUNNING"),
            &serde_json::Value::Null
        ),
        "case \"before the pause\": a job nothing has paused reports no pause, got {before:?}"
    );

    let (status, reply) = update(&router, REGION, &job_id, "USER_PAUSED").await;
    assert_eq!(
        status, 200,
        "case \"the pause\": must answer 200, got {status} with {reply:?}"
    );

    let (_, paused) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        paused["jobStatus"],
        serde_json::json!("USER_PAUSED"),
        "case \"the pause\": expected USER_PAUSED, got {paused:?}"
    );
    let details = &paused["userPausedDetails"];
    assert!(
        details.is_object(),
        "case \"the pause\": userPausedDetails is present while the pause is in effect, got \
         {paused:?}"
    );
    let paused_at = reported_instant("the pause", "jobPausedAt", details);
    let expires_at = reported_instant("the pause", "jobExpiresAt", details);
    assert_eq!(
        expires_at - paused_at,
        PAUSE_EXPIRES_AFTER_SECONDS,
        "case \"the pause\": a paused job expires {PAUSE_EXPIRES_AFTER_SECONDS} seconds after \
         it was paused, got {details:?}"
    );
    assert_eq!(
        details["jobImminentExpirationHealthEventArn"],
        serde_json::Value::Null,
        "case \"the pause\": this emulator sends no Health event, so there is no event ARN to \
         report, got {details:?}"
    );

    // The pause outlasts the run it interrupted: a sibling job created
    // alongside reaches the end of ITS run, and the paused job is still paused
    // — the CONDITION, not a fixed wait, is what this waits for.
    let (sibling, _) = completed_job(&router, "t-pause-sibling").await;
    assert_eq!(
        reported_status(&router, &sibling).await,
        "COMPLETE",
        "the sibling job's run must have ended for this case to say anything"
    );
    assert_eq!(
        reported_status(&router, &job_id).await,
        "USER_PAUSED",
        "case \"a pause left in place\": a pause does not end when the run it interrupted \
         would have"
    );

    let (status, reply) = update(&router, REGION, &job_id, "RUNNING").await;
    assert_eq!(
        status, 200,
        "case \"the resume\": must answer 200, got {status} with {reply:?}"
    );
    let (_, resumed) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        resumed["userPausedDetails"],
        serde_json::Value::Null,
        "case \"the resume\": a resumed job reports no pause, got {resumed:?}"
    );

    // What was left of the run when the pause interrupted it is what is left to
    // go, so the resumed run reaches its own end.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    let ended = loop {
        let status = reported_status(&router, &job_id).await;
        if status != "RUNNING" {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case \"the resumed run\": the run never ended"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        ended, "COMPLETE",
        "case \"the resumed run\": a one-time job's run ends COMPLETE, got {ended:?}"
    );
}

/// A cancel is terminal: "You can't resume or restart a job after you cancel
/// it." Every status a later request names is refused, the job stays
/// CANCELLED, and nothing the clock owes moves it either.
#[tokio::test]
async fn a_cancelled_job_is_terminal() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let (job_id, job_arn) = job_in(&router, "CANCELLED", "t-cancel-terminal").await;
    let (_, cancelled) = describe(&router, REGION, &job_id).await;
    assert_eq!(
        (&cancelled["jobStatus"], &cancelled["userPausedDetails"]),
        (
            &serde_json::json!("CANCELLED"),
            &serde_json::Value::Null
        ),
        "case \"the cancel\": expected CANCELLED and no pause, got {cancelled:?}"
    );

    for requested in JOB_STATUSES {
        let (status, reply) = update(&router, REGION, &job_id, requested).await;
        let expected = if valid_from(requested).is_some() {
            transition_refusal("CANCELLED", &job_arn)
        } else {
            value_refusal(&job_arn)
        };
        assert_eq!(
            (status, error_code(&reply), error_message(&reply)),
            (400, "ValidationException".to_string(), expected.as_str()),
            "case {requested:?} on a cancelled job: expected the refusal a cancelled job \
             carries, got {status} with {reply:?}"
        );
        assert_eq!(
            reported_status(&router, &job_id).await,
            "CANCELLED",
            "case {requested:?} on a cancelled job: the job must stay cancelled"
        );
    }

    // Nor does the clock move it: a sibling job's run reaches its end, and the
    // cancelled job is still cancelled.
    let (sibling, _) = completed_job(&router, "t-cancel-sibling").await;
    assert_eq!(
        reported_status(&router, &sibling).await,
        "COMPLETE",
        "the sibling job's run must have ended for this case to say anything"
    );
    assert_eq!(
        reported_status(&router, &job_id).await,
        "CANCELLED",
        "case \"a cancelled job later on\": CANCELLED is terminal"
    );
}

/// What the operation refuses before it looks at any transition: a job the
/// addressed region holds none of, and a region Macie is not enabled in.
#[tokio::test]
async fn a_job_the_region_does_not_hold_is_refused() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;

    let (job_id, _) = job_in(&router, "RUNNING", "t-scope").await;

    // The same job id, addressed in the region that holds no such job.
    let (status, reply) = update(&router, OTHER_REGION, &job_id, "USER_PAUSED").await;
    assert_eq!(
        (status, error_code(&reply), error_message(&reply)),
        (
            404,
            "ResourceNotFoundException".to_string(),
            format!("Classification Job not found for id: {job_id}").as_str()
        ),
        "case \"another region\": expected 404, got {status} with {reply:?}"
    );
    assert_eq!(
        reported_status(&router, &job_id).await,
        "RUNNING",
        "case \"another region\": the job in its own region must be untouched"
    );

    // An id no region holds.
    let absent = "0123456789abcdefghijkl";
    let (status, reply) = update(&router, REGION, absent, "CANCELLED").await;
    assert_eq!(
        (status, error_code(&reply), error_message(&reply)),
        (
            404,
            "ResourceNotFoundException".to_string(),
            format!("Classification Job not found for id: {absent}").as_str()
        ),
        "case \"an id no region holds\": expected 404, got {status} with {reply:?}"
    );

    // A region Macie is not enabled in: the refusal comes before the job is
    // looked up at all.
    let elsewhere = emulator();
    let (status, reply) = update(&elsewhere, REGION, absent, "CANCELLED").await;
    assert_eq!(
        (status, error_code(&reply)),
        (403, "AccessDeniedException".to_string()),
        "case \"Macie not enabled\": expected 403, got {status} with {reply:?}"
    );
}

/// A `jobStatus` the modelled enum does not declare is refused 400, and so is a
/// request that names no `jobStatus` at all: the member is required.
#[tokio::test]
async fn a_job_status_the_model_does_not_declare_is_refused() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let (job_id, _) = job_in(&router, "RUNNING", "t-unmodelled").await;
    let cases: Vec<(&str, String)> = vec![
        (
            "a status the enum does not declare",
            serde_json::json!({ "jobStatus": "STOPPED" }).to_string(),
        ),
        (
            "a status in the wrong case",
            serde_json::json!({ "jobStatus": "cancelled" }).to_string(),
        ),
        ("no jobStatus at all", "{}".to_string()),
    ];
    for (case, body) in cases {
        let (status, reply) = call_in(
            &router,
            REGION,
            "PATCH",
            &format!("{JOBS_URI}/{job_id}"),
            &body,
        )
        .await;
        assert_eq!(
            (status, error_code(&reply)),
            (400, "ValidationException".to_string()),
            "case {case:?}: expected 400 ValidationException, got {status} with {reply:?}"
        );
        assert_eq!(
            reported_status(&router, &job_id).await,
            "RUNNING",
            "case {case:?}: a refused request must leave the job alone"
        );
    }
}
