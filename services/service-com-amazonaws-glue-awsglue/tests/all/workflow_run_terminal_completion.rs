// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the terminal `WorkflowRunStatus` a Glue workflow run comes to rest
//! in, and the `WorkflowRunStatistics` reported beside it, once every action the run
//! initiated has run its course having SUCCEEDED.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and every job
//! action's script is staged in the emulator's own Amazon S3 and really executed, so the
//! premise of the rule — a run with nothing errored, failed, stopped or timed out in it —
//! holds of the job runs the store actually holds rather than by assumption.
//!
//! The rule under test: a workflow run's terminal status is DERIVED from what the actions in
//! its graph came to. A run none of whose actions errored, failed, stopped or timed out comes
//! to rest `COMPLETED`, with `SucceededActions` equal to its `TotalActions`, every failure
//! counter at 0 and no `ErrorMessage` — `ERROR` is the status of a run that errored, and
//! `WorkflowRun$ErrorMessage` "describes any error that may have occurred in starting the
//! workflow run", so a run that went wrong at nothing reports neither. That settled run is
//! what every reader of it projects.
//!
//! The rows differ along which call begins the run (firing the workflow's own `ON_DEMAND`
//! entry point with `StartTrigger`, or `StartWorkflowRun` on the workflow), how many job
//! actions the entry point initiates, the `JobCommand` kind each of those jobs carries
//! (`pythonshell`, `glueetl`), what each script does, and whether the read asks for the
//! workflow graph at all.
//!
//! The due instants the lifecycle turns on are made due THROUGH THE STORE and polled for
//! rather than slept out, so no row depends on how fast the host runs.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    JOB_RUN_STATE_SUCCEEDED, NODE_TYPE_JOB, TERMINAL_WORKFLOW_RUN_STATUSES,
    WORKFLOW_RUN_STATUS_COMPLETED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{
    already_past, set_workflow_due_instant, RUN_AT_REST_DEADLINE, RUN_POLL_INTERVAL,
};
use crate::job_script_execution::{create_bucket, emulator_with_s3, stage_script, ROLE_ARN};
use crate::wire::ok;

/// A script that exits cleanly, so the run of the job it belongs to comes to rest having
/// SUCCEEDED.
const EXITS_CLEANLY: &str = "total = sum([1, 2, 3])\n";

/// A script that reads its own command line the way any Python program does and still exits
/// cleanly.
const READS_ITS_COMMAND_LINE: &str = "import sys\n\nnamed = list(sys.argv)\n";

/// A script that computes over a list and exits cleanly.
const COMPUTES_OVER_A_LIST: &str = "doubled = [n * 2 for n in range(4)]\ntotal = sum(doubled)\n";

/// Every counter of a run's `WorkflowRunStatistics` (`smithy_query query="shape"
/// shape="WorkflowRunStatistics"`), so a row can name the ones it expects and have every
/// other asserted at the model's own default of 0.
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

/// Which call begins the run a row reads back.
#[derive(Clone, Copy, Debug)]
enum Begun {
    /// `StartTrigger` on the workflow's own `ON_DEMAND` entry point, which FIRES it: the
    /// trigger initiates its actions and begins a run of the workflow it belongs to.
    FiringTheTrigger,
    /// `StartWorkflowRun` on the workflow, which begins a run at the same entry point.
    StartingTheWorkflow,
}

/// What a row's read asks for with `IncludeGraph` ("specifies whether to include the
/// workflow graph in response or not").
#[derive(Clone, Copy, Debug)]
enum Graph {
    /// `IncludeGraph: true`, so the run reports its graph and the node of each job action
    /// reports the run that action began.
    Asked,
    /// `IncludeGraph: false`.
    Declined,
    /// No `IncludeGraph` member at all.
    Omitted,
}

