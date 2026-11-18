// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a Glue workflow RUN reports about the actions it initiated:
//! the runs each `JOB` node of its graph carries ("the information for the job runs
//! represented by the job node" — `JobNodeDetails$JobRuns`) and the counts its
//! `WorkflowRunStatistics` reports.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: a workflow run reports its actions from the job runs the store
//! holds for THAT run. Beginning a run initiates the actions of the trigger it begins at,
//! and each action naming a job begins a run of that job; a later read of the run reports
//! exactly those runs under the `JOB` node of the component they belong to, and counts each
//! of them in the statistic for the `JobRunState` it has reached by then — so the counts
//! move as the clock moves the runs, and `TotalActions` stays what the entry point
//! initiates. A node with no run of this workflow run reports no `JobDetails` at all, and a
//! run of the same job that another workflow run — or a caller's own `StartJobRun` — began
//! belongs to none of it. `GetWorkflow` reports the graph beside "the information about the
//! last execution of the workflow" (`Workflow$LastRun`), so its `JOB` nodes carry that
//! run's job runs.
//!
//! The rows differ along how many actions the entry point initiates and of which kind (one
//! job, two jobs, the same job twice, a job beside a crawler, a crawler alone), which call
//! begins the run (the entry point's own start or `StartWorkflowRun`), the entry point's
//! type, and the state the clock and a submitted stop leave the job runs in.
//!
//! A run's actions are not only its entry point's. The second half of the suite pins the GATES
//! of a run: an armed `CONDITIONAL` trigger of the same workflow "fires when its predicate is
//! satisfied" (`CreateTriggerRequest$Predicate`), and the states its conditions name are the
//! ones the job runs THAT run began have reached — so the actions it initiates are actions of
//! the same run, counted in its statistics and reported under its graph's `JOB` nodes, and it
//! fires at most once per run. Those rows differ along the `JobRunState` a condition watches,
//! the `Logical` over the conditions, how many of them the run satisfies, what each condition
//! names (the run's own upstream job, a job the run never began a run of, a crawler), and
//! whether the create armed the gate at all.
//!
//! The due instants the run lifecycle turns on are moved THROUGH THE STORE rather than
//! slept for, so each row observes the state it names and not whatever the wall clock had
//! reached.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_glue_awsglue::state::{
    JOB_RUN_STATE_FAILED, NODE_TYPE_CRAWLER, NODE_TYPE_JOB, TERMINAL_WORKFLOW_RUN_STATUSES,
    WORKFLOW_RUN_STATUS_COMPLETED,
};

use crate::job_runs::{
    already_past, emulator_with_engine, far_future, job_body, set_due_instant,
    set_workflow_due_instant, RUN_AT_REST_DEADLINE, RUN_POLL_INTERVAL,
};
use crate::wire::{call_in_region, ok, REGION};

/// The role ARN the crawler below is registered with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The crawler an action can name instead of a job. One per suite: a crawler lives in a
/// single per-account namespace, so every row's actions can name this one.
const CRAWLER: &str = "workflow-action-crawler";

/// The `CreateJob` body every job below is registered with: a concurrency ceiling high
/// enough that no row here is bounded by it, since the ceiling's own effect on a firing has
/// its own suite (`trigger_action_runs`).
fn roomy_job(name: &str) -> Value {
    job_body(
        name,
        json!({ "ExecutionProperty": { "MaxConcurrentRuns": 10 } }),
    )
}

/// Register the crawler the `Crawler` target names, so an action naming it resolves.
async fn register_crawler(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": CRAWLER,
            "Role": ROLE_ARN,
            "Targets": { "S3Targets": [{ "Path": "s3://workflow-actions/data/" }] },
        }),
    )
    .await;
}

/// What one action of a row's entry point names.
#[derive(Clone, Copy)]
enum Target {
    /// The row's job with this index, so a row can name two different jobs or the same job
    /// twice without knowing the names the suite gives them.
    Job(usize),
    /// The crawler: an action naming one initiates a crawl, which is no job run.
    Crawler,
}

/// The `Actions` list `targets` amounts to, over the row's own job names.
fn actions_of(targets: &[Target], jobs: &[String]) -> Value {
    Value::Array(
        targets
            .iter()
            .map(|target| match target {
                Target::Job(index) => json!({ "JobName": jobs[*index] }),
                Target::Crawler => json!({ "CrawlerName": CRAWLER }),
            })
            .collect(),
    )
}

/// Fire a trigger, asserting the start was served.
async fn start_trigger(router: &MultiServiceRouter, case: &str, trigger: &str) {
    let (status, body) =
        call_in_region(router, REGION, "StartTrigger", &json!({ "Name": trigger })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartTrigger must answer 200, got {status} with {body}"
    );
}

/// The `WorkflowRunId` of every run `GetWorkflowRuns` reports for `workflow`.
async fn workflow_run_ids(
    router: &MultiServiceRouter,
    case: &str,
    workflow: &str,
) -> BTreeSet<String> {
    workflow_runs(router, workflow, false)
        .await
        .into_iter()
        .map(|run| run_id_of(case, &run))
        .collect()
}

