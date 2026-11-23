// SPDX-License-Identifier: CC0-1.0

//! Athena's CALCULATION run, reads and cancellation, driven end to end over the same router
//! the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`), so every
//! assertion is about what an SDK client observes: `StartCalculationExecution` "submits
//! calculations for execution within a session", `GetCalculationExecution` "describes a
//! previously submitted calculation execution", `GetCalculationExecutionStatus` "gets the
//! status of a current calculation", and `StopCalculationExecution` "requests the
//! cancellation of a calculation".
//!
//! THE RULES UNDER TEST:
//!
//! 1. A submitted calculation RUNS: the code block is evaluated, and what a read of the
//!    completed calculation reports is what that code COMPUTED — its `stdout` saved as the
//!    object the read names, under the working directory of the session it ran in — with the
//!    DPU execution time the host measured beside it. A code block that raises FAILS, and its
//!    `stderr` object carries the exception the run produced.
//! 2. A read reports what the submission persisted: the id it was minted under, the session
//!    it ran in, the description it carried and that working directory. `Result` is
//!    "populated only if the calculation is completed" and `Statistics` "only when statistics
//!    are available", so neither is reported before the run is over.
//! 3. The status a full read and a status read report for one calculation at the same instant
//!    is the same status, out of the same record.
//! 4. A cancellation is a request the clock owes: a calculation that has not finished is
//!    `CANCELING` from the instant it is asked for and `CANCELED` once the stop is done, which
//!    is when it reports a `CompletionDateTime` — while "a `StopCalculationExecution` call on
//!    a calculation that is already in a terminal state […] succeeds but has no effect", so
//!    such a calculation keeps the state and the completion instant it reached.
//! 5. A `CalculationExecutionId` this account minted no calculation under is refused by each
//!    of those operations with the class and the sentence this service pairs for a reference
//!    it will not act on, and nothing is cancelled for it.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the code
//! the calculation runs (one expression, a loop whose answer is in no text, the standard
//! library, text outside ASCII, code that raises), the description a submission carried (none,
//! one line, text outside ASCII, the modelled maximum length), which session the calculation
//! ran in (started in a notebook, started for programmatic access), which operation resolves
//! the id, and the shape of an id that names no calculation.
//!
//! Nothing here waits on the wall clock: a session is seeded past the span its resources are
//! due after, a calculation that must sit at a given point of its own lifecycle is seeded with
//! the instants that put it there, and the transitions the clock owes are waited on by POLLING
//! the service for the state itself, under a deadline far above the span they are due after.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, CalculationOutcome, CalculationRecord, SessionRecord, CALCULATION_NOT_FOUND,
    CALCULATION_STATE_CANCELED, CALCULATION_STATE_CANCELING, CALCULATION_STATE_COMPLETED,
    CALCULATION_STATE_CREATED, CALCULATION_STATE_FAILED, CALCULATION_STATE_RUNNING,
    ERROR_CODE_UNAUTHORIZED, PYSPARK_ENGINE_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call};
use crate::query_result_objects::{object, parse_location};

/// Athena and Amazon S3 mounted on ONE shared engine, with the bucket the workgroup writes
/// into held in it.
///
/// Both are needed because a calculation's own output is SAVED as objects: what the control
/// plane names in a read is what a client then fetches out of the object store, so the suite
/// drives both over one router, exactly as `tests/all/query_result_objects.rs` does for a
/// query's results. The engine comes back as well, for the records a row seeds.
fn engine_and_emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    crate::results_bucket::hold_results_bucket(&engine, OUTPUT_LOCATION);
    (engine, router)
}

/// The Spark enabled workgroup the notebooks and sessions below live in.
const WORK_GROUP: &str = "spark-calculation-reads";

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// Where that workgroup writes its results, which is the working directory a session started
/// in it runs under and therefore the one its calculations save their output under.
const OUTPUT_LOCATION: &str = "s3://athena-results/calculations/";

