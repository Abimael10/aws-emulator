// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Glue statement slice: the `RunStatement` that records a
//! statement on an interactive session, and the `GetStatement` that reads it back.
//!
//! RULE: a statement is a record of the session it was run on, addressed by the session's
//! `Id` and an `Id` that session mints for it in submission order, and a session takes one
//! whenever its runtime is up — so a run is refused, as invalid input, only when the session
//! is in a status other than `READY`, a statement submitted while the session is actioning
//! another is queued rather than refused, and the statement a run records carries the code it
//! submitted until the clock moves it to its terminal state.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The round-trip rows differ along what the rule generalises over: the size and the form
//! of the code submitted, each published bound of `Code` included, and the number of
//! statements one session holds.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    messages, ComAmazonawsGlueAwsglueState, StatementEntry, FIRST_STATEMENT_ID,
    MAX_STATEMENTS_PER_SESSION, STATEMENT_CLAIM_ABANDONED_AFTER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{already_past, far_future};
use crate::wire::{call, call_in_region, emulator_on, error_code, error_message, ok, REGION};

/// The longest `Code` the model accepts: `OrchestrationStatementCodeString`, "len 0..68000"
/// (`smithy_query operation=RunStatement`).
const MAX_CODE_LENGTH: usize = 68_000;

/// The `StatementState` values this suite observes, of the modelled enum (`WAITING`,
/// `RUNNING`, `AVAILABLE`, `CANCELLING`, `CANCELLED`, `ERROR`, from
/// `smithy_query shape=StatementState`).
const STATE_WAITING: &str = "WAITING";
const STATE_RUNNING: &str = "RUNNING";
pub(crate) const STATE_AVAILABLE: &str = "AVAILABLE";

/// The `SessionStatus` values a run is refused in here, of the modelled enum
/// (`PROVISIONING`, `READY`, `FAILED`, `TIMEOUT`, `STOPPING`, `STOPPED`, from
/// `smithy_query shape=SessionStatus`).
const STATUS_PROVISIONING: &str = "PROVISIONING";
const STATUS_TIMEOUT: &str = "TIMEOUT";
const STATUS_STOPPING: &str = "STOPPING";
const STATUS_STOPPED: &str = "STOPPED";
const STATUS_FAILED: &str = "FAILED";

/// An IAM role ARN of the form the modelled `OrchestrationRoleArn` pattern requires
/// (`^arn:aws[^:]*:iam::[0-9]*:role/.+$`).
const ROLE: &str = "arn:aws:iam::000000000000:role/GlueSessionRole";

/// Seconds in one minute: both documented session timeouts are stated in minutes.
pub(crate) const SECONDS_PER_MINUTE: u64 = 60;

/// Create the session `id`, with the create body `extra` merged over the required members.
pub(crate) async fn create_session(router: &MultiServiceRouter, id: &str, extra: Value) {
    let mut body = json!({
        "Id": id,
        "Role": ROLE,
        "Command": { "Name": "glueetl" },
    });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    ok(router, "CreateSession", &body).await;
}

/// Put the instant the clock owes the session `id` its next status at `at`.
pub(crate) fn set_session_due(engine: &Arc<engine::InMemoryEngine>, id: &str, at: SystemTime) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .session_mut(REGION, id)
        .unwrap_or_else(|| panic!("{id} was created"));
    entry.settles_at = Some(at);
}

/// Put the instant the clock owes every unsettled statement of the session `id` its next
/// state at `at`.
pub(crate) fn set_statement_due(engine: &Arc<engine::InMemoryEngine>, id: &str, at: SystemTime) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .session_mut(REGION, id)
        .unwrap_or_else(|| panic!("{id} was created"));
    for statement in &mut entry.statements {
        if statement.settles_at.is_some() {
            statement.settles_at = Some(at);
        }
    }
}

/// Put EVERY state the clock still owes the statements of the session `id` at `at` — the one
/// a statement is owed next, and the one its own execution decided where that execution has
/// already answered.
///
/// A statement is owed `RUNNING` and then, once its code has answered, the state that
/// execution came to, so this is what a suite makes due to observe a statement at rest
/// without waiting for either span.
pub(crate) fn make_owed_statement_states_due(
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
    at: SystemTime,
) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let Some(entry) = store.session_mut(REGION, id) else {
        return;
    };
    for statement in &mut entry.statements {
        if statement.settles_at.is_some() {
            statement.settles_at = Some(at);
        }
        if statement.rests_at.is_some() {
            statement.rests_at = Some(at);
        }
    }
}

/// How long a poll of a session's statements waits for one to come to rest.
///
/// Deliberately generous, and nothing is asserted about how long anything took: what the poll
/// waits for is the EXECUTION the service owes the statement, performed by whichever call
/// finds it owed, plus the states the clock then owes the statement itself. A window far
/// longer than both is what separates a statement that has not finished YET from one that
/// never finishes, on a host of any speed.
pub(crate) const STATEMENT_AT_REST_DEADLINE: Duration = Duration::from_secs(180);

