// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a Glue workflow whose stages are GATED on one another SUCCEEDING:
//! a `CONDITIONAL` trigger whose predicate watches the `SUCCEEDED` state of the job the
//! stage before it ran, over jobs whose scripts really execute and really write their
//! output.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the scripts
//! the stages execute are staged through Amazon S3's own `PutObject`, so what is asserted is
//! HTTP behaviour over data the object store really holds.
//!
//! The rule under test: inside a workflow run, an armed `CONDITIONAL` trigger "fires when its
//! predicate is satisfied" (`CreateTriggerRequest$Predicate`) against the states the job runs
//! THAT run began have reached — `Condition$State` being one of the states "a trigger can
//! listen for" — so every stage whose predicate the run satisfies is initiated inside the same
//! run, counted in its `WorkflowRunStatistics` and reported under its graph's `JOB` nodes, and
//! the run's terminal `WorkflowRunStatus` follows from its actions having run their course
//! rather than from how many of them there were. A stage therefore begins no earlier than the
//! stage it waits on finished, which is what makes the chain a dependency: each stage's script
//! reads the object the stage before it wrote, so the last object exists at all only if every
//! stage ran, in order.
//!
//! The recorded case — one producer behind an `ON_DEMAND` entry point and one consumer behind a
//! single-condition `ANY` gate on the producer's `SUCCEEDED` — is ONE row of the table below.
//! The rows differ along how many gated stages the chain has (one, two, three), whether the
//! predicate names a `Logical` at all, the `Logical` over its conditions, how many conditions
//! each gate carries, and the seed each chain's values are derived from.
//!
//! The due instants the run lifecycle turns on are moved THROUGH THE STORE rather than slept
//! for, and the run is read back until it has come to rest, so no outcome here depends on how
//! fast the host runs. Where ORDER is asserted it is asserted on the instants the SERVICE
//! reported for its own runs, never on the wall clock.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::time::SystemTime;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_glue_awsglue::ops::types::LOGICAL_OPERATOR_VALUES;
use service_com_amazonaws_glue_awsglue::state::{
    JOB_RUN_STATE_SUCCEEDED, LOGICAL_AND, LOGICAL_ANY, NODE_TYPE_JOB,
    TERMINAL_WORKFLOW_RUN_STATUSES, WORKFLOW_RUN_STATUS_COMPLETED,
};

use crate::job_runs::{
    already_past, set_workflow_due_instant, RUN_AT_REST_DEADLINE, RUN_POLL_INTERVAL,
};
use crate::job_script_execution::{
    create_bucket, emulator_with_s3, object_body, stage_script, BUCKET, ROLE_ARN, SCRIPTS,
};
use crate::wire::ok;

/// The one `LogicalOperator` a condition can carry. Source: the generated
/// `LOGICAL_OPERATOR_VALUES` (`smithy_query shape=LogicalOperator`), whose single entry it is.
const LOGICAL_OPERATOR: &str = LOGICAL_OPERATOR_VALUES[0];

/// What the FIRST stage of a chain multiplies its seed by to reach the value it writes.
///
/// The suite's own arithmetic, not anything AWS publishes: all it has to be is a value no
/// later stage could have produced by accident, so an object carrying it can only have been
/// written by the script that computes it.
const SEED_FACTOR: i64 = 3;

/// What each LATER stage adds to the value the stage before it wrote, for the same reason.
const DERIVED_STEP: i64 = 7;

/// The producer's script: the first stage of a chain, which writes the value every stage after
/// it derives its own from. It reads its arguments off its own command line, the way any
/// Python program reads them, and writes through the emulator's own Amazon S3.
const SEEDS_THE_CHAIN: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"],
    Key=named["--out"],
    Body=str(int(named["--seed"]) * 3).encode("utf-8"),
)
"#;