/// Every run `GetWorkflowRuns` reports for `workflow`, with the graph when asked for.
async fn workflow_runs(
    router: &MultiServiceRouter,
    workflow: &str,
    include_graph: bool,
) -> Vec<Value> {
    let page = ok(
        router,
        "GetWorkflowRuns",
        &json!({ "Name": workflow, "IncludeGraph": include_graph }),
    )
    .await;
    page.get("Runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `WorkflowRunId` a reported run carries.
fn run_id_of(case: &str, run: &Value) -> String {
    run.get("WorkflowRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a run reports its WorkflowRunId, got {run}"))
        .to_string()
}

/// The `Run` `GetWorkflowRun` reports for one run of `workflow`, with the graph when asked
/// for.
async fn workflow_run(
    router: &MultiServiceRouter,
    case: &str,
    workflow: &str,
    run_id: &str,
    include_graph: bool,
) -> Value {
    let body = ok(
        router,
        "GetWorkflowRun",
        &json!({ "Name": workflow, "RunId": run_id, "IncludeGraph": include_graph }),
    )
    .await;
    body.get("Run")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetWorkflowRun must report a Run, got {body}"))
}

/// The node of `graph` for the component `name` of type `node_type`, or `None` when the
/// graph holds no such node.
fn node_of(case: &str, graph: &Value, node_type: &str, name: &str) -> Option<Value> {
    let listed = graph
        .get("Nodes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: Graph.Nodes must be a list, got {graph}"));
    listed
        .iter()
        .find(|node| {
            node.get("Type").and_then(Value::as_str) == Some(node_type)
                && node.get("Name").and_then(Value::as_str) == Some(name)
        })
        .cloned()
}

/// The `Id` of every run the `JOB` node for `job` reports under `JobDetails.JobRuns`, and
/// an empty set when that node's `JobDetails` carries no `JobRuns` — which is what a node
/// with no run of this workflow run has to report.
fn job_node_run_ids(case: &str, graph: &Value, job: &str) -> BTreeSet<String> {
    let Some(node) = node_of(case, graph, NODE_TYPE_JOB, job) else {
        return BTreeSet::new();
    };
    let details = node
        .get("JobDetails")
        .filter(|details| !details.is_null())
        .unwrap_or_else(|| panic!("case {case:?}: a JOB node must carry JobDetails, got {node}"));
    let Some(runs) = details.get("JobRuns") else {
        return BTreeSet::new();
    };
    let runs = runs
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: JobRuns must be a list, got {node}"));
    runs.iter()
        .map(|run| {
            assert_eq!(
                run.get("JobName").and_then(Value::as_str),
                Some(job),
                "case {case:?}: a job node reports runs of its own job, got {run}"
            );
            run.get("Id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a reported run carries an Id, got {run}"))
                .to_string()
        })
        .collect()
}

/// The `Id` of every run `GetJobRuns` reports for `job` — the runs the store actually holds
/// of that job definition, which is what a graph's job node has to be reporting.
async fn stored_job_run_ids(
    router: &MultiServiceRouter,
    case: &str,
    job: &str,
) -> BTreeSet<String> {
    let page = ok(router, "GetJobRuns", &json!({ "JobName": job })).await;
    page.get("JobRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|run| {
            run.get("Id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a run carries an Id, got {run}"))
                .to_string()
        })
        .collect()
}

/// Every counter of a run's `Statistics`, so a row can name the ones it expects and have
/// every other one asserted at the model's own default of 0.
const STATISTICS_COUNTERS: [&str; 8] = [
    "TotalActions",
    "RunningActions",
    "SucceededActions",
    "FailedActions",
    "StoppedActions",
    "TimeoutActions",
    "ErroredActions",
    "WaitingActions",
];

/// Assert that the `Statistics` of `run` are exactly `expected`, with every counter
/// `expected` does not name at 0.
fn assert_statistics(case: &str, run: &Value, expected: &BTreeMap<&str, i64>) {
    let statistics = run
        .get("Statistics")
        .unwrap_or_else(|| panic!("case {case:?}: a run reports its Statistics, got {run}"));
    for counter in STATISTICS_COUNTERS {
        let want = expected.get(counter).copied().unwrap_or(0);
        let got = statistics.get(counter).and_then(Value::as_i64).unwrap_or(0);
        assert_eq!(
            got, want,
            "case {case:?}: expected {counter} {want}, got {got} in {statistics}"
        );
    }
}