/// How long one turn of that poll waits before reading again — the gap between two reads of a
/// client that polls, not a span anything is expected to finish inside.
const STATEMENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `StatementState` values a statement never leaves again, of the modelled enum.
const TERMINAL_STATES: [&str; 3] = ["AVAILABLE", "CANCELLED", "ERROR"];

/// Read the statement `statement` of the session `id` back until it has come to REST, and
/// answer the `Statement` it then reports.
///
/// The state a statement comes to rest in is owed to it by its own code's EXECUTION — which
/// whichever call finds that execution owed performs, the run or a later read — and then by
/// the clock, one owed span later. So the statement is polled for that CONDITION rather than
/// assumed to have reached it by the first read, which is what a client polling a cell does;
/// the owed spans are made due through the store on every turn rather than slept out, and a
/// statement still in flight when the window closes fails the case instead of hanging.
pub(crate) async fn statement_at_rest(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
    statement: i64,
) -> Value {
    let deadline = SystemTime::now() + STATEMENT_AT_REST_DEADLINE;
    loop {
        make_owed_statement_states_due(engine, id, already_past());
        let read = get(router, id, statement).await;
        if TERMINAL_STATES.contains(&read["State"].as_str().unwrap_or_default()) {
            return read;
        }
        assert!(
            SystemTime::now() < deadline,
            "statement {statement} of {id:?} must come to rest, and it reports {read}"
        );
        tokio::time::sleep(STATEMENT_POLL_INTERVAL).await;
    }
}

/// Put the stored session `id` into `status`.
///
/// The way a suite reaches a status no call of this emulator produces, so a rule can be
/// asserted over the whole modelled `SessionStatus` enum and not only over the statuses a
/// create, the clock and a stop reach. ONE copy, shared by every session suite, so two of them
/// cannot drift apart on how a status is forced.
pub(crate) fn force_session_status(engine: &Arc<engine::InMemoryEngine>, id: &str, status: &str) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .session_mut(REGION, id)
        .unwrap_or_else(|| panic!("{id} was created"));
    entry.settles_at = None;
    entry.status = status.to_string();
}

/// Whether the store itself holds a session under `id` in `region`.
///
/// One copy, shared: a suite that asserts what a write or a refusal left behind reads the
/// store the same way wherever it does it.
pub(crate) fn stored_session(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    id: &str,
) -> bool {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store.session(region, id).is_some()
}

/// How many statements the store itself holds for the session `id`.
///
/// The one way to see what a run recorded for a session whose own statement surface is
/// refused: a session that has ENDED holds no runtime for a statement to be read from, so the
/// reads of it are refused whether or not a statement was written.
pub(crate) fn stored_statement_count(engine: &Arc<engine::InMemoryEngine>, id: &str) -> usize {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store
        .session(REGION, id)
        .map(|session| session.statements.len())
        .unwrap_or_default()
}

/// Pull the session `id`'s own instants back by `elapsed`, so a suite can observe the
/// status the clock owes it without sleeping for the span.
pub(crate) fn rewind_session(engine: &Arc<engine::InMemoryEngine>, id: &str, elapsed: Duration) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .session_mut(REGION, id)
        .unwrap_or_else(|| panic!("{id} was created"));
    let back = |at: SystemTime| at.checked_sub(elapsed).expect("an instant in range");
    entry.created_on = back(entry.created_on);
    entry.last_active_on = back(entry.last_active_on);
    entry.settles_at = entry.settles_at.map(back);
}

/// A session in `READY`: created, with the status its provisioning owed already due.
pub(crate) async fn ready_session(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>, id: &str) {
    create_session(router, id, json!({})).await;
    set_session_due(engine, id, already_past());
}

/// Run `code` on the session `id` and return the `Id` the answer reports.
pub(crate) async fn run(router: &MultiServiceRouter, id: &str, code: &str) -> i64 {
    let answer = ok(
        router,
        "RunStatement",
        &json!({ "SessionId": id, "Code": code }),
    )
    .await;
    answer["Id"]
        .as_i64()
        .unwrap_or_else(|| panic!("RunStatement must report the statement Id, got {answer}"))
}

/// The `Statement` the `GetStatement` answer for `(id, statement)` reports.
pub(crate) async fn get(router: &MultiServiceRouter, id: &str, statement: i64) -> Value {
    let read = ok(
        router,
        "GetStatement",
        &json!({ "SessionId": id, "Id": statement }),
    )
    .await;
    read.get("Statement")
        .cloned()
        .unwrap_or_else(|| panic!("GetStatement must report a Statement, got {read}"))
}

// ---------------------------------------------------------------------------
// Run, then read back
// ---------------------------------------------------------------------------

