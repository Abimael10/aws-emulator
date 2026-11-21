// SPDX-License-Identifier: CC0-1.0

//! Athena's CALCULATION submission, driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.StartCalculationExecution`), so every
//! assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: a calculation is the code of a cell of the notebook a session
//! serves, so `StartCalculationExecution` persists the submitted code block for a session
//! that is still running AND was started for a notebook this account holds — answering with
//! the `CalculationExecutionId` it minted and the state the calculation was created in,
//! which `GetCalculationExecutionCode` then answers the same code out of — while every other
//! session-addressed submission (an id this account holds no session under, a session whose
//! resources are no longer running, a session serving no notebook) is refused HTTP 400
//! `InvalidRequestException` with the one sentence Athena states for the whole class, having
//! persisted nothing and left the session exactly as it was. A submission that names neither
//! code block, or both of them, is refused for that instead.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! CODE BLOCK (empty, a single expression, several lines, text outside ASCII, the modelled
//! maximum length), which member carries it (the current one, the deprecated
//! `CalculationConfiguration`, neither, both), WHICH SESSION is addressed (notebook-bound,
//! bound to no notebook, bound to a notebook that is gone, shutting down, shut down, an id
//! nothing was started under in several forms) and the idempotency token a caller repeats.
//!
//! Nothing here waits on the wall clock: a session that must be at a given point of its
//! lifecycle is seeded with the instants that put it there — the same seeding
//! `tests/all/notebook_session_listing.rs` uses, through this service's own
//! [`SessionRecord`] — and every call is made over the wire.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, SessionRecord, CALCULATION_CODE_REQUIRED, CALCULATION_NOT_FOUND,
    CALCULATION_STATE_CREATED, ERROR_CODE_INVALID_INPUT, ERROR_CODE_UNAUTHORIZED,
    IDEMPOTENT_PARAMETERS_DO_NOT_MATCH, PYSPARK_ENGINE_VERSION, SESSION_NOT_FOUND,
    SESSION_STATE_IDLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, engine_and_router, request_token};

/// The operation under test.
const OPERATION: &str = "StartCalculationExecution";

/// The Spark enabled workgroup the notebooks and sessions below live in.
const WORK_GROUP: &str = "spark-calculations";

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// A span long enough that a session seeded as started that long ago is past every span this
/// emulator owes, for as long as any test run takes: so a session seeded as started is IDLE
/// however fast or slow the host runs.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// The longest code block a submission may carry: the modelled `CodeBlock` length (0..68000),
/// from `smithy_query(query="operation",operation="StartCalculationExecution")`.
const MAX_CODE_BLOCK_BYTES: usize = 68_000;

/// The ceiling of the modelled `CalculationExecutionId` length (1..36), which bounds the id a
/// submission answers with.
const MAX_CALCULATION_ID_LENGTH: usize = 36;

/// Create the Spark enabled workgroup the notebooks live in and the sessions run in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": EXECUTION_ROLE,
                "ResultConfiguration": { "OutputLocation": "s3://athena-spark/calculations/" }
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

/// Where in its lifecycle a seeded session sits.
#[derive(Clone, Copy, Debug)]
enum Lifecycle {
    /// Started long enough ago to have its resources: IDLE, "able to accept a calculation".
    Running,
    /// Asked to shut down just now: TERMINATING, so its resources are on their way out.
    ShuttingDown,
    /// Asked to shut down long enough ago that the shutdown is complete: TERMINATED.
    ShutDown,
}

/// Seed one session into `engine`'s store, bound to `notebook_id` and at the point of its
/// lifecycle `lifecycle` names, and hand back its `SessionId`.
///
/// Seeding is what lets a row place a session at an exact point of its lifecycle without
/// waiting for the spans this emulator owes; the submission itself is made over the wire.
fn given_session(
    engine: &engine::InMemoryEngine,
    notebook_id: Option<&str>,
    lifecycle: Lifecycle,
) -> String {
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
    match lifecycle {
        Lifecycle::Running => {}
        Lifecycle::ShuttingDown => record.terminate(SystemTime::now()),
        Lifecycle::ShutDown => record.terminate(started_at),
    }
    // The seeded instants must really put the session where the row claims, so a mis-seeded
    // row cannot pass by accident.
    let seeded = record.state_at(SystemTime::now());
    let active = matches!(lifecycle, Lifecycle::Running);
    assert_eq!(
        record.is_active_at(SystemTime::now()),
        active,
        "a session seeded as {lifecycle:?} must be active={active}, got the state {seeded:?}"
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

/// The state `GetSessionStatus` reports for `session_id`.
async fn session_state(router: &MultiServiceRouter, session_id: &str) -> String {
    let (status, read) = call(
        router,
        "GetSessionStatus",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetSessionStatus({session_id}) must answer 200, got {status} with {read:?}"
    );
    read["Status"]["State"]
        .as_str()
        .unwrap_or_else(|| panic!("a status read must report a State, got {read:?}"))
        .to_string()
}

/// Submit one calculation and hand back `(status, body)`.
async fn submit(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, OPERATION, request).await
}

/// Submit one calculation that must be accepted, and hand back its `CalculationExecutionId`.
async fn accepted(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, answer) = submit(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {OPERATION} to be accepted, got {status} with {answer:?}"
    );
    assert_eq!(
        answer["State"].as_str(),
        Some(CALCULATION_STATE_CREATED),
        "case {case:?}: expected the state {CALCULATION_STATE_CREATED:?}, got {answer:?}"
    );
    let id = answer["CalculationExecutionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a CalculationExecutionId, got {answer:?}")
        })
        .to_string();
    assert!(
        !id.is_empty() && id.len() <= MAX_CALCULATION_ID_LENGTH,
        "case {case:?}: expected an id inside the modelled length 1..{MAX_CALCULATION_ID_LENGTH}, \
         got {id:?}"
    );
    id
}

