// SPDX-License-Identifier: CC0-1.0

//! A suite run's EXECUTION window and the per-case result it reports, over the wire.
//!
//! The rule under test: a suite run executes the test cases its definition's root group
//! names, each for the window that document configures for it (`EXECUTION_TIMEOUT`), so the
//! run is `RUNNING` for as long as its suite asks to run — which is the only window in
//! which a device under test could satisfy those cases — and `GetSuiteRun` reports a group
//! and test-case result for them throughout. No device connects to a run's endpoint here,
//! so every case times out unsatisfied and the run ends in the modelled `FAIL` with a
//! per-case `FAIL`; a run whose suite names no test case to execute at all could not be
//! executed and ends `ERROR` with no result to report.
//!
//! The tables span what the rule generalises over — the window's size, where in the
//! document it is configured, how many cases a run holds, and how many groups they are
//! spread over — rather than the one configuration the issue cited. Every case drives real
//! HTTP requests through the same `MultiServiceRouter` the emulator serves on a socket (the
//! harness its parent suite owns), and every wait is a poll for the CONDITION under a
//! generous deadline: nothing here asserts on the wall clock.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_iotdeviceadvisor_iotsenateservice as iotdeviceadvisor;

use super::{run_configuration, thing_arn, Answer, Emulator, ROLE_ARN};

/// The region every case below signs for.
const REGION: &str = "us-east-1";

/// How long a poll loop keeps looking before it gives up. Generous on purpose: what is
/// under test is the status a run reaches, never how fast the host reaches it.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A root group document holding `groups` groups of `cases` test cases each, with
/// `seconds` as every case's configured `EXECUTION_TIMEOUT`, or with none configured at all
/// when `seconds` is `None`.
///
/// The document shape is Device Advisor's own for a test suite definition: a tree of
/// `tests`, where a group holds further `tests` and a test case names the catalog test it
/// runs under `test.id` and takes its window from the nearest `configuration`.
fn root_group(groups: usize, cases: usize, seconds: Option<f64>) -> String {
    let configuration = match seconds {
        Some(seconds) => json!({ "EXECUTION_TIMEOUT": seconds }),
        None => json!({}),
    };
    let group_list: Vec<Value> = (0..groups)
        .map(|group| {
            let case_list: Vec<Value> = (0..cases)
                .map(|case| {
                    json!({
                        "name": format!("case-{group}-{case}"),
                        "configuration": configuration,
                        "test": { "id": "MQTT_Connect", "version": "0.0.0" },
                    })
                })
                .collect();
            json!({
                "name": format!("group-{group}"),
                "configuration": {},
                "tests": case_list,
            })
        })
        .collect();
    json!({ "configuration": {}, "tests": group_list }).to_string()
}

/// Create a definition whose root group is `root_group` and hand back its id.
async fn definition(emulator: &Emulator, name: &str, root_group: &str) -> String {
    let created = emulator
        .create_definition(
            &json!({
                "suiteDefinitionName": name,
                "rootGroup": root_group,
                "devicePermissionRoleArn": ROLE_ARN,
                "devices": [{ "thingArn": thing_arn(0) }],
            }),
            REGION,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: creating the definition {name:?} failed with {} {}",
        created.status, created.body
    );
    created.string("suiteDefinitionId")
}

