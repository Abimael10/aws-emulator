// SPDX-License-Identifier: CC0-1.0

//! What a `transactionId` whose transaction the service ABANDONED for inactivity is
//! answered with, driven end to end over the real wire path.
//!
//! The rule under test
//! ([`a_transaction_abandoned_for_inactivity_is_reported_as_idle`]): a transaction no call
//! used its id in for the inactivity window the service documents is rolled back by the
//! CLOCK, and every later operation taking a `transactionId` answers its id with HTTP 404
//! `TransactionNotFoundException` carrying the sentence AWS words an idleness expiry with
//! — which is the only thing that tells a client that apart from a transaction a previous
//! call ended, since both carry the same code. The rows differ along what the abandoned
//! transaction had done, which database it was opened for, and how far either window had
//! run.
//!
//! The sentence for every OTHER way a transaction stops running is pinned beside this
//! suite (`ended_transaction_sentence`), and what a value this resource could never have
//! issued gets by `transaction_id_classes`. The two classes are pinned APART on one cluster
//! by [`the_sentence_names_how_each_transaction_of_a_cluster_stopped_running`], and what
//! the inactivity window is measured from by
//! [`the_inactivity_window_is_measured_from_the_last_call_that_used_the_id`].

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_rdsdata_rdsdataservice::state::{
    TRANSACTION_COMMITTED_STATUS, TRANSACTION_IDLE_MESSAGE, TRANSACTION_IDLE_TIMEOUT,
    TRANSACTION_MAX_LIFETIME, TRANSACTION_NOT_RUNNING_MESSAGE,
};

use crate::sql_data_api::{error_code, parameter, DataApi};

/// AWS's own sentence for a transaction that timed out while idle, written out here so a
/// change of wording in the service's own constant fails this suite.
const EXPECTED_IDLE_MESSAGE: &str =
    "INVALID_ARGUMENT: Transaction error: transaction has been terminated due to an idleness";

/// AWS's own sentence for a transaction that stopped running any other way.
const EXPECTED_TERMINATED_MESSAGE: &str =
    "INVALID_ARGUMENT: Transaction error: one previous call resulted in a termination of \
     your transaction";

/// AWS's own `transactionStatus` for a `CommitTransaction` that was served — the positive
/// control an id still running must still answer.
const EXPECTED_COMMITTED_STATUS: &str = "Transaction Committed";

/// Room either window has left to run, generous enough that a loaded host cannot spend it
/// while the case is being driven: a transaction this far inside its window is still
/// RUNNING.
const WELL_INSIDE_THE_WINDOW: Duration = Duration::from_secs(60);

/// The table every row below writes into.
async fn given_entries(api: &DataApi, database: Option<&str>) {
    let mut create = json!({
        "sql": "CREATE TABLE entries (id INT PRIMARY KEY, note TEXT NOT NULL)",
    });
    let mut seed = json!({"sql": "INSERT INTO entries (id, note) VALUES (1, 'first')"});
    if let Some(database) = database {
        create["database"] = json!(database);
        seed["database"] = json!(database);
    }
    api.run(create).await;
    api.run(seed).await;
}

/// Call `transaction_id` on every operation that takes one, with members that are
/// otherwise valid, and report what each answered.
async fn every_operation_taking_an_id(
    api: &DataApi,
    transaction_id: &str,
) -> Vec<(&'static str, (u16, Value))> {
    vec![
        (
            "ExecuteStatement",
            api.execute(json!({
                "sql": "INSERT INTO entries (id, note) VALUES (9, 'late')",
                "transactionId": transaction_id,
            }))
            .await,
        ),
        (
            "BatchExecuteStatement",
            api.batch(json!({
                "sql": "INSERT INTO entries (id, note) VALUES (:id, :note)",
                "parameterSets": [[
                    parameter("id", json!({"longValue": 10})),
                    parameter("note", json!({"stringValue": "batched"})),
                ]],
                "transactionId": transaction_id,
            }))
            .await,
        ),
        ("CommitTransaction", api.commit(transaction_id).await),
        ("RollbackTransaction", api.rollback(transaction_id).await),
    ]
}

