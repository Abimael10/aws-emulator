// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a job run reports as the resources it consumed:
//! `JobRun$ExecutionTime`, "the amount of time (in seconds) that the job run consumed
//! resources" (`smithy_query shape=JobRun member=ExecutionTime`, an `ExecutionTime` integer
//! whose modelled default is 0).
//!
//! The rule under test: `ExecutionTime` counts the resources an EXECUTOR of the run consumed,
//! so a run no executor was ever provisioned for — one the service could not dispatch at all,
//! whatever stopped it and however old the record is by the time it is read — reports the
//! modelled 0, while a run whose script really was executed reports the span its own lifecycle
//! took. Both reads of a run report the same figure, because they are one projection: the
//! single-run read (`GetJobRun`) and the per-job listing (`GetJobRuns`).
//!
//! The rows differ along what the rule generalises over: the class of failure that stopped the
//! run before anything of it ran (a role Glue cannot assume, a script the object store does
//! not serve, a script the role may not read, a language no engine here runs) and, on the other
//! side of the rule, the two ways an execution that DID happen ends (a script that exits
//! cleanly and one that raises).
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness; the scripts are
//! staged through Amazon S3's own `PutObject` and the IAM records the role resolution asks
//! about are seeded into the SAME shared engine the service reads them from. The instant each
//! run STARTED is moved back THROUGH THE STORE and the due instants of its lifecycle are made
//! due the same way rather than slept for, so every row reads a record whose own span is
//! several whole seconds wide without any outcome depending on how fast the host runs.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{glue_trust, seed_role, trust};
use crate::job_run_execution_role::{
    emulator_with_iam_and_s3, every_object, grant_role, statement,
};
use crate::job_runs::run_at_rest;
use crate::job_script_execution::{create_bucket, stage_script, BUCKET, SCRIPTS};
use crate::wire::{ok, REGION};

/// How far back a row moves the instant its run started, so the record every row reads spans
/// whole seconds of its own: a figure derived from the record's AGE would be this many seconds
/// rather than the zero a run nothing ran for owes.
const RUN_AGE: Duration = Duration::from_secs(30);

/// The fewest whole seconds a row's record has to span for the row to say anything: below
/// this the run's own age could not be told from zero.
const MIN_SPANNED_SECONDS: f64 = 1.0;

/// A script that exits cleanly, which is what a run whose work succeeded executes.
const EXITS_CLEANLY: &str = r#"
print("done")
"#;

/// A script that raises, which is a run whose EXECUTOR ran and whose work then failed.
const RAISES: &str = r#"
raise ValueError("the input partition is malformed")
"#;

/// The grants a role needs to read the script its job names.
fn reads_the_script() -> Vec<Value> {
    vec![
        statement("Allow", &["s3:GetObject"], &every_object()),
        statement("Allow", &["s3:ListBucket"], &format!("arn:aws:s3:::{BUCKET}")),
    ]
}

/// What one row does to its submission: the ONE thing that decides whether an executor was
/// ever provisioned for the run.
enum Fault {
    /// Nothing: the script is staged, readable and runnable, so an executor runs it.
    None,
    /// The script itself raises — an executor ran it all the same.
    ScriptRaises,
    /// The job's role is one Glue cannot assume, so the run is never dispatched.
    RoleGlueCannotAssume,
    /// Nothing is staged at the location the job's command names.
    ScriptAbsent,
    /// The role may not read the object the job's script location names.
    ScriptUnreadableToTheRole,
    /// The job names a `--job-language` no engine here runs.
    LanguageNotRunnable,
}

impl Fault {
    /// Whether an executor was provisioned for the run and ran its script.
    fn executor_ran(&self) -> bool {
        matches!(self, Self::None | Self::ScriptRaises)
    }

    /// The terminal `JobRunState` the run comes to rest in.
    fn terminal_state(&self) -> &'static str {
        match self {
            Self::None => JOB_RUN_STATE_SUCCEEDED,
            _ => JOB_RUN_STATE_FAILED,
        }
    }
}

/// Stage one submission — its role, its grants, its script and its job definition — start a
/// run of it, and answer the job name and the run id.
async fn start_a_submission(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    index: usize,
    fault: &Fault,
) -> (String, String) {
    let role_name = format!("execution-time-role-{index}");
    let trust_document = match fault {
        // A role trusting another service is one IAM answers that Glue cannot assume.
        Fault::RoleGlueCannotAssume => trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole"),
        _ => glue_trust(),
    };
    let role = seed_role(engine, &role_name, Some(trust_document));
    let grants = match fault {
        Fault::ScriptUnreadableToTheRole => Vec::new(),
        _ => reads_the_script(),
    };
    grant_role(engine, &role_name, &grants, &[]);

    let job_name = format!("execution_time_{index}");
    let script_key = format!("{SCRIPTS}{job_name}.py");
    let source = match fault {
        Fault::ScriptRaises => RAISES,
        _ => EXITS_CLEANLY,
    };
    let location = match fault {
        // Nothing is staged, so the location names an object the store does not serve.
        Fault::ScriptAbsent => format!("s3://{BUCKET}/{script_key}"),
        _ => stage_script(router, &script_key, source).await,
    };
    let mut body = json!({
        "Name": job_name,
        "Role": role,
        "Command": { "Name": "glueetl", "ScriptLocation": location },
        "MaxCapacity": 0.0625,
        "Timeout": 5,
    });
    if matches!(fault, Fault::LanguageNotRunnable) {
        body["DefaultArguments"] = json!({ "--job-language": "scala" });
    }
    ok(router, "CreateJob", &body).await;

    let started = ok(router, "StartJobRun", &json!({ "JobName": job_name })).await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
        .to_string();
    (job_name, run_id)
}

