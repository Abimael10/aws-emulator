// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Glue interactive session STOP slice: `StopSession`, and the
//! `GetSession` / `GetStatement` / `RunStatement` calls that observe what it took down.
//!
//! RULE: a stop addressed by a session id takes DOWN the remote Spark runtime environment the
//! Region it is served in holds under that id, and answers with that id — the session reports
//! the modelled `SessionStatus` `STOPPING` from then until the instant the clock owes it
//! `STOPPED`, a whole span out, where it comes to rest with `CompletedOn` stamped at that
//! instant; the statements it was still running end `CANCELLED` with it, the statements that
//! had already finished keep what they came to, and no further statement is taken. The
//! statements of a session are held by the runtime it is, so once that runtime is gone its
//! whole statement surface — the run, the read and the listing — is refused, where a session
//! whose runtime is still coming down still reports them. Only a
//! session whose environment is up has one to take down, so a stop of a session already
//! coming down or already down — its environment never came up, its own timeout ended it, or
//! a stop already took it down — has nothing further to begin: it is served and changes
//! nothing, which makes a repeated stop a no-op. A session the service is still BRINGING UP
//! has no settled environment to take down, so that one is refused with the modelled
//! `IllegalSessionStateException` naming the status it is in and the record survives
//! unchanged. An id the Region holds no session under is the service's entity miss.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: how many statements the session
//! holds and whether one is still in flight, the form and the length of the id, which of the
//! modelled `SessionStatus` values a refused stop lands on and how that status was reached
//! (the clock, a stop of this emulator's own, or a status nothing but a caller reaches), the
//! Region the call is served in, and how a requested id misses. Nothing here sleeps: a row
//! positions the stored instants the statuses turn on, which is what fixes which side of a
//! span a call lands on.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    messages, ComAmazonawsGlueAwsglueState, FIRST_STATEMENT_ID, SESSION_SETTLES_AFTER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{already_past, far_future};
use crate::statements::{
    create_session, force_session_status, get as get_statement, make_owed_statement_states_due,
    ready_session, rewind_session, run, set_session_due, statement_at_rest, stored_session,
    SECONDS_PER_MINUTE,
};
use crate::wire::{call, call_in_region, emulator_on, error_code, error_message, ok, REGION};

/// The modelled `SessionStatus` values this suite observes (`PROVISIONING`, `READY`,
/// `FAILED`, `TIMEOUT`, `STOPPING`, `STOPPED`, from `smithy_query shape=SessionStatus`),
/// written out as literals rather than read from the service's own constants, so this suite
/// pins the VALUES the wire carries.
const STATUS_PROVISIONING: &str = "PROVISIONING";
const STATUS_READY: &str = "READY";
const STATUS_FAILED: &str = "FAILED";
const STATUS_TIMEOUT: &str = "TIMEOUT";
const STATUS_STOPPING: &str = "STOPPING";
const STATUS_STOPPED: &str = "STOPPED";

/// The modelled `StatementState` values this suite observes, of the published enum
/// (`WAITING`, `RUNNING`, `AVAILABLE`, `CANCELLING`, `CANCELLED`, `ERROR`, from
/// `smithy_query shape=StatementState`), as literals for the same reason.
const STATE_AVAILABLE: &str = "AVAILABLE";
const STATE_CANCELLED: &str = "CANCELLED";

/// The `StatementState` values a statement never leaves again, of that same enum.
const TERMINAL_STATES: [&str; 3] = [STATE_AVAILABLE, STATE_CANCELLED, "ERROR"];

/// The longest `Id` the model accepts: `NameString`, "len 1..255"
/// (`smithy_query operation=StopSession`).
const MAX_ID_LENGTH: usize = 255;

/// Minutes past which a session with a one-minute idle timeout has timed out, so the clock
/// alone carries it to [`STATUS_TIMEOUT`].
const IDLE_MINUTES: u64 = 5;

/// An emulator and the engine behind it, so a suite can reach a stored session's record.
fn harness() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    (router, engine)
}

