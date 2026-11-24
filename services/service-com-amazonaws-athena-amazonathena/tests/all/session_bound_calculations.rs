// SPDX-License-Identifier: CC0-1.0

//! Athena's calculations as CELLS OF THEIR SESSION, driven end to end over the same router
//! the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`), so every
//! assertion is about what an SDK client observes.
//!
//! THE RULES UNDER TEST:
//!
//! 1. A session takes a calculation only while it is able to accept one — "IDLE - The session
//!    is able to accept a calculation" — so a submission into a session this account HOLDS in
//!    any other state is refused HTTP 400 `InvalidRequestException` in the sentence that NAMES
//!    the state the session is in and the state it would have to be in, having minted nothing
//!    and left the session where it was; while an id this account holds no session under keeps
//!    the one sentence this service answers for a reference it will not act on, which names no
//!    state at all.
//! 2. A code block runs as a cell of its session, so it finds that session's own engine
//!    already bound — `spark` for the session and `sc` for its context — and what the
//!    completed calculation reports is what the cell COMPUTED through it: the `stdout` object
//!    the read names holds that output, and the DPU execution time the host measured is
//!    reported beside it.
//! 3. The work IN a session goes with the session: `TerminateSession` cancels every
//!    calculation of that session that has not reached a terminal state — CANCELED, carrying
//!    the reason `CalculationStatus$StateChangeReason` documents for exactly this change —
//!    while a calculation that already finished keeps the state it reached and a calculation
//!    of another session is untouched.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! session's own lifecycle state (acquiring resources, shutting down, resources gone, an id
//! nothing was started under), along what the cell computes through the bound session (a span
//! aggregate, a mapped collection, rows the cell builds itself) and along which calculation a
//! termination finds (one still in flight, one that completed, one belonging to another
//! session).
//!
//! Nothing here waits on a span of the host's own speed: a session that must sit at a point of
//! its lifecycle is seeded with the instants that put it there, and every transition the clock
//! owes is waited on by POLLING the service for the state itself under a deadline far above
//! the span it is due after.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, SessionRecord, CALCULATION_CANCELED_WITH_SESSION, CALCULATION_STATE_CANCELED,
    CALCULATION_STATE_COMPLETED, CALCULATION_STATE_CREATED, CALCULATION_STATE_RUNNING,
    ERROR_CODE_INVALID_INPUT, ERROR_CODE_UNAUTHORIZED, PYSPARK_ENGINE_VERSION, SESSION_NOT_FOUND,
    SESSION_STATE_CREATING, SESSION_STATE_IDLE, SESSION_STATE_TERMINATED,
    SESSION_STATE_TERMINATING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call};
use crate::query_result_objects::{object, parse_location};

/// The Spark enabled workgroup the sessions below run in.
const WORK_GROUP: &str = "spark-session-cells";

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// Where that workgroup writes its results, which is the working directory a session started
/// in it runs under and therefore the one its calculations save their output under.
const OUTPUT_LOCATION: &str = "s3://athena-results/session-cells/";

/// A span long enough that a record seeded with it is past every span this emulator owes — or,
/// used the other way, nowhere near due — for as long as any test run takes.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// A cell that cannot finish inside the bounds this emulator holds one run to, so the
/// calculation it is submitted as is work still IN FLIGHT: its run is cut short, nothing is
/// known about what it came to, and it stays RUNNING until something cancels it.
const CELL_STILL_IN_FLIGHT: &str = "import time\ntime.sleep(600)";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span any of them is due after, so what is
/// waited on is the CONDITION rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 2_400;

/// Athena and Amazon S3 mounted on ONE shared engine, with the bucket the workgroup writes
/// into held in it: a calculation's output is SAVED as objects, so what the control plane
/// names in a read is what a client then fetches out of the object store.
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

