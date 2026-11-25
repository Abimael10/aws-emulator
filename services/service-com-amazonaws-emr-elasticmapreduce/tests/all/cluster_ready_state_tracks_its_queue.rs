// SPDX-License-Identifier: CC0-1.0

//! Which of the two states being UP a cluster reports, as its step queue moves.
//!
//! THE RULE under test: **a cluster that is up reports `RUNNING` while its queue still holds
//! a step to run, and `WAITING` only once it holds none — "if the `JobFlowInstancesConfig`
//! `KeepJobFlowAliveWhenNoSteps` parameter is set to TRUE, the cluster transitions to the
//! `WAITING` state rather than shutting down AFTER the steps have completed" (`RunJobFlow`
//! doc), so resting is what a cluster does once its steps are done with, and a cluster whose
//! queue holds a step that has not reached a terminal `StepState` is running one.** A cluster
//! that did NOT ask to stay up is the other half of that same sentence — it shuts down instead
//! of resting — so it reports `RUNNING` for as long as it is up at all and never `WAITING`.
//!
//! Every case drives the wire path `DescribeCluster`/`ListSteps`/`AddJobFlowSteps` reach, so
//! nothing here is evidence of anything but the persisted state.
//!
//! The rows differ along what the rule generalises over: how many steps the queue holds (none,
//! one, several), how many may run at once, how the steps arrived (with the launch or through
//! `AddJobFlowSteps` on a cluster already resting), how each step ENDED (a command that exits
//! zero and a submission nothing runs, which fails) and whether the launch asked the cluster to
//! stay up at all.
//!
//! Nothing here waits a fixed time: every phase is a stored due instant, so each wait LOOKS
//! AGAIN until the condition holds or a generous deadline passes. The invariant is checked by
//! reading the CLUSTER first and its STEPS second: a step's state only ever moves toward a
//! terminal one, so a step found outstanding after the cluster was read was outstanding when
//! the cluster was read too.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{at, await_cluster_state, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// How long a wait keeps looking before it gives up, and how often it looks. Generous, and
/// only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL_STEP: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The states a cluster that is UP reports (`com.amazonaws.emr#ClusterState`).
const RUNNING: &str = "RUNNING";
const WAITING: &str = "WAITING";

/// The states a cluster is still coming up through, which say nothing about its queue yet.
const COMING_UP: [&str; 2] = ["STARTING", "BOOTSTRAPPING"];

/// How many times a resting cluster is re-read to show that nothing moves it out of `WAITING`
/// once its queue is empty. CHOSEN: more than one, so resting is a repeated answer.
const RESTING_REREADS: usize = 4;

/// What one submitted step's work is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// A command line that exits zero, so the step ends `COMPLETED`.
    Echo,
    /// A jar of the submitter's own, which nothing here runs, so the step ends `FAILED`.
    Jar,
}

/// One submitted step: its work, under `CONTINUE` so a step that fails neither cancels the
/// queue behind it nor takes its cluster down ("`CONTINUE` — Continues to the next step in the
/// queue", `StepConfig$ActionOnFailure` doc).
fn step(work: Work, ordinal: usize) -> Value {
    let hadoop = match work {
        Work::Echo => json!({
            "Jar": "command-runner.jar",
            "Args": ["bash", "-c", format!("echo queue-state-{ordinal}")],
        }),
        Work::Jar => json!({
            "Jar": format!("s3://no-such-bucket/step-{ordinal}.jar"),
            "Args": [format!("{ordinal}")],
        }),
    };
    json!({
        "Name": format!("queue-state-{ordinal}"),
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": hadoop,
    })
}

/// The steps a row submits, alternating the two endings so the rule is read over a queue whose
/// steps did not all end the same way.
fn steps(count: usize) -> Vec<Value> {
    (0..count)
        .map(|ordinal| {
            step(
                if ordinal % 2 == 0 {
                    Work::Echo
                } else {
                    Work::Jar
                },
                ordinal,
            )
        })
        .collect()
}