/// The status the clock owes the stored session `id`, and the instant it owes it at.
fn owed_status(engine: &Arc<engine::InMemoryEngine>, id: &str) -> (String, Option<SystemTime>) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    let entry = store
        .session(REGION, id)
        .unwrap_or_else(|| panic!("{id} was created"));
    (entry.settles_to.clone(), entry.settles_at)
}

/// Stop the session `id` and return the `Id` the answer reports, if any.
async fn stop(router: &MultiServiceRouter, id: &str) -> Option<String> {
    let stopped = ok(router, "StopSession", &json!({ "Id": id })).await;
    stopped
        .get("Id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `Session` the `GetSession` answer for `id` reports.
async fn get_session(router: &MultiServiceRouter, id: &str) -> Value {
    let read = ok(router, "GetSession", &json!({ "Id": id })).await;
    read.get("Session")
        .cloned()
        .unwrap_or_else(|| panic!("GetSession must report a Session, got {read}"))
}

/// A `f64` member of a reported session.
fn number(case: &str, session: &Value, member: &str) -> f64 {
    session
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: expected a {member} figure, got {session}"))
}

/// `at` in the Unix milliseconds a reported STATEMENT instant is stated in, rounded down —
/// so it is the floor an instant at or after `at` must report.
fn unix_millis_floor(at: SystemTime) -> i64 {
    let millis = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("an instant after the Unix epoch")
        .as_millis();
    i64::try_from(millis).expect("an instant inside the reported range")
}

/// Assert that a run of code on the session `id` is refused with `expected_error` because the
/// session is in `status` rather than ready to take code.
async fn assert_run_refused(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    status: &str,
    expected_error: &str,
) {
    let (code, body) = call(
        router,
        "RunStatement",
        &json!({ "SessionId": id, "Code": "1 + 1" }),
    )
    .await;
    assert_eq!(
        code, 400,
        "case {case}: a session in {status} must take no statement, got {code} with {body}"
    );
    assert!(
        error_code(&body).ends_with(expected_error),
        "case {case}: expected {expected_error} running on a {status} session, got {} in {body}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        messages::session_not_ready(id, status),
        "case {case}: the refusal must name the session and the status it is in, got {body}"
    );
}

// ---------------------------------------------------------------------------
// Stop a ready session, then observe what came down
// ---------------------------------------------------------------------------

/// One row of the stop table: the id to create the session under, how many statements are run
/// and brought to rest on it first, and whether one is still in flight when the stop lands.
struct Row {
    case: &'static str,
    id: String,
    finished_statements: usize,
    statement_in_flight: bool,
}

/// A stop of a ready session takes its runtime down: the answer names the session, the clock
/// owes it `STOPPED` a whole span out and reports `STOPPING` until then, it takes no further
/// statement, the statement it was still running ends `CANCELLED` while the finished ones keep
/// what they came to, and once the owed instant is reached it rests in `STOPPED` with the
/// instant it ended stamped on it.
#[tokio::test]
async fn stopping_a_ready_session_takes_its_runtime_down() {
    let rows = vec![
        Row {
            // The shape a notebook client sends: create, work, stop.
            case: "a ready session with nothing run on it",
            id: "ready-idle".to_string(),
            finished_statements: 0,
            statement_in_flight: false,
        },
        Row {
            case: "a ready session whose statements have all finished",
            id: "ready-finished".to_string(),
            finished_statements: 2,
            statement_in_flight: false,
        },
        Row {
            case: "a ready session still running a statement",
            id: "ready-busy".to_string(),
            finished_statements: 0,
            statement_in_flight: true,
        },
        Row {
            case: "a ready session with a finished statement and one in flight",
            id: "Mixed.Case-session_42".to_string(),
            finished_statements: 1,
            statement_in_flight: true,
        },
        Row {
            case: "the shortest id the model accepts",
            id: "x".to_string(),
            finished_statements: 0,
            statement_in_flight: false,
        },
        Row {
            case: "an id at the modelled length bound",
            id: "i".repeat(MAX_ID_LENGTH),
            finished_statements: 0,
            statement_in_flight: false,
        },
    ];

    for row in &rows {
        let (router, engine) = harness();
        let case = row.case;
        let id = row.id.as_str();

        // A second session nothing in this row addresses: the stop must leave it alone.
        ready_session(&router, &engine, "bystander").await;
        ready_session(&router, &engine, id).await;

        // The statements that had already finished when the stop lands.
        let mut finished = Vec::new();
        for index in 0..row.finished_statements {
            let statement = run(&router, id, &format!("{index} + 1")).await;
            statement_at_rest(&router, &engine, id, statement).await;
            finished.push(statement);
        }
        // The statement still in flight, held in the state its run put it in by holding every
        // state the clock still owes it ahead of the clock, so the row's stop lands on a
        // statement the runtime has not finished.
        let in_flight = if row.statement_in_flight {
            let statement = run(&router, id, "1 + 1").await;
            make_owed_statement_states_due(&engine, id, far_future());
            let read = get_statement(&router, id, statement).await;
            let state = read["State"].as_str().unwrap_or_default();
            assert!(
                !TERMINAL_STATES.contains(&state),
                "case {case}: the row's statement must still be in flight when the stop lands, \
                 got {read}"
            );
            Some(statement)
        } else {
            None
        };

        // "Returns the Id of the stopped session."
        let before = SystemTime::now();
        assert_eq!(
            stop(&router, id).await.as_deref(),
            Some(id),
            "case {case}: StopSession must report the id it stopped"
        );

        // The clock owes the session the status it comes to rest in, a whole span after the
        // stop — read off the stored record, so no call has to land inside the span for this
        // to hold.
        let (settles_to, due) = owed_status(&engine, id);
        assert_eq!(
            settles_to, STATUS_STOPPED,
            "case {case}: the status the clock owes a stopped session must be {STATUS_STOPPED}"
        );
        let due = due.unwrap_or_else(|| {
            panic!("case {case}: a stopped session is owed its status at a stored instant")
        });
        let span = due
            .duration_since(before)
            .unwrap_or_else(|error| panic!("case {case}: the due instant must follow the stop, got {error}"));
        assert!(
            span >= SESSION_SETTLES_AFTER,
            "case {case}: expected the owed instant at least {SESSION_SETTLES_AFTER:?} after the \
             stop, got {span:?}"
        );

        // Until that instant the session reports its environment coming down, and has not
        // completed. The owed instant is held far enough ahead that the wall clock cannot
        // cross it while the row's calls are made, since the status turns on that instant.
        set_session_due(&engine, id, far_future());
        let stopping = get_session(&router, id).await;
        assert_eq!(
            stopping["Status"].as_str(),
            Some(STATUS_STOPPING),
            "case {case}: a session whose stop has not finished must report {STATUS_STOPPING}, \
             got {stopping}"
        );
        assert!(
            stopping["CompletedOn"].is_null(),
            "case {case}: a session still coming down has not completed, got {stopping}"
        );

        // A runtime that is coming down takes no code: the service is still moving the
        // session, so the run is refused as a state the session is in.
        assert_run_refused(
            &router,
            case,
            id,
            STATUS_STOPPING,
            "IllegalSessionStateException",
        )
        .await;

        // The statement the runtime was still running ends with it; the ones that had already
        // finished keep what their own execution came to.
        if let Some(statement) = in_flight {
            let read = get_statement(&router, id, statement).await;
            assert_eq!(
                read["State"].as_str(),
                Some(STATE_CANCELLED),
                "case {case}: a statement in flight when the session stops must end \
                 {STATE_CANCELLED}, got {read}"
            );
            let completed_on = read["CompletedOn"].as_i64().unwrap_or_else(|| {
                panic!(
                    "case {case}: a cancelled statement must carry the instant it ended, got {read}"
                )
            });
            let stopped_at = unix_millis_floor(before);
            assert!(
                completed_on >= stopped_at,
                "case {case}: expected the cancelled statement to end at or after the stop \
                 ({stopped_at} ms), got {completed_on} in {read}"
            );
        }
        for (index, statement) in finished.iter().enumerate() {
            let read = get_statement(&router, id, *statement).await;
            let submitted = format!("{index} + 1");
            assert_eq!(
                read["State"].as_str(),
                Some(STATE_AVAILABLE),
                "case {case}: a statement that had finished must keep its state, got {read}"
            );
            assert_eq!(
                read["Code"].as_str(),
                Some(submitted.as_str()),
                "case {case}: a finished statement must keep the code it was run with, got {read}"
            );
        }

        // Once the owed instant is behind the clock the session rests in STOPPED, with the
        // instant it ended stamped on it and the time it ran accounted — and two reads of it
        // report the same record. The stored instants are positioned rather than waited for:
        // the session's own instants are pulled back, and the instant its tear-down was owed
        // is put behind the clock.
        rewind_session(&engine, id, SESSION_SETTLES_AFTER * 2);
        set_session_due(&engine, id, already_past());
        let stopped = get_session(&router, id).await;
        assert_eq!(
            stopped["Status"].as_str(),
            Some(STATUS_STOPPED),
            "case {case}: a session whose stop has finished must report {STATUS_STOPPED}, got \
             {stopped}"
        );
        let created_on = number(case, &stopped, "CreatedOn");
        let completed_on = number(case, &stopped, "CompletedOn");
        let execution_time = number(case, &stopped, "ExecutionTime");
        let ran_for = completed_on - created_on;
        assert!(
            ran_for > 0.0,
            "case {case}: a stopped session must have ended after it was created, got {stopped}"
        );
        assert!(
            (ran_for - execution_time).abs() < 0.01,
            "case {case}: ExecutionTime must be the span from CreatedOn to CompletedOn; expected \
             {ran_for}, got {execution_time} in {stopped}"
        );
        let again = get_session(&router, id).await;
        assert_eq!(
            again, stopped,
            "case {case}: two reads of a stopped session must report the same record, got {again}"
        );

        // A session that has come down takes no code either — and it has no runtime left at
        // all, so its id names nothing that could take code.
        assert_run_refused(&router, case, id, STATUS_STOPPED, "InvalidInputException").await;

        // The statements of a session are held by the runtime it is, so a session whose
        // runtime is gone has no statement surface left to read at all: both the listing and
        // the single-statement read are refused the same way the run is, where the very same
        // reads were served while the session was still coming down.
        for (operation, request) in [
            ("ListStatements", json!({ "SessionId": id })),
            (
                "GetStatement",
                json!({ "SessionId": id, "Id": FIRST_STATEMENT_ID }),
            ),
        ] {
            let (code, body) = call(&router, operation, &request).await;
            assert_eq!(
                code, 400,
                "case {case}: {operation} on a {STATUS_STOPPED} session must be refused, got \
                 {code} with {body}"
            );
            assert!(
                error_code(&body).ends_with("InvalidInputException"),
                "case {case}: expected InvalidInputException from {operation}, got {} in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                messages::session_not_ready(id, STATUS_STOPPED),
                "case {case}: {operation}'s refusal must name the session and its status, got \
                 {body}"
            );
        }

        // The session the row never addressed is still up.
        let bystander = get_session(&router, "bystander").await;
        assert_eq!(
            bystander["Status"].as_str(),
            Some(STATUS_READY),
            "case {case}: the stop must leave every other session alone, got {bystander}"
        );
    }
}

// ---------------------------------------------------------------------------
// A session the service is still bringing up
// ---------------------------------------------------------------------------

/// A stop of a session the service is still BRINGING UP is refused with the modelled
/// `IllegalSessionStateException` naming the status it is in — there is no settled environment
/// for it to take down yet — and the record survives the refusal unchanged, while the very
/// same stop is served once that provisioning has finished.
#[tokio::test]
async fn stopping_a_session_still_being_provisioned_is_refused() {
    let (router, engine) = harness();
    let id = "coming-up";
    create_session(&router, id, json!({ "Description": "Coming up." })).await;
    // The status turns on the stored due instant, so holding that instant far enough ahead
    // that the wall clock cannot cross it is what fixes these calls on the provisioning side
    // of it.
    set_session_due(&engine, id, far_future());

    let (code, body) = call(&router, "StopSession", &json!({ "Id": id })).await;
    assert_eq!(
        code, 400,
        "a session still being provisioned must not be stopped, got {code} with {body}"
    );
    assert!(
        error_code(&body).ends_with("IllegalSessionStateException"),
        "expected IllegalSessionStateException, got {} in {body}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        messages::session_in_illegal_state(STATUS_PROVISIONING),
        "the refusal must name the status the session is in, got {body}"
    );
    assert!(
        body.get("Id").is_none_or(Value::is_null),
        "a refused stop reports no stopped id, got {body}"
    );

    // The record survives the refusal exactly as it was.
    assert!(
        stored_session(&engine, REGION, id),
        "a refused stop must leave the session stored"
    );
    let read = get_session(&router, id).await;
    assert_eq!(
        read["Status"].as_str(),
        Some(STATUS_PROVISIONING),
        "the refused session must still be being provisioned, got {read}"
    );
    assert_eq!(
        read["Description"].as_str(),
        Some("Coming up."),
        "the stored session must survive the refusal, got {read}"
    );

    // Once the provisioning the clock owed has finished, the very same stop is served.
    set_session_due(&engine, id, already_past());
    assert_eq!(
        stop(&router, id).await.as_deref(),
        Some(id),
        "a session whose provisioning has finished must stop"
    );
    set_session_due(&engine, id, far_future());
    let read = get_session(&router, id).await;
    assert_eq!(
        read["Status"].as_str(),
        Some(STATUS_STOPPING),
        "the served stop must take the session's environment down, got {read}"
    );
}

// ---------------------------------------------------------------------------
// A session already coming down, or already down
// ---------------------------------------------------------------------------

/// How a row's session reaches the status the repeated stop lands on.
enum Reach {
    /// Stopped, with the status its tear-down owes it still ahead of the clock.
    BeingStopped,
    /// Stopped, with that status already due.
    Stopped,
    /// Idle `minutes` past its declared idle window, so the clock alone ended it.
    IdleFor { minutes: u64 },
    /// A status only a caller reaches, written straight onto the stored record.
    Forced(&'static str),
}

/// A stop of a session that is already coming down, or already down, has nothing further to
/// begin: it is SERVED, reports the id it was addressed by and changes nothing about the
/// record — the status the session is in, the status the clock still owes it and the instant
/// it ended all stand — so stopping one session twice is a no-op rather than a refusal or a
/// second tear-down.
///
/// The rows differ along the status the repeated stop lands on and how it was reached: a stop
/// of this emulator's own, the clock, or a status nothing but a caller reaches.
#[tokio::test]
async fn stopping_a_session_already_coming_down_or_down_changes_nothing() {
    let rows: &[(&str, &str, Reach)] = &[
        (
            "a session already being taken down",
            STATUS_STOPPING,
            Reach::BeingStopped,
        ),
        (
            "a session a stop already took all the way down",
            STATUS_STOPPED,
            Reach::Stopped,
        ),
        (
            "a session its own idle window ended",
            STATUS_TIMEOUT,
            Reach::IdleFor {
                minutes: IDLE_MINUTES,
            },
        ),
        (
            "a session that failed to come up",
            STATUS_FAILED,
            Reach::Forced(STATUS_FAILED),
        ),
    ];

    for (case, status, reach) in rows {
        let (router, engine) = harness();
        let id = "already-down";

        match reach {
            Reach::BeingStopped | Reach::Stopped => {
                create_session(&router, id, json!({ "Description": "Coming up." })).await;
                set_session_due(&engine, id, already_past());
                assert_eq!(
                    stop(&router, id).await.as_deref(),
                    Some(id),
                    "case {case}: the first stop of a ready session is served"
                );
                // Where the row puts the clock relative to the instant the tear-down is owed
                // to finish: still ahead of it, or already behind it.
                set_session_due(
                    &engine,
                    id,
                    if matches!(reach, Reach::Stopped) {
                        already_past()
                    } else {
                        far_future()
                    },
                );
            }
            Reach::IdleFor { minutes } => {
                create_session(
                    &router,
                    id,
                    json!({ "Description": "Coming up.", "IdleTimeout": 1 }),
                )
                .await;
                rewind_session(&engine, id, Duration::from_secs(minutes * SECONDS_PER_MINUTE));
            }
            Reach::Forced(forced) => {
                create_session(&router, id, json!({ "Description": "Coming up." })).await;
                force_session_status(&engine, id, forced);
            }
        }

        // The whole record as it stands before the repeated stop, and the status the clock
        // still owes it.
        let before = get_session(&router, id).await;
        assert_eq!(
            before["Status"].as_str(),
            Some(*status),
            "case {case}: the row's session must be in {status}, got {before}"
        );
        let owed_before = owed_status(&engine, id);

        // Twice, so a no-op cannot be a change that only the first call makes.
        for attempt in 1..=2 {
            assert_eq!(
                stop(&router, id).await.as_deref(),
                Some(id),
                "case {case}: stop {attempt} of a session already down must be served and \
                 report that id"
            );
            let after = get_session(&router, id).await;
            assert_eq!(
                after, before,
                "case {case}: stop {attempt} must change nothing about the record, got {after}"
            );
            assert_eq!(
                owed_status(&engine, id),
                owed_before,
                "case {case}: stop {attempt} must put no new status in the clock's hands"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// An id the Region holds no session under
// ---------------------------------------------------------------------------

/// A stop of an id this Region holds no session under addresses no runtime, so it is refused
/// as the service's entity miss naming that id and writes nothing. A session is addressed by
/// its id and the Region it was created in together, so a differently-cased or partial id, an
/// id already deleted, and a stored id asked for through another Region's endpoint all hold no
/// session — while the stored id itself stops.
#[tokio::test]
async fn stopping_an_id_no_session_is_held_under_is_an_entity_miss() {
    let (router, engine) = harness();
    ready_session(&router, &engine, "StoredSession").await;
    ready_session(&router, &engine, "already-deleted").await;
    ok(
        &router,
        "DeleteSession",
        &json!({ "Id": "already-deleted" }),
    )
    .await;

    let longest = "i".repeat(MAX_ID_LENGTH);
    let misses: Vec<(&str, &str)> = vec![
        ("never created", "no-such-session"),
        ("created and then deleted", "already-deleted"),
        ("a prefix of a stored id", "Stored"),
        ("a suffix of a stored id", "Session"),
        ("a stored id lowercased", "storedsession"),
        ("a stored id uppercased", "STOREDSESSION"),
        ("an id carrying dots, dashes and underscores", "a.b-c_d"),
        ("the shortest id the model accepts", "x"),
        ("an id at the longest the model accepts", &longest),
    ];

    for (case, id) in &misses {
        let (code, body) = call(&router, "StopSession", &json!({ "Id": id })).await;
        assert_eq!(
            code, 400,
            "case {case}: stopping {id:?} must be refused, got {code} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException for {id:?}, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            messages::session_not_found(id),
            "case {case}: the refusal must name the session asked for, got {body}"
        );
        assert!(
            !stored_session(&engine, REGION, id),
            "case {case}: a refused stop must write no session under {id:?}"
        );
        // Nothing else moved with it: the one stored session is still up.
        let read = get_session(&router, "StoredSession").await;
        assert_eq!(
            read["Status"].as_str(),
            Some(STATUS_READY),
            "case {case}: a stop that found nothing must leave the stored session alone, got \
             {read}"
        );
    }

    // A session is served only through the endpoint that created it, so another Region's
    // endpoint holds no session under a stored id — and the record it names stays as it is.
    let (code, body) = call_in_region(
        &router,
        "eu-west-1",
        "StopSession",
        &json!({ "Id": "StoredSession" }),
    )
    .await;
    assert_eq!(
        code, 400,
        "case a stored id in another Region: expected the stop to be refused, got {code} with \
         {body}"
    );
    assert!(
        error_code(&body).ends_with("EntityNotFoundException"),
        "case a stored id in another Region: expected EntityNotFoundException, got {} in {body}",
        error_code(&body)
    );
    let read = get_session(&router, "StoredSession").await;
    assert_eq!(
        read["Status"].as_str(),
        Some(STATUS_READY),
        "case a stored id in another Region: the session must stay up where it was created, got \
         {read}"
    );

    // The stored id itself stops, so the ids above held no session and the lookup is not
    // simply broken.
    assert_eq!(
        stop(&router, "StoredSession").await.as_deref(),
        Some("StoredSession"),
        "case the stored id: the stop reports the id it stopped"
    );
}