/// The expectation map a row builds from pairs, so a row names only the counters it moves.
fn counters(pairs: &[(&'static str, i64)]) -> BTreeMap<&'static str, i64> {
    pairs.iter().copied().collect()
}

// ---------------------------------------------------------------------------
// What a run's graph reports
// ---------------------------------------------------------------------------

/// One row of the action table.
struct ActionCase {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The `CreateTrigger` members that decide WHEN the entry point fires: its `Type` and
    /// whatever that type requires.
    firing: Value,
    /// The actions the entry point initiates.
    targets: Vec<Target>,
    /// Per job index, how many runs of that job the workflow run's `JOB` node must report.
    job_runs: Vec<(usize, usize)>,
    /// Whether the run under test is begun by starting the entry point (which fires an
    /// `ON_DEMAND` trigger) or by `StartWorkflowRun` at it.
    by_trigger_start: bool,
}

/// The rows the suites below drive. A `SCHEDULED` entry point carries the schedule its type
/// requires and an `EVENT` one its batching condition, so every row is a definition
/// `CreateTrigger` accepts.
fn action_cases() -> Vec<ActionCase> {
    vec![
        ActionCase {
            // The recorded shape: an on-demand entry point running one job.
            case: "an on-demand entry point running one job",
            firing: json!({ "Type": "ON_DEMAND" }),
            targets: vec![Target::Job(0)],
            job_runs: vec![(0, 1), (1, 0)],
            by_trigger_start: true,
        },
        ActionCase {
            case: "two actions naming different jobs",
            firing: json!({ "Type": "ON_DEMAND" }),
            targets: vec![Target::Job(0), Target::Job(1)],
            job_runs: vec![(0, 1), (1, 1)],
            by_trigger_start: true,
        },
        ActionCase {
            // An action is what is initiated, not a job: two actions naming one job begin
            // two runs of it, and the node reports both.
            case: "two actions naming one job",
            firing: json!({ "Type": "ON_DEMAND" }),
            targets: vec![Target::Job(0), Target::Job(0)],
            job_runs: vec![(0, 2), (1, 0)],
            by_trigger_start: true,
        },
        ActionCase {
            case: "a job beside a crawler",
            firing: json!({ "Type": "ON_DEMAND" }),
            targets: vec![Target::Job(0), Target::Crawler],
            job_runs: vec![(0, 1), (1, 0)],
            by_trigger_start: true,
        },
        ActionCase {
            case: "a crawler alone, which begins no job run",
            firing: json!({ "Type": "ON_DEMAND" }),
            targets: vec![Target::Crawler],
            job_runs: vec![(0, 0), (1, 0)],
            by_trigger_start: true,
        },
        ActionCase {
            case: "a run started at an on-demand entry point",
            firing: json!({ "Type": "ON_DEMAND" }),
            targets: vec![Target::Job(1)],
            job_runs: vec![(0, 0), (1, 1)],
            by_trigger_start: false,
        },
        ActionCase {
            case: "a run started at a scheduled entry point",
            firing: json!({ "Type": "SCHEDULED", "Schedule": "cron(0 */6 * * ? *)" }),
            targets: vec![Target::Job(0), Target::Job(1)],
            job_runs: vec![(0, 1), (1, 1)],
            by_trigger_start: false,
        },
        ActionCase {
            case: "a run started at an event entry point",
            firing: json!({ "Type": "EVENT", "EventBatchingCondition": { "BatchSize": 2 } }),
            targets: vec![Target::Job(0), Target::Crawler],
            job_runs: vec![(0, 1), (1, 0)],
            by_trigger_start: false,
        },
    ]
}

/// Register a row's workflow, its two jobs and its entry point, begin the run the row names
/// and answer `(workflow, jobs, run id)`.
async fn begin_row(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    index: usize,
    row: &ActionCase,
) -> (String, Vec<String>, String) {
    let workflow = format!("wf-actions-{index}");
    ok(router, "CreateWorkflow", &json!({ "Name": workflow })).await;
    let jobs: Vec<String> = (0..2).map(|n| format!("wf-action-job-{index}-{n}")).collect();
    for job in &jobs {
        ok(router, "CreateJob", &roomy_job(job)).await;
    }

    let trigger = format!("wf-actions-trigger-{index}");
    let mut body = row.firing.clone();
    body["Name"] = json!(trigger);
    body["WorkflowName"] = json!(workflow.clone());
    body["Actions"] = actions_of(&row.targets, &jobs);
    ok(router, "CreateTrigger", &body).await;

    let run_id = if row.by_trigger_start {
        start_trigger(router, row.case, &trigger).await;
        let begun = workflow_run_ids(router, row.case, &workflow).await;
        assert_eq!(
            begun.len(),
            1,
            "case {:?}: the firing must begin exactly one workflow run, got {begun:?}",
            row.case
        );
        begun.into_iter().next().expect("one run")
    } else {
        let started = ok(router, "StartWorkflowRun", &json!({ "Name": workflow })).await;
        started
            .get("RunId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: StartWorkflowRun must answer a RunId, got {started}",
                    row.case
                )
            })
            .to_string()
    };

    // Hold every run the actions began in flight, so this row observes the state it names.
    for job in &jobs {
        set_due_instant(engine, job, far_future());
    }
    (workflow, jobs, run_id)
}

/// The `JOB` node of a run's graph reports exactly the runs that run's actions began of
/// that job, and a node with none of them reports no `JobDetails`.
#[tokio::test]
async fn a_runs_graph_reports_the_job_runs_its_actions_began() {
    let (router, engine) = emulator_with_engine();
    register_crawler(&router).await;

    for (index, row) in action_cases().iter().enumerate() {
        let case = row.case;
        let (workflow, jobs, run_id) = begin_row(&router, &engine, index, row).await;
        let run = workflow_run(&router, case, &workflow, &run_id, true).await;
        let graph = run
            .get("Graph")
            .unwrap_or_else(|| panic!("case {case:?}: IncludeGraph must report a Graph, got {run}"));

        for (job_index, expected) in &row.job_runs {
            let job = &jobs[*job_index];
            let reported = job_node_run_ids(case, graph, job);
            assert_eq!(
                reported.len(),
                *expected,
                "case {case:?}: the JOB node for {job:?} must report {expected} run(s), got \
                 {reported:?}"
            );
            // The ids are the runs the store actually holds of that job — not ids the
            // projection made up — so the node round-trips to `GetJobRuns`.
            let stored = stored_job_run_ids(&router, case, job).await;
            assert_eq!(
                reported, stored,
                "case {case:?}: the JOB node for {job:?} must report the stored runs \
                 {stored:?}, got {reported:?}"
            );
        }

        // A crawler initiates a crawl, never a job run, so its node carries no job details
        // however many jobs the same entry point runs.
        if let Some(node) = node_of(case, graph, NODE_TYPE_CRAWLER, CRAWLER) {
            assert!(
                node.get("JobDetails").is_none(),
                "case {case:?}: a crawler node reports no JobDetails, got {node}"
            );
        }
    }
}

/// A run's statistics count its actions by the state each one's job run has reached: the
/// total is what the entry point initiates and every other counter follows the runs.
#[tokio::test]
async fn a_runs_statistics_count_its_actions_by_their_job_run_state() {
    let (router, engine) = emulator_with_engine();
    register_crawler(&router).await;

    for (index, row) in action_cases().iter().enumerate() {
        let case = row.case;
        let (workflow, _jobs, run_id) = begin_row(&router, &engine, index, row).await;
        let begun: usize = row.job_runs.iter().map(|(_, count)| *count).sum();
        let total = i64::try_from(row.targets.len()).expect("a row names few actions");

        let run = workflow_run(&router, case, &workflow, &run_id, false).await;
        // Every run the actions began is still in flight, so each counts as an action in
        // running state and none has finished.
        assert_statistics(
            case,
            &run,
            &counters(&[
                ("TotalActions", total),
                (
                    "RunningActions",
                    i64::try_from(begun).expect("a row begins few runs"),
                ),
            ]),
        );
    }
}

/// The counters a row of the lifecycle table moves, as the state its job runs come to rest
/// in.
#[tokio::test]
async fn a_runs_statistics_move_as_the_clock_moves_its_job_runs() {
    // (case, whether a stop is submitted for the runs, whether the owed transition is made
    //  due, the counter the two runs must land in)
    let cases: [(&str, bool, bool, &str); 4] = [
        ("still in flight", false, false, "RunningActions"),
        (
            // The jobs here point their script at an object no Amazon S3 serves, so the
            // state a run nobody stopped comes to rest in is the modelled failure state.
            "the clock has finished them",
            false,
            true,
            "FailedActions",
        ),
        ("a stop submitted but not finished", true, false, "RunningActions"),
        ("the stop has finished them", true, true, "StoppedActions"),
    ];

    for (index, (case, stop, due, counter)) in cases.into_iter().enumerate() {
        let (router, engine) = emulator_with_engine();
        let workflow = format!("wf-lifecycle-{index}");
        let job = format!("wf-lifecycle-job-{index}");
        ok(&router, "CreateWorkflow", &json!({ "Name": workflow })).await;
        ok(&router, "CreateJob", &roomy_job(&job)).await;
        ok(
            &router,
            "CreateTrigger",
            &json!({
                "Name": format!("wf-lifecycle-trigger-{index}"),
                "WorkflowName": workflow,
                "Type": "ON_DEMAND",
                "Actions": [{ "JobName": job }, { "JobName": job }],
            }),
        )
        .await;
        start_trigger(&router, case, &format!("wf-lifecycle-trigger-{index}")).await;
        set_due_instant(&engine, &job, far_future());

        if stop {
            let run_ids: Vec<String> = stored_job_run_ids(&router, case, &job)
                .await
                .into_iter()
                .collect();
            ok(
                &router,
                "BatchStopJobRun",
                &json!({ "JobName": job, "JobRunIds": run_ids }),
            )
            .await;
            // The stop is owed its own transition, which this row holds open or makes due.
            set_due_instant(&engine, &job, far_future());
        }
        if due {
            // Each run the actions began is read back until it HAS come to rest: what a run
            // comes to rest in is what its own execution came to — performed past the call
            // that began the run — or the stop a client submitted for it.
            crate::job_runs::runs_at_rest(&router, &engine, case, &job).await;
        }

        let runs = workflow_runs(&router, &workflow, false).await;
        assert_eq!(
            runs.len(),
            1,
            "case {case:?}: the firing must begin exactly one workflow run, got {runs:?}"
        );
        assert_statistics(
            case,
            &runs[0],
            &counters(&[("TotalActions", 2), (counter, 2)]),
        );
    }
}

/// A `JOB` node reports only the runs of the workflow run being read: a run of the same job
/// that another workflow run began, or that a caller started directly, belongs to neither.
#[tokio::test]
async fn a_job_node_reports_only_the_runs_of_the_run_being_read() {
    const WORKFLOW: &str = "wf-isolation";
    const JOB: &str = "wf-isolation-job";
    const TRIGGER: &str = "wf-isolation-trigger";
    let case = "two runs of one workflow beside a direct start";

    let (router, engine) = emulator_with_engine();
    ok(&router, "CreateWorkflow", &json!({ "Name": WORKFLOW })).await;
    ok(&router, "CreateJob", &roomy_job(JOB)).await;
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": TRIGGER,
            "WorkflowName": WORKFLOW,
            "Type": "ON_DEMAND",
            "Actions": [{ "JobName": JOB }],
        }),
    )
    .await;

    // A run of the job nothing about this workflow began.
    let direct = ok(&router, "StartJobRun", &json!({ "JobName": JOB })).await;
    let direct_run = direct
        .get("JobRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId, got {direct}"))
        .to_string();
    set_due_instant(&engine, JOB, far_future());

    start_trigger(&router, case, TRIGGER).await;
    set_due_instant(&engine, JOB, far_future());
    let after_first = workflow_run_ids(&router, case, WORKFLOW).await;
    start_trigger(&router, case, TRIGGER).await;
    set_due_instant(&engine, JOB, far_future());
    let after_second = workflow_run_ids(&router, case, WORKFLOW).await;

    assert_eq!(
        after_first.len(),
        1,
        "case {case:?}: the first firing begins one run, got {after_first:?}"
    );
    assert_eq!(
        after_second.len(),
        2,
        "case {case:?}: the second firing begins a second run, got {after_second:?}"
    );

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for run_id in &after_second {
        let run = workflow_run(&router, case, WORKFLOW, run_id, true).await;
        let graph = run
            .get("Graph")
            .unwrap_or_else(|| panic!("case {case:?}: IncludeGraph must report a Graph, got {run}"));
        let reported = job_node_run_ids(case, graph, JOB);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: run {run_id:?} began exactly one job run, got {reported:?}"
        );
        assert!(
            !reported.contains(&direct_run),
            "case {case:?}: run {run_id:?} must not report the directly started run \
             {direct_run:?}, got {reported:?}"
        );
        for id in reported {
            assert!(
                seen.insert(id.clone()),
                "case {case:?}: job run {id:?} is reported by two workflow runs"
            );
        }
    }
    // The store holds three runs of that job: one per firing plus the direct start, and only
    // the first two belong to a workflow run.
    let stored = stored_job_run_ids(&router, case, JOB).await;
    assert_eq!(
        stored.len(),
        3,
        "case {case:?}: the job must hold three runs, got {stored:?}"
    );
    assert!(
        seen.is_subset(&stored) && !seen.contains(&direct_run),
        "case {case:?}: the runs the workflow runs report must be stored runs other than \
         {direct_run:?}, got {seen:?} of {stored:?}"
    );
}

