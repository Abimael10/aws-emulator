// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the status lifecycle of a workflow run, and for which optional
//! members `GetWorkflowRun` reports.
//!
//! Two rules are under test, both over the real wire path (`MultiServiceRouter::dispatch`
//! over an `awsJson1_0` request).
//!
//! 1. A run walks the modelled `WorkflowRunStatus` lifecycle on the CLOCK, one state at a
//!    time. `StartWorkflowRun` accepts the run into `STARTING` — "This is typically
//!    STARTING when you first create the run" (`smithy_query(query="operation",
//!    operation="StartWorkflowRun")`, `Status`) — the control plane's admission of the run
//!    makes `STARTING` -> `QUEUED` due `RUN_ADMITTED_AFTER` later, an executor picking it
//!    up makes `QUEUED` -> `RUNNING` due `RUN_DEQUEUED_AFTER` after that, and the
//!    execution of the tasks its definition declares carries it to `SUCCESS`, with
//!    `CompletedOn` and `Duration` set and no `ErrorMessage` — "This value is null if the
//!    run completed successfully". Every transition is owed by the clock and dated from the
//!    instant it fell DUE, so a read never changes what a later caller sees and a terminal
//!    status is never left. WHICH terminal status a run reaches is decided by the task
//!    graph of the definition it runs, which `run_execution_outcome.rs` covers.
//!
//! 2. An optional member is reported only when the stored run HOLDS a value for it: a run
//!    of a version whose definition declares no task omits `TaskInstances` rather than
//!    answering an empty list — the same rule `GetTaskInstance` applies to the `Xcom` of an
//!    instance that exchanged no cross-task data. The parameter set a run reports is the
//!    one it EXECUTES with: the entry the service injects to name the run's workflow, with
//!    whatever the start supplied merged over it.
//!
//! 3. The WINDOW a run reports — `StartedOn`, `CompletedOn` and the `Duration` derived
//!    from them — spans the work the definition declared: each dependency level occupies
//!    the longest wait its own tasks declare, so a run whose task declares a long wait is
//!    observable executing for it and a run whose task declares a brief one reports a
//!    brief window. The last two suites cover that rule.
//!
//! The first suite drives the schedule from SEEDED clock fields, so every state of the
//! lifecycle (including the intermediate `QUEUED`) is asserted deterministically rather
//! than raced against the wall clock; the second suite then walks one real run with the
//! real clock and checks the order of what a polling client observes.

use std::time::{Duration, SystemTime};

use service_com_amazonaws_mwaaserverless_amazonmwaaserverless::state::declared_runtime::MAX_DECLARED_RUNTIME;
use service_com_amazonaws_mwaaserverless_amazonmwaaserverless::state::{
    WorkflowRun, RUN_ADMITTED_AFTER, RUN_DEQUEUED_AFTER, RUN_STATUS_FAILED, RUN_STATUS_QUEUED,
    RUN_STATUS_RUNNING, RUN_STATUS_STARTING, RUN_STATUS_SUCCESS, TASK_LEVEL_RUNS_FOR,
};
use service_com_amazonaws_mwaaserverless_amazonmwaaserverless::ComAmazonawsMwaaserverlessAmazonmwaaserverlessState;

use crate::task_instance_reads::{epoch_seconds, instance, seed};
use crate::workflow_definition_snapshot::{
    call, create_bucket, create_workflow, definition_router, definition_router_on, stage,
};
use crate::workflow_lifecycle::{role_arn, Harness};

/// The `WorkflowRunStatus` values a run of a definition that declares no failing task
/// passes through, in the order the clock owes them
/// (`smithy_query(query="shape", shape="WorkflowRunStatus")` declares
/// `STARTING | QUEUED | RUNNING | SUCCESS | FAILED | TIMEOUT | STOPPING | STOPPED`).
const LIFECYCLE: &[&str] = &[
    RUN_STATUS_STARTING,
    RUN_STATUS_QUEUED,
    RUN_STATUS_RUNNING,
    RUN_STATUS_SUCCESS,
];