/// Every later stage's script: it READS the object the stage before it wrote and writes a value
/// derived from it. The object it leaves behind therefore exists only if the stage it waits on
/// really ran first — which is the dependency a gate is there to enforce.
const DERIVES_FROM_THE_STAGE_BEFORE: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
client = boto3.client("s3")
upstream = client.get_object(Bucket=named["--bucket"], Key=named["--in"])["Body"].read()
client.put_object(
    Bucket=named["--bucket"],
    Key=named["--out"],
    Body=str(int(upstream) + 7).encode("utf-8"),
)
"#;

/// One row of the table: a chain of gated stages behind one `ON_DEMAND` entry point.
struct ChainCase {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The `Logical` every gate's predicate carries, or `None` for a predicate naming none —
    /// "an optional field if only one condition is listed" (`Predicate$Logical`).
    logical: Option<&'static str>,
    /// How many GATED stages follow the producer, each waiting on the stage before it.
    gated: usize,
    /// Whether a gate past the first also carries a condition on the chain's PRODUCER, so its
    /// predicate holds two conditions rather than one.
    also_watches_the_producer: bool,
}

/// The rows this suite drives.
fn chain_cases() -> Vec<ChainCase> {
    vec![
        ChainCase {
            // The recorded shape: one gate, one condition, on the producer's SUCCEEDED.
            case: "one gated stage under ANY over the producer's SUCCEEDED",
            logical: Some(LOGICAL_ANY),
            gated: 1,
            also_watches_the_producer: false,
        },
        ChainCase {
            // "An optional field if only one condition is listed": the same gate.
            case: "one gated stage whose predicate names no Logical at all",
            logical: None,
            gated: 1,
            also_watches_the_producer: false,
        },
        ChainCase {
            // A gate's own action is an action of the run, so it can satisfy the next gate.
            case: "two gated stages, each waiting on the stage before it",
            logical: Some(LOGICAL_ANY),
            gated: 2,
            also_watches_the_producer: false,
        },
        ChainCase {
            // Under AND every condition has to hold, so the second gate waits on both.
            case: "two gated stages, the second under AND over both stages before it",
            logical: Some(LOGICAL_AND),
            gated: 2,
            also_watches_the_producer: true,
        },
        ChainCase {
            case: "three gated stages chained under ANY",
            logical: Some(LOGICAL_ANY),
            gated: 3,
            also_watches_the_producer: false,
        },
    ]
}

impl ChainCase {
    /// How many actions the run must report in total: the producer plus every gated stage.
    fn actions(&self) -> i64 {
        1 + i64::try_from(self.gated).unwrap_or(i64::MAX - 1)
    }
}

/// The seed row `index`'s producer is given, so no two chains compute the same values.
fn seed_of(index: usize) -> i64 {
    2 + i64::try_from(index).unwrap_or(0)
}

/// The value stage `stage` of row `index` must have written: the seed through the producer's
/// own arithmetic, then one step per gated stage that followed it.
fn expected_value(index: usize, stage: usize) -> i64 {
    seed_of(index) * SEED_FACTOR + DERIVED_STEP * i64::try_from(stage).unwrap_or(0)
}

/// The job definition stage `stage` of row `index` runs.
fn stage_job(index: usize, stage: usize) -> String {
    format!("wf-chain-{index}-stage-{stage}")
}

/// The object key stage `stage` of row `index` writes its value under.
fn stage_key(index: usize, stage: usize) -> String {
    format!("chains/{index}/stage-{stage}.txt")
}

/// The condition that waits on `job` having SUCCEEDED — one of the states `Condition$State`
/// documents a trigger as able to listen for.
fn succeeded_condition(job: &str) -> Value {
    json!({
        "LogicalOperator": LOGICAL_OPERATOR,
        "JobName": job,
        "State": JOB_RUN_STATE_SUCCEEDED,
    })
}