/// One row: a workflow whose entry point initiates job actions that all succeed, and how the
/// run of it is begun and read back.
#[derive(Debug)]
struct Row {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The call that begins the run.
    begun: Begun,
    /// What the read asks for with `IncludeGraph`.
    graph: Graph,
    /// One `(JobCommand$Name, script source)` per JOB action the entry point initiates.
    /// Every script exits cleanly, so no action of the run errors, fails, stops or times
    /// out.
    actions: &'static [(&'static str, &'static str)],
}

/// The rows the suite drives: one per axis the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one pythonshell action, fired by StartTrigger, read with the graph",
            begun: Begun::FiringTheTrigger,
            graph: Graph::Asked,
            actions: &[("pythonshell", EXITS_CLEANLY)],
        },
        Row {
            case: "one pythonshell action, begun by StartWorkflowRun, read with the graph",
            begun: Begun::StartingTheWorkflow,
            graph: Graph::Asked,
            actions: &[("pythonshell", EXITS_CLEANLY)],
        },
        Row {
            case: "two actions of different command kinds, fired by StartTrigger",
            begun: Begun::FiringTheTrigger,
            graph: Graph::Declined,
            actions: &[
                ("pythonshell", READS_ITS_COMMAND_LINE),
                ("glueetl", EXITS_CLEANLY),
            ],
        },
        Row {
            case: "three actions, begun by StartWorkflowRun, read without IncludeGraph",
            begun: Begun::StartingTheWorkflow,
            graph: Graph::Omitted,
            actions: &[
                ("glueetl", EXITS_CLEANLY),
                ("pythonshell", COMPUTES_OVER_A_LIST),
                ("glueetl", READS_ITS_COMMAND_LINE),
            ],
        },
        Row {
            case: "one glueetl action computing over a list, fired by StartTrigger",
            begun: Begun::FiringTheTrigger,
            graph: Graph::Asked,
            actions: &[("glueetl", COMPUTES_OVER_A_LIST)],
        },
    ]
}

/// The `CreateJob` body for one action: the job's command names the kind the row asked for
/// and the script staged for it, with room for this row's one run of it.
fn job_body(name: &str, command_name: &str, location: &str) -> Value {
    let mut command = json!({ "Name": command_name, "ScriptLocation": location });
    // "When you specify a Python shell job (JobCommand.Name='pythonshell')" the interpreter
    // version is the job's own, and such a job names the capacity it is allowed.
    let mut body = json!({
        "Name": name,
        "Role": ROLE_ARN,
        "ExecutionProperty": { "MaxConcurrentRuns": 10 },
        "Timeout": 5,
    });
    if command_name == "pythonshell" {
        command["PythonVersion"] = json!("3.9");
        body["MaxCapacity"] = json!(0.0625);
    }
    body["Command"] = command;
    body
}

/// Lay out one row: its workflow, one job per action with its script staged, and the
/// `ON_DEMAND` entry point over those actions. Answers `(workflow, trigger, job names)`.
async fn lay_out(
    router: &MultiServiceRouter,
    index: usize,
    row: &Row,
) -> (String, String, Vec<String>) {
    let workflow = format!("completed-run-wf-{index}");
    ok(router, "CreateWorkflow", &json!({ "Name": &workflow })).await;

    let mut jobs: Vec<String> = Vec::new();
    let mut actions: Vec<Value> = Vec::new();
    for (action, (command_name, script)) in row.actions.iter().enumerate() {
        let job = format!("completed-run-job-{index}-{action}");
        let location = stage_script(router, &format!("scripts/{job}.py"), script).await;
        ok(router, "CreateJob", &job_body(&job, command_name, &location)).await;
        actions.push(json!({ "JobName": &job }));
        jobs.push(job);
    }

    let trigger = format!("completed-run-trigger-{index}");
    ok(
        router,
        "CreateTrigger",
        &json!({
            "Name": &trigger,
            "WorkflowName": &workflow,
            "Type": "ON_DEMAND",
            "Actions": Value::Array(actions),
        }),
    )
    .await;
    (workflow, trigger, jobs)
}

