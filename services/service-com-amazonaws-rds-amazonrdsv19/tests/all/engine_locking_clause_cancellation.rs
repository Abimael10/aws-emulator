// SPDX-License-Identifier: CC0-1.0

//! End-to-end test for WHICH CONDITION a PostgreSQL DB instance's endpoint reports when a
//! locking statement runs out of its own budget.
//!
//! The rule under test: a statement this endpoint serves in more than one step — a locking
//! `SELECT`, whose rows the endpoint identifies with a row query of its own before the
//! statement itself runs — is bounded as a whole by that statement's deadline, and a
//! statement that ran past it is answered with the engine's own CANCELLATION (SQLSTATE
//! `57014`, in PostgreSQL's one sentence for the condition where the session's
//! `statement_timeout` decided it), never with a limit of this endpoint (`54000`,
//! `program_limit_exceeded`). The class is what a client branches on: a cancelled statement
//! is retryable under a longer `statement_timeout`, a statement refused as a limit is not.
//!
//! The rows span the locking clauses a PostgreSQL `SELECT` may carry — the four row-level
//! lock strengths and the two policies a clause may name for a row another transaction
//! already holds — because the row query above runs for every one of them. Each form is
//! also run with a predicate that finishes inside the budget, so neither a blanket refusal
//! of locking statements nor a blanket cancellation could pass this suite: the clause is
//! served, and it is served again after the cancellation, because a cancelled statement
//! leaves no row lock behind.
//!
//! The sibling suites cover the other halves of this bound: which budget the row query runs
//! under ([`crate::engine_row_lock_statement_bound`]) and the statement forms whose row
//! query the endpoint likewise evaluates ahead of the statement, an `UPDATE` and a `DELETE`
//! ([`crate::engine_server_surface`]).
//!
//! The client is the one the sibling endpoint suites drive
//! ([`crate::db_instance_engine_endpoint`]), written against the PostgreSQL wire itself, and
//! everything runs in ONE process: the instance's listener is opened by the
//! `CreateDBInstance` call below.

use std::time::{Duration, Instant};

use crate::db_instance_engine_endpoint::{available_instance, master_session, must_run};
use crate::db_instances::emulator;

/// The session timeout the statements of the first test run under: long enough that
/// reaching the engine is not a race, far shorter than [`PREDICATE_WAIT`].
const STATEMENT_TIMEOUT_MS: u64 = 250;

/// How long the predicate of each statement that must be cancelled asks to wait — far past
/// both [`STATEMENT_TIMEOUT_MS`] and anything this endpoint serves one statement for, so a
/// statement that served the wait cannot be mistaken for one a deadline cancelled.
const PREDICATE_WAIT: &str = "30";

/// The longest a statement cancelled on its own SESSION's timeout may take to come back:
/// far under [`PREDICATE_WAIT`], far over [`STATEMENT_TIMEOUT_MS`].
const CANCELLED_WITHIN: Duration = Duration::from_secs(4);

/// The longest a statement cancelled on THIS ENDPOINT's budget may take to come back: far
/// under [`PREDICATE_WAIT`], and generous room over a budget no session can lengthen.
const ENDPOINT_CANCELLED_WITHIN: Duration = Duration::from_secs(25);

/// What PostgreSQL answers a client whose statement its `statement_timeout` cancelled: the
/// `query_canceled` SQLSTATE and the sentence that names the timeout.
const TIMEOUT_CANCELLATION: (&str, &str) = (
    "57014",
    "canceling statement due to statement timeout",
);

/// What a client reads when no `statement_timeout` of its own was in force and this
/// endpoint's own budget is what stopped the statement: the same cancellation SQLSTATE
/// (`query_canceled`, as against `54000` — `program_limit_exceeded` — which is a limit of
/// the server rather than a statement that was stopped), worded for that budget.
const ENDPOINT_CANCELLATION: (&str, &str) = (
    "57014",
    "canceling statement: it ran longer than this endpoint serves",
);

/// The locking clauses a PostgreSQL `SELECT` may carry: `FOR UPDATE | NO KEY UPDATE | SHARE
/// | KEY SHARE [ NOWAIT | SKIP LOCKED ]` — the four row-level lock strengths and the two
/// policies for a row another transaction holds, spanning every form whose rows this
/// endpoint identifies ahead of the statement.
const LOCKING_CLAUSES: [&str; 6] = [
    "FOR UPDATE",
    "FOR NO KEY UPDATE",
    "FOR SHARE",
    "FOR KEY SHARE",
    "FOR UPDATE NOWAIT",
    "FOR SHARE SKIP LOCKED",
];

/// The table every statement below locks rows of, and the one row each asks for.
const CREATE_TABLE: &str =
    "CREATE TABLE lock_waits (id integer PRIMARY KEY, label text NOT NULL)";
const SEED_ROWS: &str = "INSERT INTO lock_waits (id, label) VALUES (1, 'a'), (2, 'b')";

/// The locking statement that asks for one row, by the clause it carries.
fn served(clause: &str) -> String {
    format!("SELECT id FROM lock_waits WHERE id = 1 {clause}")
}

