// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a `DeleteJob` leaves behind: the runs already started of the
//! definition it removed.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: `DeleteJob` deletes a job DEFINITION ("deletes a specified job
//! definition"), and a run `StartJobRun` has already minted an id for is a record of its
//! own — so the read that addresses a run by its (`JobName`, `RunId`) PAIR keeps reporting
//! that run, unchanged, once the definition is gone, while every read that addresses the
//! DEFINITION — the definition itself, the listing of definitions, the batch read, the
//! tags, and the run LISTING, which is the runs *of a job* — answers the modelled
//! not-found. A name the Region holds neither a definition nor a started run under is that
//! same miss, and a create that takes the name again starts with no runs: the runs of the
//! definition that is gone are not runs of the new one.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the job
//! command, the capacity form the definition expresses, how many runs had been started of
//! it, the arguments those runs carried, whether the definition carried tags, and the form
//! of its name.
//!
//! The states a run's lifecycle owes are made due THROUGH THE STORE and every wait is a
//! poll for the CONDITION it waits on ([`crate::job_runs::runs_at_rest`]), so nothing here
//! depends on how fast the host runs.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, TaggedResourceKind, JOB_RUN_ID_PREFIX,
    MAX_DELETED_JOB_RUN_HISTORIES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{emulator_with_engine, runs_at_rest, start_run, stored_runs};
use crate::wire::{call, call_in_region, error_code, error_message, names_in, ok, REGION};

/// The role ARN every job definition in this suite is created with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// Another Region's endpoint, to show a retained run is served only by the endpoint whose
/// Region started it.
const OTHER_REGION: &str = "eu-west-1";

/// The one sentence the service answers whenever a (`JobName`, `RunId`) pair resolves to no
/// run, written out here rather than read from the crate's constant so this suite pins the
/// wording.
const JOB_RUN_NOT_FOUND: &str = "Job run not found for the given job name and runId.";

/// The one sentence the reads that address a job DEFINITION answer for a name this Region
/// holds no definition under, written out here for the same reason.
const JOB_NOT_FOUND: &str = "Job not found.";

/// One row: a job definition to store, how many runs to start of it, and the arguments
/// those runs carry.
struct Row {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The job definition's name.
    name: String,
    /// The `JobCommand$Name` of the stored definition.
    command: &'static str,
    /// The members the definition carries beside `Name`, `Role` and `Command`.
    extras: Value,
    /// How many runs `StartJobRun` starts of it before the delete.
    runs: usize,
    /// The `Arguments` each of those starts carries, if any.
    arguments: Option<Value>,
}

/// The rows: the class the rule generalises over, one row per axis.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Spark ETL definition with one run",
            name: "etl-usage-events".to_string(),
            command: "glueetl",
            extras: json!({ "WorkerType": "G.1X", "NumberOfWorkers": 2, "GlueVersion": "4.0" }),
            runs: 1,
            arguments: None,
        },
        Row {
            case: "a Python shell definition sized in DPUs, with three runs",
            name: "python shell report".to_string(),
            command: "pythonshell",
            extras: json!({
                "MaxCapacity": 0.0625,
                "ExecutionProperty": { "MaxConcurrentRuns": 3 },
                "Tags": { "owner": "data-platform" },
            }),
            runs: 3,
            arguments: Some(json!({ "--day": "2026-11-02" })),
        },
        Row {
            case: "a streaming definition with two runs carrying arguments",
            name: "Stream.Ingest_job".to_string(),
            command: "gluestreaming",
            extras: json!({
                "GlueVersion": "3.0",
                "ExecutionProperty": { "MaxConcurrentRuns": 2 },
                "Timeout": 5,
            }),
            runs: 2,
            arguments: Some(json!({ "--window": "60", "--sink": "s3://lake/out/" })),
        },
        Row {
            case: "a Ray definition at a name of the modelled maximum length",
            // `NameString`'s `length 1..255` (smithy_query operation=DeleteJob): the
            // longest name a job can be stored under.
            name: "r".repeat(255),
            command: "glueray",
            extras: json!({ "WorkerType": "Z.2X", "NumberOfWorkers": 2, "GlueVersion": "4.0" }),
            runs: 1,
            arguments: None,
        },
        Row {
            case: "a one-character name with no capacity members at all",
            // The other published bound of that same `length 1..255`.
            name: "j".to_string(),
            command: "glueetl",
            extras: json!({}),
            runs: 2,
            arguments: None,
        },
    ]
}