/// The code block `GetCalculationExecutionCode` reports for the calculation `id`.
async fn code_of(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let (status, read) = call(
        router,
        "GetCalculationExecutionCode",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCalculationExecutionCode({id}) must answer 200, got {status} with \
         {read:?}"
    );
    read["CodeBlock"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a CodeBlock, got {read:?}"))
        .to_string()
}

/// RULE: a submission into a running notebook session persists the code block it carried, and
/// the code a later read reports is that same code — whatever the code is.
///
/// The rows differ along the code block itself: empty (the modelled minimum length), one
/// expression, several lines, text outside ASCII, and the modelled maximum length.
#[tokio::test]
async fn a_notebook_session_keeps_the_code_each_submission_carried() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "analysis").await;
    let session = given_session(&engine, Some(&notebook), Lifecycle::Running);

    let long_block = "x".repeat(MAX_CODE_BLOCK_BYTES);
    let rows: Vec<(&str, &str)> = vec![
        ("an empty code block", ""),
        ("one expression", "1"),
        ("a call", "print(1 + 1)"),
        ("several lines", "a = 1\nb = 2\nprint(a + b)\n"),
        ("text outside ASCII", "print('héllo wörld ✓')"),
        ("the modelled maximum length", long_block.as_str()),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, code_block) in &rows {
        let id = accepted(
            &router,
            case,
            &json!({ "SessionId": session, "CodeBlock": code_block }),
        )
        .await;
        let read_back = code_of(&router, case, &id).await;
        assert_eq!(
            &read_back, code_block,
            "case {case:?}: expected the submitted code back, got {} bytes",
            read_back.len()
        );
        assert!(
            !minted.contains(&id),
            "case {case:?}: every submission mints its own id, got {id:?} twice"
        );
        minted.push(id);
    }
    assert_eq!(
        held_calculations(&engine),
        rows.len(),
        "case \"every row\": expected {} calculations held, got {}",
        rows.len(),
        held_calculations(&engine)
    );
}

/// RULE: the code may be carried by either member — "The request syntax requires the
/// StartCalculationExecutionRequest$CodeBlock parameter or the
/// CalculationConfiguration$CodeBlock parameter, but not both" — so a submission naming
/// exactly one is accepted and reads back that code, and one naming neither or both is
/// refused and persists nothing.
#[tokio::test]
async fn a_submission_names_exactly_one_code_block() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "carriers").await;
    let session = given_session(&engine, Some(&notebook), Lifecycle::Running);

    // Each row is the request's code-bearing members, and the code it must then read back.
    let accepted_rows: Vec<(&str, Value, &str)> = vec![
        (
            "the current member alone",
            json!({ "CodeBlock": "print('current')" }),
            "print('current')",
        ),
        (
            "the deprecated configuration alone",
            json!({ "CalculationConfiguration": { "CodeBlock": "print('deprecated')" } }),
            "print('deprecated')",
        ),
        (
            "the deprecated configuration carrying an empty code block",
            json!({ "CalculationConfiguration": { "CodeBlock": "" } }),
            "",
        ),
    ];
    for (case, members, expected) in &accepted_rows {
        let mut request = json!({ "SessionId": session });
        for (member, value) in members.as_object().expect("the row's members") {
            request[member] = value.clone();
        }
        let id = accepted(&router, case, &request).await;
        let read_back = code_of(&router, case, &id).await;
        assert_eq!(
            &read_back, expected,
            "case {case:?}: expected the code {expected:?} back, got {read_back:?}"
        );
    }

    let held = held_calculations(&engine);
    let refused_rows: Vec<(&str, Value)> = vec![
        ("neither member", json!({})),
        (
            "both members, the same code",
            json!({
                "CodeBlock": "print('both')",
                "CalculationConfiguration": { "CodeBlock": "print('both')" }
            }),
        ),
        (
            "both members, different code",
            json!({
                "CodeBlock": "print('one')",
                "CalculationConfiguration": { "CodeBlock": "print('other')" }
            }),
        ),
        (
            "both members, one of them empty",
            json!({ "CodeBlock": "", "CalculationConfiguration": { "CodeBlock": "1" } }),
        ),
    ];
    for (case, members) in &refused_rows {
        let mut request = json!({ "SessionId": session });
        for (member, value) in members.as_object().expect("the row's members") {
            request[member] = value.clone();
        }
        let (status, refused) = submit(&router, &request).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            CALCULATION_CODE_REQUIRED,
        );
        assert_eq!(
            held_calculations(&engine),
            held,
            "case {case:?}: a refused submission must persist nothing, got {} calculations",
            held_calculations(&engine)
        );
    }
}