/// What `RunStatement` records is what `GetStatement` reports: the code the run submitted,
/// the state the session took it in, and the id that session minted for it.
///
/// The rows differ along the size and the form of the code, both published bounds of
/// `Code` included.
#[tokio::test]
async fn run_statements_are_read_back_from_the_store() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    // Exactly the longest code the model accepts, as a comment so it is still a statement.
    let longest_code = format!("# {}", "c".repeat(MAX_CODE_LENGTH - 2));

    // (case, the session to run it on, the code submitted)
    let cases: Vec<(&str, &str, String)> = vec![
        (
            // "len 0..68000": the empty string is inside the modelled length.
            "code at the modelled length floor",
            "empty-code",
            String::new(),
        ),
        (
            "a one-line expression",
            "one-liner",
            "1 + 1".to_string(),
        ),
        (
            "a multi-line cell",
            "multi-line",
            "import sys\nprint(sys.version)\nspark.sql('SELECT 1').show()\n".to_string(),
        ),
        (
            // A notebook magic, which is code to the session like any other.
            "a cell magic",
            "magic",
            "%%sql\nSELECT count(*) FROM sales".to_string(),
        ),
        (
            "code outside ASCII",
            "unicode",
            "print('h\u{e9}llo w\u{f6}rld \u{2014} \u{1f389}')".to_string(),
        ),
        (
            "code carrying quotes and backslashes",
            "quoting",
            "path = \"s3://bucket/key\\\\part=1\"\nprint(path)".to_string(),
        ),
        (
            "code at the modelled length bound",
            "longest-code",
            longest_code,
        ),
    ];

    for (case, session, code) in &cases {
        ready_session(&router, &engine, session).await;

        let id = run(&router, session, code).await;
        // The first statement of a session carries the id its sequence counts from.
        assert_eq!(
            id,
            i64::from(FIRST_STATEMENT_ID),
            "case {case}: expected the first statement of a session to be \
             {FIRST_STATEMENT_ID}, got {id}"
        );

        let read = get(&router, session, id).await;
        assert_eq!(
            read["Id"].as_i64(),
            Some(id),
            "case {case}: the read must report the id the run answered, got {read}"
        );
        // "The execution code of the statement", exactly as the run submitted it.
        assert_eq!(
            read["Code"].as_str().unwrap_or_default(),
            code.as_str(),
            "case {case}: the code must round-trip, got {} bytes",
            read["Code"].as_str().unwrap_or_default().len()
        );
        // "The state while request is actioned": the session has taken it and nothing has
        // actioned it yet.
        assert_eq!(
            read["State"].as_str(),
            Some(STATE_WAITING),
            "case {case}: expected {STATE_WAITING} straight after the run, got {read}"
        );
        // "The unix time and date that the job definition was started."
        assert!(
            read["StartedOn"].as_i64().is_some_and(|at| at > 0),
            "case {case}: expected a StartedOn instant, got {read}"
        );
        // A statement that has not finished has neither an end nor output, and its code
        // execution has made no progress.
        assert_eq!(
            read["CompletedOn"].as_i64().unwrap_or_default(),
            0,
            "case {case}: an unfinished statement reports no CompletedOn, got {read}"
        );
        assert_eq!(
            read["Progress"].as_f64().unwrap_or_default(),
            0.0,
            "case {case}: an unfinished statement has made no progress, got {read}"
        );
        assert!(
            read.get("Output").is_none_or(Value::is_null),
            "case {case}: an unfinished statement reports no Output, got {read}"
        );
    }

    // Each run is its own record on its own session: after every one has landed, each
    // session still reports ITS code under the same id, so no run overwrote another.
    for (case, session, code) in &cases {
        let read = get(&router, session, i64::from(FIRST_STATEMENT_ID)).await;
        assert_eq!(
            read["Code"].as_str().unwrap_or_default(),
            code.as_str(),
            "case {case}: every session keeps its own statement, got {} bytes",
            read["Code"].as_str().unwrap_or_default().len()
        );
    }
}

/// A session mints its statement ids in SUBMISSION ORDER, counting up from the first, and
/// every id keeps addressing the code it was minted for.
#[tokio::test]
async fn one_session_mints_statement_ids_in_submission_order() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "ordered";
    ready_session(&router, &engine, session).await;

    // Rows: how many statements one session holds, from one up.
    let submitted: Vec<String> = (0..4).map(|index| format!("print({index})")).collect();
    let mut minted = Vec::new();
    for (index, code) in submitted.iter().enumerate() {
        let id = run(&router, session, code).await;
        minted.push(id);
        let expected = i64::from(FIRST_STATEMENT_ID) + index as i64;
        assert_eq!(
            id, expected,
            "case statement {index}: expected the ids to count up to {expected}, got {minted:?}"
        );
        // The session takes the next statement once this one has come to rest, which its
        // own execution and then the clock decide.
        statement_at_rest(&router, &engine, session, id).await;
    }

    for (index, (id, code)) in minted.iter().zip(submitted.iter()).enumerate() {
        let read = get(&router, session, *id).await;
        assert_eq!(
            read["Code"].as_str(),
            Some(code.as_str()),
            "case statement {index}: id {id} must still address its own code, got {read}"
        );
    }
}