/// The same statement with a predicate that waits far past any budget it can run under.
fn waiting(clause: &str) -> String {
    format!("SELECT id FROM lock_waits WHERE pg_sleep({PREDICATE_WAIT}) IS NULL {clause}")
}

/// Whatever locking clause it carries, a statement whose predicate outruns the session's
/// `statement_timeout` is CANCELLED in PostgreSQL's own words — and the same statement with
/// a predicate the budget admits is served, before and after.
#[tokio::test]
async fn a_locking_statement_that_outruns_its_timeout_is_cancelled_rather_than_limited() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-lockcancel-postgres",
        "postgres",
        &[("EngineVersion", "16.4".to_string())],
    )
    .await;
    let mut client = master_session("seed", &address, port).await;
    must_run("seed", &mut client, CREATE_TABLE).await;
    must_run("seed", &mut client, SEED_ROWS).await;
    // "The time in milliseconds after which a statement is aborted."
    must_run(
        "seed",
        &mut client,
        &format!("SET statement_timeout = {STATEMENT_TIMEOUT_MS}"),
    )
    .await;

    for clause in LOCKING_CLAUSES {
        let case = clause;
        // The clause is served for the rows this endpoint can identify inside the budget:
        // the statement answers the row it locks.
        let statement = served(clause);
        let rows = client
            .query(&statement)
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: {statement:?} must be served, got {refusal:?}")
            });
        assert_eq!(
            rows.only(),
            Some("1"),
            "case {case:?}: {statement:?} must answer the row it locks, got {rows:?}"
        );

        // The same statement whose own predicate outruns the budget is the cancellation,
        // and never the endpoint's own limit.
        let statement = waiting(clause);
        let started = Instant::now();
        let answer = client.query(&statement).await;
        let elapsed = started.elapsed();
        let refusal = answer.err().unwrap_or_else(|| {
            panic!(
                "case {case:?}: a statement waiting {PREDICATE_WAIT}s under a \
                 {STATEMENT_TIMEOUT_MS}ms statement_timeout must be cancelled, but \
                 {statement:?} was served after {elapsed:?}"
            )
        });
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            TIMEOUT_CANCELLATION,
            "case {case:?}: the refusal must be the engine's own cancellation, got {refusal:?}"
        );
        assert!(
            elapsed < CANCELLED_WITHIN,
            "case {case:?}: the statement must stop at its own deadline rather than after \
             the {PREDICATE_WAIT}s its predicate asked for, got {elapsed:?}"
        );

        // The endpoint is still serving and the cancelled statement took no row with it:
        // the same locking statement runs again on the same session.
        let statement = served(clause);
        let rows = client
            .query(&statement)
            .await
            .unwrap_or_else(|refusal| {
                panic!(
                    "case {case:?}: a cancelled statement leaves no lock behind, so \
                     {statement:?} must still be served, got {refusal:?}"
                )
            });
        assert_eq!(
            rows.only(),
            Some("1"),
            "case {case:?}: {statement:?} must answer the row it locks, got {rows:?}"
        );
    }
}

/// A locking statement run by a session that set NO `statement_timeout` of its own is
/// bounded by this endpoint's budget instead — and what it answers is still a cancellation
/// (`57014`), not a limit of the endpoint.
#[tokio::test]
async fn a_locking_statement_with_no_timeout_of_its_own_is_cancelled_on_the_endpoints_budget() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-lockcancel-budget-postgres",
        "postgres",
        &[("EngineVersion", "16.4".to_string())],
    )
    .await;
    let case = "the timeout disabled";
    let mut client = master_session(case, &address, port).await;
    must_run(case, &mut client, CREATE_TABLE).await;
    must_run(case, &mut client, SEED_ROWS).await;
    // "0 disables the timeout", so the only budget left is this endpoint's own.
    must_run(case, &mut client, "SET statement_timeout = 0").await;

    let statement = waiting("FOR UPDATE");
    let started = Instant::now();
    let answer = client.query(&statement).await;
    let elapsed = started.elapsed();
    let refusal = answer.err().unwrap_or_else(|| {
        panic!(
            "case {case:?}: a statement waiting {PREDICATE_WAIT}s must be cancelled by this \
             endpoint's own budget, but {statement:?} was served after {elapsed:?}"
        )
    });
    // The class is the cancellation, and the sentence is the one for the deadline that
    // stopped it: this session left only the endpoint's own budget in force.
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ENDPOINT_CANCELLATION,
        "case {case:?}: expected the cancellation this endpoint's own budget words, got \
         {refusal:?}"
    );
    assert!(
        elapsed < ENDPOINT_CANCELLED_WITHIN,
        "case {case:?}: the statement must stop at this endpoint's budget rather than after \
         the {PREDICATE_WAIT}s its predicate asked for, got {elapsed:?}"
    );

    // And the endpoint goes on serving the session that ran it.
    let statement = served("FOR UPDATE");
    let rows = client
        .query(&statement)
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: {statement:?} must be served, got {refusal:?}")
        });
    assert_eq!(
        rows.only(),
        Some("1"),
        "case {case:?}: {statement:?} must answer the row it locks, got {rows:?}"
    );
}