/// Seed one session into `engine`'s store sitting at the lifecycle state `state` names, and
/// hand back its `SessionId`.
///
/// Seeding the stored INSTANTS is what puts a session at an exact point of its lifecycle
/// without waiting for the spans this emulator owes; every call under test is made over the
/// wire. The seed is checked against the record's own projection, so a mis-seeded row cannot
/// pass by accident.
fn given_session(engine: &engine::InMemoryEngine, state: &str) -> String {
    let id = state::new_session_id();
    let now = SystemTime::now();
    let long_ago = now
        .checked_sub(WELL_PAST_ANY_RUN)
        .expect("an instant this far back exists");
    let mut record = SessionRecord::started(id.clone(), WORK_GROUP.to_string(), None, long_ago);
    // The working directory a calculation of this session saves its output under is the one
    // the session was STARTED under, which is this workgroup's result configuration.
    record.configuration.working_directory = Some(OUTPUT_LOCATION.to_string());
    match state {
        // Resources not acquired for another hour, so the session stays in the state it
        // acquires them in however long the row takes.
        SESSION_STATE_CREATING => {
            record.created_at = now;
            record.resources_due_at = now
                .checked_add(WELL_PAST_ANY_RUN)
                .expect("an instant this far ahead exists");
        }
        SESSION_STATE_IDLE => {}
        SESSION_STATE_TERMINATING => record.terminate(now),
        SESSION_STATE_TERMINATED => record.terminate(long_ago),
        other => panic!("a session cannot be seeded in the state {other:?}"),
    }
    let seeded = record.state_at(SystemTime::now());
    assert_eq!(
        seeded, state,
        "a session seeded as {state:?} must report it, got {seeded:?}"
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().sessions.insert(record);
    id
}

/// How many calculations this account holds, read off the store: the count a refusal must
/// leave where it was.
fn held_calculations(engine: &engine::InMemoryEngine) -> usize {
    let store = state::service_state(engine).expect("this service's state");
    let held = store.lock();
    held.calculations.len()
}

/// Submit one calculation and hand back `(status, body)`.
async fn submit(router: &MultiServiceRouter, session_id: &str, code_block: &str) -> (u16, Value) {
    call(
        router,
        "StartCalculationExecution",
        &json!({ "SessionId": session_id, "CodeBlock": code_block }),
    )
    .await
}

/// Submit one calculation that must be accepted, and hand back its `CalculationExecutionId`.
async fn accepted(
    router: &MultiServiceRouter,
    case: &str,
    session_id: &str,
    code_block: &str,
) -> String {
    let (status, answer) = submit(router, session_id, code_block).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the submission must be accepted, got {status} with {answer:?}"
    );
    assert_eq!(
        answer["State"].as_str(),
        Some(CALCULATION_STATE_CREATED),
        "case {case:?}: expected the state {CALCULATION_STATE_CREATED:?}, got {answer:?}"
    );
    answer["CalculationExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a CalculationExecutionId, got {answer:?}"))
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

/// The `CalculationStatus` `GetCalculationExecutionStatus` reports for `id`.
async fn status_of(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
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
    body["Status"].clone()
}