// ---------------------------------------------------------------------------
// Queueing
// ---------------------------------------------------------------------------

/// How many statements of the session `id` the store itself holds as being EXECUTED right
/// now.
///
/// A session runs the statements it has taken ONE AT A TIME, which is not observable from a
/// state a reader sees, so it is asserted on the store: never more than one of a session's
/// statements carries its runtime at the same instant.
fn executing_statement_count(engine: &Arc<engine::InMemoryEngine>, id: &str) -> usize {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store
        .session(REGION, id)
        .map(|session| {
            session
                .statements
                .iter()
                .filter(|statement| statement.executing_since.is_some())
                .count()
        })
        .unwrap_or_default()
}

/// The `Id` of every statement `ListStatements` reports for `id`, in the order it served
/// them — read through the listing suite's own page reader, so both suites read a page the
/// same way.
async fn listed_statement_ids(router: &MultiServiceRouter, id: &str) -> Vec<i64> {
    let page = crate::statement_listing::list(router, id, None).await;
    crate::statement_listing::listed_ids(&page)
}

/// A session whose runtime is up takes EVERY statement submitted to it: a run submitted
/// while the session is still actioning another is queued rather than refused, each answers
/// with its own id in submission order, the listing then reports all of them in that order,
/// and each comes to rest carrying what its OWN code computed — while the session never runs
/// two of them at the same instant.
///
/// The rows differ along how many statements are submitted back to back, from two up.
#[tokio::test]
async fn a_session_queues_the_statements_submitted_back_to_back() {
    // (case, the session, how many statements are submitted without waiting for any)
    let rows: &[(&str, &str, usize)] = &[
        ("two back-to-back runs", "queued-two", 2),
        ("three back-to-back runs", "queued-three", 3),
        ("five back-to-back runs", "queued-five", 5),
    ];

    for (case, session, count) in rows {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_on(engine.clone());
        ready_session(&router, &engine, session).await;

        // Code whose answer has to be COMPUTED: the product of the cell's own position,
        // which nothing of the request echoes.
        let submitted: Vec<String> = (0..*count)
            .map(|index| format!("print({index} * 7 + 3)"))
            .collect();
        let mut minted: Vec<i64> = Vec::new();
        for (index, code) in submitted.iter().enumerate() {
            let id = run(&router, session, code).await;
            let expected = i64::from(FIRST_STATEMENT_ID) + index as i64;
            assert_eq!(
                id, expected,
                "case {case}: statement {index} must be taken and minted {expected}, got \
                 {id} after {minted:?}"
            );
            minted.push(id);
            let executing = executing_statement_count(&engine, session);
            assert!(
                executing <= 1,
                "case {case}: a session runs one statement at a time, and {executing} of \
                 them were being executed after submitting statement {index}"
            );
        }

        // Every queued statement is a record of the session, in submission order.
        let listed = listed_statement_ids(&router, session).await;
        assert_eq!(
            listed, minted,
            "case {case}: the listing must report every queued statement in submission \
             order, got {listed:?}"
        );

        // Each of them runs, and comes to rest in the state its own code came to with that
        // code's own output on it.
        for (index, id) in minted.iter().enumerate() {
            let settled = statement_at_rest(&router, &engine, session, *id).await;
            let executing = executing_statement_count(&engine, session);
            assert!(
                executing <= 1,
                "case {case}: a session runs one statement at a time, and {executing} of \
                 them were being executed while statement {index} came to rest"
            );
            assert_eq!(
                settled["State"].as_str(),
                Some(STATE_AVAILABLE),
                "case {case}: queued statement {index} must run to its end, got {settled}"
            );
            let computed = format!("{}\n", index * 7 + 3);
            assert_eq!(
                settled
                    .pointer("/Output/Data/TextPlain")
                    .and_then(Value::as_str),
                Some(computed.as_str()),
                "case {case}: queued statement {index} must report what its own code \
                 computed, got {settled}"
            );
        }
    }
}