/// One row: the launch's shape and what the rule says its cluster reports.
struct Row {
    case: &'static str,
    /// Whether the launch asks the cluster to stay up once it has no steps left.
    kept_alive: bool,
    /// How many steps the launch carries.
    steps_at_launch: usize,
    /// How many steps are submitted afterwards, once the cluster is already resting.
    steps_added_while_resting: usize,
    /// The `StepConcurrencyLevel` the launch asks for, or `None` for the documented default
    /// of one.
    concurrency: Option<i64>,
    /// Whether this row's queue is long enough that an observation of the cluster RUNNING its
    /// queue is required rather than merely allowed.
    requires_a_running_observation: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "kept alive with no steps at all",
            kept_alive: true,
            steps_at_launch: 0,
            steps_added_while_resting: 0,
            concurrency: None,
            requires_a_running_observation: false,
        },
        Row {
            case: "kept alive with one step at the default concurrency",
            kept_alive: true,
            steps_at_launch: 1,
            steps_added_while_resting: 0,
            concurrency: None,
            requires_a_running_observation: false,
        },
        Row {
            case: "kept alive with six steps one at a time",
            kept_alive: true,
            steps_at_launch: 6,
            steps_added_while_resting: 0,
            concurrency: Some(1),
            requires_a_running_observation: true,
        },
        Row {
            case: "kept alive with four steps and two slots",
            kept_alive: true,
            steps_at_launch: 4,
            steps_added_while_resting: 0,
            concurrency: Some(2),
            requires_a_running_observation: false,
        },
        Row {
            case: "kept alive, its only steps submitted while it rests",
            kept_alive: true,
            steps_at_launch: 0,
            steps_added_while_resting: 2,
            concurrency: Some(1),
            requires_a_running_observation: false,
        },
        Row {
            case: "a cluster that does not stay up, with two steps",
            kept_alive: false,
            steps_at_launch: 2,
            steps_added_while_resting: 0,
            concurrency: Some(1),
            requires_a_running_observation: false,
        },
    ]
}

/// The `RunJobFlow` request one row launches with.
fn launch_request(row: &Row) -> Value {
    let mut request = json!({
        "Name": row.case,
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": {
            "InstanceCount": 1,
            "MasterInstanceType": "m5.xlarge",
            "KeepJobFlowAliveWhenNoSteps": row.kept_alive,
        },
    });
    if row.steps_at_launch > 0 {
        request["Steps"] = Value::Array(steps(row.steps_at_launch));
    }
    if let Some(level) = row.concurrency {
        request["StepConcurrencyLevel"] = json!(level);
    }
    request
}