/// Start one run of `definition_id` and hand back the path its reads are addressed at.
async fn start_run(emulator: &Emulator, case: &str, definition_id: &str) -> String {
    let started = emulator
        .call(
            "POST",
            &format!("/suiteDefinitions/{definition_id}/suiteRuns"),
            REGION,
            Some(json!({
                "suiteRunConfiguration": run_configuration(&thing_arn(1), None, None),
            })),
        )
        .await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: starting a run must succeed, got {} {}",
        started.status, started.body
    );
    let run_id = started.string("suiteRunId");
    format!("/suiteDefinitions/{definition_id}/suiteRuns/{run_id}")
}

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
            "case {case:?}: the run never {what}; it is {} with {}",
            answer.string("status"),
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every test-case entry of one answer's `testResult`, as `(group name, case name, status)`.
fn reported_cases(answer: &Answer) -> Vec<(String, String, String)> {
    answer
        .member("testResult")
        .get("groups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|group| {
            let group_name = group["groupName"].as_str().unwrap_or_default().to_string();
            group["tests"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |test| {
                    (
                        group_name.clone(),
                        test["testCaseDefinitionName"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        test["status"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `suiteRunsList` entry a listing of `definition_id`'s runs carries for `run_path`'s
/// run.
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
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|entry| entry["suiteRunId"].as_str() == Some(run_id))
        .unwrap_or_else(|| panic!("the listing carries run {run_id}, got {}", listing.body))
        .clone()
}

/// The whole rule, over a table of root groups: the run is observably `RUNNING` while its
/// cases execute, reports every case throughout, and ends `FAIL` with each case `FAIL` and
/// the listing's failed count once the window has elapsed.
#[tokio::test]
async fn a_run_executes_its_test_cases_and_fails_them_on_their_window() {
    // (case, groups, cases per group, the EXECUTION_TIMEOUT each case configures). The
    // windows are small so the suite is quick; what varies is the shape of the document and
    // the number of cases, which is what the rule generalises over.
    let rows: Vec<(&str, usize, usize, Option<f64>)> = vec![
        ("one group, one case of 2 s", 1, 1, Some(2.0)),
        ("one group, three cases of 0.5 s", 1, 3, Some(0.5)),
        ("two groups of two cases of 0.5 s", 2, 2, Some(0.5)),
        ("a fractional window", 1, 1, Some(1.5)),
        // No `EXECUTION_TIMEOUT` anywhere in the document: the case takes the span the
        // service gives an unconfigured one.
        ("no window configured", 1, 1, None),
    ];

    for (index, (case, groups, cases, seconds)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let document = root_group(groups, cases, seconds);
        let definition_id =
            definition(&emulator, &format!("execution-suite-{index}"), &document).await;
        let run_path = start_run(&emulator, case, &definition_id).await;
        let run_id = run_path
            .rsplit('/')
            .next()
            .expect("the run path ends in the run id")
            .to_string();
        let total_cases = groups * cases;

        // The run is RUNNING for its cases' window, so a poll observes that status rather
        // than finding the run already finished.
        let running = poll_until(&emulator, case, &run_path, "left PENDING", |answer| {
            answer.string("status") != "PENDING"
        })
        .await;
        assert_eq!(
            running.string("status"),
            "RUNNING",
            "case {case:?}: a run executing its test cases is RUNNING, got {}",
            running.body
        );
        assert!(
            running.member("endTime").is_null(),
            "case {case:?}: a run still executing has no endTime, got {}",
            running.body
        );
        let while_running = reported_cases(&running);
        assert_eq!(
            while_running.len(),
            total_cases,
            "case {case:?}: every test case of the suite is reported, got {while_running:?}"
        );
        assert!(
            while_running
                .iter()
                .all(|(_, _, status)| matches!(status.as_str(), "PENDING" | "RUNNING" | "FAIL")),
            "case {case:?}: a case is scheduled, executing or finished, got {while_running:?}"
        );

        let finished = poll_until(
            &emulator,
            case,
            &run_path,
            "reached a terminal status",
            |answer| !answer.member("endTime").is_null(),
        )
        .await;
        assert_eq!(
            finished.string("status"),
            "FAIL",
            "case {case:?}: a run whose test cases were never satisfied FAILS, got {}",
            finished.body
        );
        assert!(
            finished.member("errorReason").is_null(),
            "case {case:?}: a run that executed its cases carries no errorReason, got {}",
            finished.body
        );
        let reported = reported_cases(&finished);
        let expected: Vec<(String, String, String)> = (0..groups)
            .flat_map(|group| {
                (0..cases).map(move |which| {
                    (
                        format!("group-{group}"),
                        format!("case-{group}-{which}"),
                        "FAIL".to_string(),
                    )
                })
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: every case of every group must report FAIL under its own name, \
             got {reported:?}"
        );
        // The ids of a reported result are STABLE: a second read reports the same ones.
        let again = emulator.call("GET", &run_path, REGION, None).await;
        assert_eq!(
            again.member("testResult"),
            finished.member("testResult"),
            "case {case:?}: two reads of a finished run must report one result, got {} and {}",
            finished.body,
            again.body
        );

        // The listing projects the same outcome as counts.
        let entry = listed_entry(&emulator, &definition_id, &run_id).await;
        assert_eq!(
            (entry["passed"].as_i64(), entry["failed"].as_i64()),
            (Some(0), Some(total_cases as i64)),
            "case {case:?}: the listing must count the cases that failed, got {entry}"
        );
    }
}

/// A run whose suite names no test case could not be executed: it ends `ERROR` with the
/// service's reason and reports no result, whatever the root group holds instead.
#[tokio::test]
async fn a_run_with_no_test_case_to_execute_errors_and_reports_no_result() {
    let rows: [(&str, &str); 3] = [
        ("a root group that is not a document", "g"),
        ("a document with an empty test list", r#"{"tests":[]}"#),
        (
            "a document whose only entry names no test",
            r#"{"tests":[{"name":"nothing","configuration":{}}]}"#,
        ),
    ];

    for (index, (case, document)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let definition_id =
            definition(&emulator, &format!("no-case-suite-{index}"), document).await;
        let run_path = start_run(&emulator, case, &definition_id).await;
        let run_id = run_path
            .rsplit('/')
            .next()
            .expect("the run path ends in the run id")
            .to_string();

        let finished = poll_until(
            &emulator,
            case,
            &run_path,
            "reached a terminal status",
            |answer| !answer.member("endTime").is_null(),
        )
        .await;
        assert_eq!(
            finished.string("status"),
            "ERROR",
            "case {case:?}: a run with nothing to execute ends in ERROR, got {}",
            finished.body
        );
        assert_eq!(
            finished.member("errorReason").as_str(),
            Some(iotdeviceadvisor::state::NO_TEST_RUNNER_REASON),
            "case {case:?}: the error must say why nothing ran, got {}",
            finished.body
        );
        assert!(
            finished.member("testResult").is_null(),
            "case {case:?}: there is no test case to report on, got {}",
            finished.body
        );
        let entry = listed_entry(&emulator, &definition_id, &run_id).await;
        assert_eq!(
            (entry["passed"].as_i64(), entry["failed"].as_i64()),
            (None, None),
            "case {case:?}: no count is claimed for tests nothing ran, got {entry}"
        );
    }
}

/// A caller can act on a LIVE run: the run is still executing when the stop reaches it, so
/// the stop is accepted and the case it interrupted reports the run's own status.
#[tokio::test]
async fn a_run_stopped_while_it_executes_reports_its_interrupted_case() {
    let emulator = Emulator::new();
    // A window far longer than this test takes, so the run is still executing whenever the
    // stop reaches it however slowly the host runs.
    let document = root_group(1, 1, Some(600.0));
    let definition_id = definition(&emulator, "stoppable-suite", &document).await;
    let case = "stopped while executing";
    let run_path = start_run(&emulator, case, &definition_id).await;

    let running = poll_until(&emulator, case, &run_path, "left PENDING", |answer| {
        answer.string("status") != "PENDING"
    })
    .await;
    assert_eq!(
        running.string("status"),
        "RUNNING",
        "case {case:?}: the run must be executing before the stop, got {}",
        running.body
    );
    let executing = reported_cases(&running);
    assert_eq!(
        executing,
        vec![(
            "group-0".to_string(),
            "case-0-0".to_string(),
            "RUNNING".to_string()
        )],
        "case {case:?}: the case in its window reports the run's own status, got {executing:?}"
    );

    let stop = emulator
        .call("POST", &format!("{run_path}/stop"), REGION, None)
        .await;
    assert_eq!(
        stop.status, 200,
        "case {case:?}: stopping a live run must succeed, got {} {}",
        stop.status, stop.body
    );

    let stopped = poll_until(
        &emulator,
        case,
        &run_path,
        "reached a terminal status",
        |answer| !answer.member("endTime").is_null(),
    )
    .await;
    assert_eq!(
        stopped.string("status"),
        "STOPPED",
        "case {case:?}: a stopped run reaches STOPPED, got {}",
        stopped.body
    );
    let interrupted = reported_cases(&stopped);
    assert_eq!(
        interrupted,
        vec![(
            "group-0".to_string(),
            "case-0-0".to_string(),
            "STOPPED".to_string()
        )],
        "case {case:?}: the interrupted case reports the stop rather than an outcome it \
         never reached, got {interrupted:?}"
    );
}

/// A stop acts on a run that is IN PROGRESS: over EVERY status the model lets a run carry,
/// a run that has not finished is stopped and a run that has nothing left to stop is
/// refused and left exactly as it was.
///
/// The non-terminal and the successful statuses are reached by SEEDING the store (the
/// fixture this crate already owns), which is not a claim that this emulator produces them:
/// what is asserted is the service's rule about a stored run, over every status at once.
#[tokio::test]
async fn a_stop_is_accepted_only_while_the_run_has_not_finished() {
    let emulator = Emulator::new();
    let definition_id = definition(
        &emulator,
        "stop-rule-suite",
        &root_group(1, 1, Some(600.0)),
    )
    .await;
    // Source for the rows: the `SuiteRunStatus` enum, re-exported from the model, so a
    // status added to the model is covered here too.
    let statuses = iotdeviceadvisor::ops::types::SUITE_RUN_STATUS_VALUES;
    assert!(
        statuses.len() >= 9,
        "the model's SuiteRunStatus list must be the source of these rows, got {statuses:?}"
    );

    for status in statuses {
        let case = format!("a run stored as {status}");
        let run_id = crate::suite_run_report::seed_run(&emulator, &definition_id, status);
        let run_path = format!("/suiteDefinitions/{definition_id}/suiteRuns/{run_id}");
        let before = emulator.call("GET", &run_path, REGION, None).await;

        let stop = emulator
            .call("POST", &format!("{run_path}/stop"), REGION, None)
            .await;
        if iotdeviceadvisor::state::is_terminal(status) {
            assert_eq!(
                (stop.status, stop.error_code.as_deref()),
                (400, Some("ValidationException")),
                "case {case:?}: a run that has finished has nothing left to stop, got {} {}",
                stop.status,
                stop.body
            );
            let expected = iotdeviceadvisor::state::not_running_message(&run_id);
            assert_eq!(
                stop.member("message").as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the message {expected:?}, got {}",
                stop.body
            );
            let after = emulator.call("GET", &run_path, REGION, None).await;
            assert_eq!(
                after.body, before.body,
                "case {case:?}: a refused stop must leave the run exactly as it was, got {}",
                after.body
            );
        } else {
            assert_eq!(
                stop.status, 200,
                "case {case:?}: a run that has not finished is stopped, got {} {}",
                stop.status, stop.body
            );
            let after = emulator.call("GET", &run_path, REGION, None).await;
            assert!(
                matches!(after.string("status").as_str(), "STOPPING" | "STOPPED"),
                "case {case:?}: a stopped run is stopping or stopped, got {}",
                after.body
            );
        }
    }
}

/// Racing starts for one suite: exactly one wins, and every loser is refused the modelled
/// conflict with the service's fixed message — no part of it taken from the request or
/// from the run that holds the suite.
#[tokio::test]
async fn only_one_of_several_racing_starts_wins_and_the_losers_share_one_message() {
    let emulator = Emulator::new();
    let document = root_group(1, 1, Some(600.0));
    let definition_id = definition(&emulator, "racing-suite", &document).await;
    let runs_path = format!("/suiteDefinitions/{definition_id}/suiteRuns");
    let start = |index: usize| {
        let runs_path = runs_path.clone();
        let emulator = &emulator;
        async move {
            emulator
                .call(
                    "POST",
                    &runs_path,
                    REGION,
                    Some(json!({
                        "suiteRunConfiguration": run_configuration(&thing_arn(index), None, None),
                    })),
                )
                .await
        }
    };

    let (first, second, third, fourth, fifth, sixth) = tokio::join!(
        start(1),
        start(2),
        start(3),
        start(4),
        start(5),
        start(6)
    );
    let answers = [first, second, third, fourth, fifth, sixth];

    let winners = answers.iter().filter(|answer| answer.status == 200).count();
    assert_eq!(
        winners,
        1,
        "exactly one racing start may win, got {:?}",
        answers
            .iter()
            .map(|answer| (answer.status, answer.body.clone()))
            .collect::<Vec<_>>()
    );
    for (index, answer) in answers.iter().enumerate().filter(|(_, a)| a.status != 200) {
        assert_eq!(
            (answer.status, answer.error_code.as_deref()),
            (400, Some("ConflictException")),
            "case \"loser {index}\": expected the modelled conflict, got {} {}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.member("message").as_str(),
            Some(iotdeviceadvisor::state::SUITE_ALREADY_RUNNING_MESSAGE),
            "case \"loser {index}\": every loser carries the same fixed message, got {}",
            answer.body
        );
    }
}
