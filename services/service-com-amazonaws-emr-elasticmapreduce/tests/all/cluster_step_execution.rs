// SPDX-License-Identifier: CC0-1.0

//! The cluster's provisioning phases and its step runner, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, and reads every outcome back through `DescribeCluster`, `DescribeStep` and
//! `ListSteps`, so nothing here is evidence of anything but the persisted state.
//!
//! THREE RULES are under test.
//!
//! The cluster's: a cluster `RunJobFlow` has just launched is not up yet — it provisions
//! its Amazon EC2 capacity (`STARTING`) and then runs its bootstrap actions
//! (`BOOTSTRAPPING`, "a list of bootstrap actions to run before Hadoop starts on the
//! cluster nodes", `RunJobFlow$BootstrapActions`) before it reports itself ready, which is
//! `WAITING` when it is kept alive with no steps and `RUNNING` otherwise.
//!
//! The step's: a step submitted to a cluster is queued, and the cluster RUNS it once it is
//! ready and one of its concurrency slots is free — "specifies the number of steps that
//! can be executed concurrently" (`RunJobFlow$StepConcurrencyLevel`) — so the step leaves
//! `PENDING` for `RUNNING` and then ENDS, reporting both instants in `Status.Timeline`. The
//! application every step of this suite submits is one no bytes of exist here (its file is
//! in a bucket this emulator holds nothing in, and no object store is mounted beside Amazon
//! EMR in this suite), so each step ends in the modelled FAILURE state with the details
//! saying why — `COMPLETED` would be a success for work nothing performed. A step whose
//! application this emulator DOES run is the sibling suite's subject
//! (`tests/all/spark_step_application.rs`). A cluster shutting down takes its queue with it:
//! a step still to run on it never does, and one that had started does not finish. And once
//! the queue has run, a cluster that did not ask to stay up stops itself: "after the steps
//! complete, the cluster stops" (`RunJobFlow`), which is what `Cluster$AutoTerminate`
//! reports.
//!
//! The rows span what those rules generalise over: how many steps the queue holds (one,
//! two, four), how many of them may run at once (the documented default of one, two, more
//! slots than steps), whether the steps arrived with the launch or through
//! `AddJobFlowSteps`, and whether the cluster stays up after them.
//!
//! The third rule is the failed step's `ActionOnFailure`: "`TERMINATE_CLUSTER` — Shuts down
//! the cluster. `CANCEL_AND_WAIT` — Cancels any pending steps and returns the cluster to the
//! `WAITING` state. `CONTINUE` — Continues to the next step in the queue. `TERMINATE_JOB_FLOW`
//! — Shuts down the cluster" (`StepConfig$ActionOnFailure`), applied when the step's own work
//! answered the failure and only on a cluster running one step at a time, which that same doc
//! is what says.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::{CLUSTER_PHASE_SPAN, STEP_RUN_SPAN};

use crate::job_flow_lifecycle::{
    at, kept_alive_request, step_config, Emr, INSTANCE_PROFILE, SERVICE_ROLE,
};

/// A margin that absorbs the time the harness itself takes between a due instant and the
/// read that observes it.
const MARGIN: Duration = Duration::from_millis(400);

/// How long a poll waits for every step of a queue to reach a terminal state before it
/// gives up: the longest row's queue, run one step at a time, with room to spare.
const QUEUE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// How close two instants have to be to count as the same one, given that each is read
/// over a separate request.
const TOLERANCE: f64 = 0.05;

/// One row of the table: a cluster, the steps it is given and how many of them may run at
/// once.
struct Case {
    label: &'static str,
    /// How many steps the cluster is launched with.
    steps_at_launch: usize,
    /// How many steps are added afterwards, through `AddJobFlowSteps`.
    steps_added: usize,
    /// The `StepConcurrencyLevel` the launch asks for, or `None` for the documented
    /// default of one.
    concurrency: Option<i32>,
    /// Whether the cluster stays up once it has no steps left to run.
    kept_alive: bool,
    /// The state the cluster reports once it is ready.
    ready_state: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one step at the default concurrency, added after the launch",
            steps_at_launch: 0,
            steps_added: 1,
            concurrency: None,
            kept_alive: true,
            ready_state: "WAITING",
        },
        Case {
            label: "four steps one at a time, submitted with the launch",
            steps_at_launch: 4,
            steps_added: 0,
            concurrency: Some(1),
            kept_alive: true,
            ready_state: "WAITING",
        },
        Case {
            label: "two steps, two slots, on a cluster that does not stay up",
            steps_at_launch: 1,
            steps_added: 1,
            concurrency: Some(2),
            kept_alive: false,
            ready_state: "RUNNING",
        },
        Case {
            label: "two steps with more slots than steps",
            steps_at_launch: 2,
            steps_added: 0,
            concurrency: Some(4),
            kept_alive: true,
            ready_state: "WAITING",
        },
    ]
}

