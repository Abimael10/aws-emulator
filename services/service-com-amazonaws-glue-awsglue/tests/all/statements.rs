// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Glue statement slice: the `RunStatement` that records a
//! statement on an interactive session, and the `GetStatement` that reads it back.
//!
//! RULE: a statement is a record of the session it was run on, addressed by the session's
//! `Id` and an `Id` that session mints for it in submission order, and a session takes one
//! only while its runtime is up and free — so a run is refused when the session is in any
//! status but `READY` and when a statement of that session has not finished, and the
//! statement a run records carries the code it submitted until the clock moves it to its
//! terminal state.
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
    messages, ComAmazonawsGlueAwsglueState, FIRST_STATEMENT_ID,
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
pub(crate) const STATE_AVAILABLE: &str = "AVAILABLE";

/// The `SessionStatus` values a run is refused in here, of the modelled enum
/// (`PROVISIONING`, `READY`, `FAILED`, `TIMEOUT`, `STOPPING`, `STOPPED`, from
/// `smithy_query shape=SessionStatus`).
const STATUS_PROVISIONING: &str = "PROVISIONING";
const STATUS_TIMEOUT: &str = "TIMEOUT";

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
// Refusals
// ---------------------------------------------------------------------------

/// A session still actioning a statement cannot take another: the run is refused, nothing
/// is recorded for it, and the session takes a statement again once the one in flight has
/// come to rest.
#[tokio::test]
async fn a_run_on_a_session_still_actioning_a_statement_is_refused() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let session = "busy";
    ready_session(&router, &engine, session).await;

    let in_flight = run(&router, session, "print('in flight')").await;
    // The statement stays in flight for the whole span, whatever a caller does next: the
    // state the clock owes it next is held open.
    set_statement_due(&engine, session, far_future());

    for (case, code) in [
        ("a second run", "print('second')"),
        ("a third run", "print('third')"),
    ] {
        let (status, body) = call(
            &router,
            "RunStatement",
            &json!({ "SessionId": session, "Code": code }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a busy session must be refused with 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            messages::session_busy_with_statement(session, in_flight as i32),
            "case {case}: the refusal must name the statement the session is still \
             running, got {body}"
        );
    }

    // A refused run records nothing: the next id the session mints is the one after the
    // statement in flight, not after the refusals.
    let (status, body) = call(
        &router,
        "GetStatement",
        &json!({ "SessionId": session, "Id": in_flight + 1 }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a refused run must record no statement, got {status} with {body}"
    );

    statement_at_rest(&router, &engine, session, in_flight).await;
    let next = run(&router, session, "print('taken')").await;
    assert_eq!(
        next,
        in_flight + 1,
        "a session takes a statement again once the one in flight has come to rest, and \
         mints the next id for it, got {next}"
    );
}

/// A session whose runtime is not up takes no statement: a run is refused in every status
/// but `READY`, and nothing is recorded for it.
///
/// The rows are the statuses a session reaches by the clock alone: the one its create put
/// it in, and the one its own documented timeout brings it to.
#[tokio::test]
async fn a_run_on_a_session_that_is_not_ready_is_refused() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    // (case, the session, the create body, how far its clock is pulled back, the status)
    let cases: Vec<(&str, &str, Value, Duration, &str)> = vec![
        (
            "a session whose provisioning has not finished",
            "provisioning",
            json!({}),
            Duration::ZERO,
            STATUS_PROVISIONING,
        ),
        (
            // "The number of minutes before session times out", at the modelled floor of
            // `Timeout` ("range 1.."), so one minute later the session has ended.
            "a session its own timeout has ended",
            "timed-out",
            json!({ "Timeout": 1, "IdleTimeout": 1 }),
            Duration::from_secs(2 * SECONDS_PER_MINUTE),
            STATUS_TIMEOUT,
        ),
    ];

    for (case, session, extra, elapsed, status_expected) in &cases {
        create_session(&router, session, extra.clone()).await;
        if elapsed.is_zero() {
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
            error_code(&body).ends_with("IllegalSessionStateException"),
            "case {case}: expected IllegalSessionStateException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            messages::session_not_ready(session, status_expected),
            "case {case}: the refusal must name the session and the status it is in, got \
             {body}"
        );

        // The refused run recorded nothing.
        let (status, body) = call(
            &router,
            "GetStatement",
            &json!({ "SessionId": session, "Id": FIRST_STATEMENT_ID }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a refused run records no statement, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::statement_not_found(session, FIRST_STATEMENT_ID),
            "case {case}: expected the miss to name the statement asked for, got {body}"
        );
    }
}

/// A run or a read for a session this endpoint never created is a miss, and so is one for a
/// statement id its session never minted.
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

    // An id the session never minted, on either side of the one it did.
    for id in [-1, 0, held + 1, held + 99] {
        let (status, body) = call(
            &router,
            "GetStatement",
            &json!({ "SessionId": session, "Id": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {id}: a statement nothing recorded must be refused, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {id}: expected EntityNotFoundException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            messages::statement_not_found(session, id as i32),
            "case {id}: the refusal must name the statement asked for, got {body}"
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