/// Every id in the committed table of `database`, in id order.
async fn entry_ids(api: &DataApi, database: Option<&str>) -> Vec<i64> {
    let mut members = json!({"sql": "SELECT id FROM entries ORDER BY id"});
    if let Some(database) = database {
        members["database"] = json!(database);
    }
    let rows = api.run(members).await;
    rows["records"]
        .as_array()
        .map(|records| {
            records
                .iter()
                .map(|record| {
                    record[0]["longValue"]
                        .as_i64()
                        .unwrap_or_else(|| panic!("an id comes back as a long: {record}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How a row below prepares the transaction it then ages out.
enum Prepared {
    /// Nothing was run inside it.
    Nothing,
    /// It wrote a row that only a commit could have installed.
    AWrite,
    /// A statement of its own FAILED in it, so it was aborted before it aged out.
    AFailedStatement,
}

/// Open a transaction on `database`, do to it what `prepared` says, and hand back its id.
async fn a_transaction(api: &DataApi, database: Option<&str>, prepared: &Prepared) -> String {
    let transaction_id = api.begin_transaction(database).await;
    match prepared {
        Prepared::Nothing => {}
        Prepared::AWrite => {
            api.run(json!({
                "sql": "INSERT INTO entries (id, note) VALUES (7, 'uncommitted')",
                "transactionId": transaction_id.clone(),
            }))
            .await;
        }
        Prepared::AFailedStatement => {
            let (status, body) = api
                .execute(json!({
                    "sql": "INSERT INTO entries (id, note) VALUES (1, 'duplicate')",
                    "transactionId": transaction_id.clone(),
                }))
                .await;
            assert_eq!(
                status, 400,
                "a statement colliding with the seeded key fails: got {status} {body}"
            );
        }
    }
    transaction_id
}

/// RULE: a transaction no call used its id in for the whole inactivity window is
/// abandoned by the service and rolled back, and from then on every operation taking a
/// `transactionId` answers its id with HTTP 404 `TransactionNotFoundException` carrying
/// the idleness sentence — whatever the transaction had done, whichever database it was
/// opened for, however many calls later, and in place of the sentence a transaction that
/// stopped running any other way carries.
#[tokio::test]
async fn a_transaction_abandoned_for_inactivity_is_reported_as_idle() {
    assert_eq!(
        EXPECTED_IDLE_MESSAGE, TRANSACTION_IDLE_MESSAGE,
        "the idleness sentence this service states must be AWS's own"
    );
    assert_eq!(
        EXPECTED_TERMINATED_MESSAGE, TRANSACTION_NOT_RUNNING_MESSAGE,
        "the termination sentence this service states must be AWS's own"
    );

    // The rows: what the abandoned transaction had done, which database it was opened
    // for, and how far past the window it sat idle.
    let rows = [
        (
            "an idle transaction that had written a row",
            None,
            Prepared::AWrite,
            TRANSACTION_IDLE_TIMEOUT,
        ),
        (
            "an idle transaction that had run nothing",
            None,
            Prepared::Nothing,
            TRANSACTION_IDLE_TIMEOUT + Duration::from_secs(45 * 60),
        ),
        (
            "an idle transaction a statement of its own had aborted",
            None,
            Prepared::AFailedStatement,
            TRANSACTION_IDLE_TIMEOUT * 2,
        ),
        (
            "an idle transaction opened for a named database",
            Some("ledger"),
            Prepared::AWrite,
            TRANSACTION_IDLE_TIMEOUT,
        ),
    ];

    for (case, database, prepared, idle_for) in &rows {
        let api = DataApi::new();
        given_entries(&api, *database).await;
        let transaction_id = a_transaction(&api, *database, prepared).await;

        // The clock, driven rather than waited for: the transaction's id was last used
        // `idle_for` ago, which is the whole window or more.
        api.age_transaction(&transaction_id, *idle_for, *idle_for);

        // Asked twice, so the answer is what the service REMEMBERS about the abandoned
        // transaction and not something the first refusal consumed.
        for round in ["the first time", "asked again"] {
            for (operation, (status, body)) in
                every_operation_taking_an_id(&api, &transaction_id).await
            {
                assert_eq!(
                    status, 404,
                    "case {case} on {operation} {round}: expected HTTP 404, got {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "TransactionNotFoundException",
                    "case {case} on {operation} {round}: expected \
                     TransactionNotFoundException, got {body}"
                );
                assert_eq!(
                    body["message"].as_str(),
                    Some(EXPECTED_IDLE_MESSAGE),
                    "case {case} on {operation} {round}: expected the idleness sentence, got \
                     {body}"
                );
            }
        }

        // The clock ROLLED IT BACK: what the abandoned transaction wrote never reached
        // the committed database, and nothing the refused calls carried did either.
        let ids = entry_ids(&api, *database).await;
        assert_eq!(
            ids,
            vec![1],
            "case {case}: expected the seeded row only after the transaction was abandoned, \
             got {ids:?}"
        );
    }
}

/// RULE (the window's own bounds): the inactivity window and the maximum lifetime are the
/// two windows the clock ends a transaction on, so a transaction still inside both is
/// served, one that reached the inactivity window is reported as idle, and one that
/// reached the lifetime without ever sitting idle is reported as terminated — which is
/// what keeps the idleness sentence to the expiry it words.
#[tokio::test]
async fn each_window_the_clock_ends_a_transaction_on_has_its_own_answer() {
    let api = DataApi::new();
    given_entries(&api, None).await;

    // `open_for`, `idle_for`, and what a rollback of that transaction must answer.
    // The bounds are the service's own documented windows: three minutes of inactivity
    // and a 24-hour lifetime (`BeginTransaction`'s model doc, carried on
    // `TRANSACTION_IDLE_TIMEOUT` and `TRANSACTION_MAX_LIFETIME`).
    let cases: [(&str, Duration, Duration, u16, &str); 6] = [
        (
            "just opened",
            Duration::ZERO,
            Duration::ZERO,
            200,
            "Rollback complete",
        ),
        (
            "idle for just less than the inactivity window",
            TRANSACTION_IDLE_TIMEOUT - WELL_INSIDE_THE_WINDOW,
            TRANSACTION_IDLE_TIMEOUT - WELL_INSIDE_THE_WINDOW,
            200,
            "Rollback complete",
        ),
        (
            "idle for exactly the inactivity window",
            TRANSACTION_IDLE_TIMEOUT,
            TRANSACTION_IDLE_TIMEOUT,
            404,
            EXPECTED_IDLE_MESSAGE,
        ),
        (
            "used within the window, open for just less than the lifetime",
            TRANSACTION_MAX_LIFETIME - WELL_INSIDE_THE_WINDOW,
            Duration::ZERO,
            200,
            "Rollback complete",
        ),
        (
            "used within the window, open for exactly the lifetime",
            TRANSACTION_MAX_LIFETIME,
            Duration::ZERO,
            404,
            EXPECTED_TERMINATED_MESSAGE,
        ),
        (
            "used within the window, open for longer than the lifetime",
            TRANSACTION_MAX_LIFETIME * 2,
            Duration::ZERO,
            404,
            EXPECTED_TERMINATED_MESSAGE,
        ),
    ];

    for (case, open_for, idle_for, expected_status, expected_sentence) in cases {
        let transaction_id = api.begin_transaction(None).await;
        api.age_transaction(&transaction_id, open_for, idle_for);
        let (status, body) = api.rollback(&transaction_id).await;
        assert_eq!(
            status, expected_status,
            "case {case}: expected HTTP {expected_status}, got {status} {body}"
        );
        if expected_status == 200 {
            assert_eq!(
                body["transactionStatus"].as_str(),
                Some(expected_sentence),
                "case {case}: expected the rollback status, got {body}"
            );
        } else {
            assert_eq!(
                error_code(&body),
                "TransactionNotFoundException",
                "case {case}: expected TransactionNotFoundException, got {body}"
            );
            assert_eq!(
                body["message"].as_str(),
                Some(expected_sentence),
                "case {case}: expected the sentence for this window, got {body}"
            );
        }
    }
}

/// RULE: a transaction the clock abandoned for inactivity no longer holds the database
/// copy it was writing into, so the slot it occupied is free and a later transaction on
/// the same cluster is served and commits — the clock's rollback is a rollback, not a
/// record that blocks the cluster.
#[tokio::test]
async fn work_goes_on_after_a_transaction_is_abandoned_for_inactivity() {
    let api = DataApi::new();
    given_entries(&api, None).await;

    let abandoned = a_transaction(&api, None, &Prepared::AWrite).await;
    api.age_transaction(&abandoned, TRANSACTION_IDLE_TIMEOUT, TRANSACTION_IDLE_TIMEOUT);

    let fresh = api.begin_transaction(None).await;
    api.run(json!({
        "sql": "INSERT INTO entries (id, note) VALUES (2, 'committed')",
        "transactionId": fresh.clone(),
    }))
    .await;
    let (status, body) = api.commit(&fresh).await;
    assert_eq!(
        status, 200,
        "case a transaction opened after an abandoned one: expected the commit to be served, \
         got {status} {body}"
    );

    let ids = entry_ids(&api, None).await;
    assert_eq!(
        ids,
        vec![1, 2],
        "case after an abandoned transaction: expected the seeded and the committed rows, got \
         {ids:?}"
    );

    // The abandoned id is still answered as one the service abandoned, after the work
    // that followed it.
    let (status, body) = api.rollback(&abandoned).await;
    assert_eq!(
        status, 404,
        "case the abandoned id after later work: expected HTTP 404, got {status} {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(EXPECTED_IDLE_MESSAGE),
        "case the abandoned id after later work: expected the idleness sentence, got {body}"
    );
}

/// RULE: the sentence a refused `transactionId` carries names how THAT transaction stopped
/// running — the service's own reclamation of an inactive one is AWS's idleness sentence
/// and every other way it stopped is AWS's previous-call sentence — so a cluster holding
/// transactions that stopped different ways answers each of its ids for its own cause at
/// one instant, and an id still running is served.
#[tokio::test]
async fn the_sentence_names_how_each_transaction_of_a_cluster_stopped_running() {
    assert_eq!(
        EXPECTED_COMMITTED_STATUS, TRANSACTION_COMMITTED_STATUS,
        "the commit status this service states must be AWS's own"
    );

    let api = DataApi::new();
    given_entries(&api, None).await;

    // Reclaimed for inactivity: no call used its id for the whole window.
    let abandoned = a_transaction(&api, None, &Prepared::AWrite).await;
    api.age_transaction(&abandoned, TRANSACTION_IDLE_TIMEOUT, TRANSACTION_IDLE_TIMEOUT);

    // Ended by the CALLER'S OWN commit, with a write only that commit could install.
    let committed = a_transaction(&api, None, &Prepared::Nothing).await;
    api.run(json!({
        "sql": "INSERT INTO entries (id, note) VALUES (2, 'committed')",
        "transactionId": committed.clone(),
    }))
    .await;
    let (status, body) = api.commit(&committed).await;
    assert_eq!(
        status, 200,
        "case the commit that ends a transaction: expected it to be served, got {status} {body}"
    );

    // Ended by the caller's own rollback.
    let rolled_back = a_transaction(&api, None, &Prepared::AWrite).await;
    let (status, body) = api.rollback(&rolled_back).await;
    assert_eq!(
        status, 200,
        "case the rollback that ends a transaction: expected it to be served, got {status} {body}"
    );

    // A statement of its own failed in it, and a commit of the caller's then ended it.
    let aborted = a_transaction(&api, None, &Prepared::AFailedStatement).await;
    let (status, body) = api.commit(&aborted).await;
    assert_eq!(
        status, 200,
        "case the commit of an aborted transaction: expected it to be served, got {status} {body}"
    );

    // Still RUNNING: opened now, so it is well inside both windows while the refusals
    // below are driven, and it is the positive control committed at the end.
    let live = a_transaction(&api, None, &Prepared::Nothing).await;
    api.run(json!({
        "sql": "INSERT INTO entries (id, note) VALUES (3, 'live')",
        "transactionId": live.clone(),
    }))
    .await;

    // The rows: how the transaction behind each id stopped running, and the sentence that
    // names that cause.
    let rows = [
        (
            "the transaction the service reclaimed for inactivity",
            abandoned.as_str(),
            EXPECTED_IDLE_MESSAGE,
        ),
        (
            "the transaction a commit of the caller's ended",
            committed.as_str(),
            EXPECTED_TERMINATED_MESSAGE,
        ),
        (
            "the transaction a rollback of the caller's ended",
            rolled_back.as_str(),
            EXPECTED_TERMINATED_MESSAGE,
        ),
        (
            "the transaction a statement of its own failed in, ended by a commit",
            aborted.as_str(),
            EXPECTED_TERMINATED_MESSAGE,
        ),
    ];

    for (case, transaction_id, expected_sentence) in rows {
        for (operation, (status, body)) in every_operation_taking_an_id(&api, transaction_id).await
        {
            assert_eq!(
                status, 404,
                "case {case} on {operation}: expected HTTP 404, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                "TransactionNotFoundException",
                "case {case} on {operation}: expected TransactionNotFoundException, got {body}"
            );
            assert_eq!(
                body["message"].as_str(),
                Some(expected_sentence),
                "case {case} on {operation}: expected {expected_sentence:?}, got {body}"
            );
        }
    }

    // The positive control, after every refusal above: the id of the transaction that is
    // still running is served and commits.
    let (status, body) = api.commit(&live).await;
    assert_eq!(
        status, 200,
        "case the transaction still running: expected the commit to be served, got {status} \
         {body}"
    );
    assert_eq!(
        body["transactionStatus"].as_str(),
        Some(EXPECTED_COMMITTED_STATUS),
        "case the transaction still running: expected the commit status, got {body}"
    );

    // Only what a commit of the caller's installed is committed: the reclaimed and the
    // rolled-back writes are gone, and nothing a refused call carried landed.
    let ids = entry_ids(&api, None).await;
    assert_eq!(
        ids,
        vec![1, 2, 3],
        "case after every refusal: expected the seeded and the committed rows, got {ids:?}"
    );
}

/// RULE (what the window is measured from): the inactivity the service reclaims a
/// transaction after is measured from the LAST call that used its `transactionId`, so a
/// transaction used again inside every window is served however long it has been open and
/// commits, while one of the same cluster that no call used for a whole window is reclaimed
/// and carries the idleness sentence.
#[tokio::test]
async fn the_inactivity_window_is_measured_from_the_last_call_that_used_the_id() {
    let api = DataApi::new();
    given_entries(&api, None).await;

    let busy = api.begin_transaction(None).await;
    let abandoned = api.begin_transaction(None).await;

    // Almost the whole window, three times over: each round presents the busy transaction
    // as idle for nearly the entire window and then USES its id, which starts the window
    // again — so it has been open for about three windows without ever sitting idle for
    // one.
    let almost_the_whole_window = TRANSACTION_IDLE_TIMEOUT - WELL_INSIDE_THE_WINDOW;
    for round in 1..=3u32 {
        api.age_transaction(
            &busy,
            almost_the_whole_window * round,
            almost_the_whole_window,
        );
        let (status, body) = api
            .execute(json!({
                "sql": format!("INSERT INTO entries (id, note) VALUES ({}, 'round')", 10 + round),
                "transactionId": busy.clone(),
            }))
            .await;
        assert_eq!(
            status, 200,
            "case round {round} inside the window: expected the statement to be served, got \
             {status} {body}"
        );
    }

    let (status, body) = api.commit(&busy).await;
    assert_eq!(
        status, 200,
        "case the transaction used inside every window: expected the commit to be served, got \
         {status} {body}"
    );
    assert_eq!(
        body["transactionStatus"].as_str(),
        Some(EXPECTED_COMMITTED_STATUS),
        "case the transaction used inside every window: expected the commit status, got {body}"
    );

    // Its neighbour, opened at the same time and used by no call since: a whole window of
    // inactivity, so every operation taking its id answers the idleness sentence.
    api.age_transaction(&abandoned, TRANSACTION_IDLE_TIMEOUT, TRANSACTION_IDLE_TIMEOUT);
    for (operation, (status, body)) in every_operation_taking_an_id(&api, &abandoned).await {
        assert_eq!(
            status, 404,
            "case the neighbour no call used on {operation}: expected HTTP 404, got {status} \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "TransactionNotFoundException",
            "case the neighbour no call used on {operation}: expected \
             TransactionNotFoundException, got {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(EXPECTED_IDLE_MESSAGE),
            "case the neighbour no call used on {operation}: expected the idleness sentence, got \
             {body}"
        );
    }

    // What the busy transaction committed is committed, and nothing the reclaimed one or a
    // refused call carried is.
    let ids = entry_ids(&api, None).await;
    assert_eq!(
        ids,
        vec![1, 11, 12, 13],
        "case after a transaction used inside every window committed: expected the seeded and \
         its own rows, got {ids:?}"
    );
}