/// Register row `index`'s workflow: one job per stage with its script staged and its arguments
/// on the job definition, an `ON_DEMAND` entry point over the producer, and one armed
/// `CONDITIONAL` gate per gated stage. Answers `(workflow, the stages' job names in order)`.
async fn lay_out_chain(
    router: &MultiServiceRouter,
    index: usize,
    row: &ChainCase,
) -> (String, Vec<String>) {
    let workflow = format!("wf-chain-{index}");
    ok(router, "CreateWorkflow", &json!({ "Name": workflow })).await;

    let mut jobs = Vec::new();
    for stage in 0..=row.gated {
        let name = stage_job(index, stage);
        let source = if stage == 0 {
            SEEDS_THE_CHAIN
        } else {
            DERIVES_FROM_THE_STAGE_BEFORE
        };
        let location = stage_script(router, &format!("{SCRIPTS}{name}.py"), source).await;
        // A run a trigger's action begins carries no `Arguments` of its own, so what its
        // script is given is the job's own `DefaultArguments`.
        let mut arguments = json!({
            "--bucket": BUCKET,
            "--out": stage_key(index, stage),
        });
        if stage == 0 {
            arguments["--seed"] = json!(seed_of(index).to_string());
        } else {
            arguments["--in"] = json!(stage_key(index, stage - 1));
        }
        ok(
            router,
            "CreateJob",
            &json!({
                "Name": name,
                "Role": ROLE_ARN,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": location,
                },
                "MaxCapacity": 0.0625,
                "Timeout": 5,
                "ExecutionProperty": { "MaxConcurrentRuns": 5 },
                "DefaultArguments": arguments,
            }),
        )
        .await;
        jobs.push(name);
    }

    ok(
        router,
        "CreateTrigger",
        &json!({
            "Name": format!("{workflow}-entry"),
            "Type": "ON_DEMAND",
            "WorkflowName": workflow,
            "Actions": [{ "JobName": jobs[0] }],
        }),
    )
    .await;

    for stage in 1..=row.gated {
        let mut conditions = vec![succeeded_condition(&jobs[stage - 1])];
        if row.also_watches_the_producer && stage > 1 {
            conditions.push(succeeded_condition(&jobs[0]));
        }
        let mut predicate = json!({ "Conditions": conditions });
        if let Some(logical) = row.logical {
            predicate["Logical"] = json!(logical);
        }
        ok(
            router,
            "CreateTrigger",
            &json!({
                "Name": format!("{workflow}-gate-{stage}"),
                "Type": "CONDITIONAL",
                "WorkflowName": workflow,
                // "Set to true to start SCHEDULED and CONDITIONAL triggers when created":
                // a gate nothing armed watches nothing.
                "StartOnCreation": true,
                "Predicate": predicate,
                "Actions": [{ "JobName": jobs[stage] }],
            }),
        )
        .await;
    }

    (workflow, jobs)
}

/// Begin a run of `workflow` and answer the `RunId` the start minted.
async fn begin_run(router: &MultiServiceRouter, case: &str, workflow: &str) -> String {
    let started = ok(router, "StartWorkflowRun", &json!({ "Name": workflow })).await;
    started
        .get("RunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartWorkflowRun must answer a RunId, got {started}")
        })
        .to_string()
}

/// The `Run` `GetWorkflowRun` reports for one run of `workflow`, with the graph when asked for.
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

/// Read `run_id` of `workflow` back until it has come to REST, and answer the `Run` with its
/// graph.
///
/// A workflow run is a COMPOSITE: it comes to rest only once every action it initiated has —
/// the stages its gates opened included — and each of those owes its script one execution,
/// performed by whichever call finds it owed. So the CONDITION is polled for, with the owed
/// instants made due through the store on every turn rather than slept out, against the same
/// generous window [`crate::job_runs::run_at_rest`] gives one job run.
async fn chain_run_at_rest(
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

/// Every counter of a run's `Statistics`, so a row names the ones it moves and has every other
/// asserted at the model's own default of 0.
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

/// Assert that the `Statistics` of `run` are exactly `expected`, with every counter `expected`
/// does not name at 0.
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

/// The name of every `JOB` node of `graph`.
fn job_node_names(case: &str, graph: &Value) -> BTreeSet<String> {
    graph
        .get("Nodes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: Graph.Nodes must be a list, got {graph}"))
        .iter()
        .filter(|node| node.get("Type").and_then(Value::as_str) == Some(NODE_TYPE_JOB))
        .map(|node| {
            node.get("Name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a node carries a Name, got {node}"))
                .to_string()
        })
        .collect()
}