/// `GetWorkflow` reports its graph beside the workflow's LAST execution, so the graph's
/// `JOB` nodes carry that run's job runs and a workflow no run has been started of reports
/// none at all.
#[tokio::test]
async fn a_workflow_read_reports_its_last_executions_job_runs() {
    const WORKFLOW: &str = "wf-last-run";
    const JOB: &str = "wf-last-run-job";
    const TRIGGER: &str = "wf-last-run-trigger";
    let case = "the workflow's last execution";

    let (router, engine) = emulator_with_engine();
    ok(&router, "CreateWorkflow", &json!({ "Name": WORKFLOW })).await;
    ok(&router, "CreateJob", &roomy_job(JOB)).await;
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": TRIGGER,
            "WorkflowName": WORKFLOW,
            "Type": "ON_DEMAND",
            "Actions": [{ "JobName": JOB }],
        }),
    )
    .await;

    // Before any run, the job node of the workflow's graph has no run to report.
    let before = ok(
        &router,
        "GetWorkflow",
        &json!({ "Name": WORKFLOW, "IncludeGraph": true }),
    )
    .await;
    let before_graph = before["Workflow"]
        .get("Graph")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: IncludeGraph must report a Graph, got {before}"));
    assert!(
        job_node_run_ids(case, &before_graph, JOB).is_empty(),
        "case {case:?}: a workflow nothing has run reports no job runs, got {before_graph}"
    );

    start_trigger(&router, case, TRIGGER).await;
    set_due_instant(&engine, JOB, far_future());
    let after_first = workflow_run_ids(&router, case, WORKFLOW).await;
    start_trigger(&router, case, TRIGGER).await;
    set_due_instant(&engine, JOB, far_future());
    let after_second = workflow_run_ids(&router, case, WORKFLOW).await;
    let last_run_id = after_second
        .difference(&after_first)
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the second firing begins a further run"))
        .clone();

    let read = ok(
        &router,
        "GetWorkflow",
        &json!({ "Name": WORKFLOW, "IncludeGraph": true }),
    )
    .await;
    let workflow = &read["Workflow"];
    assert_eq!(
        workflow
            .get("LastRun")
            .and_then(|run| run.get("WorkflowRunId"))
            .and_then(Value::as_str),
        Some(last_run_id.as_str()),
        "case {case:?}: LastRun must be the most recently begun run, got {workflow}"
    );
    let graph = workflow
        .get("Graph")
        .unwrap_or_else(|| panic!("case {case:?}: IncludeGraph must report a Graph, got {read}"));
    // The graph reports the last execution's job run, which is the one that run's own read
    // reports — and not the earlier execution's.
    let from_graph = job_node_run_ids(case, graph, JOB);
    let last_run = workflow_run(&router, case, WORKFLOW, &last_run_id, true).await;
    let from_run = job_node_run_ids(
        case,
        last_run
            .get("Graph")
            .unwrap_or_else(|| panic!("case {case:?}: the run read reports a Graph")),
        JOB,
    );
    assert_eq!(
        from_graph, from_run,
        "case {case:?}: the workflow's graph reports its last execution's job runs \
         {from_run:?}, got {from_graph:?}"
    );
    assert_eq!(
        from_graph.len(),
        1,
        "case {case:?}: that execution began one job run, got {from_graph:?}"
    );
    // And the last run's statistics count that one action, not both executions'.
    assert_statistics(
        case,
        workflow.get("LastRun").expect("LastRun was asserted above"),
        &counters(&[("TotalActions", 1), ("RunningActions", 1)]),
    );
}