/// The `WorkflowRunId` of the newest run `GetWorkflowRuns` reports for `workflow` — the runs
/// come newest started first, so a run just begun is the first of them. This is how a client
/// that fired a trigger finds the run that firing began.
async fn newest_run_id(router: &MultiServiceRouter, case: &str, workflow: &str) -> String {
    let page = ok(router, "GetWorkflowRuns", &json!({ "Name": workflow })).await;
    let newest = page
        .get("Runs")
        .and_then(Value::as_array)
        .and_then(|runs| runs.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: the listing must report the run, got {page}"));
    newest
        .get("WorkflowRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a listed run carries its id, got {newest}"))
        .to_string()
}

/// The `GetWorkflowRun` request body for one row, asking for the graph the way the row says.
fn read_body(workflow: &str, run_id: &str, graph: Graph) -> Value {
    let mut body = json!({ "Name": workflow, "RunId": run_id });
    match graph {
        Graph::Asked => body["IncludeGraph"] = json!(true),
        Graph::Declined => body["IncludeGraph"] = json!(false),
        Graph::Omitted => {}
    }
    body
}

/// Read the run `run_id` of `workflow` back until it has come to REST, and answer the `Run`
/// it reports then.
///
/// A workflow run is a composite: it comes to rest only once the job runs its actions began
/// have, and a job run's script is executed by whichever call finds that execution owed. So
/// the CONDITION is polled for — with everything the clock owes the run and its actions made
/// due through the store on every turn rather than slept out — and a run still in flight when
/// the generous window closes fails the case instead of hanging.
async fn run_at_rest(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    workflow: &str,
    run_id: &str,
    graph: Graph,
) -> Value {
    let deadline = SystemTime::now() + RUN_AT_REST_DEADLINE;
    loop {
        set_workflow_due_instant(engine, workflow, already_past());
        let body = ok(router, "GetWorkflowRun", &read_body(workflow, run_id, graph)).await;
        let run = body
            .get("Run")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the read must report a Run, got {body}"));
        let status = run.get("Status").and_then(Value::as_str).unwrap_or_default();
        if TERMINAL_WORKFLOW_RUN_STATUSES.contains(&status) {
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

/// Assert that `run`'s statistics report `succeeded` actions succeeded out of `total`, with
/// every other counter at the model's own default of 0.
fn assert_statistics(case: &str, run: &Value, total: i64, succeeded: i64) {
    let statistics = run
        .get("Statistics")
        .unwrap_or_else(|| panic!("case {case:?}: a run reports its Statistics, got {run}"));
    for counter in STATISTICS_COUNTERS {
        let expected = match counter {
            "TotalActions" => total,
            "SucceededActions" => succeeded,
            _ => 0,
        };
        // An absent counter is the zero the shape defaults it to, which awsJson may omit.
        let actual = statistics.get(counter).and_then(Value::as_i64).unwrap_or(0);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {counter} {expected}, got {actual} in {statistics}"
        );
    }
}

/// Assert that the `JOB` node of `graph` for `job` reports exactly one job run, in the
/// `JobRunState` of a run whose script exited cleanly: the outcome the run's own statistics
/// are counting.
fn assert_job_node_succeeded(case: &str, graph: &Value, job: &str) {
    let node = graph
        .get("Nodes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: Graph.Nodes must be a list, got {graph}"))
        .iter()
        .find(|node| {
            node.get("Type").and_then(Value::as_str) == Some(NODE_TYPE_JOB)
                && node.get("Name").and_then(Value::as_str) == Some(job)
        })
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the graph must carry the node of job {job:?}, got {graph}")
        });
    let runs = node
        .get("JobDetails")
        .and_then(|details| details.get("JobRuns"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        runs.len(),
        1,
        "case {case:?}: the node of job {job:?} must report the one run its action began, got \
         {node}"
    );
    let states: Vec<&str> = runs
        .iter()
        .filter_map(|run| run.get("JobRunState").and_then(Value::as_str))
        .collect();
    assert_eq!(
        states,
        vec![JOB_RUN_STATE_SUCCEEDED],
        "case {case:?}: the run of job {job:?} really executed and exited cleanly, got {node}"
    );
}