/// RULE: a session serves the notebook its start bound it to, so a session serving none runs
/// no calculation: the submission is refused with the sentence every session-addressed
/// request this service will not act on is refused with, nothing is persisted, and the
/// session is left exactly as it was.
#[tokio::test]
async fn a_session_serving_no_notebook_takes_no_calculation() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    // A notebook exists throughout, so each refusal below is about the session the request
    // addressed rather than about this account holding no notebooks at all.
    let notebook = given_notebook(&router, "served").await;
    let gone = state::new_notebook_id();

    let rows: Vec<(&str, Option<String>)> = vec![
        ("a session started for no notebook", None),
        (
            "a session bound to a notebook this account does not hold",
            Some(gone),
        ),
    ];
    for (case, bound_to) in &rows {
        let session = given_session(&engine, bound_to.as_deref(), Lifecycle::Running);
        let before = session_state(&router, &session).await;
        assert_eq!(
            before, SESSION_STATE_IDLE,
            "case {case:?}: the session under test must be idle first, got {before:?}"
        );

        let (status, refused) = submit(
            &router,
            &json!({ "SessionId": session, "CodeBlock": "1" }),
        )
        .await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
        assert_eq!(
            held_calculations(&engine),
            0,
            "case {case:?}: a refused submission must persist nothing, got {} calculations",
            held_calculations(&engine)
        );
        let after = session_state(&router, &session).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused submission starts nothing, so the session must still \
             report {before:?}, got {after:?}"
        );
    }
    // The notebook-bound session of the same account still takes one, so what the rows above
    // assert is the notebook the session serves and not this workgroup refusing everything.
    let serving = given_session(&engine, Some(&notebook), Lifecycle::Running);
    accepted(
        &router,
        "a session serving a notebook",
        &json!({ "SessionId": serving, "CodeBlock": "1" }),
    )
    .await;
}

/// RULE: every session-addressed submission this service will not act on is refused the same
/// way — an id this account holds no session under, whatever its form, and a session whose
/// resources are on their way out or gone.
#[tokio::test]
async fn a_session_this_service_will_not_act_on_takes_no_calculation() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "unreachable").await;

    let mut rows: Vec<(&str, String)> = vec![
        ("an id nothing was started under", state::new_session_id()),
        ("an id in the short Athena form", "s-0123456789abcdef".to_string()),
        ("the notebook's own id", notebook.clone()),
        ("a single character", "a".to_string()),
        (
            "the longest id the model admits",
            "s".repeat(256),
        ),
    ];
    rows.push((
        "a session shutting down",
        given_session(&engine, Some(&notebook), Lifecycle::ShuttingDown),
    ));
    rows.push((
        "a session whose resources are gone",
        given_session(&engine, Some(&notebook), Lifecycle::ShutDown),
    ));

    for (case, session_id) in &rows {
        let (status, refused) = submit(
            &router,
            &json!({ "SessionId": session_id, "CodeBlock": "1" }),
        )
        .await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
        assert_eq!(
            held_calculations(&engine),
            0,
            "case {case:?}: a refused submission must persist nothing, got {} calculations",
            held_calculations(&engine)
        );
    }
}