/// The `CreateJob` body one row stores.
fn create_body(row: &Row) -> Value {
    let mut body = json!({
        "Name": row.name,
        "Role": ROLE_ARN,
        "Command": { "Name": row.command, "ScriptLocation": "s3://etl-scripts/etl.py" },
    });
    if let Some(members) = row.extras.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    if row.runs > 1 && body.get("ExecutionProperty").is_none() {
        // "The maximum number of concurrent runs allowed for the job": the row holds
        // several runs at once, so the stored definition admits them.
        body["ExecutionProperty"] = json!({ "MaxConcurrentRuns": row.runs });
    }
    body
}

/// Assert that `operation` answered `name` with the modelled miss carrying `expected`.
async fn assert_miss(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    expected: &str,
) {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?} / {operation}: expected 400, got {status} with {answer}"
    );
    assert!(
        error_code(&answer).ends_with("EntityNotFoundException"),
        "case {case:?} / {operation}: expected EntityNotFoundException, got {answer}"
    );
    assert_eq!(
        error_message(&answer),
        expected,
        "case {case:?} / {operation}: expected the service's own sentence {expected:?}, got \
         {answer}"
    );
}

/// The `JobRun` a read of one run reports, which must be served.
async fn read_run(router: &MultiServiceRouter, case: &str, job_name: &str, run_id: &str) -> Value {
    let body = ok(
        router,
        "GetJobRun",
        &json!({ "JobName": job_name, "RunId": run_id }),
    )
    .await;
    body.get("JobRun")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetJobRun must report a JobRun, got {body}"))
}