// ---------------------------------------------------------------------------
// The GATES of a run: what a satisfied CONDITIONAL trigger adds to it
// ---------------------------------------------------------------------------

/// The job this suite's gates can watch that no workflow run here ever begins a run of, so a
/// condition naming it is one the run itself does not satisfy. One per suite: a job definition
/// is addressed by name inside one Region.
const STRANGER: &str = "workflow-gate-stranger-job";

/// What one condition of a gate waits on.
#[derive(Clone, Copy)]
enum Watched {
    /// The row's own upstream job: the action its entry point initiates.
    Upstream,
    /// [`STRANGER`] — a job this workflow run began no run of, though the account holds runs
    /// of it.
    Stranger,
    /// The crawler: a condition on one waits on a crawl, which is no job run.
    Crawler,
}

/// One row of the gate table: the predicate the workflow's `CONDITIONAL` trigger carries,
/// whether the create arms it, and whether the states the run's own job runs reach satisfy it.
///
/// Every job of this suite points its `ScriptLocation` at an object no Amazon S3 serves (Glue
/// is mounted on its own), so every run of one comes to rest in the modelled failure state —
/// which is why the state a row's conditions are satisfied by is that one, and why a condition
/// on any other state is one this run never reaches.
struct GateCase {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The predicate's `Logical`, or `None` for a predicate that names none.
    logical: Option<&'static str>,
    /// Its conditions, as `(what the condition names, the `JobRunState` it watches)`.
    conditions: &'static [(Watched, &'static str)],
    /// The gate's own `StartOnCreation`.
    armed: bool,
    /// Whether the gate fires inside the run.
    fires: bool,
}