/// The state `cluster_id` reports now.
async fn cluster_state(emr: &Emr, cluster_id: &str) -> String {
    let cluster = emr.describe_cluster(cluster_id).await;
    at(&cluster, &["Status", "State"])
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The state every step of `cluster_id` reports now, newest first as `ListSteps` answers them.
async fn step_states(emr: &Emr, cluster_id: &str) -> Vec<String> {
    let listed = emr.list_steps(json!({ "ClusterId": cluster_id })).await;
    at(&listed, &["Steps"])
        .as_array()
        .map(|steps| {
            steps
                .iter()
                .map(|step| {
                    at(step, &["Status", "State"])
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Watch `cluster_id` until its queue holds no step still to run, holding the rule on every
/// look, and answer whether the cluster was ever seen `RUNNING` with work outstanding.
///
/// The CLUSTER is read before its steps, so a step found outstanding here was outstanding when
/// the cluster was read: a step's state only moves toward a terminal one, never back.
async fn held_while_the_queue_ran(emr: &Emr, case: &str, cluster_id: &str) -> bool {
    let deadline = Instant::now() + DEADLINE;
    let mut saw_running_with_work = false;
    loop {
        let state = cluster_state(emr, cluster_id).await;
        let states = step_states(emr, cluster_id).await;
        let outstanding: Vec<&String> = states
            .iter()
            .filter(|state| !TERMINAL_STEP.contains(&state.as_str()))
            .collect();
        if !COMING_UP.contains(&state.as_str()) && !outstanding.is_empty() {
            assert_eq!(
                state, RUNNING,
                "case {case:?}: a cluster that is up with {} step(s) still to run must report \
                 {RUNNING}, got {state:?} with steps {states:?}",
                outstanding.len()
            );
            saw_running_with_work = true;
        }
        if outstanding.is_empty() && !COMING_UP.contains(&state.as_str()) {
            return saw_running_with_work;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every step to end, got cluster {state:?} with steps \
             {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// THE RULE, over the whole table: a cluster that is up reports `RUNNING` while its queue
/// holds a step to run and `WAITING` only once it holds none — and a cluster that was not
/// asked to stay up reports `WAITING` at no point in its life.
#[tokio::test]
async fn a_cluster_that_is_up_rests_only_once_its_queue_has_run() {
    let emr = Emr::new();
    let rows = rows();

    // Every row's cluster is launched FIRST, so the phases the clock owes them all run
    // together rather than one row's waiting standing in front of the next row's launch.
    let mut launched: Vec<String> = Vec::new();
    for row in &rows {
        launched.push(emr.run_job_flow(launch_request(row)).await);
    }

    // The clusters that do NOT stay up are watched FIRST: such a cluster shuts itself down
    // once its queue has run, so the whole of its life is observable from here on, while a
    // cluster that rests keeps answering the same state however long it is left.
    for (row, cluster_id) in rows
        .iter()
        .zip(&launched)
        .filter(|(row, _)| !row.kept_alive)
    {
        let case = row.case;
        // A cluster that was not asked to stay up never rests: it reports RUNNING while it is
        // up and then shuts itself down, so WAITING appears at no point in its life.
        let deadline = Instant::now() + DEADLINE;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let state = cluster_state(&emr, cluster_id).await;
            if seen.last() != Some(&state) {
                seen.push(state.clone());
            }
            assert_ne!(
                state, WAITING,
                "case {case:?}: a cluster that does not stay up must never rest, got states \
                 {seen:?}"
            );
            if state == "TERMINATED" || state == "TERMINATED_WITH_ERRORS" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the cluster to shut itself down, got states {seen:?}"
            );
            tokio::time::sleep(POLL).await;
        }
        assert!(
            seen.iter().any(|state| state == RUNNING),
            "case {case:?}: a cluster that is up to run its steps reports {RUNNING}, got states \
             {seen:?}"
        );
    }

    for (row, cluster_id) in rows.iter().zip(&launched).filter(|(row, _)| row.kept_alive) {
        let case = row.case;

        // (1) While the queue the launch carried still holds a step, the cluster that is
        // up reports RUNNING; once it holds none, the cluster rests.
        let saw_running = held_while_the_queue_ran(&emr, case, cluster_id).await;
        assert!(
            !row.requires_a_running_observation || saw_running,
            "case {case:?}: a cluster running a queue of {} steps one at a time must be \
             seen {RUNNING}",
            row.steps_at_launch
        );
        let resting = await_cluster_state(&emr, cluster_id, case, WAITING).await;
        assert_eq!(
            at(&resting, &["Status", "State"]).as_str(),
            Some(WAITING),
            "case {case:?}: a cluster kept alive with no step left to run rests, got {resting}"
        );
        // Nothing moves a resting cluster out of WAITING on its own: read after read, it
        // answers the same state.
        for round in 0..RESTING_REREADS {
            let state = cluster_state(&emr, cluster_id).await;
            assert_eq!(
                state, WAITING,
                "case {case:?}: re-read {round} of a resting cluster must answer {WAITING}, \
                 got {state:?}"
            );
        }

        // (2) A step submitted to a cluster that is RESTING is work again: the cluster
        // reports RUNNING while it has that step to run, and rests once more afterwards.
        if row.steps_added_while_resting > 0 {
            let added = emr
                .ok(
                    "AddJobFlowSteps",
                    json!({
                        "JobFlowId": cluster_id,
                        "Steps": Value::Array(steps(row.steps_added_while_resting)),
                    }),
                )
                .await;
            assert_eq!(
                at(&added, &["StepIds"]).as_array().map(Vec::len),
                Some(row.steps_added_while_resting),
                "case {case:?}: AddJobFlowSteps answers one id per step, got {added}"
            );
            held_while_the_queue_ran(&emr, case, cluster_id).await;
            let resting = cluster_state(&emr, cluster_id).await;
            assert_eq!(
                resting, WAITING,
                "case {case:?}: a cluster rests again once the steps added to it have run, \
                 got {resting:?}"
            );
        }
    }
}

/// THE RULE's other side, for the one call that takes the launch's keep-alive intent back:
/// "to terminate a cluster that no longer has any steps to run, set
/// `KeepJobFlowAliveWhenNoSteps` to false" (`SetKeepJobFlowAliveWhenNoSteps` doc). A cluster
/// already RESTING with nothing queued is not running a queue, so the call that asks it to stop
/// staying up leaves it `WAITING` until the shutdown that call owes it begins — it is never
/// re-reported as `RUNNING` on the way out.
#[tokio::test]
async fn a_resting_cluster_asked_to_stop_staying_up_keeps_resting_until_it_shuts_down() {
    let case = "keep-alive cleared on a resting cluster";
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(launch_request(&Row {
            case,
            kept_alive: true,
            steps_at_launch: 1,
            steps_added_while_resting: 0,
            concurrency: None,
            requires_a_running_observation: false,
        }))
        .await;
    held_while_the_queue_ran(&emr, case, &cluster_id).await;
    await_cluster_state(&emr, &cluster_id, case, WAITING).await;

    emr.ok(
        "SetKeepJobFlowAliveWhenNoSteps",
        json!({ "JobFlowIds": [&cluster_id], "KeepJobFlowAliveWhenNoSteps": false }),
    )
    .await;

    // From the call on, the cluster is owed a shutdown and nothing else: every state it
    // reports until it is shutting down is the one it was already resting in.
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let state = cluster_state(&emr, &cluster_id).await;
        if seen.last() != Some(&state) {
            seen.push(state.clone());
        }
        assert_ne!(
            state, RUNNING,
            "case {case:?}: a resting cluster asked to stop staying up must not be reported as \
             running a queue that holds nothing, got states {seen:?}"
        );
        if state == "TERMINATING" || state == "TERMINATED" {
            break;
        }
        assert_eq!(
            state, WAITING,
            "case {case:?}: expected the cluster to keep resting until it shuts down, got states \
             {seen:?}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the cluster to shut itself down, got states {seen:?}"
        );
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        seen.first().map(String::as_str),
        Some(WAITING),
        "case {case:?}: the cluster was resting when the call arrived, got states {seen:?}"
    );
}