/// Create a workflow and start a run under it: `(WorkflowArn, RunId)`.
async fn started_run(harness: &Harness, name: &str) -> (String, String) {
    let (status, created) = harness
        .call(
            "CreateWorkflow",
            serde_json::json!({
                "Name": name,
                "DefinitionS3Location": {"Bucket": "defs", "ObjectKey": "dags/lifecycle.yaml"},
                "RoleArn": role_arn(),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateWorkflow to succeed, got {status} {created:?}"
    );
    let arn = created["WorkflowArn"]
        .as_str()
        .expect("setup: a created workflow has an ARN")
        .to_string();
    let (status, started) = harness
        .call("StartWorkflowRun", serde_json::json!({"WorkflowArn": arn}))
        .await;
    assert_eq!(
        status, 200,
        "setup: expected StartWorkflowRun to succeed, got {status} {started:?}"
    );
    assert_eq!(
        started["Status"], RUN_STATUS_STARTING,
        "setup: a just-started run carries the documented initial status, got {:?}",
        started["Status"]
    );
    let run_id = started["RunId"]
        .as_str()
        .expect("setup: a started run has a RunId")
        .to_string();
    (arn, run_id)
}

/// Edit the stored run in place. The clock fields are what the schedule is driven from, so
/// setting them is how a row puts a deadline in the past without sleeping for it.
///
/// `pub(crate)` so another suite that needs a run in a DETERMINISTIC state — one whose
/// next change is not due, so the status it reports cannot move mid-test — seeds it
/// through the same store rather than building a second seam onto the engine.
pub(crate) fn with_run<R>(
    harness: &Harness,
    workflow_arn: &str,
    run_id: &str,
    edit: impl FnOnce(&mut WorkflowRun) -> R,
) -> R {
    with_stored_run(harness.engine(), workflow_arn, run_id, edit)
}

/// The same edit against an engine the caller holds directly, for a suite that drives its
/// requests through the assembled router rather than through [`Harness`].
pub(crate) fn with_stored_run<R>(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    workflow_arn: &str,
    run_id: &str,
    edit: impl FnOnce(&mut WorkflowRun) -> R,
) -> R {
    let state = engine.state::<ComAmazonawsMwaaserverlessAmazonmwaaserverlessState>();
    let mut store = state.lock();
    let run = store
        .workflows
        .get_mut(workflow_arn)
        .expect("setup: the created workflow is stored")
        .runs
        .get_mut(run_id)
        .expect("setup: the started run is stored");
    edit(run)
}

/// Assert a timestamp member reports `expected`, or is absent when it is `None`.
fn assert_timestamp(
    detail: &serde_json::Value,
    member: &str,
    expected: Option<SystemTime>,
    case: &str,
) {
    match expected {
        Some(instant) => {
            let want = instant
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("an instant after the epoch")
                .as_secs_f64();
            let got = epoch_seconds(&detail[member]);
            assert!(
                got.is_some_and(|seconds| (seconds - want).abs() < 0.002),
                "case {case:?}: expected {member} at {want} epoch seconds, got {:?}",
                detail[member]
            );
        }
        None => assert!(
            detail[member].is_null(),
            "case {case:?}: expected {member} absent, got {:?}",
            detail[member]
        ),
    }
}

/// One row of the schedule table: the state a run is in, how far back it started, when its
/// next change falls due, and what a read must then report.
struct Row {
    case: &'static str,
    /// The `WorkflowRunStatus` the stored run is in.
    status: &'static str,
    /// `StartedOn`, this many milliseconds before the row's reference instant.
    started_millis_ago: i64,
    /// `next_status_at`, this many milliseconds after the reference instant (negative puts
    /// the deadline in the past, so the change is already owed).
    due_in_millis: i64,
    /// The `RunState` the read must report.
    expected_state: &'static str,
    /// `Duration`, in whole seconds rounded up, for a row that reaches a terminal state.
    expected_duration: Option<i32>,
    /// How long after the reference instant `CompletedOn` falls, for a terminal row.
    completed_in_millis: Option<i64>,
    /// The `ErrorMessage` the read must report.
    expected_error: Option<&'static str>,
}

/// The rows. They differ along the state the run is in, whether the next change is due,
/// how far overdue it is (so that one change or EVERY remaining one is owed at once), and
/// the length of the span `Duration` is computed over (zero, sub-second, seconds, an
/// hour) — not merely along the run's identity.
///
/// The workflow these rows are seeded on snapshotted no definition, so its runs declare
/// no task: such a run has nothing to execute and so nothing that can fail, and the
/// execution it walks takes one `TASK_LEVEL_RUNS_FOR` span.
fn rows() -> Vec<Row> {
    let dequeued = i64::try_from(RUN_DEQUEUED_AFTER.as_millis()).expect("a span of a few hundred ms");
    let level = i64::try_from(TASK_LEVEL_RUNS_FOR.as_millis()).expect("a span of a few hundred ms");
    vec![
        Row {
            case: "starting/no change due yet",
            status: RUN_STATUS_STARTING,
            started_millis_ago: 0,
            due_in_millis: 10_000,
            expected_state: RUN_STATUS_STARTING,
            expected_duration: None,
            completed_in_millis: None,
            expected_error: None,
        },
        Row {
            // ONE change is owed: admission is done, so the run is waiting for an
            // executor. The intermediate state IS observable.
            case: "starting/admission due, nothing further",
            status: RUN_STATUS_STARTING,
            started_millis_ago: 1_000,
            due_in_millis: 0,
            expected_state: RUN_STATUS_QUEUED,
            expected_duration: None,
            completed_in_millis: None,
            expected_error: None,
        },
        Row {
            // EVERY change is owed, so one read applies them all and lands on the
            // terminal state, dated from when each fell due: the admission deadline,
            // then RUN_DEQUEUED_AFTER, then one task level.
            case: "starting/whole schedule overdue",
            status: RUN_STATUS_STARTING,
            started_millis_ago: 10_000,
            due_in_millis: -5_000,
            expected_state: RUN_STATUS_SUCCESS,
            expected_duration: Some(6),
            completed_in_millis: Some(-5_000 + dequeued + level),
            expected_error: None,
        },
        Row {
            case: "queued/the executor is not due to pick it up yet",
            status: RUN_STATUS_QUEUED,
            started_millis_ago: 0,
            due_in_millis: 10_000,
            expected_state: RUN_STATUS_QUEUED,
            expected_duration: None,
            completed_in_millis: None,
            expected_error: None,
        },
        Row {
            // The executor has picked the run up: it is executing, and an execution that
            // has not finished reports neither a completion nor a duration.
            case: "queued/execution due to start",
            status: RUN_STATUS_QUEUED,
            started_millis_ago: 1,
            due_in_millis: 0,
            expected_state: RUN_STATUS_RUNNING,
            expected_duration: None,
            completed_in_millis: None,
            expected_error: None,
        },
        Row {
            // A sub-second run still reports the second it occupied.
            case: "running/completion due, sub-second span",
            status: RUN_STATUS_RUNNING,
            started_millis_ago: 1,
            due_in_millis: 0,
            expected_state: RUN_STATUS_SUCCESS,
            expected_duration: Some(1),
            completed_in_millis: Some(0),
            expected_error: None,
        },
        Row {
            case: "running/completion due at the instant it started",
            status: RUN_STATUS_RUNNING,
            started_millis_ago: 0,
            due_in_millis: 0,
            expected_state: RUN_STATUS_SUCCESS,
            expected_duration: Some(0),
            completed_in_millis: Some(0),
            expected_error: None,
        },
        Row {
            case: "running/completion due, hour-long span",
            status: RUN_STATUS_RUNNING,
            started_millis_ago: 3_600_000,
            due_in_millis: 0,
            expected_state: RUN_STATUS_SUCCESS,
            expected_duration: Some(3_600),
            completed_in_millis: Some(0),
            expected_error: None,
        },
        Row {
            // A terminal state is never left, and never rewritten: the deadline is long
            // past, and the run keeps the completion it already had.
            case: "failed/terminal state is never left",
            status: RUN_STATUS_FAILED,
            started_millis_ago: 9_000,
            due_in_millis: -10_000,
            expected_state: RUN_STATUS_FAILED,
            expected_duration: Some(7),
            completed_in_millis: Some(-1_000),
            expected_error: Some("a failure this run already recorded"),
        },
        Row {
            // The other terminal state this lifecycle reaches, equally never left and
            // equally never given a reason it did not fail for.
            case: "success/terminal state is never left",
            status: RUN_STATUS_SUCCESS,
            started_millis_ago: 9_000,
            due_in_millis: -10_000,
            expected_state: RUN_STATUS_SUCCESS,
            expected_duration: Some(7),
            completed_in_millis: Some(-1_000),
            expected_error: None,
        },
    ]
}

/// The instant `offset_millis` after `reference`.
fn shifted(reference: SystemTime, offset_millis: i64) -> SystemTime {
    if offset_millis >= 0 {
        reference + Duration::from_millis(offset_millis.unsigned_abs())
    } else {
        reference - Duration::from_millis(offset_millis.unsigned_abs())
    }
}

#[tokio::test]
async fn a_run_reports_the_state_the_clock_owes_it_and_nothing_further() {
    let harness = Harness::new("us-east-1");
    let (arn, run_id) = started_run(&harness, "runSchedule").await;

    for row in rows() {
        // The reference instant every offset in the row is measured from. `settle` dates
        // what it writes from the DUE instants, never from the instant of the read, so
        // every expectation below is an exact value rather than a window.
        let reference = SystemTime::now();
        let started_on = shifted(reference, -row.started_millis_ago);
        with_run(&harness, &arn, &run_id, |run| {
            run.status = row.status.to_string();
            run.started_on = started_on;
            run.created_at = started_on;
            run.modified_at = started_on;
            run.next_status_at = shifted(reference, row.due_in_millis);
            // A terminal row keeps the completion it already recorded; every other row
            // starts from a run that has not completed.
            if row.status == RUN_STATUS_FAILED || row.status == RUN_STATUS_SUCCESS {
                run.completed_on = row.completed_in_millis.map(|at| shifted(reference, at));
                run.duration = row.expected_duration;
                run.error_message = row.expected_error.map(str::to_string);
            } else {
                run.completed_on = None;
                run.duration = None;
                run.error_message = None;
            }
        });

        let request = serde_json::json!({"WorkflowArn": arn, "RunId": run_id});
        let (status, answered) = harness.call("GetWorkflowRun", request.clone()).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetWorkflowRun to succeed, got {status} {answered:?}",
            row.case
        );
        let detail = &answered["RunDetail"];
        assert_eq!(
            detail["RunState"].as_str(),
            Some(row.expected_state),
            "case {:?}: expected RunState {:?}, got {:?}",
            row.case,
            row.expected_state,
            detail["RunState"]
        );
        assert_eq!(
            detail["Duration"].as_i64(),
            row.expected_duration.map(i64::from),
            "case {:?}: expected Duration {:?}, got {:?}",
            row.case,
            row.expected_duration,
            detail["Duration"]
        );
        assert_eq!(
            detail["ErrorMessage"].as_str(),
            row.expected_error,
            "case {:?}: expected ErrorMessage {:?}, got {:?}",
            row.case,
            row.expected_error,
            detail["ErrorMessage"]
        );
        assert_timestamp(
            detail,
            "CompletedOn",
            row.completed_in_millis.map(|at| shifted(reference, at)),
            row.case,
        );
        assert!(
            detail["TaskInstances"].is_null(),
            "case {:?}: a run whose definition declared no task holds no task instance, so \
             the member is absent, got {:?}",
            row.case,
            detail["TaskInstances"]
        );

        // A reporting operation changes nothing a later caller sees: the second read of
        // the same run answers identically.
        let (status, again) = harness.call("GetWorkflowRun", request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the repeated read to succeed, got {status} {again:?}",
            row.case
        );
        assert_eq!(
            again, answered,
            "case {:?}: a read must not change what a later caller sees, got {again:?} \
             after {answered:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_real_run_is_observed_walking_the_lifecycle_in_order() {
    let harness = Harness::new("eu-west-1");
    let (arn, run_id) = started_run(&harness, "runProgression").await;
    let request = serde_json::json!({"WorkflowArn": arn, "RunId": run_id});

    // Poll the way a client waiting for completion does, and record each state as it
    // CHANGES. Nothing but the clock drives the run, so the poll neither hurries it nor
    // holds it back.
    let mut observed: Vec<String> = Vec::new();
    let mut last = serde_json::Value::Null;
    let deadline = SystemTime::now() + Duration::from_secs(10);
    while SystemTime::now() < deadline {
        let (status, answered) = harness.call("GetWorkflowRun", request.clone()).await;
        assert_eq!(
            status, 200,
            "case poll: expected GetWorkflowRun to succeed, got {status} {answered:?}"
        );
        let state = answered["RunDetail"]["RunState"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if observed.last() != Some(&state) {
            observed.push(state.clone());
        }
        last = answered;
        if state == RUN_STATUS_SUCCESS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        observed.first().map(String::as_str),
        Some(RUN_STATUS_STARTING),
        "case poll: the first state a client sees is the one the start reported, got \
         {observed:?}"
    );
    assert_eq!(
        observed.last().map(String::as_str),
        Some(RUN_STATUS_SUCCESS),
        "case poll: a run of a definition with no failing task must complete successfully \
         within {:?}, got {observed:?}",
        RUN_ADMITTED_AFTER + RUN_DEQUEUED_AFTER + TASK_LEVEL_RUNS_FOR
    );
    let positions: Vec<Option<usize>> = observed
        .iter()
        .map(|state| LIFECYCLE.iter().position(|known| known == state))
        .collect();
    assert!(
        positions.iter().all(Option::is_some),
        "case poll: every observed state belongs to the lifecycle {LIFECYCLE:?}, got \
         {observed:?}"
    );
    let positions: Vec<usize> = positions.into_iter().flatten().collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "case poll: the states must be observed in lifecycle order and never revisited, \
         got {observed:?}"
    );

    let detail = &last["RunDetail"];
    assert!(
        detail["ErrorMessage"].is_null(),
        "case poll: a run that completed successfully reports no ErrorMessage, got {:?}",
        detail["ErrorMessage"]
    );
    assert!(
        epoch_seconds(&detail["CompletedOn"]).is_some(),
        "case poll: a complete run carries CompletedOn, got {:?}",
        detail["CompletedOn"]
    );
    assert!(
        detail["Duration"].as_i64().is_some_and(|seconds| seconds >= 0),
        "case poll: a complete run carries a Duration, got {:?}",
        detail["Duration"]
    );
    assert!(
        detail["TaskInstances"].is_null(),
        "case poll: this workflow's version snapshotted no definition, so its run declares \
         no task and holds no task instance, got {:?}",
        detail["TaskInstances"]
    );

    // The terminal state is stable: a later read answers exactly the same.
    let (status, again) = harness.call("GetWorkflowRun", request).await;
    assert_eq!(
        status, 200,
        "case settled re-read: expected GetWorkflowRun to succeed, got {status} {again:?}"
    );
    assert_eq!(
        again["RunDetail"], last["RunDetail"],
        "case settled re-read: a settled run must not move again, got {:?}",
        again["RunDetail"]
    );
}

/// The `OverrideParameters` maps the omission table covers: a `map<string, document>`, so
/// the rows differ along the number of entries and the JSON type and nesting of the
/// values, with the two forms of "no overrides at all" (absent, and an empty map) first.
fn override_parameter_rows() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("absent", serde_json::Value::Null),
        ("an empty map", serde_json::json!({})),
        ("one string entry", serde_json::json!({"env": "test"})),
        (
            "a scalar of every JSON type",
            serde_json::json!({"retries": 3, "ratio": 0.5, "dry_run": true, "note": "mixed"}),
        ),
        (
            "a nested object and a list",
            serde_json::json!({
                "config": {"region": "us-east-1", "tags": ["a", "b"]},
                "partitions": [1, 2, 3],
            }),
        ),
    ]
}

#[tokio::test]
async fn an_optional_run_member_is_reported_only_when_the_run_holds_it() {
    let harness = Harness::new("ap-south-1");
    let (status, created) = harness
        .call(
            "CreateWorkflow",
            serde_json::json!({
                "Name": "optionalRunMembers",
                "DefinitionS3Location": {"Bucket": "defs", "ObjectKey": "dags/optional.yaml"},
                "RoleArn": role_arn(),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateWorkflow to succeed, got {status} {created:?}"
    );
    let arn = created["WorkflowArn"]
        .as_str()
        .expect("setup: a created workflow has an ARN")
        .to_string();

    // The task-instance ids each row seeds. This workflow's version snapshotted no
    // definition, so its runs declare no task and produce no instance of their own: the
    // non-empty rows seed the store the reader reads — the same records
    // `GetTaskInstance` serves.
    let seeded_tasks: &[&[&str]] = &[
        &[],
        &["extract.task-01"],
        &["c.third-task", "a.first-task", "b.second-task"],
    ];

    for (parameter_case, parameters) in override_parameter_rows() {
        for ids in seeded_tasks {
            let case = format!("{parameter_case} + {} task instance(s)", ids.len());
            let mut start = serde_json::json!({"WorkflowArn": arn});
            if !parameters.is_null() {
                start["OverrideParameters"] = parameters.clone();
            }
            let (status, started) = harness.call("StartWorkflowRun", start).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected StartWorkflowRun to succeed, got {status} {started:?}"
            );
            let run_id = started["RunId"]
                .as_str()
                .expect("a started run has a RunId")
                .to_string();
            for id in *ids {
                seed(&harness, &arn, &run_id, instance(id, "SUCCESS"));
            }

            let (status, answered) = harness
                .call(
                    "GetWorkflowRun",
                    serde_json::json!({"WorkflowArn": arn, "RunId": run_id}),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetWorkflowRun to succeed, got {status} {answered:?}"
            );

            // The parameter set a run reports is the one it executes with: the entry the
            // service injects to name the run's workflow, with whatever the start
            // supplied merged over it.
            assert_eq!(
                answered["OverrideParameters"],
                crate::workflow_lifecycle::expected_run_parameters(&arn, &parameters),
                "case {case:?}: expected the injected workflow-id parameter with the \
                 supplied overrides over it, got {:?}",
                answered["OverrideParameters"]
            );

            let listed = &answered["RunDetail"]["TaskInstances"];
            if ids.is_empty() {
                assert!(
                    listed.is_null(),
                    "case {case:?}: a run holding no task instance omits TaskInstances \
                     rather than answering an empty list, got {listed:?}"
                );
            } else {
                // The store keys them in one total order, so the reported ids are sorted.
                let mut expected: Vec<&str> = ids.to_vec();
                expected.sort_unstable();
                let reported: Vec<&str> = listed
                    .as_array()
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(
                    reported, expected,
                    "case {case:?}: TaskInstances must name the instances the run holds, \
                     got {listed:?}"
                );
            }
        }
    }
}

// === The window a run REPORTS is the window the work its definition declared occupies ===
//
// `StartedOn` is "The timestamp when the workflow run started execution", `CompletedOn`
// "The timestamp when the workflow run completed execution" and `Duration` "The total
// duration of the workflow run execution in seconds"
// (`smithy_query(query="shape", shape="WorkflowRunDetail")`), so the window a run reports
// describes the execution it performed: each dependency level of its plan occupies the
// longest time its own tasks' declared command lines STATE they take — performed by the
// workspace's one bounded POSIX/bash session, which states a program's time without
// waiting for it — and the run is non-terminal for the levels one after another. A task
// whose program states no time occupies the emulator's own default span
// (`TASK_LEVEL_RUNS_FOR`).

/// One row of the declared-window table: a definition, and the spans its dependency
/// levels must therefore occupy, deepest level last.
struct WindowRow {
    case: &'static str,
    /// The definition content staged in Amazon S3.
    definition: &'static str,
    /// The span each level of the run occupies, in the order the run executes them.
    levels: Vec<Duration>,
}

/// The table. The rows differ along what the rule generalises over: whether a task
/// declares a command at all, the UNIT the time it states is written in (a bare number of
/// seconds and each suffix the waiting command accepts — `s`, `m`, `h`, `d`), whether that
/// number is fractional, the shell FORM the line is written in (one command, an and-or list
/// with and without spaces around the operator, a command list, a quoted operand, an
/// assignment before the command, a nested `sh -c` program, a sequence of words), the SHAPE
/// of the graph the waits sit in (one task, two at one level, a chain of two levels), the
/// FORM the definition itself is written in (a mapping of tasks under the DAG id, a
/// sequence at the root), a program written over several LINES — and the edges: a program
/// that states no time at all, one the shell refuses, and a time past the longest this
/// emulator holds a run for.
fn window_rows() -> Vec<WindowRow> {
    vec![
        WindowRow {
            case: "a task declaring no command at all",
            definition: "tasks:\n  - id: only\n    operator: EmptyOperator\n",
            levels: vec![TASK_LEVEL_RUNS_FOR],
        },
        WindowRow {
            // The Apache Airflow YAML shape: the DAG's own id, its tasks one mapping in.
            case: "one task waiting half a minute, declared as the DAG's own mapping",
            definition: "lifecycle_dag:\n  start_date: \"2024-01-01\"\n  schedule: null\n  \
                         tasks:\n    only_task:\n      operator: \
                         airflow.providers.standard.operators.bash.BashOperator\n      \
                         bash_command: \"sleep 25\"\n",
            levels: vec![Duration::from_secs(25)],
        },
        WindowRow {
            case: "a wait shorter than a second",
            definition: "tasks:\n  - id: brief\n    bash_command: \"sleep 0.4\"\n",
            levels: vec![Duration::from_millis(400)],
        },
        WindowRow {
            case: "a wait in seconds, written with the explicit suffix",
            definition: "tasks:\n  - id: explicit\n    bash_command: \"sleep 30s\"\n",
            levels: vec![Duration::from_secs(30)],
        },
        WindowRow {
            case: "a wait in minutes",
            definition: "tasks:\n  - id: minutes\n    bash_command: \"sleep 2m\"\n",
            levels: vec![Duration::from_secs(120)],
        },
        WindowRow {
            case: "a wait in hours",
            definition: "tasks:\n  - id: hours\n    bash_command: \"sleep 1h\"\n",
            levels: vec![Duration::from_secs(60 * 60)],
        },
        WindowRow {
            case: "a fractional wait in days",
            definition: "tasks:\n  - id: days\n    bash_command: \"sleep 0.5d\"\n",
            levels: vec![Duration::from_secs(12 * 60 * 60)],
        },
        WindowRow {
            // The shell's own and-or list: both commands run, so the program states the
            // time of both.
            case: "two waits on one command line",
            definition: "tasks:\n  - id: chained\n    bash_command: \"sleep 1 && sleep 2\"\n",
            levels: vec![Duration::from_secs(3)],
        },
        WindowRow {
            // The same list with no space around the operator, which is how one is
            // ordinarily written: the grammar tokenizes the operator, so the words are the
            // shell's rather than whitespace-separated text.
            case: "an and-or list written without spaces around the operator",
            definition: "tasks:\n  - id: unspaced\n    bash_command: \"sleep 1&&sleep 2\"\n",
            levels: vec![Duration::from_secs(3)],
        },
        WindowRow {
            case: "a command list, the wait first",
            definition: "tasks:\n  - id: listed\n    bash_command: \"sleep 30; echo done\"\n",
            levels: vec![Duration::from_secs(30)],
        },
        WindowRow {
            case: "a quoted operand",
            definition: "tasks:\n  - id: quoted\n    bash_command: 'sleep \"5\"'\n",
            levels: vec![Duration::from_secs(5)],
        },
        WindowRow {
            case: "an assignment written before the waiting command",
            definition: "tasks:\n  assigned:\n    bash_command: TZ=UTC sleep 8\n",
            levels: vec![Duration::from_secs(8)],
        },
        WindowRow {
            case: "the wait carried by a nested program",
            definition: "tasks:\n  - id: nested\n    bash_command: 'sh -c \"sleep 12\"'\n",
            levels: vec![Duration::from_secs(12)],
        },
        WindowRow {
            case: "a command declared as a sequence of words",
            definition: "tasks:\n  - id: worded\n    operator: BashOperator\n    command:\n      \
                         - sleep\n      - \"9\"\n",
            levels: vec![Duration::from_secs(9)],
        },
        WindowRow {
            // Two tasks that wait for nothing run together, so the level is done when the
            // longer of them is.
            case: "two tasks at one level, the longer wait deciding",
            definition: "tasks:\n  - id: quick\n    bash_command: \"sleep 2\"\n  - id: slow\n    \
                         bash_command: \"sleep 7\"\n",
            levels: vec![Duration::from_secs(7)],
        },
        WindowRow {
            case: "a chain of two levels, each adding its own wait",
            definition: "tasks:\n  - id: extract\n    bash_command: \"sleep 3\"\n  - id: load\n    \
                         dependencies: [extract]\n    bash_command: \"sleep 4\"\n",
            levels: vec![Duration::from_secs(3), Duration::from_secs(4)],
        },
        WindowRow {
            // A program written over several LINES states the time of each of them.
            case: "a program written over several lines",
            definition: "tasks:\n  - id: scripted\n    bash_command: |\n      echo starting\n      \
                         sleep 2\n      sleep 3\n",
            levels: vec![Duration::from_secs(5)],
        },
        WindowRow {
            // A program that waits for nothing states no time of its own, and a command
            // that merely CARRIES the word as an operand of another states none either.
            case: "a command that waits for nothing",
            definition: "tasks:\n  - id: prints\n    bash_command: \"echo sleep 30\"\n",
            levels: vec![TASK_LEVEL_RUNS_FOR],
        },
        WindowRow {
            // The waiting command with no operand is the utility's own usage failure, so
            // the program states no time.
            case: "the waiting command given no operand",
            definition: "tasks:\n  - id: operandless\n    bash_command: \"sleep\"\n",
            levels: vec![TASK_LEVEL_RUNS_FOR],
        },
        WindowRow {
            // A wait of zero states no time either, so the default span covers it.
            case: "a declared wait of zero",
            definition: "tasks:\n  - id: immediate\n    bash_command: \"sleep 0\"\n",
            levels: vec![TASK_LEVEL_RUNS_FOR],
        },
        WindowRow {
            case: "a wait past the longest this emulator holds a run for",
            definition: "tasks:\n  - id: forever\n    bash_command: \"sleep 2000000000\"\n",
            levels: vec![MAX_DECLARED_RUNTIME],
        },
    ]
}

/// The whole window a run of `levels` occupies: the control plane's admission of the run,
/// the wait for an executor, and then each dependency level.
fn expected_window(levels: &[Duration]) -> Duration {
    levels
        .iter()
        .fold(RUN_ADMITTED_AFTER + RUN_DEQUEUED_AFTER, |total, level| {
            total + *level
        })
}

/// `CompletedOn - StartedOn` as the report carries them, in seconds.
fn reported_window(detail: &serde_json::Value, case: &str) -> f64 {
    let started = epoch_seconds(&detail["StartedOn"]).unwrap_or_else(|| {
        panic!("case {case:?}: an executed run reports StartedOn, got {detail:?}")
    });
    let completed = epoch_seconds(&detail["CompletedOn"]).unwrap_or_else(|| {
        panic!("case {case:?}: a completed run reports CompletedOn, got {detail:?}")
    });
    completed - started
}

#[tokio::test]
async fn the_window_a_run_reports_spans_the_work_its_definition_declared() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = definition_router_on(std::sync::Arc::clone(&engine));
    create_bucket(&router).await;

    for (index, row) in window_rows().into_iter().enumerate() {
        let case = row.case;
        let key = format!("dags/declared-window-{index}.yaml");
        stage(&router, &key, row.definition.as_bytes()).await;
        let arn = create_workflow(&router, &format!("declaredWindow{index}"), &key).await;
        let (status, started) = call(
            &router,
            "StartWorkflowRun",
            serde_json::json!({"WorkflowArn": arn}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartWorkflowRun to succeed, got {status} {started:?}"
        );
        let run_id = started["RunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a started run has a RunId, got {started:?}"))
            .to_string();

        // The run is put back to the status its start reported, with its whole schedule
        // already owed: one read then materialises every transition the clock owes, each
        // dated from the instant it fell due, so the window below is an exact value
        // rather than a race against the wall clock — and a declared wait of hours costs
        // this test nothing.
        let window = expected_window(&row.levels);
        let started_on = SystemTime::now() - window - Duration::from_secs(1);
        with_stored_run(&engine, &arn, &run_id, |run| {
            run.status = RUN_STATUS_STARTING.to_string();
            run.created_at = started_on;
            run.started_on = started_on;
            run.modified_at = started_on;
            run.next_status_at = started_on + RUN_ADMITTED_AFTER;
            run.completed_on = None;
            run.duration = None;
            run.error_message = None;
            run.task_instances.clear();
        });

        let (status, answered) = call(
            &router,
            "GetWorkflowRun",
            serde_json::json!({"WorkflowArn": arn, "RunId": run_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetWorkflowRun to succeed, got {status} {answered:?}"
        );
        let detail = &answered["RunDetail"];
        assert_eq!(
            detail["RunState"].as_str(),
            Some(RUN_STATUS_SUCCESS),
            "case {case:?}: every declared task finished, so the run completed \
             successfully, got {:?}",
            detail["RunState"]
        );

        let expected = window.as_secs_f64();
        let reported = reported_window(detail, case);
        assert!(
            (reported - expected).abs() < 0.002,
            "case {case:?}: expected the reported window to span the declared work \
             ({expected} s), got {reported} s"
        );
        let expected_duration = i64::try_from(window.as_millis().div_ceil(1000))
            .expect("a window of at most a day, in seconds");
        assert_eq!(
            detail["Duration"].as_i64(),
            Some(expected_duration),
            "case {case:?}: expected Duration {expected_duration} s, derived from the same \
             window, got {:?}",
            detail["Duration"]
        );
    }
}

#[tokio::test]
async fn a_run_of_a_declared_wait_is_observable_executing_and_its_window_tracks_that_wait() {
    let router = definition_router();
    create_bucket(&router).await;

    // Two definitions of the same shape, differing only in the wait their one task
    // declares: the window each run reports has to follow its own declaration, so the
    // longer-declared run is observable in flight and the shorter-declared one reports a
    // shorter window.
    let declared = Duration::from_secs(2);
    let brief = Duration::from_millis(100);
    let long_key = "dags/observable-long.yaml";
    let brief_key = "dags/observable-brief.yaml";
    stage(
        &router,
        long_key,
        b"observable_dag:\n  tasks:\n    only_task:\n      operator: BashOperator\n      \
          bash_command: \"sleep 2\"\n",
    )
    .await;
    stage(
        &router,
        brief_key,
        b"observable_dag:\n  tasks:\n    only_task:\n      operator: BashOperator\n      \
          bash_command: \"sleep 0.1\"\n",
    )
    .await;
    let long_arn = create_workflow(&router, "observableLong", long_key).await;
    let brief_arn = create_workflow(&router, "observableBrief", brief_key).await;

    let mut runs = Vec::new();
    for (case, arn) in [
        ("the declared wait", &long_arn),
        ("a briefer wait", &brief_arn),
    ] {
        let (status, started) = call(
            &router,
            "StartWorkflowRun",
            serde_json::json!({"WorkflowArn": arn}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartWorkflowRun to succeed, got {status} {started:?}"
        );
        assert_eq!(
            started["Status"], RUN_STATUS_STARTING,
            "case {case:?}: a just-started run carries the documented initial status, got {:?}",
            started["Status"]
        );
        runs.push((
            case,
            arn.clone(),
            started["RunId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a started run has a RunId"))
                .to_string(),
        ));
    }

    // Poll each run the way a client waiting for completion does, recording the states it
    // passes through. The deadline is generous: what is asserted is the CONDITION the run
    // reaches, never how fast the host got there.
    let mut windows = Vec::new();
    for (case, arn, run_id) in runs {
        let request = serde_json::json!({"WorkflowArn": arn, "RunId": run_id});
        let mut observed: Vec<String> = Vec::new();
        let mut last = serde_json::Value::Null;
        let deadline = SystemTime::now() + Duration::from_secs(120);
        while SystemTime::now() < deadline {
            let (status, answered) = call(&router, "GetWorkflowRun", request.clone()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetWorkflowRun to succeed, got {status} {answered:?}"
            );
            let state = answered["RunDetail"]["RunState"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if observed.last() != Some(&state) {
                observed.push(state.clone());
            }
            last = answered;
            if state == RUN_STATUS_SUCCESS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            observed.last().map(String::as_str),
            Some(RUN_STATUS_SUCCESS),
            "case {case:?}: the run must reach SUCCESS, got the states {observed:?}"
        );
        windows.push((case, reported_window(&last["RunDetail"], case), observed));
    }

    let (long_case, long_window, long_states) = &windows[0];
    let (brief_case, brief_window, _) = &windows[1];
    assert!(
        long_states.iter().any(|state| state == RUN_STATUS_RUNNING),
        "case {long_case:?}: a run whose task declares a wait is observable executing, got \
         the states {long_states:?}"
    );
    assert!(
        *long_window >= declared.as_secs_f64(),
        "case {long_case:?}: expected a window of at least the declared {:?}, got \
         {long_window} s",
        declared
    );
    assert!(
        *brief_window < declared.as_secs_f64(),
        "case {brief_case:?}: expected the window to shrink with the declared wait of \
         {brief:?}, got {brief_window} s against the {long_window} s of a {declared:?} wait"
    );
}