/// RULE: "If another StartCalculationExecutionRequest is received, the same response is
/// returned and another calculation is not created. If a parameter has changed, an error is
/// returned."
#[tokio::test]
async fn a_repeated_idempotency_token_submits_one_calculation() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "idempotent").await;
    let session = given_session(&engine, Some(&notebook), Lifecycle::Running);
    let token = request_token("calculation-token");

    let first = accepted(
        &router,
        "the first submission",
        &json!({ "SessionId": session, "CodeBlock": "1", "ClientRequestToken": token }),
    )
    .await;
    let repeat = accepted(
        &router,
        "the same submission again",
        &json!({ "SessionId": session, "CodeBlock": "1", "ClientRequestToken": token }),
    )
    .await;
    assert_eq!(
        repeat, first,
        "case \"the same submission again\": expected the id {first:?} back, got {repeat:?}"
    );
    assert_eq!(
        held_calculations(&engine),
        1,
        "case \"the same submission again\": expected one calculation held, got {}",
        held_calculations(&engine)
    );

    // A parameter the token did not mint: each of the three the submission persists.
    let other_session = given_session(&engine, Some(&notebook), Lifecycle::Running);
    let changed: Vec<(&str, Value)> = vec![
        (
            "another code block under the same token",
            json!({ "SessionId": session, "CodeBlock": "2", "ClientRequestToken": token }),
        ),
        (
            "another session under the same token",
            json!({ "SessionId": other_session, "CodeBlock": "1", "ClientRequestToken": token }),
        ),
        (
            "another description under the same token",
            json!({
                "SessionId": session,
                "CodeBlock": "1",
                "Description": "the second cell",
                "ClientRequestToken": token
            }),
        ),
    ];
    for (case, request) in &changed {
        let (status, refused) = submit(&router, request).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            IDEMPOTENT_PARAMETERS_DO_NOT_MATCH,
        );
        assert_eq!(
            held_calculations(&engine),
            1,
            "case {case:?}: a refused repeat must persist nothing, got {} calculations",
            held_calculations(&engine)
        );
    }

    // Another token is another calculation, however alike the submissions are.
    let second = accepted(
        &router,
        "the same code under another token",
        &json!({
            "SessionId": session,
            "CodeBlock": "1",
            "ClientRequestToken": request_token("another-token")
        }),
    )
    .await;
    assert_ne!(
        second, first,
        "case \"the same code under another token\": expected a calculation of its own, got \
         {first:?} twice"
    );
    assert_eq!(
        held_calculations(&engine),
        2,
        "case \"the same code under another token\": expected two calculations held, got {}",
        held_calculations(&engine)
    );
}

/// RULE: a `CalculationExecutionId` this account minted no calculation under has no code to
/// read, which is a refusal rather than an empty answer.
#[tokio::test]
async fn an_unminted_calculation_has_no_code_to_read() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "reads").await;
    let session = given_session(&engine, Some(&notebook), Lifecycle::Running);
    // One calculation exists throughout, so each refusal below is about the id the request
    // carried rather than about the account holding none.
    let minted = accepted(
        &router,
        "a calculation to read",
        &json!({ "SessionId": session, "CodeBlock": "1" }),
    )
    .await;

    let rows: Vec<(&str, String)> = vec![
        ("an id nothing was minted under", state::new_calculation_execution_id()),
        ("the session's own id", session.clone()),
        ("the notebook's own id", notebook.clone()),
        ("a single character", "a".to_string()),
    ];
    for (case, id) in &rows {
        let (status, refused) = call(
            &router,
            "GetCalculationExecutionCode",
            &json!({ "CalculationExecutionId": id }),
        )
        .await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            CALCULATION_NOT_FOUND,
        );
    }

    let read_back = code_of(&router, "a calculation to read", &minted).await;
    assert_eq!(
        read_back, "1",
        "case \"a calculation to read\": the minted calculation must still read back, got \
         {read_back:?}"
    );
}

/// RULE: a calculation is addressed inside the session it was submitted into, and a session
/// goes with its workgroup — so deleting the workgroup takes its calculations with it.
#[tokio::test]
async fn deleting_the_work_group_takes_its_calculations() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "deleted").await;
    let session = given_session(&engine, Some(&notebook), Lifecycle::Running);
    let id = accepted(
        &router,
        "a calculation in the workgroup",
        &json!({ "SessionId": session, "CodeBlock": "print('gone')" }),
    )
    .await;

    let (status, deleted) = call(
        &router,
        "DeleteWorkGroup",
        &json!({ "WorkGroup": WORK_GROUP, "RecursiveDeleteOption": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup must be deleted, got {status} with {deleted:?}"
    );

    assert_eq!(
        held_calculations(&engine),
        0,
        "case \"after the delete\": expected no calculation held, got {}",
        held_calculations(&engine)
    );
    let (status, refused) = call(
        &router,
        "GetCalculationExecutionCode",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_refusal(
        "after the delete",
        status,
        &refused,
        ERROR_CODE_UNAUTHORIZED,
        CALCULATION_NOT_FOUND,
    );
}