/// The runs the `JOB` node for `job` reports under `JobDetails.JobRuns`.
fn job_node_runs(case: &str, graph: &Value, job: &str) -> Vec<Value> {
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
        .unwrap_or_else(|| panic!("case {case:?}: the graph must hold a JOB node for {job:?}"));
    node.get("JobDetails")
        .filter(|details| !details.is_null())
        .and_then(|details| details.get("JobRuns"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The ONE run the `JOB` node for `job` reports, asserting it reports exactly one.
fn the_one_job_run(case: &str, graph: &Value, job: &str) -> Value {
    let runs = job_node_runs(case, graph, job);
    assert_eq!(
        runs.len(),
        1,
        "case {case:?}: the node for {job:?} must report the one run its action began, got \
         {runs:?}"
    );
    runs.into_iter()
        .next()
        .expect("the length was just asserted to be one")
}

/// A reported timestamp member of a run, as the seconds the wire carries it as.
fn instant_of(case: &str, run: &Value, member: &str) -> f64 {
    run.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: a finished run reports its {member}, got {run}"))
}

/// Every chain stage runs INSIDE the one workflow run, after the stage it waits on succeeded,
/// and the run comes to rest completed with every action counted as succeeded.
#[tokio::test]
async fn a_gated_chain_runs_every_stage_in_order_inside_one_completed_run() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (index, row) in chain_cases().iter().enumerate() {
        let case = row.case;
        let (workflow, jobs) = lay_out_chain(&router, index, row).await;
        let run_id = begin_run(&router, case, &workflow).await;
        let run = chain_run_at_rest(&router, &engine, case, &workflow, &run_id).await;

        // "COMPLETED" names a run whose actions have run their course: a run every one of
        // whose actions succeeded is one of them, whatever the chain's shape.
        assert_eq!(
            run.get("Status").and_then(Value::as_str),
            Some(WORKFLOW_RUN_STATUS_COMPLETED),
            "case {case:?}: the run must come to rest completed, got {run}"
        );
        let actions = row.actions();
        assert_statistics(
            case,
            &run,
            &[("TotalActions", actions), ("SucceededActions", actions)]
                .into_iter()
                .collect(),
        );

        let graph = run
            .get("Graph")
            .unwrap_or_else(|| panic!("case {case:?}: IncludeGraph must report a Graph, got {run}"));
        // The run's JOB nodes are exactly the chain's stages — no stage is missing, and the
        // graph invents none.
        assert_eq!(
            job_node_names(case, graph),
            jobs.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: the graph must hold a JOB node per stage, got {graph}"
        );

        let mut previous: Option<Value> = None;
        for (stage, job) in jobs.iter().enumerate() {
            let reported = the_one_job_run(case, graph, job);
            assert_eq!(
                reported.get("JobRunState").and_then(Value::as_str),
                Some(JOB_RUN_STATE_SUCCEEDED),
                "case {case:?}: stage {stage} of the chain must have succeeded, got {reported}"
            );

            if let Some(upstream) = previous.as_ref() {
                // The gate fired at the instant its predicate held, which is when the run it
                // waited on finished — so a stage never begins before its predecessor ended.
                let began = instant_of(case, &reported, "StartedOn");
                let upstream_ended = instant_of(case, upstream, "CompletedOn");
                assert!(
                    began >= upstream_ended,
                    "case {case:?}: stage {stage} must begin no earlier than stage {} ended, \
                     got {began} against {upstream_ended}",
                    stage - 1
                );
                // "A job run that was used in the predicate of a conditional trigger that
                // triggered this job run": the stage the gate waited on.
                let predecessors = reported
                    .get("PredecessorRuns")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let named: BTreeSet<String> = predecessors
                    .iter()
                    .filter_map(|predecessor| predecessor.get("JobName"))
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                assert!(
                    named.contains(&jobs[stage - 1]),
                    "case {case:?}: stage {stage} must name the run it waited on among its \
                     PredecessorRuns, got {predecessors:?}"
                );
            }

            // The stage's own EFFECT: the object its script wrote, whose value a later stage
            // could only compute by reading the one before it.
            let written = object_body(&router, &stage_key(index, stage)).await;
            let expected = expected_value(index, stage).to_string();
            assert_eq!(
                written.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: stage {stage} must have written {expected:?}, got {written:?}"
            );

            previous = Some(reported);
        }
    }
}

/// The completed chain reads back the same through every reader: the run listing reports the
/// same status and statistics as the single-run read, and the workflow's own `LastRun` is that
/// run.
#[tokio::test]
async fn a_gated_chains_finished_run_reads_back_the_same_through_every_reader() {
    let case = "a two-stage chain read through every reader";
    let row = ChainCase {
        case,
        logical: Some(LOGICAL_ANY),
        gated: 1,
        also_watches_the_producer: false,
    };
    // An index of its own, so this chain's jobs, keys and seed are nobody else's.
    let index = 90;

    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;
    let (workflow, jobs) = lay_out_chain(&router, index, &row).await;
    let run_id = begin_run(&router, case, &workflow).await;
    let run = chain_run_at_rest(&router, &engine, case, &workflow, &run_id).await;

    let actions = row.actions();
    let expected: BTreeMap<&str, i64> = [("TotalActions", actions), ("SucceededActions", actions)]
        .into_iter()
        .collect();
    assert_statistics(case, &run, &expected);

    let page = ok(
        &router,
        "GetWorkflowRuns",
        &json!({ "Name": workflow, "IncludeGraph": true }),
    )
    .await;
    let listed = page
        .get("Runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: the workflow must report the one run it has, got {page}"
    );
    let listed_run = &listed[0];
    assert_eq!(
        listed_run.get("WorkflowRunId").and_then(Value::as_str),
        Some(run_id.as_str()),
        "case {case:?}: the listing must report the run that was started, got {listed_run}"
    );
    assert_eq!(
        listed_run.get("Status").and_then(Value::as_str),
        Some(WORKFLOW_RUN_STATUS_COMPLETED),
        "case {case:?}: the listing must report the same status, got {listed_run}"
    );
    assert_statistics(case, listed_run, &expected);
    let listed_graph = listed_run.get("Graph").unwrap_or_else(|| {
        panic!("case {case:?}: IncludeGraph must report a Graph, got {listed_run}")
    });
    assert_eq!(
        job_node_names(case, listed_graph),
        jobs.iter().cloned().collect::<BTreeSet<String>>(),
        "case {case:?}: the listed run's graph must hold a JOB node per stage, got {listed_graph}"
    );

    let read = ok(
        &router,
        "GetWorkflow",
        &json!({ "Name": workflow, "IncludeGraph": false }),
    )
    .await;
    let last_run = read
        .get("Workflow")
        .and_then(|workflow| workflow.get("LastRun"))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetWorkflow must report a LastRun, got {read}"));
    assert_eq!(
        last_run.get("WorkflowRunId").and_then(Value::as_str),
        Some(run_id.as_str()),
        "case {case:?}: the workflow's LastRun must be the run that was started, got {last_run}"
    );
    assert_statistics(case, &last_run, &expected);
}