/// The state `GetSessionStatus` reports for `session_id`.
async fn session_state(router: &MultiServiceRouter, case: &str, session_id: &str) -> String {
    let (status, read) = call(
        router,
        "GetSessionStatus",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSessionStatus({session_id}) must answer 200, got {status} with {read:?}"
    );
    read["Status"]["State"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a status read must report a State, got {read:?}"))
        .to_string()
}

/// Poll the calculation `id` until it reports `wanted`, and answer the status that reported it.
///
/// The CONDITION is what is waited on: every look asks the service what state the calculation
/// is in, under a deadline far above the span the transition is due after.
async fn settled_in(router: &MultiServiceRouter, case: &str, id: &str, wanted: &str) -> Value {
    for _ in 0..POLL_LOOKS {
        let status = status_of(router, case, id).await;
        if status["State"].as_str() == Some(wanted) {
            return status;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the calculation {id} never reached {wanted}");
}

/// RULE 1: a session this account holds that is not able to accept a calculation is refused in
/// the sentence that names its own state, and an id naming no session keeps the sentence that
/// names none.
#[tokio::test]
async fn a_session_that_cannot_accept_a_calculation_is_refused_naming_its_state() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;

    // Every state a session of this service can be in other than the one it accepts a
    // calculation in.
    let rows: Vec<&str> = vec![
        SESSION_STATE_CREATING,
        SESSION_STATE_TERMINATING,
        SESSION_STATE_TERMINATED,
    ];
    for state in &rows {
        let case = format!("a session in the {state} state");
        let session = given_session(&engine, state);
        let held = held_calculations(&engine);

        let (status, refused) = submit(&router, &session, "1 + 1").await;
        assert_refusal(
            &case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &state::session_not_accepting_calculations(state),
        );
        assert_eq!(
            held_calculations(&engine),
            held,
            "case {case:?}: a refused submission must mint nothing, got {} calculations",
            held_calculations(&engine)
        );
        // The refusal names the session's CURRENT state, which is the state a status read of
        // that same session reports — the one case whose state a row cannot read back is the
        // one whose shutdown is still in flight, which the clock moves on by itself.
        if *state != SESSION_STATE_TERMINATING {
            let reported = session_state(&router, &case, &session).await;
            assert_eq!(
                &reported, state,
                "case {case:?}: the refused session must still report {state:?}, got {reported:?}"
            );
        }
    }

    // The contrast: an id this account holds no session under names no state, so it takes the
    // sentence this service answers for a reference it will not act on.
    let unknown: Vec<(&str, String)> = vec![
        ("an id nothing was started under", state::new_session_id()),
        ("an id in the short Athena form", "s-0123456789abcdef".to_string()),
        ("a single character", "a".to_string()),
    ];
    for (case, session_id) in &unknown {
        let (status, refused) = submit(&router, session_id, "1 + 1").await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
    }

    // And the session that IS able to accept one takes it, so the rule above is about the
    // state and not about this workgroup or this store.
    let idle = given_session(&engine, SESSION_STATE_IDLE);
    let reported = session_state(&router, "a session able to accept a calculation", &idle).await;
    assert_eq!(
        reported, SESSION_STATE_IDLE,
        "case \"a session able to accept a calculation\": it must report {SESSION_STATE_IDLE:?}, \
         got {reported:?}"
    );
    accepted(
        &router,
        "a session able to accept a calculation",
        &idle,
        "1 + 1",
    )
    .await;
}

/// RULE 2: a cell finds its session's own engine bound, and what the completed calculation
/// reports is what the cell computed through it.
///
/// The rows are cells whose answer no part of this emulator holds: an aggregate over a span the
/// session hands out, a collection mapped through the session's context, and rows the cell
/// builds itself.
#[tokio::test]
async fn a_cell_computes_through_the_session_bound_for_it() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;

    // (case, the cell, the standard output it must produce)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "an aggregate over the span the session hands out",
            "print('total', spark.range(1, 101).groupBy().sum().collect()[0][0])",
            "total 5050\n",
        ),
        (
            "a collection mapped through the session's context",
            "print(sc.parallelize(range(1, 11)).map(lambda n: n * n).sum())",
            "385\n",
        ),
        (
            "rows the cell builds itself",
            "rows = spark.createDataFrame([(1,), (2,), (3,)], ['n'])\nprint(rows.count())",
            "3\n",
        ),
    ];

    for (case, cell, expected_stdout) in &rows {
        let session = given_session(&engine, SESSION_STATE_IDLE);
        let id = accepted(&router, case, &session, cell).await;

        let status = settled_in(&router, case, &id, CALCULATION_STATE_COMPLETED).await;
        assert!(
            !status["CompletionDateTime"].is_null(),
            "case {case:?}: a completed calculation reports when it stopped processing, got \
             {status:?}"
        );

        let found = read(&router, case, &id).await;
        assert_eq!(
            found["SessionId"].as_str(),
            Some(session.as_str()),
            "case {case:?}: the read must report the session the cell ran in, got {found:?}"
        );
        // "The data processing unit execution time in milliseconds for the calculation": the
        // host's own measurement of a run that really happened, so it is not zero.
        let measured = found["Statistics"]["DpuExecutionInMillis"].as_i64();
        assert!(
            measured.is_some_and(|millis| millis > 0),
            "case {case:?}: a cell that ran must report the time its run took, got {found:?}"
        );

        let stdout_uri = found["Result"]["StdOutS3Uri"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a StdOutS3Uri, got {found:?}"))
            .to_string();
        let (bucket, key) = parse_location(case, &stdout_uri);
        let (code, body) = object(&router, &bucket, &key).await;
        assert_eq!(
            (code, body.as_str()),
            (200, *expected_stdout),
            "case {case:?}: the object the read names must hold what the cell computed, got \
             {code} with {body:?}"
        );

        // The code a read reports is the code the submission carried, so what ran is what was
        // submitted.
        let (code, read_back) = call(
            &router,
            "GetCalculationExecutionCode",
            &json!({ "CalculationExecutionId": id }),
        )
        .await;
        assert_eq!(
            (code, read_back["CodeBlock"].as_str()),
            (200, Some(*cell)),
            "case {case:?}: the submitted code must read back, got {code} with {read_back:?}"
        );
    }
}