/// A delete removes the DEFINITION and leaves the runs already started of it readable, one
/// by one, under the ids `StartJobRun` handed out — reporting exactly the records the reads
/// before the delete reported.
#[tokio::test]
async fn a_run_outlives_the_job_definition_it_was_started_of() {
    for row in rows() {
        let (router, engine) = emulator_with_engine();
        let case = row.case;
        // A bystander beside each row's job, so anything the delete took beyond the one
        // record it names is visible.
        let bystander = "bystander-etl";
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": bystander,
                "Role": ROLE_ARN,
                "Command": {
                    "Name": "glueetl",
                    "ScriptLocation": "s3://etl-scripts/bystander.py",
                },
            }),
        )
        .await;
        let bystander_run = ok(&router, "StartJobRun", &json!({ "JobName": bystander })).await
            ["JobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the bystander's start must answer an id"))
            .to_string();

        ok(&router, "CreateJob", &create_body(&row)).await;
        let mut started: Vec<String> = Vec::new();
        for _ in 0..row.runs {
            let mut body = json!({ "JobName": row.name });
            if let Some(arguments) = &row.arguments {
                body["Arguments"] = arguments.clone();
            }
            let answer = ok(&router, "StartJobRun", &body).await;
            started.push(
                answer["JobRunId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: StartJobRun must answer a JobRunId, got {answer}")
                    })
                    .to_string(),
            );
        }
        // Every run is brought to rest BEFORE the delete, so what the reads after it report
        // is a settled record and no state is owed to a definition that is gone.
        runs_at_rest(&router, &engine, case, &row.name).await;
        let mut before: Vec<Value> = Vec::new();
        for run_id in &started {
            before.push(read_run(&router, case, &row.name, run_id).await);
        }

        let deleted = ok(&router, "DeleteJob", &json!({ "JobName": row.name })).await;
        assert_eq!(
            deleted["JobName"].as_str(),
            Some(row.name.as_str()),
            "case {case:?}: the delete must answer with the name it deleted, got {deleted}"
        );

        // The DEFINITION is gone, in every projection of the job namespace.
        assert_miss(
            &router,
            case,
            "GetJob",
            &json!({ "JobName": row.name }),
            JOB_NOT_FOUND,
        )
        .await;
        assert_miss(
            &router,
            case,
            "GetJobRuns",
            &json!({ "JobName": row.name }),
            JOB_NOT_FOUND,
        )
        .await;
        let listed = ok(&router, "ListJobs", &json!({})).await;
        assert_eq!(
            listed["JobNames"],
            json!([bystander]),
            "case {case:?}: the delete must leave only the bystander listed, got {listed}"
        );
        let batch = ok(
            &router,
            "BatchGetJobs",
            &json!({ "JobNames": [row.name, bystander] }),
        )
        .await;
        assert_eq!(
            batch["JobsNotFound"],
            json!([row.name]),
            "case {case:?}: the batch read must report the deleted name as missing, got {batch}"
        );
        assert_eq!(
            names_in(&batch, "Jobs"),
            vec![bystander.to_string()],
            "case {case:?}: the batch read must describe only the bystander, got {batch}"
        );
        assert_miss(
            &router,
            case,
            "GetTags",
            &json!({ "ResourceArn": TaggedResourceKind::Job.arn(REGION, &row.name) }),
            JOB_NOT_FOUND,
        )
        .await;

        // Each run the definition had started is still reported under the id the start
        // handed out, and reports exactly what it reported while the definition stood.
        for (run_id, expected) in started.iter().zip(before.iter()) {
            let after = read_run(&router, case, &row.name, run_id).await;
            assert_eq!(
                &after, expected,
                "case {case:?}: run {run_id:?} must describe itself the same way after the \
                 delete; expected {expected}, got {after}"
            );
            // Including the members the SERVICE supplies a value for, which a read of a run
            // reports whatever its job definition's fate: the arguments the run was started
            // with (the empty map for a start that named none), the queuing flag the service
            // applied, the log group the run's logs are written under, and the runs that
            // preceded it (the empty list for a run nothing preceded).
            assert_eq!(
                after.get("ExecutionArguments"),
                Some(&row.arguments.clone().unwrap_or_else(|| json!({}))),
                "case {case:?}: run {run_id:?} must report the arguments it was started \
                 with, got {after}"
            );
            assert_eq!(
                after.get("JobRunQueuingEnabled").and_then(Value::as_bool),
                Some(false),
                "case {case:?}: run {run_id:?} must report the queuing flag the service \
                 applied, got {after}"
            );
            assert!(
                after
                    .get("LogGroupName")
                    .and_then(Value::as_str)
                    .is_some_and(|group| group.starts_with("/aws-glue/")),
                "case {case:?}: run {run_id:?} must report the log group its logs are \
                 written under, got {after}"
            );
            assert_eq!(
                after.get("PredecessorRuns"),
                Some(&json!([])),
                "case {case:?}: run {run_id:?} must report the runs that preceded it, got \
                 {after}"
            );
        }

        // What the pair resolves is a RUN of that name and nothing else: another job's run
        // id, and an id nothing minted, are the run miss under the deleted name — and the
        // deleted name's own run is served only by the Region that started it.
        assert_miss(
            &router,
            case,
            "GetJobRun",
            &json!({ "JobName": row.name, "RunId": bystander_run }),
            JOB_RUN_NOT_FOUND,
        )
        .await;
        assert_miss(
            &router,
            case,
            "GetJobRun",
            &json!({ "JobName": row.name, "RunId": "jr_0000000000000000" }),
            JOB_RUN_NOT_FOUND,
        )
        .await;
        let (status, answer) = call_in_region(
            &router,
            OTHER_REGION,
            "GetJobRun",
            &json!({ "JobName": row.name, "RunId": started[0] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: another Region must not serve this Region's run, got {status} with \
             {answer}"
        );

        // The bystander and its own run are untouched.
        let kept = ok(&router, "GetJob", &json!({ "JobName": bystander })).await;
        assert_eq!(
            kept["Job"]["Name"].as_str(),
            Some(bystander),
            "case {case:?}: the delete must leave the bystander readable, got {kept}"
        );
        let kept_run = read_run(&router, case, bystander, &bystander_run).await;
        assert_eq!(
            kept_run["Id"].as_str(),
            Some(bystander_run.as_str()),
            "case {case:?}: the bystander's run must still be readable, got {kept_run}"
        );

        // A create takes the name again: the new definition starts with no runs, and the
        // runs of the definition that is gone are not reachable under it any more.
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": row.name,
                "Role": ROLE_ARN,
                "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl-scripts/after.py" },
            }),
        )
        .await;
        let history = ok(&router, "GetJobRuns", &json!({ "JobName": row.name })).await;
        assert_eq!(
            history["JobRuns"],
            json!([]),
            "case {case:?}: the re-created job starts with no runs, got {history}"
        );
        for run_id in &started {
            assert_miss(
                &router,
                case,
                "GetJobRun",
                &json!({ "JobName": row.name, "RunId": run_id }),
                JOB_RUN_NOT_FOUND,
            )
            .await;
        }
    }
}

