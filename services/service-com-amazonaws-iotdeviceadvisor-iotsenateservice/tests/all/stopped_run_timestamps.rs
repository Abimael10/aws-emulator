// SPDX-License-Identifier: CC0-1.0

//! A suite run's start stamp belongs to having LEFT the queue, not to one way of leaving it.
//!
//! `GetSuiteRun$startTime` is "Date (in Unix epoch time) when the test suite run started"
//! and `SuiteRunInformation$startedAt` the same for the listing, so a run that is no longer
//! queued reports one and a run that reports an end instant reports a start instant too.
//! The table below spans the statuses a run can be in when a stop reaches it — still
//! queued, already executing, already finished — because that is what the rule generalises
//! over; both readers that project the pair are asserted for every row.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::suite_lifecycle::{run_configuration, thing_arn, Answer, Emulator, ROLE_ARN};

/// How long a poll loop keeps looking before it gives up — generous on purpose: what is
/// under test is the stamps a run reports, never how fast the host gets there.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

const REGION: &str = "eu-west-1";

/// Read the run at `run_path` until `satisfied` holds, and hand back that answer.
///
/// Waits for the CONDITION, never for a span: the loop looks again until the predicate
/// holds or [`DEADLINE`] passes, so a slow or loaded host only makes it look more times.
async fn poll_until(
    emulator: &Emulator,
    case: &str,
    run_path: &str,
    what: &str,
    satisfied: impl Fn(&Answer) -> bool,
) -> Answer {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = emulator.call("GET", run_path, REGION, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: reading the run must succeed, got {} {}",
            answer.status, answer.body
        );
        if satisfied(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run never reached {what}; it is {} with {}",
            answer.string("status"),
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One timestamp member, as a client reading epoch time off the wire sees it.
fn epoch(answer: &Answer, member: &str) -> Option<f64> {
    answer.member(member).as_f64()
}

/// The `suiteRunsList` entry for `run_id`, from a listing of its definition's runs.
async fn listed_entry(emulator: &Emulator, definition_id: &str, run_id: &str) -> Value {
    let listing = emulator
        .call(
            "GET",
            &format!("/suiteRuns?suiteDefinitionId={definition_id}"),
            REGION,
            None,
        )
        .await;
    listing
        .member("suiteRunsList")
        .as_array()
        .unwrap_or_else(|| panic!("ListSuiteRuns answers a suiteRunsList, got {}", listing.body))
        .iter()
        .find(|entry| entry["suiteRunId"].as_str() == Some(run_id))
        .unwrap_or_else(|| panic!("the listing carries run {run_id}, got {}", listing.body))
        .clone()
}

/// Start one run on its own definition, whose root group is `root_group`, so no row
/// contends with another for the definition's single in-progress slot.
async fn start_run(
    emulator: &Emulator,
    case: &str,
    index: usize,
    root_group: &str,
) -> (String, String) {
    let definition = emulator
        .create_definition(
            &json!({
                "suiteDefinitionName": format!("stop-stamp-suite-{index}"),
                "rootGroup": root_group,
                "devicePermissionRoleArn": ROLE_ARN,
            }),
            REGION,
        )
        .await;
    assert_eq!(
        definition.status, 200,
        "case {case:?}: setting up a definition must succeed, got {} {}",
        definition.status, definition.body
    );
    let definition_id = definition.string("suiteDefinitionId");

    let started = emulator
        .call(
            "POST",
            &format!("/suiteDefinitions/{definition_id}/suiteRuns"),
            REGION,
            Some(json!({
                "suiteRunConfiguration": run_configuration(&thing_arn(index), None, None),
            })),
        )
        .await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: starting a run must succeed, got {} {}",
        started.status, started.body
    );
    let run_id = started.string("suiteRunId");
    (definition_id, run_id)
}

/// Whatever status a run is in when a stop reaches it, the run that comes out of the stop
/// reports a `startTime`, never later than its `endTime`, and the stamp it already carried
/// is not moved.
#[tokio::test]
async fn a_stopped_run_reports_the_start_stamp_it_left_the_queue_with() {
    let emulator = Emulator::new();

    // A root group naming one test case with a window far longer than this suite takes, so
    // the row that stops a run while it EXECUTES finds it executing however slowly the
    // host runs. The document shape is Device Advisor's own for a test suite definition.
    const EXECUTING: &str = r#"{"tests":[{"name":"g","tests":[{"name":"t",
        "configuration":{"EXECUTION_TIMEOUT":600},"test":{"id":"MQTT_Connect"}}]}]}"#;
    // A root group naming no test case, so the run reaches its terminal status without
    // waiting for an execution window.
    const NOTHING_TO_RUN: &str = "g";

    // (case, the root group the run's suite holds, the status to wait for before stopping,
    // whether the stop is accepted in that status, the status the run ends in).
    let rows: Vec<(&str, &str, &str, bool, &str)> = vec![
        (
            "stopped while still queued",
            NOTHING_TO_RUN,
            "PENDING",
            true,
            "STOPPED",
        ),
        (
            "stopped while executing",
            EXECUTING,
            "RUNNING",
            true,
            "STOPPED",
        ),
        (
            // Already terminal: there is nothing left to stop, so the call is refused and
            // the stamps the run reached stand.
            "stopped after it already finished",
            NOTHING_TO_RUN,
            "ERROR",
            false,
            "ERROR",
        ),
    ];

    for (index, (case, root_group, status_before_stop, stop_accepted, status_after_stop)) in
        rows.into_iter().enumerate()
    {
        let (definition_id, run_id) = start_run(&emulator, case, index, root_group).await;
        let run_path = format!("/suiteDefinitions/{definition_id}/suiteRuns/{run_id}");

        // Wait for the STATUS the row stops the run in, not for a span.
        let before = poll_until(&emulator, case, &run_path, status_before_stop, |answer| {
            answer.string("status") == status_before_stop
        })
        .await;
        let start_before_stop = epoch(&before, "startTime");

        let stop = emulator
            .call("POST", &format!("{run_path}/stop"), REGION, None)
            .await;
        if stop_accepted {
            assert_eq!(
                stop.status, 200,
                "case {case:?}: stopping a run that is in progress answers 200, got {} {}",
                stop.status, stop.body
            );
        } else {
            assert_eq!(
                (stop.status, stop.error_code.as_deref()),
                (400, Some("ValidationException")),
                "case {case:?}: a run that has finished has nothing left to stop, got {} {}",
                stop.status,
                stop.body
            );
        }

        // The status the row ends in is what the next read waits for.
        let after = poll_until(&emulator, case, &run_path, status_after_stop, |answer| {
            answer.string("status") == status_after_stop
        })
        .await;

        let start_time = epoch(&after, "startTime");
        let end_time = epoch(&after, "endTime");
        assert!(
            start_time.is_some(),
            "case {case:?}: a run that left PENDING carries a startTime, got {}",
            after.body
        );
        assert!(
            end_time.is_some(),
            "case {case:?}: a run in a terminal status carries an endTime, got {}",
            after.body
        );
        assert!(
            start_time <= end_time,
            "case {case:?}: expected startTime no later than endTime, got {start_time:?} \
             and {end_time:?}"
        );
        if let Some(already_stamped) = start_before_stop {
            assert_eq!(
                start_time,
                Some(already_stamped),
                "case {case:?}: a run that already carried a startTime must keep it, got \
                 {start_time:?}"
            );
        }

        // The listing projects the SAME pair under its own member names.
        let entry = listed_entry(&emulator, &definition_id, &run_id).await;
        assert_eq!(
            entry["startedAt"].as_f64(),
            start_time,
            "case {case:?}: ListSuiteRuns must report the startedAt GetSuiteRun reports, \
             got {entry}"
        );
        assert_eq!(
            entry["endAt"].as_f64(),
            end_time,
            "case {case:?}: ListSuiteRuns must report the endAt GetSuiteRun reports, got \
             {entry}"
        );
        let created_at = entry["createdAt"].as_f64();
        assert!(
            created_at.is_some() && created_at <= start_time,
            "case {case:?}: expected createdAt no later than startedAt, got {created_at:?} \
             and {start_time:?}"
        );

        // The run has finished, so there is nothing left for a second stop to do: it is
        // refused and neither stamp moves.
        let restop = emulator
            .call("POST", &format!("{run_path}/stop"), REGION, None)
            .await;
        assert_eq!(
            (restop.status, restop.error_code.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: a second StopSuiteRun on a finished run is refused, got {} {}",
            restop.status,
            restop.body
        );
        let again = emulator.call("GET", &run_path, REGION, None).await;
        assert_eq!(
            (
                again.string("status"),
                epoch(&again, "startTime"),
                epoch(&again, "endTime"),
            ),
            (status_after_stop.to_string(), start_time, end_time),
            "case {case:?}: a second stop must leave the finished run exactly as it was, \
             got {}",
            again.body
        );
    }
}
