// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for how `StopTrainingJob` resolves the job it is asked to stop,
//! and for the stop being backed by the ONE stored job every reader of it reports.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, reusing
//! the harness in [`crate::inference_deployment_lifecycle`], so what these tests exercise
//! is what a real SDK client gets.
//!
//! [`crate::training_job_stop`] pins what a stop DOES to a job it resolves, and the
//! refusals it answers. This suite pins the two halves either side of that:
//!
//! * the name the caller sends is judged against the constraints `TrainingJobName` declares
//!   (`len 1..63`, pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$` — smithy_query
//!   operation=StopTrainingJob), so a value outside them is refused as a
//!   `ValidationException` naming the member and every requirement it failed, at each
//!   published bound and one past it, and a value inside them reaches the lookup;
//! * a stop writes to the stored job itself, so the OTHER reader of a training job
//!   (`ListTrainingJobs`, which projects each stored job into a `TrainingJobSummary` and
//!   filters on `StatusEquals`) reports exactly what `DescribeTrainingJob` reports — and
//!   reports it for the one job the stop named and for no other.
//!
//! Every wait here is a poll for the CONDITION, under a deadline far longer than the span
//! this state gives a stop, so no claim rests on how fast the host runs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator};
use crate::rejection_fidelity::{clause, pattern_requirement, violations};
use crate::training_job_admission::{image_specification, training_body};

/// The member path every clause in this suite reports, as the framework spells a top-level
/// input member.
const MEMBER_PATH: &str = "trainingJobName";

/// Far longer than the span the state gives a requested stop, so a loaded runner still
/// reaches the terminal status; it bounds a BROKEN emulator, never the condition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long to wait between two looks at the job, so the loop re-reads rather than spins.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(20);

/// Seed one running training job under `name` and answer the ARN the create minted.
async fn create_job(emulator: &Emulator, case: &str, name: &str) -> String {
    let body = training_body(name, image_specification(), "s3://demo/output/");
    let (status, created) = emulator.call("CreateTrainingJob", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateTrainingJob to answer 200, got {status} with {created:?}"
    );
    str_field(&created, "TrainingJobArn")
}

/// `StopTrainingJob` for `name`, as the raw (status, body) pair.
async fn stop(emulator: &Emulator, name: &str) -> (u16, Value) {
    emulator
        .call("StopTrainingJob", &json!({ "TrainingJobName": name }))
        .await
}