/// The rows the gate suites drive.
fn gate_cases() -> Vec<GateCase> {
    vec![
        GateCase {
            // The recorded shape, in the terminal state this suite's runs reach: one
            // condition on the upstream job's own state.
            case: "one condition on the upstream job's state, under ANY",
            logical: Some("ANY"),
            conditions: &[(Watched::Upstream, JOB_RUN_STATE_FAILED)],
            armed: true,
            fires: true,
        },
        GateCase {
            // "An optional field if only one condition is listed": a predicate naming no
            // Logical at all is the same gate.
            case: "one condition and no Logical at all",
            logical: None,
            conditions: &[(Watched::Upstream, JOB_RUN_STATE_FAILED)],
            armed: true,
            fires: true,
        },
        GateCase {
            // The state a condition watches is part of it: a state the run never reaches
            // satisfies nothing.
            case: "a condition on SUCCEEDED the upstream never reaches",
            logical: Some("ANY"),
            conditions: &[(Watched::Upstream, "SUCCEEDED")],
            armed: true,
            fires: false,
        },
        GateCase {
            case: "a condition on TIMEOUT the upstream never reaches",
            logical: Some("ANY"),
            conditions: &[(Watched::Upstream, "TIMEOUT")],
            armed: true,
            fires: false,
        },
        GateCase {
            // "Currently, the only job states that a trigger can listen for are SUCCEEDED,
            // STOPPED, FAILED, and TIMEOUT" (`Condition$State`): RUNNING is not one of them,
            // so a condition naming it fires nothing even though a run passes through it.
            case: "a condition on a state no trigger can listen for",
            logical: Some("ANY"),
            conditions: &[(Watched::Upstream, "RUNNING")],
            armed: true,
            fires: false,
        },
        GateCase {
            // The conditions are about the runs THIS workflow run began: the account holds a
            // failed run of the stranger job, and this run began none of it.
            case: "AND over the upstream and a job this run began no run of",
            logical: Some("AND"),
            conditions: &[
                (Watched::Upstream, JOB_RUN_STATE_FAILED),
                (Watched::Stranger, JOB_RUN_STATE_FAILED),
            ],
            armed: true,
            fires: false,
        },
        GateCase {
            // Under ANY the upstream condition alone is enough.
            case: "ANY over the upstream and a job this run began no run of",
            logical: Some("ANY"),
            conditions: &[
                (Watched::Upstream, JOB_RUN_STATE_FAILED),
                (Watched::Stranger, JOB_RUN_STATE_FAILED),
            ],
            armed: true,
            fires: true,
        },
        GateCase {
            // A condition on a CRAWLER waits on a crawl, which is no job run, so it holds an
            // AND predicate closed.
            case: "AND over the upstream and a crawler",
            logical: Some("AND"),
            conditions: &[
                (Watched::Upstream, JOB_RUN_STATE_FAILED),
                (Watched::Crawler, "SUCCEEDED"),
            ],
            armed: true,
            fires: false,
        },
        GateCase {
            // "Set to true to start SCHEDULED and CONDITIONAL triggers when created": a gate
            // nothing armed is watching nothing.
            case: "a satisfied predicate on a gate the create did not arm",
            logical: Some("ANY"),
            conditions: &[(Watched::Upstream, JOB_RUN_STATE_FAILED)],
            armed: false,
            fires: false,
        },
    ]
}