/// A statement the session has TAKEN but not begun actioning does not stop it taking the
/// next one: the run is served while the earlier statement is still queued, each keeps its
/// own id, and the queued statement is left where it was.
#[tokio::test]
async fn a_statement_still_in_flight_does_not_refuse_the_next_run() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "still-in-flight";
    ready_session(&router, &engine, session).await;

    let in_flight = run(&router, session, "print('in flight')").await;
    // The statement stays where it is for the whole test: every state the clock still owes
    // it is held open, so the session has taken it and not begun actioning it.
    make_owed_statement_states_due(&engine, session, far_future());

    for (case, code) in [
        ("a second run", "print('second')"),
        ("a third run", "print('third')"),
    ] {
        let next = run(&router, session, code).await;
        assert!(
            next > in_flight,
            "case {case}: a session that has taken statement {in_flight} must take another \
             and mint it a later id, got {next}"
        );
        make_owed_statement_states_due(&engine, session, far_future());
        let read = get(&router, session, in_flight).await;
        assert!(
            !TERMINAL_STATES.contains(&read["State"].as_str().unwrap_or_default()),
            "case {case}: taking another statement must not finish the one in flight, got \
             {read}"
        );
    }

    let listed = listed_statement_ids(&router, session).await;
    assert_eq!(
        listed,
        vec![in_flight, in_flight + 1, in_flight + 2],
        "every statement the session took must be a record of it, in submission order, got \
         {listed:?}"
    );
}

/// Put the statement `statement` of the session `id` into the state its runtime ACTIONS it
/// in, and hold it there: `RUNNING`, with every state the clock could still owe it ahead of
/// the row.
///
/// The way a suite observes what a session does while its runtime is actioning a statement
/// without waiting for a span or for a cell's own code, written through the store the way
/// every other owed instant in these suites is.
fn force_statement_running(engine: &Arc<engine::InMemoryEngine>, id: &str, statement: i64) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .session_mut(REGION, id)
        .unwrap_or_else(|| panic!("{id} was created"));
    let held = entry
        .statements
        .iter_mut()
        .find(|held| i64::from(held.id) == statement)
        .unwrap_or_else(|| panic!("session {id:?} must hold statement {statement}"));
    held.state = STATE_RUNNING.to_string();
    held.settles_at = Some(far_future());
    held.rests_at = Some(far_future());
}

/// A session whose runtime is ACTIONING a statement takes the next one too: it is queued
/// behind the one being actioned, which is left where it was, and the listing then reports
/// both in submission order.
///
/// This is the other side of the queueing rule from the back-to-back rows: the statement in
/// flight is in the state its runtime actions it in rather than one the session has merely
/// taken.
#[tokio::test]
async fn a_session_actioning_a_statement_queues_the_next_run() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "actioning";
    ready_session(&router, &engine, session).await;

    let actioned = run(&router, session, "print('actioned')").await;
    force_statement_running(&engine, session, actioned);
    assert_eq!(
        get(&router, session, actioned).await["State"].as_str(),
        Some(STATE_RUNNING),
        "the row's statement must be the one the runtime is actioning"
    );

    for (case, code) in [
        ("a second run", "print('second')"),
        ("a third run", "print('third')"),
    ] {
        let queued = run(&router, session, code).await;
        assert!(
            queued > actioned,
            "case {case}: a session actioning statement {actioned} must take another and \
             mint it a later id, got {queued}"
        );
        let read = get(&router, session, actioned).await;
        assert_eq!(
            read["State"].as_str(),
            Some(STATE_RUNNING),
            "case {case}: taking another statement must leave the one being actioned where \
             it was, got {read}"
        );
    }

    let listed = listed_statement_ids(&router, session).await;
    assert_eq!(
        listed,
        vec![actioned, actioned + 1, actioned + 2],
        "every statement the session took must be a record of it, in submission order, got \
         {listed:?}"
    );
}

/// A claim NOTHING ever answers for does not stall the queue: a statement the session took
/// up long ago and whose execution never reported stops counting as carrying the runtime, so
/// the next statement is still run and comes to rest carrying what its own code computed.
///
/// The stale claim is written through the store, which is the only way a claim nothing
/// answers for exists at all — the task that performs one records an outcome on every path.
#[tokio::test]
async fn a_claim_nothing_answers_for_does_not_stall_the_queue() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "stale-claim";
    ready_session(&router, &engine, session).await;

    // A statement the session took up longer ago than a claim counts for, owing no further
    // state and awaiting no execution: whatever was running it is gone.
    let stale = {
        let state = engine.state::<ComAmazonawsGlueAwsglueState>();
        let mut store = state.lock();
        let entry = store
            .session_mut(REGION, session)
            .unwrap_or_else(|| panic!("{session} was created"));
        let mut statement = StatementEntry::run(entry.next_statement_id, "print('lost')", SystemTime::now());
        statement.state = STATE_RUNNING.to_string();
        statement.settles_at = Some(far_future());
        statement.awaiting_execution = false;
        statement.executing_since = SystemTime::now()
            .checked_sub(2 * STATEMENT_CLAIM_ABANDONED_AFTER);
        let id = statement.id;
        entry.statements.push(statement);
        entry.next_statement_id = id + 1;
        i64::from(id)
    };

    let next = run(&router, session, "print(6 * 7)").await;
    assert!(
        next > stale,
        "the session must take another statement after a claim nothing answered for, got \
         {next} after {stale}"
    );
    let settled = statement_at_rest(&router, &engine, session, next).await;
    assert_eq!(
        settled["State"].as_str(),
        Some(STATE_AVAILABLE),
        "the statement behind the lost one must run to its end, got {settled}"
    );
    assert_eq!(
        settled
            .pointer("/Output/Data/TextPlain")
            .and_then(Value::as_str),
        Some("42\n"),
        "it must report what its own code computed, got {settled}"
    );
}