/// Move the instant the run `run_id` of `job_name` STARTED back by [`RUN_AGE`], so the record
/// a row reads is as old as a record a client reads minutes after starting a run.
fn backdate_the_start(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    job_name: &str,
    run_id: &str,
) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let run = store
        .job_mut(REGION, job_name)
        .and_then(|job| job.run_mut(run_id))
        .unwrap_or_else(|| panic!("case {case:?}: the store must hold run {run_id:?}"));
    run.started_on -= RUN_AGE;
}

/// The number `run` reports for `member`, which every instant and span of a finished run has.
fn reported_number(case: &str, run: &Value, member: &str) -> f64 {
    run.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: a finished run reports {member}, got {run}"))
}

/// The run `run_id` of `job_name` as the per-job LISTING reports it, which is the other read
/// of the same projection.
async fn listed_run(
    router: &MultiServiceRouter,
    case: &str,
    job_name: &str,
    run_id: &str,
) -> Value {
    let body = ok(router, "GetJobRuns", &json!({ "JobName": job_name })).await;
    body.get("JobRuns")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: GetJobRuns must report a list, got {body}"))
        .iter()
        .find(|run| run.get("Id").and_then(Value::as_str) == Some(run_id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the listing must hold run {run_id:?}, got {body}"))
}

/// A run no executor was provisioned for reports no consumed seconds however old its record
/// is, and a run whose script really was executed reports the span its lifecycle took — in
/// both reads of the one projection.
#[tokio::test]
async fn execution_time_counts_only_what_an_executor_consumed() {
    // (case, the one thing that decides whether an executor ran)
    let rows: Vec<(&str, Fault)> = vec![
        ("a script that exits cleanly", Fault::None),
        ("a script that raises", Fault::ScriptRaises),
        ("a role Glue cannot assume", Fault::RoleGlueCannotAssume),
        (
            "a script the object store does not serve",
            Fault::ScriptAbsent,
        ),
        (
            "a script the role may not read",
            Fault::ScriptUnreadableToTheRole,
        ),
        (
            "a job language no engine here runs",
            Fault::LanguageNotRunnable,
        ),
    ];

    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    for (index, (case, fault)) in rows.iter().enumerate() {
        let (job_name, run_id) = start_a_submission(&router, &engine, case, index, fault).await;
        backdate_the_start(&engine, case, &job_name, &run_id);
        let run = run_at_rest(&router, &engine, case, &job_name, &run_id).await;
        assert_eq!(
            run["JobRunState"].as_str(),
            Some(fault.terminal_state()),
            "case {case:?}: expected the run to come to rest in {}, got {run}",
            fault.terminal_state()
        );

        // The record really does span whole seconds, so reporting its AGE and reporting what
        // an executor consumed are two different figures here.
        let started = reported_number(case, &run, "StartedOn");
        let completed = reported_number(case, &run, "CompletedOn");
        let spanned = completed - started;
        assert!(
            spanned >= MIN_SPANNED_SECONDS,
            "case {case:?}: the record must span at least {MIN_SPANNED_SECONDS} second for \
             this row to say anything, got {spanned} in {run}"
        );

        let reported = reported_number(case, &run, "ExecutionTime");
        if fault.executor_ran() {
            assert!(
                (reported - spanned).abs() <= 1.0,
                "case {case:?}: a run whose script was executed reports the {spanned} seconds \
                 its lifecycle took, got {reported} in {run}"
            );
        } else {
            assert_eq!(
                reported, 0.0,
                "case {case:?}: a run no executor was provisioned for consumed no resources, \
                 got {reported} in {run}"
            );
        }

        // The listing is the same projection, so it reports the same figure.
        let listed = listed_run(&router, case, &job_name, &run_id).await;
        assert_eq!(
            reported_number(case, &listed, "ExecutionTime"),
            reported,
            "case {case:?}: the listing must report the run's own ExecutionTime, got {listed}"
        );

        // And a read moves nothing: the figure a later caller reads is the same one.
        let again = ok(
            &router,
            "GetJobRun",
            &json!({ "JobName": job_name, "RunId": run_id }),
        )
        .await;
        assert_eq!(
            reported_number(case, &again["JobRun"], "ExecutionTime"),
            reported,
            "case {case:?}: reading a settled run must not move its ExecutionTime, got {again}"
        );
    }
}

/// A run still in flight has consumed nothing to report: the member's modelled default stands
/// until the run has finished, whatever its record's age.
#[tokio::test]
async fn a_run_that_has_not_finished_reports_no_consumed_seconds() {
    let case = "a run whose lifecycle has not come to rest";
    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    let (job_name, run_id) = start_a_submission(&router, &engine, case, 99, &Fault::None).await;
    // The record is moved [`RUN_AGE`] into the past, so a figure derived from its age would be
    // that many seconds rather than the zero an unfinished run owes.
    backdate_the_start(&engine, case, &job_name, &run_id);
    // Nothing of what the clock owes this run is made due, so it is still in flight when it is
    // read.
    let answer = ok(
        &router,
        "GetJobRun",
        &json!({ "JobName": job_name, "RunId": run_id }),
    )
    .await;
    let run = &answer["JobRun"];
    assert!(
        run.get("CompletedOn").is_none_or(Value::is_null),
        "case {case:?}: a run that has not finished reports no CompletedOn, got {run}"
    );
    assert_eq!(
        run.get("ExecutionTime").and_then(Value::as_i64),
        Some(0),
        "case {case:?}: a run that has not finished consumed no whole second, got {run}"
    );
}