/// The `RunJobFlow` request of one row.
fn launch_request(case: &Case) -> Value {
    let mut request = if case.kept_alive {
        kept_alive_request(case.label)
    } else {
        json!({
            "Name": case.label,
            "JobFlowRole": INSTANCE_PROFILE, "ServiceRole": SERVICE_ROLE,
            "Instances": { "InstanceCount": 1, "MasterInstanceType": "m5.xlarge" },
        })
    };
    if let Some(level) = case.concurrency {
        request["StepConcurrencyLevel"] = json!(level);
    }
    if case.steps_at_launch > 0 {
        request["Steps"] = json!(submitted_steps("launched", case.steps_at_launch));
    }
    request
}

/// `count` steps named after `origin`, each a `command-runner.jar` invocation of its own.
fn submitted_steps(origin: &str, count: usize) -> Vec<Value> {
    (0..count)
        .map(|ordinal| {
            step_config(
                &format!("{origin}-{ordinal}"),
                "command-runner.jar",
                json!(["spark-submit", format!("s3://example/{origin}-{ordinal}.py")]),
                Value::Null,
            )
        })
        .collect()
}

/// The ids a `StepIds`-carrying answer names, in the order it names them.
fn ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|ids| {
            ids.iter()
                .map(|id| id.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The step the cluster carries under `step_id`, read back through `DescribeStep`.
async fn described(emr: &Emr, cluster_id: &str, step_id: &str) -> Value {
    emr.ok(
        "DescribeStep",
        json!({ "ClusterId": cluster_id, "StepId": step_id }),
    )
    .await["Step"]
        .clone()
}

/// The state of one step.
async fn state_of(emr: &Emr, cluster_id: &str, step_id: &str) -> String {
    at(&described(emr, cluster_id, step_id).await, &["Status", "State"])
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every step of the cluster in SUBMISSION order, as `ListSteps` projects it: that operation
/// reports them newest first, so the order its answer is read back in is the reverse.
///
/// One call per read of a whole queue rather than one per step, which is what lets a poll over
/// several clusters cost one read per cluster.
async fn listed_queue(emr: &Emr, cluster_id: &str) -> Vec<Value> {
    let listed = emr.list_steps(json!({ "ClusterId": cluster_id })).await;
    at(&listed, &["Steps"])
        .as_array()
        .map(|steps| steps.iter().rev().cloned().collect())
        .unwrap_or_default()
}

/// The ids of that queue, in the same order.
async fn queue_of(emr: &Emr, cluster_id: &str) -> Vec<String> {
    listed_queue(emr, cluster_id)
        .await
        .iter()
        .map(|step| step["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Poll until every step of `step_ids` is in a terminal state, answering the steps as they
/// were last read.
async fn drained(case: &str, emr: &Emr, cluster_id: &str, step_ids: &[String]) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let mut steps = Vec::with_capacity(step_ids.len());
        for step_id in step_ids {
            steps.push(described(emr, cluster_id, step_id).await);
        }
        let states: Vec<String> = steps
            .iter()
            .map(|step| {
                at(step, &["Status", "State"])
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        if states.iter().all(|state| TERMINAL.contains(&state.as_str())) {
            return steps;
        }
        assert!(
            started.elapsed() < QUEUE_DEADLINE,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An instant member of a step's timeline, or a panic naming the case.
fn instant(case: &str, step: &Value, member: &str) -> f64 {
    at(step, &["Status", "Timeline", member])
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {step}"))
}

/// Every row: the cluster occupies both provisioning phases with its queue untouched, then
/// reports itself ready and runs the queue to the end, at most `StepConcurrencyLevel`
/// steps at a time, recording each step's own timeline.
#[tokio::test]
async fn a_cluster_comes_up_in_phases_and_then_runs_its_queue() {
    let emr = Emr::new();
    let cases = cases();

    // Launch every row first, so the phases below are observed once for the whole table.
    let mut launched: Vec<(String, Vec<String>)> = Vec::new();
    for case in &cases {
        let label = case.label;
        let cluster_id = emr.run_job_flow(launch_request(case)).await;
        let mut step_ids = queue_of(&emr, &cluster_id).await;
        assert_eq!(
            step_ids.len(),
            case.steps_at_launch,
            "case {label:?}: expected the launched steps to be listed, got {step_ids:?}"
        );
        if case.steps_added > 0 {
            let out = emr
                .ok(
                    "AddJobFlowSteps",
                    json!({
                        "JobFlowId": &cluster_id,
                        "Steps": submitted_steps("added", case.steps_added),
                    }),
                )
                .await;
            let added = ids(&out["StepIds"]);
            assert_eq!(
                added.len(),
                case.steps_added,
                "case {label:?}: AddJobFlowSteps answers one id per step, got {out}"
            );
            step_ids.extend(added);
        }
        launched.push((cluster_id, step_ids));
    }

    // Phase one: the cluster is acquiring its capacity, and its queue has not moved.
    for (case, (cluster_id, step_ids)) in cases.iter().zip(&launched) {
        let label = case.label;
        let cluster = emr.describe_cluster(cluster_id).await;
        assert_eq!(
            at(&cluster, &["Status", "State"]).as_str(),
            Some("STARTING"),
            "case {label:?}: a just-launched cluster is still starting, got {cluster}"
        );
        for step_id in step_ids {
            let step = described(&emr, cluster_id, step_id).await;
            assert_eq!(
                at(&step, &["Status", "State"]).as_str(),
                Some("PENDING"),
                "case {label:?}: a step of a cluster that is not up yet is queued, got {step}"
            );
            assert!(
                at(&step, &["Status", "Timeline", "StartDateTime"]).is_null(),
                "case {label:?}: a queued step has not started, got {step}"
            );
        }
    }

    // Phase two: one span in, the cluster is running its bootstrap actions and its queue
    // still has not moved.
    tokio::time::sleep(CLUSTER_PHASE_SPAN + MARGIN).await;
    for (case, (cluster_id, step_ids)) in cases.iter().zip(&launched) {
        let label = case.label;
        let cluster = emr.describe_cluster(cluster_id).await;
        assert_eq!(
            at(&cluster, &["Status", "State"]).as_str(),
            Some("BOOTSTRAPPING"),
            "case {label:?}: a cluster whose capacity is up runs its bootstrap actions, \
             got {cluster}"
        );
        assert!(
            at(&cluster, &["Status", "Timeline", "ReadyDateTime"]).is_null(),
            "case {label:?}: a bootstrapping cluster is not ready yet, got {cluster}"
        );
        for step_id in step_ids {
            assert_eq!(
                state_of(&emr, cluster_id, step_id).await,
                "PENDING",
                "case {label:?}: a step of a cluster that is not up yet is queued"
            );
        }
    }

    // Past the second span the cluster is ready, and the queue runs. Every row's ready
    // state is read FIRST, before any queue is waited out: a cluster that does not stay up
    // stops once its own queue has drained, so the ready state is only observable while
    // every row still has work.
    tokio::time::sleep(CLUSTER_PHASE_SPAN + MARGIN).await;
    let mut ready_instants: Vec<f64> = Vec::new();
    for (case, (cluster_id, _)) in cases.iter().zip(&launched) {
        let label = case.label;
        let cluster = emr.describe_cluster(cluster_id).await;
        assert_eq!(
            at(&cluster, &["Status", "State"]).as_str(),
            Some(case.ready_state),
            "case {label:?}: expected {} once both phases are due, got {cluster}",
            case.ready_state
        );
        ready_instants.push(
            at(&cluster, &["Status", "Timeline", "ReadyDateTime"])
                .as_f64()
                .unwrap_or_else(|| panic!("case {label:?}: no ReadyDateTime in {cluster}")),
        );
    }

    for ((case, (cluster_id, step_ids)), ready_at) in
        cases.iter().zip(&launched).zip(ready_instants)
    {
        let label = case.label;
        let steps = drained(label, &emr, cluster_id, step_ids).await;
        let slots = usize::try_from(case.concurrency.unwrap_or(1)).unwrap_or(1);
        let mut intervals: Vec<(f64, f64)> = Vec::new();
        let mut reasons: BTreeSet<String> = BTreeSet::new();
        for step in &steps {
            assert_eq!(
                at(step, &["Status", "State"]).as_str(),
                Some("FAILED"),
                "case {label:?}: a step whose application nothing could run ends in the \
                 modelled failure state, got {step}"
            );
            // The failure is this service's own, and it says so in both members the model
            // gives a step failure — which is what tells it apart from an application that
            // really failed.
            let reason = at(step, &["Status", "FailureDetails", "Reason"])
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !reason.is_empty()
                    && at(step, &["Status", "FailureDetails", "Message"])
                        .as_str()
                        .is_some_and(|message| !message.is_empty()),
                "case {label:?}: an ended step says why it ended, got {step}"
            );
            assert_eq!(
                at(step, &["Status", "StateChangeReason", "Message"]).as_str(),
                Some(reason.as_str()),
                "case {label:?}: the state change gives the same reason, got {step}"
            );
            reasons.insert(reason);
            let created = instant(label, step, "CreationDateTime");
            let start = instant(label, step, "StartDateTime");
            let end = instant(label, step, "EndDateTime");
            assert!(
                start >= created && end > start,
                "case {label:?}: expected created {created} <= start {start} < end {end}"
            );
            assert!(
                start + TOLERANCE >= ready_at,
                "case {label:?}: a step cannot start before its cluster is ready ({start} \
                 against {ready_at})"
            );
            let ran_for = end - start;
            assert!(
                (ran_for - STEP_RUN_SPAN.as_secs_f64()).abs() < TOLERANCE,
                "case {label:?}: expected a step to run for {:?}, got {ran_for}s",
                STEP_RUN_SPAN
            );
            intervals.push((start, end));
        }

        // At most `slots` steps overlap at any instant, and a step waits for a slot: the
        // one `slots` places back in the queue cannot start before the step in front of
        // it has ended.
        for (ordinal, (start, _)) in intervals.iter().enumerate() {
            let overlapping = intervals
                .iter()
                .filter(|(other_start, other_end)| other_start <= start && start < other_end)
                .count();
            assert!(
                overlapping <= slots,
                "case {label:?}: {overlapping} steps ran at once with {slots} slot(s), \
                 intervals {intervals:?}"
            );
            if let Some(freed) = ordinal
                .checked_sub(slots)
                .and_then(|earlier| intervals.get(earlier))
                .map(|(_, end)| *end)
            {
                assert!(
                    *start + TOLERANCE >= freed,
                    "case {label:?}: step {ordinal} started at {start} before a slot freed \
                     at {freed}, intervals {intervals:?}"
                );
            }
        }
        // With one slot the intervals never overlap; with several, the steps that fit the
        // slots start together.
        if slots == 1 {
            for pair in intervals.windows(2) {
                assert!(
                    pair[1].0 + TOLERANCE >= pair[0].1,
                    "case {label:?}: one slot runs one step at a time, got {intervals:?}"
                );
            }
        } else if intervals.len() >= 2 {
            assert!(
                (intervals[0].0 - intervals[1].0).abs() < TOLERANCE,
                "case {label:?}: {slots} slots run the first steps at once, got {intervals:?}"
            );
        }

        // The reason is the SERVICE's, not one step's: every step of the row carries the
        // same one, so it is not built out of the step that was submitted.
        assert_eq!(
            reasons.len(),
            1,
            "case {label:?}: one reason for every step of the queue, got {reasons:?}"
        );

        // `ListSteps` projects the same records.
        let listed = emr.list_steps(json!({ "ClusterId": cluster_id })).await;
        let states: Vec<&str> = at(&listed, &["Steps"])
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .map(|step| at(step, &["Status", "State"]).as_str().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            states.len() == steps.len() && states.iter().all(|state| *state == "FAILED"),
            "case {label:?}: the listing reports the same steps, got {listed}"
        );

        // "After the steps complete, the cluster stops … if KeepJobFlowAliveWhenNoSteps is
        // set to TRUE, the cluster transitions to the WAITING state rather than shutting
        // down after the steps have completed" (`RunJobFlow` doc): a cluster that did not
        // ask to stay up shuts itself down once its last step has ended, for the modelled
        // reason, and one that did keeps waiting.
        let after = emr.describe_cluster(cluster_id).await;
        let state = at(&after, &["Status", "State"])
            .as_str()
            .unwrap_or_default()
            .to_string();
        if case.kept_alive {
            assert_eq!(
                state, "WAITING",
                "case {label:?}: a cluster kept alive with no steps stays up, got {after}"
            );
        } else {
            assert!(
                state == "TERMINATING" || state == "TERMINATED",
                "case {label:?}: a cluster that does not stay up stops once its queue has \
                 run, got {after}"
            );
            assert_eq!(
                at(&after, &["Status", "StateChangeReason", "Code"]).as_str(),
                Some("ALL_STEPS_COMPLETED"),
                "case {label:?}: the shutdown names why it happened, got {after}"
            );
        }
    }
}

/// A step still queued when its cluster is shut down never runs: it leaves `PENDING` for
/// `CANCELLED` when the shutdown completes, with no start instant and — "the date and time
/// when the cluster step EXECUTION completed or failed" (`StepTimeline$EndDateTime` doc) —
/// no end instant either, since there was no execution; and the cluster itself reports
/// `TERMINATED`.
#[tokio::test]
async fn a_queued_step_is_cancelled_when_its_cluster_is_terminated() {
    let case = "a pending step on a terminated cluster";
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(kept_alive_request("terminated-queue"))
        .await;
    let out = emr
        .ok(
            "AddJobFlowSteps",
            json!({
                "JobFlowId": &cluster_id,
                "Steps": submitted_steps("doomed", 2),
            }),
        )
        .await;
    let step_ids = ids(&out["StepIds"]);
    assert_eq!(
        step_ids.len(),
        2,
        "case {case:?}: expected two step ids, got {out}"
    );
    // The cluster is still coming up, so both steps are queued behind it.
    for step_id in &step_ids {
        assert_eq!(
            state_of(&emr, &cluster_id, step_id).await,
            "PENDING",
            "case {case:?}: a step of a cluster that is not up yet is queued"
        );
    }

    emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
        .await;
    let steps = drained(case, &emr, &cluster_id, &step_ids).await;
    for step in &steps {
        assert_eq!(
            at(step, &["Status", "State"]).as_str(),
            Some("CANCELLED"),
            "case {case:?}: a step that never ran is cancelled with its cluster, got {step}"
        );
        assert!(
            at(step, &["Status", "Timeline", "StartDateTime"]).is_null(),
            "case {case:?}: a step that never ran reports no start, got {step}"
        );
        assert!(
            at(step, &["Status", "Timeline", "EndDateTime"]).is_null(),
            "case {case:?}: a step that never executed reports no end either, got {step}"
        );
    }
    let cluster = emr.describe_cluster(&cluster_id).await;
    assert_eq!(
        at(&cluster, &["Status", "State"]).as_str(),
        Some("TERMINATED"),
        "case {case:?}: the cluster itself has gone, got {cluster}"
    );
}

/// A step that was already RUNNING when its cluster was shut down does not finish: it ends
/// `INTERRUPTED`, keeping the instant it started and recording the instant it was cut
/// short.
#[tokio::test]
async fn a_running_step_is_interrupted_when_its_cluster_is_terminated() {
    let case = "a running step on a terminated cluster";
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(kept_alive_request("interrupted-step"))
        .await;
    let out = emr
        .ok(
            "AddJobFlowSteps",
            json!({
                "JobFlowId": &cluster_id,
                "Steps": submitted_steps("interrupted", 1),
            }),
        )
        .await;
    let step_ids = ids(&out["StepIds"]);
    let step_id = step_ids
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a step id, got {out}"))
        .clone();

    // Wait for the cluster to start running it, which is what makes the shutdown land on a
    // step that had already begun.
    let started = Instant::now();
    loop {
        if state_of(&emr, &cluster_id, &step_id).await == "RUNNING" {
            break;
        }
        assert!(
            started.elapsed() < QUEUE_DEADLINE,
            "case {case:?}: expected the cluster to start the step"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let running = described(&emr, &cluster_id, &step_id).await;
    let start = instant(case, &running, "StartDateTime");

    emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
        .await;
    let steps = drained(case, &emr, &cluster_id, &step_ids).await;
    let step = steps
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the step back"));
    assert_eq!(
        at(step, &["Status", "State"]).as_str(),
        Some("INTERRUPTED"),
        "case {case:?}: a step cut short by its cluster is interrupted, got {step}"
    );
    assert!(
        (instant(case, step, "StartDateTime") - start).abs() < TOLERANCE,
        "case {case:?}: an interrupted step keeps the instant it started, got {step}"
    );
    let end = instant(case, step, "EndDateTime");
    assert!(
        end >= start,
        "case {case:?}: expected an end at or after the start {start}, got {end}"
    );
}

// ---------------------------------------------------------------------------
// What a failed step's ActionOnFailure DOES
// ---------------------------------------------------------------------------

/// What becomes of the CLUSTER once a row's queue has run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClusterEnd {
    /// Nothing shut it down, so a cluster asked to stay up rests in `WAITING` and reports no
    /// end instant.
    StaysUp,
    /// The step failure shut it down: `TERMINATED_WITH_ERRORS` carrying the modelled
    /// `STEP_FAILURE` reason code, and an `EndDateTime` at or after the terminating step's.
    ShutDownOnStepFailure,
}

/// One step of a row: what it submits and the `ActionOnFailure` it carries.
#[derive(Clone, Copy)]
struct ActionStep {
    /// The shell program its `command-runner.jar` command submits — the status that program
    /// exits with is what the step ends in — or `None` for a step that submits a jar of its
    /// own, which is work this emulator runs nothing of: such a step FAILS without its
    /// submission having answered anything.
    program: Option<&'static str>,
    action: &'static str,
}

/// One row of the table: a queue, the states its steps must settle in and what becomes of
/// the cluster carrying it.
struct ActionRow {
    case: &'static str,
    steps: Vec<ActionStep>,
    states: Vec<&'static str>,
    cluster: ClusterEnd,
}

/// A step submitting `program` through `command-runner.jar`, carrying `action`.
fn ran(program: &'static str, action: &'static str) -> ActionStep {
    ActionStep {
        program: Some(program),
        action,
    }
}

/// A step submitting a jar of the caller's own, carrying `action`.
fn unrun(action: &'static str) -> ActionStep {
    ActionStep {
        program: None,
        action,
    }
}

/// The rows the rule is asserted over.
///
/// They span what it generalises over: WHICH action the failing step carries (each of the
/// four `ActionOnFailure` values the model declares), whether the step that carries it
/// actually failed (a non-zero status against a zero one), whether its failure is one its own
/// work answered (a command that exited non-zero against a jar nothing ran), and how long the
/// queue behind it is (one step behind, two steps either side).
fn action_rows() -> Vec<ActionRow> {
    vec![
        ActionRow {
            case: "CONTINUE continues to the next step in the queue",
            steps: vec![ran("exit 3", "CONTINUE"), ran("exit 0", "CONTINUE")],
            states: vec!["FAILED", "COMPLETED"],
            cluster: ClusterEnd::StaysUp,
        },
        ActionRow {
            case: "CANCEL_AND_WAIT cancels the pending step and leaves the cluster up",
            steps: vec![ran("exit 3", "CANCEL_AND_WAIT"), ran("exit 0", "CONTINUE")],
            states: vec!["FAILED", "CANCELLED"],
            cluster: ClusterEnd::StaysUp,
        },
        ActionRow {
            case: "TERMINATE_CLUSTER shuts the cluster down",
            steps: vec![ran("exit 4", "TERMINATE_CLUSTER"), ran("exit 0", "CONTINUE")],
            states: vec!["FAILED", "CANCELLED"],
            cluster: ClusterEnd::ShutDownOnStepFailure,
        },
        ActionRow {
            case: "TERMINATE_JOB_FLOW, the same shutdown under its older name",
            steps: vec![
                ran("exit 1", "TERMINATE_JOB_FLOW"),
                ran("exit 0", "CONTINUE"),
            ],
            states: vec!["FAILED", "CANCELLED"],
            cluster: ClusterEnd::ShutDownOnStepFailure,
        },
        ActionRow {
            case: "a terminating action in the middle of a queue",
            steps: vec![
                ran("exit 3", "CONTINUE"),
                ran("exit 0", "CONTINUE"),
                ran("exit 4", "TERMINATE_CLUSTER"),
                ran("exit 0", "CONTINUE"),
            ],
            states: vec!["FAILED", "COMPLETED", "FAILED", "CANCELLED"],
            cluster: ClusterEnd::ShutDownOnStepFailure,
        },
        ActionRow {
            case: "a step that SUCCEEDS applies no action, whichever one it names",
            steps: vec![ran("exit 0", "TERMINATE_CLUSTER"), ran("exit 0", "CONTINUE")],
            states: vec!["COMPLETED", "COMPLETED"],
            cluster: ClusterEnd::StaysUp,
        },
        ActionRow {
            case: "a step whose work nothing ran applies no action either",
            steps: vec![unrun("TERMINATE_CLUSTER"), ran("exit 0", "CONTINUE")],
            states: vec!["FAILED", "COMPLETED"],
            cluster: ClusterEnd::StaysUp,
        },
    ]
}

/// The `RunJobFlow` request one row launches: a cluster that stays up, running its queue one
/// step at a time, so the action is one its `StepConcurrencyLevel` admits.
fn action_launch(row: &ActionRow) -> Value {
    let steps: Vec<Value> = row
        .steps
        .iter()
        .enumerate()
        .map(|(ordinal, step)| {
            let name = format!("step-{ordinal}");
            let hadoop = match step.program {
                Some(program) => json!({
                    "Jar": "command-runner.jar",
                    "Args": ["bash", "-c", program],
                }),
                None => json!({ "Jar": "s3://jars/analytics.jar" }),
            };
            json!({
                "Name": name,
                "ActionOnFailure": step.action,
                "HadoopJarStep": hadoop,
            })
        })
        .collect();
    let mut request = kept_alive_request(row.case);
    request["StepConcurrencyLevel"] = json!(1);
    request["Steps"] = Value::Array(steps);
    request
}

/// How long the table waits for every row's queue to settle: each row runs its steps one at a
/// time behind two provisioning phases, and this is several times the longest row's work, so a
/// slow host delays the table rather than failing it.
const ACTION_DEADLINE: Duration = Duration::from_secs(180);

/// How long the table waits between rounds of reads. Longer than [`POLL_INTERVAL`] because one
/// round reads every row's whole queue, and each read performs the commands those steps
/// submitted.
const ACTION_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// THE RULE: `ActionOnFailure` is not a label a read echoes back — when a step whose own work
/// FAILED carries one, it decides what becomes of the rest of the queue and of the cluster.
/// "`TERMINATE_CLUSTER` — Shuts down the cluster. `CANCEL_AND_WAIT` — Cancels any pending
/// steps and returns the cluster to the `WAITING` state. `CONTINUE` — Continues to the next
/// step in the queue. `TERMINATE_JOB_FLOW` — Shuts down the cluster"
/// (`StepConfig$ActionOnFailure` doc). A shutdown it brings is the cluster's own failure, so
/// the cluster ends `TERMINATED_WITH_ERRORS` with the modelled `STEP_FAILURE` reason code and
/// a `Timeline.EndDateTime` no earlier than the terminating step's own.
///
/// A failure the step's work did NOT answer decides nothing: a step this emulator ran no
/// application for failed for a reason of this service's rather than the application's, so it
/// cancels no step and shuts no cluster down.
#[tokio::test]
async fn a_failed_steps_action_on_failure_decides_the_queue_and_the_cluster() {
    let emr = Emr::new();
    let rows = action_rows();

    // Every row is launched first and then drained TOGETHER, so the clock runs each queue
    // alongside the others rather than one after another.
    let mut launched: Vec<(String, Vec<String>)> = Vec::new();
    for row in &rows {
        let case = row.case;
        let cluster_id = emr.run_job_flow(action_launch(row)).await;
        let step_ids = queue_of(&emr, &cluster_id).await;
        assert_eq!(
            step_ids.len(),
            row.steps.len(),
            "case {case:?}: expected the launched steps to be listed, got {step_ids:?}"
        );
        launched.push((cluster_id, step_ids));
    }

    let started = Instant::now();
    let mut settled: Vec<Vec<Value>> = vec![Vec::new(); rows.len()];
    loop {
        let mut all_done = true;
        for (index, (row, (cluster_id, _))) in rows.iter().zip(&launched).enumerate() {
            if !settled[index].is_empty() {
                continue;
            }
            let steps = listed_queue(&emr, cluster_id).await;
            let terminal = steps.len() == row.steps.len()
                && steps.iter().all(|step| {
                    at(step, &["Status", "State"])
                        .as_str()
                        .is_some_and(|state| TERMINAL.contains(&state))
                });
            // A row whose cluster is shutting down is not settled until the shutdown has
            // completed either, since the cluster's own end is part of what is asserted.
            let cluster_settled = row.cluster == ClusterEnd::StaysUp
                || at(&emr.describe_cluster(cluster_id).await, &["Status", "State"])
                    .as_str()
                    == Some("TERMINATED_WITH_ERRORS");
            if terminal && cluster_settled {
                settled[index] = steps;
            } else {
                all_done = false;
            }
        }
        if all_done {
            break;
        }
        assert!(
            started.elapsed() < ACTION_DEADLINE,
            "expected every row's queue and cluster to settle; still waiting on {:?}",
            rows.iter()
                .zip(&settled)
                .filter(|(_, steps)| steps.is_empty())
                .map(|(row, _)| row.case)
                .collect::<Vec<&str>>()
        );
        tokio::time::sleep(ACTION_POLL_INTERVAL).await;
    }

    for ((row, (cluster_id, _)), steps) in rows.iter().zip(&launched).zip(&settled) {
        let case = row.case;
        let states: Vec<&str> = steps
            .iter()
            .map(|step| at(step, &["Status", "State"]).as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            states, row.states,
            "case {case:?}: expected the queue to settle in {:?}, got {states:?} from {steps:?}",
            row.states
        );

        // A step the action cancelled never ran at all: it reports no start, and it ended
        // when the queue stopped rather than when anybody looked.
        for (step, state) in steps.iter().zip(&states) {
            if *state == "CANCELLED" {
                assert!(
                    at(step, &["Status", "Timeline", "StartDateTime"]).is_null(),
                    "case {case:?}: a cancelled step never started, got {step}"
                );
                // "The date and time when the cluster step EXECUTION completed or failed."
                // (`StepTimeline$EndDateTime` doc.) There was no execution, so there is no
                // end to report.
                assert!(
                    at(step, &["Status", "Timeline", "EndDateTime"]).is_null(),
                    "case {case:?}: a cancelled step reports no end either, got {step}"
                );
            }
        }

        let cluster = emr.describe_cluster(cluster_id).await;
        match row.cluster {
            ClusterEnd::StaysUp => {
                assert_eq!(
                    at(&cluster, &["Status", "State"]).as_str(),
                    Some("WAITING"),
                    "case {case:?}: nothing shut this cluster down, got {cluster}"
                );
                assert!(
                    at(&cluster, &["Status", "Timeline", "EndDateTime"]).is_null(),
                    "case {case:?}: a cluster that is still up has not ended, got {cluster}"
                );
            }
            ClusterEnd::ShutDownOnStepFailure => {
                assert_eq!(
                    at(&cluster, &["Status", "State"]).as_str(),
                    Some("TERMINATED_WITH_ERRORS"),
                    "case {case:?}: a step failure shutdown ends with errors, got {cluster}"
                );
                assert_eq!(
                    at(&cluster, &["Status", "StateChangeReason", "Code"]).as_str(),
                    Some("STEP_FAILURE"),
                    "case {case:?}: the shutdown names the modelled reason, got {cluster}"
                );
                assert!(
                    at(&cluster, &["Status", "StateChangeReason", "Message"])
                        .as_str()
                        .is_some_and(|message| !message.is_empty()),
                    "case {case:?}: the shutdown carries a message beside the code, got \
                     {cluster}"
                );
                // The terminating step is the one whose action asked for the shutdown, and
                // the cluster cannot have ended before it did.
                let terminating = row
                    .steps
                    .iter()
                    .position(|step| step.action.starts_with("TERMINATE"))
                    .and_then(|ordinal| steps.get(ordinal))
                    .unwrap_or_else(|| panic!("case {case:?}: no terminating step in {steps:?}"));
                let step_ended = instant(case, terminating, "EndDateTime");
                let cluster_ended = at(&cluster, &["Status", "Timeline", "EndDateTime"])
                    .as_f64()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a terminated cluster records its end, got {cluster}")
                    });
                assert!(
                    cluster_ended + TOLERANCE >= step_ended,
                    "case {case:?}: the cluster ended at {cluster_ended}, before the \
                     terminating step's own end {step_ended}"
                );
            }
        }
    }
}

/// THE RULE: a cluster running MORE than one step at a time does not act on a failed step's
/// `ActionOnFailure` — "if you change a cluster's `StepConcurrencyLevel` to be greater than 1
/// while a step is running … for a step that fails with this parameter set to
/// `CANCEL_AND_WAIT`, pending steps and the running step are not canceled; for a step that
/// fails with this parameter set to `TERMINATE_CLUSTER`, the cluster does not terminate"
/// (`StepConfig$ActionOnFailure` doc) — and `AddJobFlowSteps` refuses the two actions on such
/// a cluster outright: "the step is not submitted and the action fails with a message that the
/// `ActionOnFailure` setting is not valid", worded naming the action asked for.
///
/// The rows span both halves: every value the model declares, submitted to a cluster at a
/// level of one (all accepted) and to one at a level of two (the two queue-acting values
/// refused, `CONTINUE` accepted), and a launch carrying a failing `TERMINATE_CLUSTER` step at
/// a level of two, whose cluster stays up and whose following step still runs.
#[tokio::test]
async fn a_cluster_running_several_steps_at_once_does_not_act_on_action_on_failure() {
    /// The sentence the refusal is worded with, with the action it names as its one variable
    /// part — spelled out here rather than imported.
    fn refusal(action: &str) -> String {
        format!("Action on failure '{action}' is invalid.")
    }
    /// Every value `ActionOnFailure` declares, with whether a cluster running more than one
    /// step at a time refuses it. SOURCE: `StepConfig$ActionOnFailure` doc, which names
    /// `CANCEL_AND_WAIT` and `TERMINATE_CLUSTER`; `TERMINATE_JOB_FLOW` is the same shutdown
    /// under its backward-compatible name.
    const ACTIONS: [(&str, bool); 4] = [
        ("CONTINUE", false),
        ("CANCEL_AND_WAIT", true),
        ("TERMINATE_CLUSTER", true),
        ("TERMINATE_JOB_FLOW", true),
    ];

    let emr = Emr::new();
    for level in [1, 2] {
        let mut request = kept_alive_request("concurrency-actions");
        request["StepConcurrencyLevel"] = json!(level);
        let cluster_id = emr.run_job_flow(request).await;
        for (action, refused_above_one) in ACTIONS {
            let case = format!("{action} at StepConcurrencyLevel {level}");
            let (status, payload) = emr
                .call(
                    "AddJobFlowSteps",
                    json!({
                        "JobFlowId": &cluster_id,
                        "Steps": [{
                            "Name": action,
                            "ActionOnFailure": action,
                            "HadoopJarStep": {
                                "Jar": "command-runner.jar",
                                "Args": ["bash", "-c", "exit 0"],
                            },
                        }],
                    }),
                )
                .await;
            if refused_above_one && level > 1 {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the submission to be refused, got {payload}"
                );
                let expected = refusal(action);
                assert_eq!(
                    crate::refusal_messages::message(&case, &payload),
                    expected,
                    "case {case:?}: expected {expected:?}, got {payload}"
                );
            } else {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the submission to be accepted, got {payload}"
                );
            }
        }
    }

    // The launch is not the operation the refusal is documented against, so a cluster CAN
    // carry such a step at a level of two — and then the action does nothing: the cluster
    // stays up and the step behind the failing one still runs.
    let case = "a failing TERMINATE_CLUSTER step at StepConcurrencyLevel two";
    let mut request = kept_alive_request("concurrency-two-terminate");
    request["StepConcurrencyLevel"] = json!(2);
    request["Steps"] = json!([
        {
            "Name": "fails",
            "ActionOnFailure": "TERMINATE_CLUSTER",
            "HadoopJarStep": { "Jar": "command-runner.jar", "Args": ["bash", "-c", "exit 5"] },
        },
        {
            "Name": "follows",
            "ActionOnFailure": "CONTINUE",
            "HadoopJarStep": { "Jar": "command-runner.jar", "Args": ["bash", "-c", "exit 0"] },
        },
    ]);
    let cluster_id = emr.run_job_flow(request).await;
    let step_ids = queue_of(&emr, &cluster_id).await;
    let steps = drained(case, &emr, &cluster_id, &step_ids).await;
    let states: Vec<&str> = steps
        .iter()
        .map(|step| at(step, &["Status", "State"]).as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        states,
        vec!["FAILED", "COMPLETED"],
        "case {case:?}: the step behind the failing one still runs, got {steps:?}"
    );
    let cluster = emr.describe_cluster(&cluster_id).await;
    assert_eq!(
        at(&cluster, &["Status", "State"]).as_str(),
        Some("WAITING"),
        "case {case:?}: the cluster does not terminate, got {cluster}"
    );
}