/// A span long enough that a record seeded with it is past every span this emulator owes — or,
/// used the other way, nowhere near due — for as long as any test run takes.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// The longest description a submission may carry: the modelled `DescriptionString` length
/// (1..1024), from `smithy_query(query="operation",operation="StartCalculationExecution")`.
const MAX_DESCRIPTION_LENGTH: usize = 1_024;

/// The ceiling of the modelled `CalculationExecutionId` length (1..36).
const MAX_CALCULATION_ID_LENGTH: usize = 36;

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span either transition is due after, so the
/// CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// Create the Spark enabled workgroup the sessions below run in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": EXECUTION_ROLE,
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {WORK_GROUP} must be created, got {status} with {created:?}"
    );
}

/// Create the notebook `name` in that workgroup and hand back the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": WORK_GROUP, "Name": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be created, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create must answer with a NotebookId, got {created:?}"))
        .to_string()
}

/// Seed one running session into `engine`'s store, bound to `notebook_id`, and hand back its
/// `SessionId`.
///
/// Seeded through this service's own [`SessionRecord`] so the session is past the span its
/// resources are due after without waiting for it; every call under test is made over the
/// wire.
fn given_session(engine: &engine::InMemoryEngine, notebook_id: Option<&str>) -> String {
    let id = state::new_session_id();
    let started_at = SystemTime::now()
        .checked_sub(WELL_PAST_ANY_RUN)
        .expect("an instant this far back exists");
    let mut record = SessionRecord::started(
        id.clone(),
        WORK_GROUP.to_string(),
        notebook_id.map(str::to_string),
        started_at,
    );
    // The working directory a calculation of this session saves its output under is the one
    // the session was STARTED under, which is the workgroup's result configuration.
    record.configuration.working_directory = Some(OUTPUT_LOCATION.to_string());
    assert!(
        record.is_active_at(SystemTime::now()),
        "a seeded session must be running, got the state {:?}",
        record.state_at(SystemTime::now())
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().sessions.insert(record);
    id
}

/// Seed one calculation of `session_id` into `engine`'s store, at the point of its lifecycle
/// `finished` names, and hand back its `CalculationExecutionId`.
///
/// Seeding is what puts a calculation at an exact point of its own lifecycle without waiting
/// for the spans this emulator owes: `finished` false leaves its completion far from due, so
/// it is RUNNING for as long as any test run takes, and `finished` true puts it past a
/// completion that has already fallen due.
fn given_seeded_calculation(
    engine: &engine::InMemoryEngine,
    session_id: &str,
    finished: bool,
) -> String {
    let now = SystemTime::now();
    let mut record = CalculationRecord::submitted(
        state::new_calculation_execution_id(),
        session_id.to_string(),
        WORK_GROUP.to_string(),
        "print('seeded')".to_string(),
        now,
    );
    let id = record.id.clone();
    if finished {
        record.submitted_at = now
            .checked_sub(WELL_PAST_ANY_RUN)
            .expect("an instant this far back exists");
        record.completion_due_at = record.submitted_at;
        record.outcome = Some(CalculationOutcome {
            failed: false,
            run_time: Duration::from_millis(1),
            stdout_uri: None,
            stderr_uri: None,
            result_uri: None,
        });
    } else {
        record.completion_due_at = now
            .checked_add(WELL_PAST_ANY_RUN)
            .expect("an instant this far ahead exists");
        record.submitted_at = now
            .checked_sub(WELL_PAST_ANY_RUN)
            .expect("an instant this far back exists");
    }
    let expected = if finished {
        CALCULATION_STATE_COMPLETED
    } else {
        CALCULATION_STATE_RUNNING
    };
    let seeded = record.state_at(SystemTime::now());
    assert_eq!(
        seeded, expected,
        "a calculation seeded as finished={finished} must report {expected:?}, got {seeded:?}"
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().calculations.insert(record, None);
    id
}

/// Submit one calculation into `session_id` and hand back its `CalculationExecutionId`.
async fn given_calculation(
    router: &MultiServiceRouter,
    session_id: &str,
    code_block: &str,
    description: Option<&str>,
) -> String {
    let mut request = json!({ "SessionId": session_id, "CodeBlock": code_block });
    if let (Some(body), Some(description)) = (request.as_object_mut(), description) {
        body.insert("Description".to_string(), json!(description));
    }
    let (status, submitted) = call(router, "StartCalculationExecution", &request).await;
    assert_eq!(
        status, 200,
        "a calculation must be submitted, got {status} with {submitted:?}"
    );
    assert_eq!(
        submitted["State"].as_str(),
        Some(CALCULATION_STATE_CREATED),
        "a submission must answer with the state it created the calculation in, got {submitted:?}"
    );
    submitted["CalculationExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a submission must answer with an id, got {submitted:?}"))
        .to_string()
}

/// `GetCalculationExecution` for `id`, which must be served.
async fn read(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetCalculationExecution",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCalculationExecution({id}) must answer 200, got {status} with {body:?}"
    );
    body
}