impl GateCase {
    /// How many actions the run must report in total: the entry point's one, plus the gate's
    /// own where the gate fired.
    fn total_actions(&self) -> i64 {
        1 + i64::from(self.fires)
    }

    /// How many of them must have come to rest in the failure state: every run this suite's
    /// jobs begin does, since none of their scripts can be read.
    fn failed_actions(&self) -> i64 {
        self.total_actions()
    }
}

/// Register the job a gate can watch that no workflow run of this suite runs, and drive one
/// run of it to rest — so a condition naming it is unsatisfied because THIS run began no run
/// of it, and not because the account holds none.
async fn register_stranger(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
) {
    ok(router, "CreateJob", &roomy_job(STRANGER)).await;
    ok(router, "StartJobRun", &json!({ "JobName": STRANGER })).await;
    let runs = crate::job_runs::runs_at_rest(router, engine, case, STRANGER).await;
    assert_eq!(
        runs.iter()
            .filter_map(|run| run.get("JobRunState").and_then(Value::as_str))
            .collect::<Vec<&str>>(),
        vec![JOB_RUN_STATE_FAILED],
        "setup {case:?}: the stranger job's own run must come to rest in the failure state, \
         got {runs:?}"
    );
}

/// Lay out one gate row: its upstream job, its gated job, the workflow, the `ON_DEMAND` entry
/// point over the upstream job and the `CONDITIONAL` gate over the row's predicate. Answers
/// `(workflow, upstream job, gated job)`.
async fn lay_out_gate(
    router: &MultiServiceRouter,
    index: usize,
    row: &GateCase,
) -> (String, String, String) {
    let workflow = format!("wf-gate-{index}");
    let upstream = format!("wf-gate-upstream-{index}");
    let gated = format!("wf-gate-gated-{index}");
    ok(router, "CreateWorkflow", &json!({ "Name": workflow })).await;
    ok(router, "CreateJob", &roomy_job(&upstream)).await;
    ok(router, "CreateJob", &roomy_job(&gated)).await;
    ok(
        router,
        "CreateTrigger",
        &json!({
            "Name": format!("{workflow}-entry"),
            "Type": "ON_DEMAND",
            "WorkflowName": workflow,
            "Actions": [{ "JobName": upstream }],
        }),
    )
    .await;

    let mut predicate = json!({
        "Conditions": row
            .conditions
            .iter()
            .map(|(watched, state)| match watched {
                Watched::Upstream => json!({
                    "LogicalOperator": "EQUALS",
                    "JobName": upstream,
                    "State": state,
                }),
                Watched::Stranger => json!({
                    "LogicalOperator": "EQUALS",
                    "JobName": STRANGER,
                    "State": state,
                }),
                Watched::Crawler => json!({
                    "LogicalOperator": "EQUALS",
                    "CrawlerName": CRAWLER,
                    "CrawlState": state,
                }),
            })
            .collect::<Vec<Value>>(),
    });
    if let Some(logical) = row.logical {
        predicate["Logical"] = json!(logical);
    }
    ok(
        router,
        "CreateTrigger",
        &json!({
            "Name": format!("{workflow}-gate"),
            "Type": "CONDITIONAL",
            "WorkflowName": workflow,
            "StartOnCreation": row.armed,
            "Predicate": predicate,
            "Actions": [{ "JobName": gated }],
        }),
    )
    .await;

    (workflow, upstream, gated)
}

/// Read `run_id` of `workflow` back until it has come to REST, and answer the `Run`
/// `GetWorkflowRun` reports then, with its graph.
///
/// A workflow run is a COMPOSITE: it comes to rest only once every action it initiated has —
/// the actions a gate opened inside it included — and each of those owes its script one
/// execution performed by whichever call finds it owed. So the CONDITION is polled for, with
/// the owed instants made due through the store on every turn rather than slept out, against
/// the same generous window [`crate::job_runs::run_at_rest`] gives one job run.
async fn gated_run_at_rest(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    workflow: &str,
    run_id: &str,
) -> Value {
    let deadline = SystemTime::now() + RUN_AT_REST_DEADLINE;
    loop {
        set_workflow_due_instant(engine, workflow, already_past());
        let run = workflow_run(router, case, workflow, run_id, true).await;
        let status = run
            .get("Status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if TERMINAL_WORKFLOW_RUN_STATUSES.contains(&status.as_str()) {
            return run;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: run {run_id:?} of workflow {workflow:?} must come to rest, and it \
             reports {run}"
        );
        tokio::time::sleep(RUN_POLL_INTERVAL).await;
    }
}