/// A session keeps at most the statement records it retains, whatever a caller submits: a
/// queued statement cannot grow what one session holds past that ceiling, the OLDEST records
/// go first, and the one the session's runtime is carrying is kept whatever its age.
#[tokio::test]
async fn a_session_keeps_at_most_the_statements_it_retains() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "retained";
    ready_session(&router, &engine, session).await;

    // Fill the session to its retention window through the store, so the ceiling is reached
    // without running a hundred cells: the first record is the one its runtime is carrying,
    // every other one is queued with nothing running it.
    let carried = {
        let state = engine.state::<ComAmazonawsGlueAwsglueState>();
        let mut store = state.lock();
        let entry = store
            .session_mut(REGION, session)
            .unwrap_or_else(|| panic!("{session} was created"));
        let now = SystemTime::now();
        entry.statements = (0..MAX_STATEMENTS_PER_SESSION)
            .map(|index| {
                let id = FIRST_STATEMENT_ID + index as i32;
                let mut statement = StatementEntry::run(id, &format!("print({id})"), now);
                statement.settles_at = Some(far_future());
                statement.awaiting_execution = false;
                if index == 0 {
                    // The record the runtime is carrying: its claim is fresh, so it is the
                    // one record the retention must keep.
                    statement.state = STATE_RUNNING.to_string();
                    statement.executing_since = Some(now);
                }
                statement
            })
            .collect();
        entry.next_statement_id = FIRST_STATEMENT_ID + MAX_STATEMENTS_PER_SESSION as i32;
        i64::from(FIRST_STATEMENT_ID)
    };

    // Every further run is taken, and none of them grows what the session holds.
    for case in 1..=3 {
        let taken = run(&router, session, &format!("print('taken {case}')")).await;
        assert_eq!(
            stored_statement_count(&engine, session),
            MAX_STATEMENTS_PER_SESSION,
            "case {case}: a session must hold at most {MAX_STATEMENTS_PER_SESSION} statements"
        );
        // The statement just taken is one of the records kept, and so is the one being
        // carried: what went is the OLDEST of the rest.
        let read = get(&router, session, taken).await;
        assert_eq!(
            read["Id"].as_i64(),
            Some(taken),
            "case {case}: the statement just taken must be a record of the session, got {read}"
        );
        let carrying = get(&router, session, carried).await;
        assert_eq!(
            carrying["State"].as_str(),
            Some(STATE_RUNNING),
            "case {case}: the statement the runtime is carrying must be kept, got {carrying}"
        );
        let dropped = carried + case;
        let (status, body) = call(
            &router,
            "GetStatement",
            &json!({ "SessionId": session, "Id": dropped }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: the oldest record must have gone, and {dropped} reads back with \
             {status} and {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Which refusal a READ of the statement a refused run would have recorded answers.
///
/// A session the service still holds a runtime for serves its statement surface, so an id
/// inside that session's own id space is refused as a state of the SESSION; a session that
/// has ENDED has no runtime whose statements could be read at all, so the whole surface is
/// refused the way the run was.
enum ReadRefusal {
    IllegalSessionStatus,
    SessionEnded,
}

/// One row of the not-ready table: the session to create, the create body, how its status is
/// reached, the status it is then in, and the refusal a read of its statements answers.
struct NotReadyRow {
    case: &'static str,
    session: &'static str,
    create: Value,
    /// How far the session's own instants are pulled back, so its documented timeout has
    /// fallen due; `ZERO` for a row that does not turn on the clock.
    elapsed: Duration,
    /// The status written straight into the store for a row no call of this emulator reaches.
    forced: Option<&'static str>,
    status: &'static str,
    read_refusal: ReadRefusal,
}

/// A session whose runtime is not up takes no statement: a run is refused in every status but
/// `READY`, nothing is recorded for it, and the refusal is the same one whatever the status
/// says about the runtime — the session is not ready, which is reported as invalid input and
/// names the status it is actually in.
///
/// The rows span the modelled `SessionStatus` values other than `READY`: the two a session
/// reaches by the clock alone (the one its create put it in and the one its own documented
/// timeout brings it to), the one a session being taken down is in, and the two a session
/// that has ended rests in.
#[tokio::test]
async fn a_run_on_a_session_that_is_not_ready_is_refused() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    let cases: Vec<NotReadyRow> = vec![
        NotReadyRow {
            case: "a session whose provisioning has not finished",
            session: "provisioning",
            create: json!({}),
            elapsed: Duration::ZERO,
            forced: None,
            status: STATUS_PROVISIONING,
            read_refusal: ReadRefusal::IllegalSessionStatus,
        },
        NotReadyRow {
            // "The number of minutes before session times out", at the modelled floor of
            // `Timeout` ("range 1.."), so one minute later the session has ended.
            case: "a session its own timeout has ended",
            session: "timed-out",
            create: json!({ "Timeout": 1, "IdleTimeout": 1 }),
            elapsed: Duration::from_secs(2 * SECONDS_PER_MINUTE),
            forced: None,
            status: STATUS_TIMEOUT,
            read_refusal: ReadRefusal::SessionEnded,
        },
        NotReadyRow {
            case: "a session whose runtime is being taken down",
            session: "stopping",
            create: json!({}),
            elapsed: Duration::ZERO,
            forced: Some(STATUS_STOPPING),
            status: STATUS_STOPPING,
            read_refusal: ReadRefusal::IllegalSessionStatus,
        },
        NotReadyRow {
            case: "a session whose runtime never came up",
            session: "failed",
            create: json!({}),
            elapsed: Duration::ZERO,
            forced: Some(STATUS_FAILED),
            status: STATUS_FAILED,
            read_refusal: ReadRefusal::SessionEnded,
        },
        NotReadyRow {
            case: "a session a stop took down",
            session: "stopped",
            create: json!({}),
            elapsed: Duration::ZERO,
            forced: Some(STATUS_STOPPED),
            status: STATUS_STOPPED,
            read_refusal: ReadRefusal::SessionEnded,
        },
    ];

    for row in &cases {
        let NotReadyRow {
            case,
            session,
            create,
            elapsed,
            forced,
            status: status_expected,
            read_refusal,
        } = row;
        create_session(&router, session, create.clone()).await;
        if let Some(status) = forced {
            force_session_status(&engine, session, status);
        } else if elapsed.is_zero() {
            // The provisioning this row is about is held open for the whole row.
            set_session_due(&engine, session, far_future());
        } else {
            rewind_session(&engine, session, *elapsed);
            set_session_due(&engine, session, already_past());
        }

        let (status, body) = call(
            &router,
            "RunStatement",
            &json!({ "SessionId": session, "Code": "print('too early')" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a session that is not ready must be refused with 400, got \
             {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            messages::session_not_ready(session, status_expected),
            "case {case}: the refusal must name the session and the status it is in, got \
             {body}"
        );

        // The refused run recorded nothing: the session holds no statement at all. Read off
        // the store, because a session that has ENDED refuses the reads of its own statement
        // surface too, so a refusal from one of them would not tell a statement that was
        // never written from one that was.
        assert_eq!(
            stored_statement_count(&engine, session),
            0,
            "case {case}: a refused run must record no statement"
        );
        let (status, body) = call(
            &router,
            "GetStatement",
            &json!({ "SessionId": session, "Id": FIRST_STATEMENT_ID }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: no statement of this session reads back, got {status} with {body}"
        );
        let (expected_code, expected_message) = match read_refusal {
            ReadRefusal::IllegalSessionStatus => (
                "IllegalSessionStateException",
                messages::STATEMENT_ILLEGAL_SESSION_STATUS.to_string(),
            ),
            ReadRefusal::SessionEnded => (
                "InvalidInputException",
                messages::session_not_ready(session, status_expected),
            ),
        };
        assert!(
            error_code(&body).ends_with(expected_code),
            "case {case}: expected {expected_code} reading the statement back, got {} in \
             {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case}: unexpected wording reading the statement back, got {body}"
        );
    }
}

/// A run or a read for a session this endpoint never created is a miss, and so is a read for
/// a statement id OUTSIDE the id space of the session it was asked for — while an id inside
/// that space the session does not hold is refused as a state of the session instead.
#[tokio::test]
async fn an_address_nothing_holds_is_a_miss() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "held";
    ready_session(&router, &engine, session).await;
    let held = run(&router, session, "print('held')").await;

    for (case, id) in [
        ("an id nothing created", "absent"),
        ("a prefix of a stored id", "hel"),
        ("a stored id in another case", "HELD"),
    ] {
        for (operation, expected) in [
            // The run words its session miss with the service's fixed, generic entity
            // sentence, which names nothing of the request.
            (
                "RunStatement",
                messages::RUN_STATEMENT_SESSION_NOT_FOUND.to_string(),
            ),
            // The statement READ surface words its own session miss, naming the member it
            // could not resolve.
            ("GetStatement", messages::session_id_not_found(id)),
        ] {
            let (status, body) = call(
                &router,
                operation,
                &json!({ "SessionId": id, "Code": "print(1)", "Id": held }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case}: {operation} on an unknown session must be refused, got \
                 {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("EntityNotFoundException"),
                "case {case}: expected EntityNotFoundException, got {} in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case}: {operation} must answer its own session miss {expected:?}, \
                 got {body}"
            );
        }
    }

    // The same session through another Region's endpoint: a session is served only by the
    // endpoint that created it.
    for region in ["eu-west-1", "us-west-2"] {
        let (status, body) = call_in_region(
            &router,
            region,
            "RunStatement",
            &json!({ "SessionId": session, "Code": "print(1)" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {region}: a session is reachable only through its own endpoint, got \
             {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::RUN_STATEMENT_SESSION_NOT_FOUND,
            "case {region}: expected the run's fixed session miss, got {body}"
        );
    }

    // An id the session does not hold, on either side of its own id space: the ids a session
    // mints count up from the first, so the ones it has minted and the one it would mint
    // next name statement slots of this session's runtime, and every other id names no
    // statement of any session.
    //
    // (case, the id asked for, the modelled error, the sentence it carries)
    let misses: Vec<(&str, i64, &str, String)> = vec![
        (
            "the id the session would mint next",
            held + 1,
            "IllegalSessionStateException",
            messages::STATEMENT_ILLEGAL_SESSION_STATUS.to_string(),
        ),
        (
            "one past the id space",
            held + 2,
            "EntityNotFoundException",
            messages::statement_not_found((held + 2) as i32),
        ),
        (
            "far above the id space",
            held + 987_654,
            "EntityNotFoundException",
            messages::statement_not_found((held + 987_654) as i32),
        ),
        (
            // "default 0" (`IntegerValue`): the id a request that named none carries, which
            // is below the first id a session mints.
            "the modelled default id",
            0,
            "EntityNotFoundException",
            messages::statement_not_found(0),
        ),
        (
            "an id below the id space",
            -1,
            "EntityNotFoundException",
            messages::statement_not_found(-1),
        ),
    ];

    for (case, id, expected_code, expected_message) in &misses {
        let (status, body) = call(
            &router,
            "GetStatement",
            &json!({ "SessionId": session, "Id": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a statement nothing recorded must be refused, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with(expected_code),
            "case {case}: expected {expected_code} for id {id}, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            &error_message(&body),
            expected_message,
            "case {case}: unexpected wording for id {id}, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// The state the clock owes
// ---------------------------------------------------------------------------

/// A statement leaves `WAITING` when the clock owes it its next state, and not before: while
/// the due instant is ahead every read reports it still waiting, and once every state it is
/// owed has fallen due every read reports the same record — the state its own execution came
/// to, with that execution's output on it.
#[tokio::test]
async fn the_clock_owes_every_statement_the_state_its_execution_came_to() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "owed";
    ready_session(&router, &engine, session).await;
    let id = run(&router, session, "print('owed')").await;

    // Before the due instant: reading does not move the statement, however often.
    set_statement_due(&engine, session, far_future());
    for read_number in 1..=3 {
        let read = get(&router, session, id).await;
        assert_eq!(
            read["State"].as_str(),
            Some(STATE_WAITING),
            "read {read_number}: a statement whose span has not elapsed is still \
             {STATE_WAITING}, got {read}"
        );
    }

    // Once every owed state has fallen due: the state the statement's own execution came to.
    let settled = statement_at_rest(&router, &engine, session, id).await;
    assert_eq!(
        settled["State"].as_str(),
        Some(STATE_AVAILABLE),
        "expected {STATE_AVAILABLE} for code that ran to its end, got {settled}"
    );
    let output = settled
        .get("Output")
        .cloned()
        .unwrap_or_else(|| panic!("a finished statement reports an Output, got {settled}"));
    assert_eq!(
        output["Status"].as_str(),
        Some(STATE_AVAILABLE),
        "the output status must be the state the statement came to rest in, got {output}"
    );
    assert_eq!(
        output.pointer("/Data/TextPlain").and_then(Value::as_str),
        Some("owed\n"),
        "the output must carry what the code itself wrote, got {output}"
    );
    assert!(
        output.get("ErrorName").is_none_or(Value::is_null),
        "code that failed at nothing reports no error name, got {output}"
    );
    assert!(
        output.get("ErrorValue").is_none_or(Value::is_null),
        "code that failed at nothing reports no error value, got {output}"
    );
    let started_on = settled["StartedOn"].as_i64().unwrap_or_default();
    let completed_on = settled["CompletedOn"].as_i64().unwrap_or_default();
    assert!(
        started_on > 0,
        "expected a StartedOn instant, got {settled}"
    );
    assert!(
        completed_on > started_on,
        "expected the statement to complete after it started, got {started_on} then \
         {completed_on}"
    );

    // Both states were owed by the CLOCK: reading again reports the same record.
    for read_number in 1..=3 {
        let again = get(&router, session, id).await;
        assert_eq!(
            again, settled,
            "read {read_number}: a settled statement reports what the first read did, got \
             {again}"
        );
    }
}