/// `GetCalculationExecutionStatus` for `id`, which must be served.
async fn read_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetCalculationExecutionStatus",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCalculationExecutionStatus({id}) must answer 200, got {status} with \
         {body:?}"
    );
    body
}

/// `StopCalculationExecution` for `id`, answering the `State` it reports.
async fn stop(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let (status, body) = call(
        router,
        "StopCalculationExecution",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StopCalculationExecution({id}) must answer 200, got {status} with {body:?}"
    );
    body["State"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a stop must answer with a State, got {body:?}"))
        .to_string()
}

/// Poll the calculation `id` until it reports `wanted`, and answer the read that reported it.
async fn settled_in(router: &MultiServiceRouter, case: &str, id: &str, wanted: &str) -> Value {
    for _ in 0..POLL_LOOKS {
        let found = read(router, case, id).await;
        if found["Status"]["State"].as_str() == Some(wanted) {
            return found;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the calculation {id} never reached {wanted}");
}

/// Assert the INVARIANT that holds of a read at any instant: `Result` is reported exactly for
/// a calculation that completed, a `CompletionDateTime` exactly for one that is no longer
/// running, and `Statistics` never while one is still running.
fn assert_read_invariants(case: &str, found: &Value) {
    let state = found["Status"]["State"].as_str().unwrap_or_default();
    let terminal = matches!(
        state,
        CALCULATION_STATE_COMPLETED | CALCULATION_STATE_FAILED | CALCULATION_STATE_CANCELED
    );
    assert_eq!(
        !found["Result"].is_null(),
        state == CALCULATION_STATE_COMPLETED,
        "case {case:?}: a Result is reported exactly for a completed calculation, got {found:?}"
    );
    // Statistics are the HOST's measurement of a run, so they belong to a calculation whose
    // run reported: a cancelled one whose code never ran has none to report, and no
    // calculation reports them while it is still running.
    assert!(
        found["Statistics"].is_null() || terminal,
        "case {case:?}: no Statistics may be reported while the calculation is still running, \
         got {found:?}"
    );
    assert_eq!(
        !found["Status"]["CompletionDateTime"].is_null(),
        terminal,
        "case {case:?}: a CompletionDateTime is reported exactly for a calculation that is no \
         longer running, got {found:?}"
    );
    assert!(
        !found["Status"]["SubmissionDateTime"].is_null(),
        "case {case:?}: every calculation reports when it was submitted, got {found:?}"
    );
}

/// RULE 1: a calculation RUNS, and what the completed read reports is what its code computed —
/// saved as the object that read names, under the session's working directory.
///
/// The rows are code blocks whose answer no part of this emulator holds: a sum, a loop, the
/// standard library's own rendering, text outside ASCII, and code that raises.
#[tokio::test]
async fn a_calculation_runs_its_code_and_saves_the_output_the_read_names() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "computed").await;

    // (case, the session's notebook, the code block, the stdout it must produce, whether it
    // must fail)
    let rows: Vec<(&str, Option<String>, &str, &str, bool)> = vec![
        (
            "one sum, in a notebook session",
            Some(notebook.clone()),
            "print(5 + 6)",
            "11\n",
            false,
        ),
        (
            "a loop whose answer is in no text",
            None,
            "total = 0\nfor index in range(1, 101):\n    total += index\nprint(total)\n",
            "5050\n",
            false,
        ),
        (
            "the standard library's own rendering",
            None,
            "import json\nprint(json.dumps({'a': [1, 2]}, separators=(',', ':')))",
            "{\"a\":[1,2]}\n",
            false,
        ),
        (
            "text outside ASCII",
            None,
            "print('héllo wörld ✓'.upper())",
            "HÉLLO WÖRLD ✓\n",
            false,
        ),
        (
            "code that raises",
            None,
            "print('before')\nraise ValueError('no')",
            "before\n",
            true,
        ),
    ];

    for (case, bound_to, code_block, expected_stdout, must_fail) in &rows {
        let session = given_session(&engine, bound_to.as_deref());
        let id = given_calculation(&router, &session, code_block, None).await;
        assert!(
            !id.is_empty() && id.len() <= MAX_CALCULATION_ID_LENGTH,
            "case {case:?}: expected an id inside the modelled length 1..\
             {MAX_CALCULATION_ID_LENGTH}, got {id:?}"
        );

        let wanted = if *must_fail {
            CALCULATION_STATE_FAILED
        } else {
            CALCULATION_STATE_COMPLETED
        };
        let found = settled_in(&router, case, &id, wanted).await;
        assert_read_invariants(case, &found);

        // The DPU execution time is the measurement the host took of the run, so it is there
        // and it is not negative; nothing here asserts how long the host took.
        let measured = found["Statistics"]["DpuExecutionInMillis"].as_i64();
        assert!(
            measured.is_some_and(|millis| millis >= 0),
            "case {case:?}: a calculation that ran must report the time its run took, got \
             {found:?}"
        );

        // A FAILED calculation reports no result location, so its output is read off the
        // objects the run wrote under the folder the completed rows name.
        if *must_fail {
            assert!(
                found["Result"].is_null(),
                "case {case:?}: a failed calculation reports no result, got {found:?}"
            );
            continue;
        }

        let stdout_uri = found["Result"]["StdOutS3Uri"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a StdOutS3Uri, got {found:?}"))
            .to_string();
        let (bucket, key) = parse_location(case, &stdout_uri);
        let (status, body) = object(&router, &bucket, &key).await;
        assert_eq!(
            (status, body.as_str()),
            (200, *expected_stdout),
            "case {case:?}: the object the read names must hold what the code computed, got \
             {status} with {body:?}"
        );

        // The two objects sit under the folder the read names, which is the session's working
        // directory with the session and the calculation under it.
        let folder = found["Result"]["ResultS3Uri"].as_str().unwrap_or_default();
        assert!(
            stdout_uri.starts_with(folder) && folder.contains(&session) && folder.contains(&id),
            "case {case:?}: the folder must be the one the output was saved under, got \
             {folder:?} for {stdout_uri:?}"
        );
        let stderr_uri = found["Result"]["StdErrorS3Uri"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a StdErrorS3Uri, got {found:?}"))
            .to_string();
        let (bucket, key) = parse_location(case, &stderr_uri);
        let (status, errors) = object(&router, &bucket, &key).await;
        assert_eq!(
            (status, errors.as_str()),
            (200, ""),
            "case {case:?}: a calculation that completed wrote nothing to its error stream, got \
             {status} with {errors:?}"
        );
    }
}

/// RULE 1 and RULE 2 and RULE 3: a read reports what the submission persisted beside the state
/// the record reads as, the two reads of one calculation agree, and a calculation that raised
/// reports the exception its run produced on its error stream.
#[tokio::test]
async fn a_read_reports_what_the_submission_persisted() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "described").await;
    let long_description = "d".repeat(MAX_DESCRIPTION_LENGTH);

    // (case, the session's notebook, the code submitted, the description it carried)
    let rows: Vec<(&str, Option<String>, &str, Option<&str>)> = vec![
        (
            "no description, in a notebook session",
            Some(notebook.clone()),
            "print(1 + 1)",
            None,
        ),
        (
            "one line of description, in a notebook session",
            Some(notebook.clone()),
            "1",
            Some("the first cell"),
        ),
        (
            "a description outside ASCII, in a programmatic session",
            None,
            "print('héllo')",
            Some("première cellule ✓"),
        ),
        (
            "the modelled maximum description, in a programmatic session",
            None,
            "",
            Some(long_description.as_str()),
        ),
    ];

    for (case, bound_to, code_block, description) in &rows {
        let session = given_session(&engine, bound_to.as_deref());
        let id = given_calculation(&router, &session, code_block, *description).await;

        let found = read(&router, case, &id).await;
        assert_eq!(
            found["CalculationExecutionId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the read must be of the calculation that was asked for, got {found:?}"
        );
        assert_eq!(
            found["SessionId"].as_str(),
            Some(session.as_str()),
            "case {case:?}: the read must report the session the calculation ran in, got {found:?}"
        );
        assert_eq!(
            found["Description"].as_str(),
            *description,
            "case {case:?}: the read must report the description the submission carried, got \
             {found:?}"
        );
        assert_eq!(
            found["WorkingDirectory"].as_str(),
            Some(OUTPUT_LOCATION),
            "case {case:?}: the read must report the session's working directory, got {found:?}"
        );
        assert_read_invariants(case, &found);

        // RULE 3: the status read is the same projection of the same record, at every point of
        // the calculation's life — before the completion falls due and after it.
        let status = read_status(&router, case, &id).await;
        assert!(
            status["Status"]["State"].as_str().is_some(),
            "case {case:?}: a status read must report a state, got {status:?}"
        );
        let settled = settled_in(&router, case, &id, CALCULATION_STATE_COMPLETED).await;
        let status = read_status(&router, case, &id).await;
        assert_eq!(
            status["Status"], settled["Status"],
            "case {case:?}: a status read must report the status the full read reports"
        );
        assert_eq!(
            status["Statistics"], settled["Statistics"],
            "case {case:?}: a status read must report the statistics the full read reports"
        );
    }
}

/// RULE 4: a stop cancels a calculation that has not finished — `CANCELING` from the instant it
/// is asked for and `CANCELED` once the stop is done — and a stop of one that is already in a
/// terminal state succeeds and has no effect.
#[tokio::test]
async fn a_stop_cancels_a_running_calculation_and_leaves_a_finished_one_alone() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "cancelled").await;

    // The rows differ along which session the calculation ran in, so the cancellation is the
    // calculation's own rather than anything about a notebook.
    let rows: Vec<(&str, Option<String>)> = vec![
        (
            "a calculation of a notebook session",
            Some(notebook.clone()),
        ),
        ("a calculation of a programmatic session", None),
    ];
    for (case, bound_to) in &rows {
        let session = given_session(&engine, bound_to.as_deref());
        // Seeded as still running, so the stop below reaches a calculation that has not
        // finished however loaded the host is.
        let running = given_seeded_calculation(&engine, &session, false);
        let finished = given_seeded_calculation(&engine, &session, true);

        let asked = stop(&router, case, &running).await;
        assert_eq!(
            asked, CALCULATION_STATE_CANCELING,
            "case {case:?}: a stop must report the cancellation it received, got {asked:?}"
        );
        let canceling = read(&router, case, &running).await;
        assert!(
            canceling["Status"]["CompletionDateTime"].is_null(),
            "case {case:?}: a calculation still stopping has not completed, got {canceling:?}"
        );
        assert_read_invariants(case, &canceling);

        // Waited on by POLLING for the state, never by sleeping out the span.
        let settled = settled_in(&router, case, &running, CALCULATION_STATE_CANCELED).await;
        let completed = settled["Status"]["CompletionDateTime"].as_f64();
        assert!(
            completed.is_some(),
            "case {case:?}: a cancelled calculation must report when it completed, got {settled:?}"
        );
        assert_read_invariants(case, &settled);

        // A repeat succeeds and moves neither the state nor that instant.
        let repeated = stop(&router, case, &running).await;
        assert_eq!(
            repeated, CALCULATION_STATE_CANCELED,
            "case {case:?}: a repeated stop must report the state it is already in, got \
             {repeated:?}"
        );
        let after = read(&router, case, &running).await;
        assert_eq!(
            after["Status"]["CompletionDateTime"].as_f64(),
            completed,
            "case {case:?}: a repeated stop must not move the completion instant, got {after:?}"
        );

        // The calculation that had already finished keeps the state it reached, which is what
        // "succeeds but has no effect" means.
        let before = read(&router, case, &finished).await;
        let stopped = stop(&router, case, &finished).await;
        assert_eq!(
            stopped, CALCULATION_STATE_COMPLETED,
            "case {case:?}: a stop of a finished calculation reports the state it is in, got \
             {stopped:?}"
        );
        let unchanged = read(&router, case, &finished).await;
        assert_eq!(
            unchanged["Status"], before["Status"],
            "case {case:?}: a stop of a finished calculation changes nothing about it"
        );

        // ... and the listing of that session reports each one under the state it is in, which
        // is what a client narrowing by `StateFilter` reads.
        for (state, wanted) in [
            (CALCULATION_STATE_CANCELED, &running),
            (CALCULATION_STATE_COMPLETED, &finished),
        ] {
            let (status, listed) = call(
                &router,
                "ListCalculationExecutions",
                &json!({ "SessionId": session, "StateFilter": state }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the listing must answer 200, got {status} with {listed:?}"
            );
            let reported: Vec<&str> = listed["Calculations"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry["CalculationExecutionId"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                reported,
                vec![wanted.as_str()],
                "case {case:?}: the calculations in {state:?} must be [{wanted:?}], got {listed:?}"
            );
        }
    }
}

/// RULE 5: an id this account minted no calculation under is refused by each of these
/// operations with the reference class and the sentence this service pairs for it, and the
/// calculation this account does hold still reads — so the rows are about the id and not about
/// the operations refusing everything.
#[tokio::test]
async fn an_unminted_calculation_is_refused_by_every_reader_and_by_the_stop() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "references").await;
    let session = given_session(&engine, Some(&notebook));
    let held = given_calculation(&router, &session, "1", None).await;
    let flipped = format!(
        "{}{}",
        if held.starts_with('a') { 'b' } else { 'a' },
        &held[1..]
    );

    let rows: Vec<(&str, String)> = vec![
        (
            "a UUID nothing was minted under",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("an id at the shortest the model admits", "c".to_string()),
        (
            "an id at the longest the model admits",
            "c".repeat(MAX_CALCULATION_ID_LENGTH),
        ),
        ("the notebook's own id", notebook.clone()),
        ("one character from a held id", flipped),
    ];
    let operations = [
        "GetCalculationExecution",
        "GetCalculationExecutionStatus",
        "StopCalculationExecution",
    ];
    for (row, unknown) in &rows {
        assert_ne!(
            unknown, &held,
            "case {row:?}: the unknown id must not be the held calculation's"
        );
        for operation in operations {
            let case = format!("{operation} with {row}");
            let (status, refused) = call(
                &router,
                operation,
                &json!({ "CalculationExecutionId": unknown }),
            )
            .await;
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_UNAUTHORIZED,
                CALCULATION_NOT_FOUND,
            );
        }
    }

    let found = read(&router, "the held calculation", &held).await;
    assert_ne!(
        found["Status"]["State"].as_str(),
        Some(CALCULATION_STATE_CANCELED),
        "case \"the held calculation\": nothing above may have cancelled it, got {found:?}"
    );
}
