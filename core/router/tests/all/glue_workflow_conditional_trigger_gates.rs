// SPDX-License-Identifier: CC0-1.0

//! What a Glue workflow run does when one of its CONDITIONAL triggers is SATISFIED: the stage
//! the gate opens runs inside the same workflow run, and its output is derived from the stage
//! before it.
//!
//! The RULE under test: **inside a run of a workflow, an armed `CONDITIONAL` trigger of that
//! workflow fires once, as soon as the job runs THAT run began satisfy its `Predicate`, and the
//! actions it initiates are actions of the same run.** A predicate is satisfied under its own
//! `Logical` — "an optional field if only one condition is listed. If multiple conditions are
//! listed, then this field is required" (`Predicate$Logical`, one of `AND`/`ANY`) — and one
//! condition holds when a run of its `Condition$JobName` inside that workflow run has reached
//! its `Condition$State` ("currently, the only job states that a trigger can listen for are
//! `SUCCEEDED`, `STOPPED`, `FAILED`, and `TIMEOUT`"). So `GetWorkflowRun` reports
//! `Statistics.TotalActions` grown by what the gate initiated, counts each of those actions in
//! the statistic for the `JobRunState` it has reached, carries a `JOB` node per job with the
//! runs that workflow run began of it ("the information for the job runs represented by the job
//! node", `JobNodeDetails$JobRuns`), and reaches a terminal `Status` only once the gated stage
//! has come to rest too. A gate the create did not arm ("set to true to start SCHEDULED and
//! CONDITIONAL triggers when created", `CreateTriggerRequest$StartOnCreation`) fires nothing
//! until a start arms it.
//!
//! Two services have to agree for any of that to hold, which is why this suite lives here: AWS
//! Glue runs the workflow and executes each stage's script, and Amazon S3 holds the object one
//! stage writes and the next stage reads — so the gated stage's output is a value only a real
//! read of the first stage's object produces, and an object that is absent is how a gate that
//! must NOT fire is observed.
//!
//! The rows differ along what the rule generalises over: the `State` a condition watches
//! (`SUCCEEDED`, `FAILED`), the `Logical` over its conditions (`ANY`, `AND`, and none at all),
//! how many conditions the predicate carries, how many of them the run satisfies, and what the
//! upstream stage came to.
//!
//! Every wait here is a POLL for the condition it waits on, against a generous deadline, and
//! the due instants the lifecycle turns on are moved THROUGH THE STORE rather than slept out —
//! so nothing asserted depends on how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED,
    NODE_TYPE_JOB, TERMINAL_WORKFLOW_RUN_STATUSES, WORKFLOW_RUN_STATUS_COMPLETED,
};

/// The Region every call below is served in.
const REGION: &str = "us-east-1";

/// The bucket the scripts, the upstream objects and the gated stages' output live in.
const BUCKET: &str = "xsvc-glue-workflow-gate";

/// The role every job runs as.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/xsvc-glue-workflow-role";

/// The seed the first stage of every row multiplies: its object carries `SEED * 3`, and the
/// gated stage reads that and writes `SEED * 3 + 7`, so the value under the gated stage's key
/// is one only a real read of the upstream object produces.
const SEED: i64 = 11;

/// What the gated stage writes when it does not derive its output from an upstream object.
const TOKEN: &str = "gate-fired";

/// How long a poll waits for a workflow run to come to rest. Deliberately generous: what it
/// waits for is the scripts of the run's stages, performed by whichever call finds one owed,
/// plus the spans the clock then owes each run. Nothing is asserted about how long any of it
/// took.
const AT_REST_DEADLINE: Duration = Duration::from_secs(120);

/// The gap between two reads of a polling client.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The first stage's script: it writes `SEED * 3` under the key its arguments name.
const PRODUCES: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"],
    Key=named["--key"],
    Body=str(int(named["--seed"]) * 3).encode("utf-8"),
)
"#;

/// A first stage that RAISES, which is what a failing Glue job does: it writes no object, so
/// nothing downstream of it can read one.
const RAISES: &str = r#"
raise ValueError("the upstream partition is malformed")
"#;