/// A gate whose predicate the run's own job runs satisfy initiates its actions INSIDE that
/// run, and one whose predicate they do not satisfy initiates nothing — over the table above.
#[tokio::test]
async fn a_satisfied_gate_initiates_its_actions_inside_the_run_that_satisfied_it() {
    let (router, engine) = emulator_with_engine();
    register_crawler(&router).await;
    register_stranger(&router, &engine, "the stranger job").await;

    for (index, row) in gate_cases().iter().enumerate() {
        let case = row.case;
        let (workflow, upstream, gated) = lay_out_gate(&router, index, row).await;
        let started = ok(&router, "StartWorkflowRun", &json!({ "Name": workflow })).await;
        let run_id = started
            .get("RunId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartWorkflowRun must answer a RunId, got {started}")
            })
            .to_string();

        let run = gated_run_at_rest(&router, &engine, case, &workflow, &run_id).await;
        // "COMPLETED" names a run whose actions have run their course, whatever each came to.
        assert_eq!(
            run.get("Status").and_then(Value::as_str),
            Some(WORKFLOW_RUN_STATUS_COMPLETED),
            "case {case:?}: the run must come to rest completed, got {run}"
        );
        assert_statistics(
            case,
            &run,
            &counters(&[
                ("TotalActions", row.total_actions()),
                ("FailedActions", row.failed_actions()),
            ]),
        );

        let graph = run
            .get("Graph")
            .unwrap_or_else(|| panic!("case {case:?}: IncludeGraph must report a Graph, got {run}"));
        assert_eq!(
            job_node_run_ids(case, graph, &upstream).len(),
            1,
            "case {case:?}: the upstream node must report the one run the entry point began, \
             got {graph}"
        );
        let reported = job_node_run_ids(case, graph, &gated);
        let expected = usize::from(row.fires);
        assert_eq!(
            reported.len(),
            expected,
            "case {case:?}: the gated node must report {expected} run(s), got {reported:?}"
        );
        // Whatever the node reports are the runs the store really holds of that job: a gate
        // that did not fire began none at all.
        assert_eq!(
            reported,
            stored_job_run_ids(&router, case, &gated).await,
            "case {case:?}: the gated node must report the stored runs of {gated:?}"
        );
        // A condition naming a job this run began no run of is unsatisfied even though the
        // account holds a run of that job in the state it watches.
        assert_eq!(
            job_node_run_ids(case, graph, STRANGER).len(),
            0,
            "case {case:?}: no run of the stranger job belongs to this workflow run, got \
             {graph}"
        );
    }
}

/// A gate fires ONCE per run: re-reading a run that has come to rest initiates nothing
/// further, and a second run of the same workflow passes the same gate once of its own.
#[tokio::test]
async fn a_gate_fires_once_per_run_however_often_the_run_is_read() {
    let case = "a gate read again after its run finished";
    let (router, engine) = emulator_with_engine();
    register_crawler(&router).await;
    register_stranger(&router, &engine, case).await;

    let row = GateCase {
        case,
        logical: Some("ANY"),
        conditions: &[(Watched::Upstream, JOB_RUN_STATE_FAILED)],
        armed: true,
        fires: true,
    };
    let (workflow, _upstream, gated) = lay_out_gate(&router, 900, &row).await;

    let mut begun: Vec<String> = Vec::new();
    for firing in 1..=2 {
        let started = ok(&router, "StartWorkflowRun", &json!({ "Name": workflow })).await;
        let run_id = started
            .get("RunId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartWorkflowRun must answer a RunId, got {started}")
            })
            .to_string();
        let run = gated_run_at_rest(&router, &engine, case, &workflow, &run_id).await;
        assert_statistics(
            case,
            &run,
            &counters(&[("TotalActions", 2), ("FailedActions", 2)]),
        );

        // Reading a finished run is a REPORT: it initiates nothing, so the counts and the one
        // gated run it reports do not move however often it is read.
        for turn in 0..3 {
            let again = workflow_run(&router, case, &workflow, &run_id, true).await;
            assert_statistics(
                case,
                &again,
                &counters(&[("TotalActions", 2), ("FailedActions", 2)]),
            );
            let graph = again
                .get("Graph")
                .unwrap_or_else(|| panic!("case {case:?}: the run read reports a Graph"));
            let reported = job_node_run_ids(case, graph, &gated);
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: read {turn} of firing {firing} must report the one gated run, \
                 got {reported:?}"
            );
            begun.extend(reported);
        }
    }

    // Two runs of the workflow passed the gate once each, so the gated job holds exactly two
    // runs in all — one per workflow run, and none per read.
    let distinct: BTreeSet<String> = begun.into_iter().collect();
    assert_eq!(
        distinct.len(),
        2,
        "case {case:?}: each of the two runs fires the gate once, got {distinct:?}"
    );
    assert_eq!(
        stored_job_run_ids(&router, case, &gated).await,
        distinct,
        "case {case:?}: the store holds exactly the runs the two workflow runs reported"
    );
}