/// RULE 3: terminating a session cancels the work still in flight in it, and nothing else.
#[tokio::test]
async fn terminating_a_session_cancels_the_calculations_still_in_flight_in_it() {
    let (engine, router) = engine_and_emulator();
    given_spark_work_group(&router).await;

    let session = given_session(&engine, SESSION_STATE_IDLE);
    let elsewhere = given_session(&engine, SESSION_STATE_IDLE);

    // A cell of the session that is still in flight, a second one beside it, a cell of the
    // session that already finished, and a cell of ANOTHER session that is still in flight.
    let in_flight = accepted(&router, "a cell still in flight", &session, CELL_STILL_IN_FLIGHT).await;
    let beside_it = accepted(
        &router,
        "a second cell still in flight",
        &session,
        CELL_STILL_IN_FLIGHT,
    )
    .await;
    let finished = accepted(&router, "a cell that completed", &session, "print(1 + 1)").await;
    let other = accepted(
        &router,
        "a cell of another session",
        &elsewhere,
        CELL_STILL_IN_FLIGHT,
    )
    .await;

    // The cell that completed is terminal before the termination, so the row below is about a
    // calculation that is already finished rather than one that races the shutdown.
    settled_in(
        &router,
        "a cell that completed",
        &finished,
        CALCULATION_STATE_COMPLETED,
    )
    .await;
    for (case, id) in [
        ("a cell still in flight", &in_flight),
        ("a second cell still in flight", &beside_it),
        ("a cell of another session", &other),
    ] {
        let state = settled_in(&router, case, id.as_str(), CALCULATION_STATE_RUNNING).await;
        assert!(
            state["CompletionDateTime"].is_null(),
            "case {case:?}: work still in flight has not stopped processing, got {state:?}"
        );
    }

    let (code, terminated) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": session }),
    )
    .await;
    assert_eq!(
        code, 200,
        "the session must be terminated, got {code} with {terminated:?}"
    );

    // (case, the calculation, the state it must come to rest in, the reason it must report)
    let rows: Vec<(&str, &String, &str, Option<&str>)> = vec![
        (
            "a cell still in flight",
            &in_flight,
            CALCULATION_STATE_CANCELED,
            Some(CALCULATION_CANCELED_WITH_SESSION),
        ),
        (
            "a second cell still in flight",
            &beside_it,
            CALCULATION_STATE_CANCELED,
            Some(CALCULATION_CANCELED_WITH_SESSION),
        ),
        (
            "a cell that completed",
            &finished,
            CALCULATION_STATE_COMPLETED,
            None,
        ),
        (
            "a cell of another session",
            &other,
            CALCULATION_STATE_RUNNING,
            None,
        ),
    ];
    for (case, id, expected, reason) in &rows {
        let status = settled_in(&router, case, id.as_str(), expected).await;
        assert_eq!(
            status["StateChangeReason"].as_str(),
            *reason,
            "case {case:?}: expected the reason {reason:?}, got {status:?}"
        );
        // A cancelled calculation stopped processing, so it reports when; one still running
        // reports no completion at all.
        assert_eq!(
            status["CompletionDateTime"].is_null(),
            *expected == CALCULATION_STATE_RUNNING,
            "case {case:?}: the completion instant must follow the state {expected:?}, got \
             {status:?}"
        );
    }

    // A repeat of the shutdown changes none of it: the cancellation keeps the instant it was
    // first asked at, so the state and the reason stay where they settled.
    let (code, again) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": session }),
    )
    .await;
    assert_eq!(
        code, 200,
        "a repeated shutdown must still answer 200, got {code} with {again:?}"
    );
    let status = status_of(&router, "a repeated shutdown", &in_flight).await;
    assert_eq!(
        (
            status["State"].as_str(),
            status["StateChangeReason"].as_str()
        ),
        (
            Some(CALCULATION_STATE_CANCELED),
            Some(CALCULATION_CANCELED_WITH_SESSION)
        ),
        "case \"a repeated shutdown\": the cancelled calculation must stand, got {status:?}"
    );
}