/// The gated stage's script where the row derives its output: it READS the upstream object and
/// writes that value plus seven, so its output exists only if the stage before it really ran.
const DERIVES: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
client = boto3.client("s3")
upstream = int(
    client.get_object(Bucket=named["--bucket"], Key=named["--upstream"])["Body"]
    .read()
    .decode("utf-8")
)
client.put_object(
    Bucket=named["--bucket"],
    Key=named["--key"],
    Body=str(upstream + 7).encode("utf-8"),
)
"#;

/// The gated stage's script where the row's upstream object does not exist: it writes a fixed
/// token, so the firing is still observable in the object store.
const WRITES_A_TOKEN: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"],
    Key=named["--key"],
    Body=named["--token"].encode("utf-8"),
)
"#;

/// What one upstream stage of a row does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Upstream {
    /// Writes its object and exits cleanly, so its run comes to rest `SUCCEEDED`.
    Succeeds,
    /// Raises, so its run comes to rest in the failure state and writes nothing.
    Raises,
}

/// What the gate's own action does when it fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gated {
    /// Derives its output from the FIRST upstream stage's object.
    DerivesFromUpstream,
    /// Writes a fixed token, for a row whose upstream wrote nothing to derive from.
    WritesAToken,
}

/// One row: the stages the entry point runs, the predicate the gate carries, and whether that
/// predicate is satisfied by what those stages came to.
struct Row {
    /// What every assertion of the row names, and the segment its objects live under.
    case: &'static str,
    /// The upstream stages, one action of the entry point each.
    upstreams: &'static [Upstream],
    /// The predicate's `Logical`, or `None` for a predicate that names none.
    logical: Option<&'static str>,
    /// The conditions, as `(upstream index, the JobRunState the condition watches)`.
    conditions: &'static [(usize, &'static str)],
    /// Whether the run's own job runs satisfy that predicate, so the gate fires.
    fires: bool,
    /// What the gated stage does.
    gated: Gated,
}

impl Row {
    /// How many actions the run must report in total: the entry point's, plus the gate's own
    /// where the gate fired.
    fn total_actions(&self) -> i64 {
        i64::try_from(self.upstreams.len()).expect("a small count")
            + i64::from(self.fires)
    }

    /// How many of them must have come to rest `SUCCEEDED`: every upstream that succeeds, plus
    /// the gated stage where the gate fired (both gated scripts succeed).
    fn succeeded_actions(&self) -> i64 {
        let upstream = self
            .upstreams
            .iter()
            .filter(|upstream| **upstream == Upstream::Succeeds)
            .count();
        i64::try_from(upstream).expect("a small count") + i64::from(self.fires)
    }

    /// How many must have come to rest in the failure state: every upstream that raised.
    fn failed_actions(&self) -> i64 {
        let raised = self
            .upstreams
            .iter()
            .filter(|upstream| **upstream == Upstream::Raises)
            .count();
        i64::try_from(raised).expect("a small count")
    }
}