/// The runs a delete leaves behind are bounded: past
/// [`MAX_DELETED_JOB_RUN_HISTORIES`] of them the history whose delete is OLDEST is the one
/// dropped, so the runs of the most recently deleted definitions are the ones still
/// readable.
///
/// ONE run is really started and brought to rest; every cycle below is then seeded with a
/// copy of that settled record through the store, the way the capacity suites of this crate
/// reach it, because what is under test is the ORDER the retained histories are evicted in
/// and not the start of a run.
#[tokio::test]
async fn the_runs_delete_leaves_behind_are_bounded_oldest_first() {
    let (router, engine) = emulator_with_engine();
    let template_job = "retention-template";
    ok(
        &router,
        "CreateJob",
        &json!({
            "Name": template_job,
            "Role": ROLE_ARN,
            "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl-scripts/etl.py" },
        }),
    )
    .await;
    start_run(&router, &json!({ "JobName": template_job })).await;
    runs_at_rest(&router, &engine, "the template run", template_job).await;
    let template = stored_runs(&engine, template_job)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the template job must hold the run it started"));
    assert!(
        template.completed_on.is_some() && !template.awaiting_script,
        "the template run must have come to rest, and it is {template:?}"
    );

    // One more definition than the store keeps histories for, each created, given a run and
    // deleted — so exactly one history has to go, and it is the first one's.
    let cycles = MAX_DELETED_JOB_RUN_HISTORIES + 1;
    let mut runs: Vec<(String, String)> = Vec::new();
    for index in 0..cycles {
        let name = format!("cycled-{index}");
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": name,
                "Role": ROLE_ARN,
                "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl-scripts/etl.py" },
            }),
        )
        .await;
        // The id has the form the service mints a run id in ([`JOB_RUN_ID_PREFIX`] and 64
        // hexadecimal characters), and one per cycle, so each cycle addresses its own run.
        let run_id = format!("{JOB_RUN_ID_PREFIX}{index:064x}");
        {
            let state = engine.state::<ComAmazonawsGlueAwsglueState>();
            let mut store = state.lock();
            let job = store
                .job_mut(REGION, &name)
                .unwrap_or_else(|| panic!("cycle {index}: the store must hold job {name:?}"));
            let mut run = template.clone();
            run.id = run_id.clone();
            run.job_name = name.clone();
            job.record_run(run);
        }
        ok(&router, "DeleteJob", &json!({ "JobName": name })).await;
        runs.push((name, run_id));
    }

    let (oldest_name, oldest_run) = &runs[0];
    assert_miss(
        &router,
        "the oldest deleted definition's run",
        "GetJobRun",
        &json!({ "JobName": oldest_name, "RunId": oldest_run }),
        JOB_RUN_NOT_FOUND,
    )
    .await;
    for (name, run_id) in &runs[1..] {
        let run = read_run(&router, "a later deleted definition's run", name, run_id).await;
        assert_eq!(
            run["Id"].as_str(),
            Some(run_id.as_str()),
            "case {name:?}: the run of a definition deleted inside the window must still be \
             readable, got {run}"
        );
    }
}
