// SPDX-License-Identifier: CC0-1.0

//! The idle leg of an interactive session's own timeout: when a `READY` session ends
//! because nothing ran on it.
//!
//! RULE: `Session$IdleTimeout` is "the number of minutes when idle before the session times
//! out", measured from the session's LAST activity — so a `READY` session the service has
//! not been asked to run a statement on for that many minutes is ended by the service
//! itself, with no client call: every reader then reports the modelled terminal
//! `SessionStatus` `TIMEOUT` (`smithy_query shape=SessionStatus`), with `CompletedOn` ("the
//! date and time that this session is completed") stamped at the instant the window fell due
//! and `ExecutionTime` ("the total time the session ran for") the span from `CreatedOn` to
//! it. Submitting a statement is activity, so it restarts the window; before the window
//! elapses the session stays `READY`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: the length of the declared window
//! (the modelled floor of one minute included), how much of it had elapsed before a
//! statement was run, whether a statement was run at all, how much had elapsed after it, and
//! which reader the status is read through. Nothing here sleeps: a row positions the stored
//! instants the window is measured from, which is what fixes which side of the window a read
//! lands on.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::already_past;
use crate::statements::{create_session, rewind_session, run, set_session_due, SECONDS_PER_MINUTE};
use crate::wire::{call_in_region, emulator_on, ok, REGION};

/// The two `SessionStatus` values this suite observes, of the modelled enum
/// (`PROVISIONING`, `READY`, `FAILED`, `TIMEOUT`, `STOPPING`, `STOPPED`, from
/// `smithy_query shape=SessionStatus`), written out as literals rather than read from the
/// service's own constants, so this suite pins the VALUES the wire carries.
const STATUS_READY: &str = "READY";
const STATUS_TIMEOUT: &str = "TIMEOUT";

/// The shortest window the model accepts: `IdleTimeout` targets `Timeout`, "@range min 1"
/// (`smithy_query operation=CreateSession`).
const MIN_IDLE_TIMEOUT_MINUTES: u64 = 1;

/// `minutes` of a session's documented timeouts, as a span.
fn minutes(minutes: u64) -> Duration {
    Duration::from_secs(minutes * SECONDS_PER_MINUTE)
}

/// The `Session` the `GetSession` answer for `id` reports.
async fn get(router: &MultiServiceRouter, id: &str) -> Value {
    let read = ok(router, "GetSession", &json!({ "Id": id })).await;
    read.get("Session")
        .cloned()
        .unwrap_or_else(|| panic!("GetSession must report a Session, got {read}"))
}

/// A session created with `idle_timeout` minutes of idle window, brought to [`STATUS_READY`]
/// by the read that materialises the status its provisioning owed it.
///
/// This is [`crate::statements::ready_session`] with the window the row under test declares:
/// that helper provisions a session with the timeouts the service defaults to, which is what
/// its own suite needs and is the one thing a row here varies.
async fn ready_session_with_idle_timeout(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
    idle_timeout: u64,
) {
    create_session(router, id, json!({ "IdleTimeout": idle_timeout })).await;
    set_session_due(engine, id, already_past());
    let read = get(router, id).await;
    assert_eq!(
        read["Status"].as_str(),
        Some(STATUS_READY),
        "case {id}: a session whose provisioning is due must be READY before the idle window \
         is measured, got {read}"
    );
}

/// A `f64` member of a reported session.
fn number(case: &str, session: &Value, member: &str) -> f64 {
    session
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: expected a {member} figure, got {session}"))
}

/// What a session reported as timed out must say about when it ended: `CompletedOn` is
/// stamped, `ExecutionTime` is the span from `CreatedOn` to it, and that span is at least the
/// window the create declared — so the transition cannot have been made early.
fn assert_completed_after_its_window(case: &str, session: &Value, idle_timeout: u64) {
    let created_on = number(case, session, "CreatedOn");
    let completed_on = number(case, session, "CompletedOn");
    let execution_time = number(case, session, "ExecutionTime");
    let ran_for = completed_on - created_on;
    assert!(
        (ran_for - execution_time).abs() < 0.01,
        "case {case}: ExecutionTime must be the span from CreatedOn to CompletedOn; expected \
         {ran_for}, got {execution_time} in {session}"
    );
    let window = minutes(idle_timeout).as_secs_f64();
    assert!(
        ran_for >= window,
        "case {case}: a session must not be timed out before its declared window of {window}s \
         has elapsed, but it ran for only {ran_for}s in {session}"
    );
}

/// A session reported as still running has not completed: nothing has been accounted to it.
fn assert_still_running(case: &str, session: &Value) {
    assert!(
        session["CompletedOn"].is_null(),
        "case {case}: a session that has not ended carries no CompletedOn, got {session}"
    );
    assert_eq!(
        number(case, session, "ExecutionTime"),
        0.0,
        "case {case}: a session that has not ended has run for no accounted time, got {session}"
    );
}