/// The rows the table drives.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: one condition on the upstream job's SUCCEEDED state.
            case: "one condition on SUCCEEDED under ANY",
            upstreams: &[Upstream::Succeeds],
            logical: Some("ANY"),
            conditions: &[(0, JOB_RUN_STATE_SUCCEEDED)],
            fires: true,
            gated: Gated::DerivesFromUpstream,
        },
        Row {
            // "An optional field if only one condition is listed": a predicate naming no
            // Logical at all is the same gate.
            case: "one condition and no Logical at all",
            upstreams: &[Upstream::Succeeds],
            logical: None,
            conditions: &[(0, JOB_RUN_STATE_SUCCEEDED)],
            fires: true,
            gated: Gated::DerivesFromUpstream,
        },
        Row {
            // The state a condition watches is part of it: a gate waiting for the failure
            // state is not satisfied by a stage that succeeded.
            case: "a condition on FAILED that a succeeding upstream does not satisfy",
            upstreams: &[Upstream::Succeeds],
            logical: Some("ANY"),
            conditions: &[(0, JOB_RUN_STATE_FAILED)],
            fires: false,
            gated: Gated::DerivesFromUpstream,
        },
        Row {
            // And it IS satisfied by a stage that reached that state, so the gate opens a
            // recovery stage.
            case: "a condition on FAILED that a raising upstream satisfies",
            upstreams: &[Upstream::Raises],
            logical: Some("ANY"),
            conditions: &[(0, JOB_RUN_STATE_FAILED)],
            fires: true,
            gated: Gated::WritesAToken,
        },
        Row {
            case: "AND over two conditions, both of them reached",
            upstreams: &[Upstream::Succeeds, Upstream::Succeeds],
            logical: Some("AND"),
            conditions: &[(0, JOB_RUN_STATE_SUCCEEDED), (1, JOB_RUN_STATE_SUCCEEDED)],
            fires: true,
            gated: Gated::DerivesFromUpstream,
        },
        Row {
            case: "AND over two conditions, one of them never reached",
            upstreams: &[Upstream::Succeeds, Upstream::Raises],
            logical: Some("AND"),
            conditions: &[(0, JOB_RUN_STATE_SUCCEEDED), (1, JOB_RUN_STATE_SUCCEEDED)],
            fires: false,
            gated: Gated::DerivesFromUpstream,
        },
        Row {
            case: "ANY over two conditions, one of them reached",
            upstreams: &[Upstream::Succeeds, Upstream::Raises],
            logical: Some("ANY"),
            conditions: &[(0, JOB_RUN_STATE_SUCCEEDED), (1, JOB_RUN_STATE_SUCCEEDED)],
            fires: true,
            gated: Gated::DerivesFromUpstream,
        },
    ]
}

/// AWS Glue and Amazon S3 on ONE shared engine: the workflow's stages are executed by Glue and
/// read and write their objects through S3's own operations over the emulator's in-process
/// callback, so the object one stage writes is the object the next stage reads.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One AWS Glue `awsJson1_1` call, as its status and parsed body.
async fn glue_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header("authorization", authorization("glue"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same call, which must be served.
async fn glue(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = glue_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon S3 request against the Region's path-style endpoint, which is the endpoint a
/// stage's own script reaches the object store through.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.as_bytes().to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Create the suite's bucket.
async fn create_bucket(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), "").await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket({BUCKET:?}) must succeed, got {status} with {body}"
    );
}

/// Stage `source` as the object `key`.
async fn put_object(router: &MultiServiceRouter, key: &str, source: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}/{key}"), source).await;
    assert!(
        (200..300).contains(&status),
        "setup: PutObject({key:?}) must succeed, got {status} with {body}"
    );
}

/// The body of the object `key`, or `None` when the store serves no object there.
async fn object_body(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), "").await;
    (200..300).contains(&status).then_some(body)
}

/// Move every due instant the Glue store still owes an unfinished run to the past, so what the
/// clock owed falls due on the next call instead of being slept out.
///
/// A job run owes up to TWO states at once — the state it is in while its script runs and the
/// state its own execution came to — so both are moved, or a run would stop half way. Nothing
/// decides WHAT any of them becomes: that is the run's own execution and the service's own
/// bookkeeping; only WHEN moves.
fn make_owed_transitions_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for jobs in store.jobs.values_mut() {
        for job in jobs.values_mut() {
            for run in &mut job.runs {
                if run.settles_at.is_some() {
                    run.settles_at = Some(past);
                }
                if run.rests_at.is_some() {
                    run.rests_at = Some(past);
                }
            }
        }
    }
    for workflow in store.workflows.values_mut() {
        for run in &mut workflow.runs {
            if run.settles_at.is_some() {
                run.settles_at = Some(past);
            }
        }
    }
}

/// The `Run` `GetWorkflowRun` reports, with its graph.
async fn workflow_run(
    router: &MultiServiceRouter,
    case: &str,
    workflow: &str,
    run_id: &str,
) -> Value {
    let body = glue(
        router,
        case,
        "GetWorkflowRun",
        &json!({ "Name": workflow, "RunId": run_id, "IncludeGraph": true }),
    )
    .await;
    body.get("Run")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetWorkflowRun must report a Run, got {body}"))
}