/// `DescribeTrainingJob` for `name`, as the raw (status, body) pair.
async fn describe(emulator: &Emulator, name: &str) -> (u16, Value) {
    emulator
        .call("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
        .await
}

/// The summary `ListTrainingJobs` reports for `name` under `status_equals`, or `None` when
/// that filter does not select it.
async fn summary(emulator: &Emulator, name: &str, status_equals: &str) -> Option<Value> {
    let listed = emulator
        .ok(
            "ListTrainingJobs",
            &json!({ "StatusEquals": status_equals, "SortBy": "Name" }),
        )
        .await;
    listed
        .get("TrainingJobSummaries")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected TrainingJobSummaries in {listed:?}"))
        .iter()
        .find(|entry| entry.get("TrainingJobName") == Some(&json!(name)))
        .cloned()
}

/// Read `name` again until `DescribeTrainingJob` reports `wanted`, or a generous deadline
/// passes. Waits for the CONDITION — the status the clock owes — never for a fixed time.
async fn poll_until_status(emulator: &Emulator, case: &str, name: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) = describe(emulator, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read of {name} to answer 200, got {status} \
             with {described:?}"
        );
        let reported = str_field(&described, "TrainingJobStatus");
        if reported == wanted {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to report {wanted} before the deadline, \
             last reported {reported:?} in {described:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Read `name` through BOTH readers until they report the same stage of the job: the status
/// `DescribeTrainingJob` reports, and the summary the listing filtered on that status
/// selects. Loops until the pair holds, so a transition the clock owes between the two
/// reads is looked at again rather than failing the caller.
///
/// Answers `(described, listed)`.
async fn read_both(emulator: &Emulator, case: &str, name: &str) -> (Value, Value) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) = describe(emulator, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read of {name} to answer 200, got {status} \
             with {described:?}"
        );
        // The two statuses this operation's doc names for a job it stopped: the transient
        // one the stop takes, and the terminal one the clock owes it.
        let reported = str_field(&described, "TrainingJobStatus");
        assert!(
            reported == "Stopping" || reported == "Stopped",
            "case {case:?}: expected the stop to take {name} into Stopping or the Stopped \
             the clock owes it, got {reported:?} in {described:?}"
        );
        if let Some(listed) = summary(emulator, name, &reported).await {
            return (described, listed);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the listing filtered on the status {name} reports to \
             select it before the deadline, last reported {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// RULE: the name a stop names is judged against the constraints its target shape declares
/// before any lookup happens, so a value outside them is refused with HTTP 400
/// `ValidationException` naming the member and the requirement it failed, and a value
/// inside them reaches the lookup — where it either resolves to a job or is refused with
/// the training-job family's not-found sentence.
///
/// Rows sit on each PUBLISHED bound of `TrainingJobName` (`len 1..63`, pattern
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$` — smithy_query operation=StopTrainingJob) and one
/// value past it, and on the character classes the expression admits and refuses.
#[tokio::test]
async fn a_stop_target_name_is_judged_against_the_modelled_constraints() {
    // The expression as the service reports it: the model's, without the anchors.
    let pattern = pattern_requirement(r"[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}");
    // `TrainingJobName`'s published bounds, and the clause a value past each one fails.
    let longest = "a".repeat(63);
    let too_long = "a".repeat(64);

    // (case, the name sent, every clause the value fails — `None` when the constraints
    // admit it and the name reaches the lookup)
    let rows: Vec<(&str, String, Option<Vec<String>>)> = vec![
        ("at the minimum length", "a".to_string(), None),
        ("at the maximum length", longest.clone(), None),
        (
            "hyphens between alphanumerics, as the expression allows",
            "stop-target-7".to_string(),
            None,
        ),
        (
            // `-*` admits a RUN of hyphens between two alphanumerics.
            "a run of hyphens inside the name",
            "stop--target".to_string(),
            None,
        ),
        ("digits only", "20260101".to_string(), None),
        (
            // Both constraints bound the length — the expression's repeated group ends at
            // 62 characters past the first — so a name one past the maximum is outside
            // both, and each one is reported.
            "one past the maximum length",
            too_long.clone(),
            Some(vec![
                clause(
                    &too_long,
                    MEMBER_PATH,
                    "Member must have length less than or equal to 63",
                ),
                clause(&too_long, MEMBER_PATH, &pattern),
            ]),
        ),
        (
            "a leading hyphen, which the expression's first class refuses",
            "-stop-target".to_string(),
            Some(vec![clause("-stop-target", MEMBER_PATH, &pattern)]),
        ),
        (
            "a trailing hyphen, which the repeated group cannot end on",
            "stop-target-".to_string(),
            Some(vec![clause("stop-target-", MEMBER_PATH, &pattern)]),
        ),
        (
            "an underscore, which no class in the expression admits",
            "stop_target".to_string(),
            Some(vec![clause("stop_target", MEMBER_PATH, &pattern)]),
        ),
        (
            "a dot, which no class in the expression admits",
            "stop.target".to_string(),
            Some(vec![clause("stop.target", MEMBER_PATH, &pattern)]),
        ),
        (
            "a space, which no class in the expression admits",
            "stop target".to_string(),
            Some(vec![clause("stop target", MEMBER_PATH, &pattern)]),
        ),
    ];

    // The name a refused row leaves alone: a job that stands throughout, so a refusal is
    // shown to stop nothing.
    let bystander = "stop-bystander";

    for (case, name, expected) in rows {
        let emulator = Emulator::new();

        let Some(clauses) = expected else {
            // A job stands under the admitted name, so the name is shown to reach the
            // lookup and the stop itself rather than merely to pass validation.
            let created = create_job(&emulator, case, &name).await;
            let (status, answer) = stop(&emulator, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected a name inside the modelled constraints to reach \
                 the stop, got {status} with {answer:?}"
            );
            let (status, described) = describe(&emulator, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the read after the stop to answer 200, got {status} \
                 with {described:?}"
            );
            assert_eq!(
                str_field(&described, "TrainingJobArn"),
                created,
                "case {case:?}: expected the stop to move the job the name resolved to, \
                 got {described:?}"
            );
            // The two statuses this operation's doc names for a job it stopped: the
            // transient one it takes, and the terminal one the clock owes it.
            let reported = str_field(&described, "TrainingJobStatus");
            assert!(
                reported == "Stopping" || reported == "Stopped",
                "case {case:?}: expected the stopped job to report Stopping or Stopped, \
                 got {reported:?} in {described:?}"
            );
            continue;
        };

        create_job(&emulator, case, bystander).await;
        let (status, answer) = stop(&emulator, &name).await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a name outside the modelled constraints to be refused \
             with 400 ValidationException, got {status} with {answer:?}"
        );
        assert_eq!(
            message(&answer),
            violations(&clauses),
            "case {case:?}: expected the refusal to name the member and every requirement \
             it failed, got {answer:?}"
        );
        // A refused name resolved to no job, so the one job that does stand is untouched by
        // it: the listing reports it under the status it was created in.
        assert!(
            summary(&emulator, bystander, "InProgress").await.is_some(),
            "case {case:?}: expected the refused stop to leave {bystander} running"
        );
    }
}

// A name the constraints admit but no job stands under resolves to no stop target, and is
// refused with the training-job family's not-found sentence: `crate::training_job_stop`
// pins that rule over the ways a name can MISS a stored one, so it is not repeated here.

/// RULE: a stop writes to the stored job itself, so every reader of that job reports the
/// stop: `ListTrainingJobs` selects it under the status the job is in and projects the same
/// ARN, instants and statuses `DescribeTrainingJob` reports — first the status the stop
/// takes, then the terminal one the clock owes — and it selects no other stored job under
/// either of them.
#[tokio::test]
async fn a_stop_is_reported_by_every_reader_of_the_job() {
    // Rows differ along the shape of the stopped job's name and along which of three stored
    // jobs the caller stops, so the reader has to select the one job the stop named.
    let rows: Vec<(&str, &str, usize)> = vec![
        ("the first of three stored jobs", "stop-reader-a", 0),
        ("the middle of three stored jobs", "s", 1),
        ("the last of three stored jobs", "stop-reader-c7", 2),
    ];

    for (case, stopped_name, position) in rows {
        let emulator = Emulator::new();
        let mut names = vec!["stop-other-1".to_string(), "stop-other-2".to_string()];
        names.insert(position, stopped_name.to_string());
        for name in &names {
            create_job(&emulator, case, name).await;
        }

        let (status, answer) = stop(&emulator, stopped_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the stop to answer 200, got {status} with {answer:?}"
        );

        // The status the stop took, read through BOTH readers at the same stage of the job.
        // The pair is taken in a loop, so a transition the clock owes between the two reads
        // cannot decide the assertion: what is under test is that the two readers agree,
        // never how fast the host got from one to the other.
        let (described, listed) = read_both(&emulator, case, stopped_name).await;
        assert_agrees(case, &listed, &described);
        assert!(
            summary(&emulator, stopped_name, "InProgress")
                .await
                .is_none(),
            "case {case:?}: expected the stopped job not to be selected under the status \
             it has left"
        );

        // The terminal status the clock owes, read through both readers again.
        let settled = poll_until_status(&emulator, case, stopped_name, "Stopped").await;
        let listed = summary(&emulator, stopped_name, "Stopped")
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the listing filtered on Stopped to select \
                     {stopped_name}"
                )
            });
        assert_agrees(case, &listed, &settled);
        assert!(
            listed.get("TrainingEndTime").is_some(),
            "case {case:?}: expected the summary of a job whose run is over to carry its \
             TrainingEndTime, got {listed:?}"
        );

        // The stop moved the one job it named: no other stored job is selected under either
        // status, and each one still reports the status it had.
        for other in names.iter().filter(|name| *name != stopped_name) {
            for status_equals in ["Stopping", "Stopped"] {
                assert!(
                    summary(&emulator, other, status_equals).await.is_none(),
                    "case {case:?}: expected {other} not to be selected under \
                     {status_equals} after stopping {stopped_name}"
                );
            }
            let (status, described) = describe(&emulator, other).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the read of {other} to answer 200, got {status} \
                 with {described:?}"
            );
            let reported = str_field(&described, "TrainingJobStatus");
            assert!(
                reported != "Stopping" && reported != "Stopped",
                "case {case:?}: expected {other} never to be stopped by a stop of \
                 {stopped_name}, got {reported:?}"
            );
        }
    }
}

/// Every member `TrainingJobSummary` shares with `DescribeTrainingJob`'s output reports the
/// same value in both, so the two readers report one job rather than two copies of it.
fn assert_agrees(case: &str, listed: &Value, described: &Value) {
    // `TrainingJobSummary`'s members (smithy_query shape=TrainingJobSummary), less the ones
    // a job without a warm pool or a training plan does not carry.
    for member in [
        "TrainingJobName",
        "TrainingJobArn",
        "CreationTime",
        "LastModifiedTime",
        "TrainingEndTime",
        "TrainingJobStatus",
        "SecondaryStatus",
    ] {
        assert_eq!(
            listed.get(member),
            described.get(member),
            "case {case:?}: expected the listing and the read to report the same {member}, \
             got {listed:?} against {described:?}"
        );
    }
    // "Set only if the training job has one of the terminal statuses"
    // (`TrainingJobSummary$TrainingEndTime`), so each reader carries it exactly when the
    // job's run is over — which the equality above then holds both readers to.
    let ended = str_field(described, "TrainingJobStatus") == "Stopped";
    assert_eq!(
        listed.get("TrainingEndTime").is_some(),
        ended,
        "case {case:?}: expected a TrainingEndTime exactly where the job has ended, \
         got {listed:?} against {described:?}"
    );
}