/// The idle window is measured from the session's LAST activity, not from its creation:
/// running a statement restarts it, so the same total span of wall-clock leaves the session
/// `READY` when a statement fell inside it and `TIMEOUT` when none did.
#[tokio::test]
async fn a_statement_restarts_the_idle_window() {
    // (case, the IdleTimeout the create declares, minutes idle before a statement, whether a
    //  statement is run, minutes idle after it, the status every reader must report)
    let rows: &[(&str, u64, u64, bool, u64, &str)] = &[
        (
            // The control row the next one is read against: the same total idle span with no
            // statement in it ends the session.
            "no statement, the window elapsed",
            10,
            4,
            false,
            6,
            STATUS_TIMEOUT,
        ),
        (
            // The SAME total span as the first row, with a statement inside it.
            "a statement, then part of the window",
            10,
            4,
            true,
            6,
            STATUS_READY,
        ),
        (
            "a statement, then the whole window again",
            10,
            9,
            true,
            10,
            STATUS_TIMEOUT,
        ),
        (
            "a statement, then well short of the window",
            5,
            4,
            true,
            4,
            STATUS_READY,
        ),
        (
            "a statement, then the shortest window the model accepts",
            MIN_IDLE_TIMEOUT_MINUTES,
            0,
            true,
            MIN_IDLE_TIMEOUT_MINUTES,
            STATUS_TIMEOUT,
        ),
        (
            // Half of a long window, which is where the first row's rule would have ended it.
            "a statement, then half of a long window",
            60,
            30,
            true,
            30,
            STATUS_READY,
        ),
    ];

    for (case, idle_timeout, before, statement, after, expected) in rows {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_on(engine.clone());
        let id = "idle-window";
        ready_session_with_idle_timeout(&router, &engine, id, *idle_timeout).await;

        // The first leg of the idle span, which every row leaves inside the window: the
        // session is still READY and can still take a statement.
        rewind_session(&engine, id, minutes(*before));
        let read = get(&router, id).await;
        assert_eq!(
            read["Status"].as_str(),
            Some(STATUS_READY),
            "case {case}: after {before} of {idle_timeout} idle minutes the session must still \
             be READY, got {read}"
        );

        if *statement {
            run(&router, id, "spark.sql('SELECT 1').show()").await;
        }

        // The second leg, measured from whichever instant the session was last active at.
        rewind_session(&engine, id, minutes(*after));
        let read = get(&router, id).await;
        assert_eq!(
            read["Status"].as_str(),
            Some(*expected),
            "case {case}: expected {expected} with a {idle_timeout}-minute idle window, \
             {before} minutes before and {after} minutes after the activity, got {read}"
        );
        if *expected == STATUS_TIMEOUT {
            assert_completed_after_its_window(case, &read, *idle_timeout);
        } else {
            assert_still_running(case, &read);
        }

        // The status is the record's, so a second read reports the same one rather than
        // moving the session on again.
        let again = get(&router, id).await;
        assert_eq!(
            again["Status"].as_str(),
            read["Status"].as_str(),
            "case {case}: two reads of the same session must agree, got {read} then {again}"
        );
        assert_eq!(
            again["CompletedOn"], read["CompletedOn"],
            "case {case}: the instant a session ended must not move between reads, got {read} \
             then {again}"
        );
    }
}

/// The expiry is the stored session's, not one reader's: the session read and the session
/// listing report the same status for the same record, and only the session whose own window
/// elapsed is terminal.
#[tokio::test]
async fn an_expired_session_is_terminal_for_every_reader() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    // (the session, the IdleTimeout it declares, the status it must report after the idle
    //  span below)
    let rows: &[(&str, u64, &str)] = &[
        ("short-window", 2, STATUS_TIMEOUT),
        ("long-window", 60, STATUS_READY),
    ];
    let idled_for = 5;

    for (id, idle_timeout, _) in rows {
        ready_session_with_idle_timeout(&router, &engine, id, *idle_timeout).await;
        rewind_session(&engine, id, minutes(idled_for));
    }

    let (status, listing) = call_in_region(&router, REGION, "ListSessions", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListSessions must answer 200, got {status} with body {listing}"
    );
    let listed = listing
        .get("Sessions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ListSessions must report a Sessions list, got {listing}"))
        .clone();

    for (id, idle_timeout, expected) in rows {
        let read = get(&router, id).await;
        assert_eq!(
            read["Status"].as_str(),
            Some(*expected),
            "case {id}: expected {expected} after {idled_for} idle minutes on a \
             {idle_timeout}-minute window, got {read}"
        );

        let listed = listed
            .iter()
            .find(|session| session["Id"].as_str() == Some(*id))
            .unwrap_or_else(|| panic!("case {id}: the listing must report the session, got {listing}"));
        assert_eq!(
            listed["Status"].as_str(),
            Some(*expected),
            "case {id}: the listing must report the same status as the read, got {listed}"
        );

        if *expected == STATUS_TIMEOUT {
            assert_completed_after_its_window(id, &read, *idle_timeout);
        } else {
            assert_still_running(id, &read);
        }
    }
}