/// Read the workflow run back until it has come to rest, and answer what it then reports.
///
/// The run comes to rest only once the stages it initiated have, and each of those stages owes
/// its script ONE execution performed by whichever call finds it owed — so this polls for that
/// CONDITION, exactly as a client polling the run does, with what the clock owed made due
/// through the store on every turn rather than waited for.
async fn workflow_run_at_rest(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    workflow: &str,
    run_id: &str,
) -> Value {
    let deadline = SystemTime::now() + AT_REST_DEADLINE;
    loop {
        make_owed_transitions_due(engine);
        let run = workflow_run(router, case, workflow, run_id).await;
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
            "case {case:?}: the workflow run must come to rest, and it reports {run}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every counter of a run's `Statistics`, so a row names the ones it expects and every other
/// one is asserted at the model's own default of 0.
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

/// Assert the run's `Statistics` are exactly `expected`, with every counter it does not name
/// at 0.
fn assert_statistics(case: &str, run: &Value, expected: &[(&str, i64)]) {
    let statistics = run
        .get("Statistics")
        .unwrap_or_else(|| panic!("case {case:?}: a run reports its Statistics, got {run}"));
    for counter in STATISTICS_COUNTERS {
        let want = expected
            .iter()
            .find(|(named, _)| *named == counter)
            .map_or(0, |(_, want)| *want);
        let got = statistics.get(counter).and_then(Value::as_i64).unwrap_or(0);
        assert_eq!(
            got, want,
            "case {case:?}: expected {counter} {want}, got {got} in {statistics}"
        );
    }
}

/// The runs the `JOB` node for `job` reports under `JobDetails.JobRuns`, and an empty list when
/// the graph carries no such node or that node reports no run.
fn job_node_runs(case: &str, run: &Value, job: &str) -> Vec<Value> {
    let nodes = run
        .pointer("/Graph/Nodes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a run's Graph must carry Nodes, got {run}"));
    let Some(node) = nodes.iter().find(|node| {
        node.get("Type").and_then(Value::as_str) == Some(NODE_TYPE_JOB)
            && node.get("Name").and_then(Value::as_str) == Some(job)
    }) else {
        return Vec::new();
    };
    node.pointer("/JobDetails/JobRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The names of one row's jobs: its upstream stages, and the stage its gate runs.
fn job_names(index: usize, upstreams: usize) -> (Vec<String>, String) {
    let upstream = (0..upstreams)
        .map(|stage| format!("wf-gate-upstream-{index}-{stage}"))
        .collect();
    (upstream, format!("wf-gate-gated-{index}"))
}

/// The key one row's upstream stage `stage` writes.
fn upstream_key(index: usize, stage: usize) -> String {
    format!("wf/{index}/produced-{stage}.txt")
}

/// The key one row's gated stage writes.
fn gated_key(index: usize) -> String {
    format!("wf/{index}/consumed.txt")
}

/// Register one job whose `Command` runs `script`, with `arguments` as its
/// `DefaultArguments` — the arguments the script reads off its own command line.
async fn create_job(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    script_key: &str,
    arguments: Value,
) {
    glue(
        router,
        case,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": ROLE_ARN,
            "Command": {
                "Name": "pythonshell",
                "PythonVersion": "3.9",
                "ScriptLocation": format!("s3://{BUCKET}/{script_key}"),
            },
            "DefaultArguments": arguments,
        }),
    )
    .await;
}

/// Lay out one row: its upstream jobs, its gated job, the workflow, the ON_DEMAND entry point
/// over every upstream stage, and the CONDITIONAL gate over the row's predicate.
///
/// `arm_the_gate` is the gate's own `StartOnCreation`.
async fn lay_out(
    router: &MultiServiceRouter,
    index: usize,
    row: &Row,
    arm_the_gate: bool,
) -> (String, Vec<String>, String) {
    let case = row.case;
    let (upstreams, gated) = job_names(index, row.upstreams.len());

    for (stage, upstream) in row.upstreams.iter().enumerate() {
        let script = match upstream {
            Upstream::Succeeds => PRODUCES,
            Upstream::Raises => RAISES,
        };
        let key = format!("scripts/upstream-{index}-{stage}.py");
        put_object(router, &key, script).await;
        create_job(
            router,
            case,
            &upstreams[stage],
            &key,
            json!({
                "--bucket": BUCKET,
                "--key": upstream_key(index, stage),
                "--seed": SEED.to_string(),
            }),
        )
        .await;
    }

    let gated_script_key = format!("scripts/gated-{index}.py");
    let (gated_script, gated_arguments) = match row.gated {
        Gated::DerivesFromUpstream => (
            DERIVES,
            json!({
                "--bucket": BUCKET,
                "--key": gated_key(index),
                "--upstream": upstream_key(index, 0),
            }),
        ),
        Gated::WritesAToken => (
            WRITES_A_TOKEN,
            json!({
                "--bucket": BUCKET,
                "--key": gated_key(index),
                "--token": TOKEN,
            }),
        ),
    };
    put_object(router, &gated_script_key, gated_script).await;
    create_job(router, case, &gated, &gated_script_key, gated_arguments).await;

    let workflow = format!("wf-gate-{index}");
    glue(router, case, "CreateWorkflow", &json!({ "Name": workflow })).await;
    glue(
        router,
        case,
        "CreateTrigger",
        &json!({
            "Name": format!("{workflow}-entry"),
            "Type": "ON_DEMAND",
            "WorkflowName": workflow,
            "Actions": upstreams
                .iter()
                .map(|job| json!({ "JobName": job }))
                .collect::<Vec<Value>>(),
        }),
    )
    .await;

    let mut predicate = json!({
        "Conditions": row
            .conditions
            .iter()
            .map(|(stage, state)| json!({
                "LogicalOperator": "EQUALS",
                "JobName": upstreams[*stage],
                "State": state,
            }))
            .collect::<Vec<Value>>(),
    });
    if let Some(logical) = row.logical {
        predicate["Logical"] = json!(logical);
    }
    glue(
        router,
        case,
        "CreateTrigger",
        &json!({
            "Name": format!("{workflow}-gate"),
            "Type": "CONDITIONAL",
            "WorkflowName": workflow,
            "StartOnCreation": arm_the_gate,
            "Predicate": predicate,
            "Actions": [{ "JobName": gated }],
        }),
    )
    .await;

    (workflow, upstreams, gated)
}

/// Start a run of `workflow` and answer the id it minted.
async fn start_run(router: &MultiServiceRouter, case: &str, workflow: &str) -> String {
    let started = glue(router, case, "StartWorkflowRun", &json!({ "Name": workflow })).await;
    started
        .get("RunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartWorkflowRun must answer a RunId, got {started}")
        })
        .to_string()
}