/// A workflow run whose every action succeeded comes to rest in the modelled status of a run
/// whose actions ran their course — `COMPLETED`, never the `ERROR` the enum reserves for a
/// run that errored — with `SucceededActions` equal to `TotalActions`, every failure counter
/// at 0 and no `ErrorMessage`; and the graph it reports when asked shows each action's run
/// having SUCCEEDED, which is the state the counters are derived from.
#[tokio::test]
async fn a_run_whose_every_action_succeeded_comes_to_rest_completed() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (workflow, trigger, jobs) = lay_out(&router, index, row).await;

        let run_id = match row.begun {
            Begun::FiringTheTrigger => {
                let started = ok(&router, "StartTrigger", &json!({ "Name": &trigger })).await;
                assert_eq!(
                    started.get("Name").and_then(Value::as_str),
                    Some(trigger.as_str()),
                    "case {case:?}: the start answers the trigger it started, got {started}"
                );
                newest_run_id(&router, case, &workflow).await
            }
            Begun::StartingTheWorkflow => {
                let started =
                    ok(&router, "StartWorkflowRun", &json!({ "Name": &workflow })).await;
                started
                    .get("RunId")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the start answers a RunId, got {started}")
                    })
                    .to_string()
            }
        };

        let run = run_at_rest(&router, &engine, case, &workflow, &run_id, row.graph).await;
        assert_eq!(
            run.get("WorkflowRunId").and_then(Value::as_str),
            Some(run_id.as_str()),
            "case {case:?}: the run reads back under the id it was begun with, got {run}"
        );
        assert_eq!(
            run.get("Status").and_then(Value::as_str),
            Some(WORKFLOW_RUN_STATUS_COMPLETED),
            "case {case:?}: nothing in the run errored, failed, stopped or timed out, so it \
             settles {WORKFLOW_RUN_STATUS_COMPLETED}, got {run}"
        );
        assert!(
            run.get("ErrorMessage").is_none_or(Value::is_null),
            "case {case:?}: nothing went wrong in starting the run, so it carries no \
             ErrorMessage, got {run}"
        );
        assert!(
            run.get("CompletedOn").and_then(Value::as_f64).is_some(),
            "case {case:?}: a finished run reports when it finished, got {run}"
        );

        // Every action of this run is a job action whose script exited cleanly, so the run's
        // succeeded count is its total count and no failure counter moved.
        let total = i64::try_from(jobs.len()).expect("a row has few actions");
        assert_statistics(case, &run, total, total);

        match row.graph {
            Graph::Asked => {
                let graph = run.get("Graph").unwrap_or_else(|| {
                    panic!("case {case:?}: a read asking for the graph reports one, got {run}")
                });
                for job in &jobs {
                    assert_job_node_succeeded(case, graph, job);
                }
            }
            Graph::Declined | Graph::Omitted => assert!(
                run.get("Graph").is_none_or(Value::is_null),
                "case {case:?}: a read that did not ask for the graph reports none, got {run}"
            ),
        }

        // The run listing reports the same settled run, which is what a client that polls
        // the listing rather than the single-run read sees.
        let listed = ok(&router, "GetWorkflowRuns", &json!({ "Name": &workflow })).await;
        let newest = listed
            .get("Runs")
            .and_then(Value::as_array)
            .and_then(|runs| runs.first().cloned())
            .unwrap_or_else(|| {
                panic!("case {case:?}: the listing must report the run, got {listed}")
            });
        assert_eq!(
            newest.get("WorkflowRunId").and_then(Value::as_str),
            Some(run_id.as_str()),
            "case {case:?}: the listing reports the run that was begun, got {newest}"
        );
        assert_eq!(
            newest.get("Status").and_then(Value::as_str),
            Some(WORKFLOW_RUN_STATUS_COMPLETED),
            "case {case:?}: the listing reports the status the run settled in, got {newest}"
        );
        assert_statistics(case, &newest, total, total);
    }
}