/// The `JobRun` member `member` of every run reported under a graph's job node.
fn run_members(runs: &[Value], member: &str) -> Vec<String> {
    runs.iter()
        .map(|run| {
            run.get(member)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A satisfied gate runs its stage inside the SAME workflow run, and an unsatisfied one runs
/// nothing — over the table above.
#[tokio::test]
async fn a_satisfied_conditional_gate_runs_its_stage_inside_the_same_workflow_run() {
    let (router, engine) = emulator();
    create_bucket(&router).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (workflow, upstreams, gated) = lay_out(&router, index, row, true).await;
        let run_id = start_run(&router, case, &workflow).await;
        let run = workflow_run_at_rest(&router, &engine, case, &workflow, &run_id).await;

        // "COMPLETED" names a run whose actions have run their course, whatever each came to.
        assert_eq!(
            run.get("Status").and_then(Value::as_str),
            Some(WORKFLOW_RUN_STATUS_COMPLETED),
            "case {case:?}: the run must complete, got {run}"
        );
        assert_statistics(
            case,
            &run,
            &[
                ("TotalActions", row.total_actions()),
                ("SucceededActions", row.succeeded_actions()),
                ("FailedActions", row.failed_actions()),
            ],
        );

        // Each upstream stage ran once inside this run, and the state its node reports is the
        // one its own script came to.
        for (stage, upstream) in row.upstreams.iter().enumerate() {
            let runs = job_node_runs(case, &run, &upstreams[stage]);
            assert_eq!(
                runs.len(),
                1,
                "case {case:?}: the upstream node {} must report its one run, got {runs:?}",
                upstreams[stage]
            );
            let expected = match upstream {
                Upstream::Succeeds => JOB_RUN_STATE_SUCCEEDED,
                Upstream::Raises => JOB_RUN_STATE_FAILED,
            };
            assert_eq!(
                run_members(&runs, "JobRunState"),
                vec![expected.to_string()],
                "case {case:?}: the upstream node {} must report {expected}, got {runs:?}",
                upstreams[stage]
            );
        }

        // The gated stage: one SUCCEEDED run under its own node and the object it wrote, or
        // neither.
        let gated_runs = job_node_runs(case, &run, &gated);
        let written = object_body(&router, &gated_key(index)).await;
        if row.fires {
            assert_eq!(
                gated_runs.len(),
                1,
                "case {case:?}: the gated node {gated} must report the one run the gate began, \
                 got {gated_runs:?}"
            );
            assert_eq!(
                run_members(&gated_runs, "JobRunState"),
                vec![JOB_RUN_STATE_SUCCEEDED.to_string()],
                "case {case:?}: the gated run must have succeeded, got {gated_runs:?}"
            );
            let expected = match row.gated {
                Gated::DerivesFromUpstream => (SEED * 3 + 7).to_string(),
                Gated::WritesAToken => TOKEN.to_string(),
            };
            assert_eq!(
                written.as_deref().map(str::trim),
                Some(expected.as_str()),
                "case {case:?}: the gated stage must have written {expected:?}, got {written:?}"
            );
            // Its output is derived from the upstream object, so it cannot have started before
            // that stage finished.
            let started_on = gated_runs[0].get("StartedOn").and_then(Value::as_f64);
            let upstream_runs = job_node_runs(case, &run, &upstreams[0]);
            let completed_on = upstream_runs
                .first()
                .and_then(|run| run.get("CompletedOn"))
                .and_then(Value::as_f64);
            assert!(
                started_on.is_some_and(|started| {
                    completed_on.is_some_and(|completed| started >= completed)
                }),
                "case {case:?}: the gated run must start at or after the stage it waited on \
                 finished, got StartedOn {started_on:?} and CompletedOn {completed_on:?}"
            );
        } else {
            assert!(
                gated_runs.is_empty(),
                "case {case:?}: an unsatisfied gate must begin no run, got {gated_runs:?}"
            );
            assert!(
                written.is_none(),
                "case {case:?}: an unsatisfied gate's stage must write nothing, got {written:?}"
            );
        }
    }
}

/// The ONE row the suite re-reads and re-runs: the recorded shape.
fn recorded_row() -> Row {
    Row {
        case: "one condition on SUCCEEDED under ANY",
        upstreams: &[Upstream::Succeeds],
        logical: Some("ANY"),
        conditions: &[(0, JOB_RUN_STATE_SUCCEEDED)],
        fires: true,
        gated: Gated::DerivesFromUpstream,
    }
}

/// A gate fires ONCE per run: re-reading a finished run initiates nothing further, and a second
/// run of the same workflow passes the same gate once of its own.
#[tokio::test]
async fn a_gate_fires_once_per_workflow_run_however_often_the_run_is_read() {
    let (router, engine) = emulator();
    create_bucket(&router).await;
    let row = recorded_row();
    let case = row.case;
    let index = 100;
    let (workflow, upstreams, gated) = lay_out(&router, index, &row, true).await;

    let first = start_run(&router, case, &workflow).await;
    let run = workflow_run_at_rest(&router, &engine, case, &workflow, &first).await;
    assert_statistics(
        case,
        &run,
        &[("TotalActions", 2), ("SucceededActions", 2)],
    );

    // A finished run is a REPORT: reading it again reports the same two actions and begins no
    // further run of the gated job.
    for turn in 0..3 {
        let again = workflow_run(&router, case, &workflow, &first).await;
        assert_statistics(
            case,
            &again,
            &[("TotalActions", 2), ("SucceededActions", 2)],
        );
        let gated_runs = job_node_runs(case, &again, &gated);
        assert_eq!(
            gated_runs.len(),
            1,
            "case {case:?}: read {turn} must report the one gated run, got {gated_runs:?}"
        );
    }

    // A second run of the same workflow passes the gate once of its own, so the gated job now
    // holds exactly two runs across the two runs of the workflow.
    let second = start_run(&router, case, &workflow).await;
    let later = workflow_run_at_rest(&router, &engine, case, &workflow, &second).await;
    assert_statistics(
        case,
        &later,
        &[("TotalActions", 2), ("SucceededActions", 2)],
    );
    assert_eq!(
        job_node_runs(case, &later, &gated).len(),
        1,
        "case {case:?}: the second run reports only the gated run IT began, got {later}"
    );

    let listed = glue(
        &router,
        case,
        "GetJobRuns",
        &json!({ "JobName": gated }),
    )
    .await;
    let runs = listed
        .get("JobRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        runs.len(),
        2,
        "case {case:?}: two runs of the workflow fire the gate twice in all, got {runs:?}"
    );
    // Every one of them was begun by the gate and follows the run that satisfied it.
    for gated_run in &runs {
        assert_eq!(
            gated_run.get("TriggerName").and_then(Value::as_str),
            Some(format!("{workflow}-gate").as_str()),
            "case {case:?}: a gated run names the trigger that began it, got {gated_run}"
        );
        let predecessors = gated_run
            .get("PredecessorRuns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            predecessors
                .iter()
                .filter_map(|predecessor| predecessor.get("JobName").and_then(Value::as_str))
                .collect::<Vec<&str>>(),
            vec![upstreams[0].as_str()],
            "case {case:?}: a gated run names the run it followed, got {gated_run}"
        );
    }
}

/// A gate the create did not ARM fires nothing; starting it arms it, and the next run of the
/// workflow passes it.
#[tokio::test]
async fn a_gate_nothing_armed_fires_nothing_until_it_is_started() {
    let (router, engine) = emulator();
    create_bucket(&router).await;
    let row = recorded_row();
    let case = "a gate the create did not arm";
    let index = 200;
    let (workflow, _upstreams, gated) = lay_out(&router, index, &row, false).await;

    let first = start_run(&router, case, &workflow).await;
    let run = workflow_run_at_rest(&router, &engine, case, &workflow, &first).await;
    assert_statistics(
        case,
        &run,
        &[("TotalActions", 1), ("SucceededActions", 1)],
    );
    assert!(
        job_node_runs(case, &run, &gated).is_empty(),
        "case {case:?}: a gate nothing armed begins no run, got {run}"
    );
    assert!(
        object_body(&router, &gated_key(index)).await.is_none(),
        "case {case:?}: a gate nothing armed writes nothing"
    );

    // "Starts an existing trigger": the gate is now armed, so the next run passes it.
    let case = "a gate a start armed";
    glue(
        &router,
        case,
        "StartTrigger",
        &json!({ "Name": format!("{workflow}-gate") }),
    )
    .await;
    let second = start_run(&router, case, &workflow).await;
    let later = workflow_run_at_rest(&router, &engine, case, &workflow, &second).await;
    assert_statistics(
        case,
        &later,
        &[("TotalActions", 2), ("SucceededActions", 2)],
    );
    assert_eq!(
        object_body(&router, &gated_key(index))
            .await
            .as_deref()
            .map(str::trim),
        Some((SEED * 3 + 7).to_string().as_str()),
        "case {case:?}: the armed gate's stage derives its output from the stage before it"
    );
}
