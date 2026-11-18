// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteNamedQuery`, driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a delete addressed by `NamedQueryId` takes exactly the saved query
//! that id names out of this account's registry — it is then absent from EVERY read of it,
//! the single read and the batch read alike, while every other saved query answers
//! unchanged — and an id the registry holds no saved query under, whether one no create ever
//! minted or one an earlier delete already took, is refused as a bad request and changes
//! nothing. Removal is therefore the one effect of a delete, and it happens at most once per
//! saved query however many callers ask for it at once.
//!
//! The surface is the model's, read with
//! `smithy_query(query="operation",operation="DeleteNamedQuery")`: the input carries only the
//! required `NamedQueryId` (1 to 128 characters, `^\S+$`), the output models no members, and
//! the client error the operation declares is `InvalidRequestException` (HTTP 400).
//!
//! The angle this suite adds beside `named_query_listing.rs`, which pins the delete against
//! the single read and the workgroup's listing: what a delete does to a registry holding
//! SEVERAL queries across workgroups and scripts, how a batch read reports a deleted id, and
//! what happens when callers race for the same id. Each test asserts the rule over a TABLE
//! rather than one request: the rows differ along which workgroup the query belongs to,
//! which optional members its create carried, the script its text is written in, the order
//! the deletes are issued in, the form of an id that names no query, and how many callers
//! race.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_work_group, error_code, message, router};

/// The error shape a refusal of this operation carries: the client error it models
/// (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// A workgroup of its own, so a row covers a saved query outside the account's default
/// workgroup as well as inside it.
const OTHER_WORK_GROUP: &str = "delete-work-group";

/// One saved query a row deletes: how the row reads, the create that saved it, and the id
/// that create minted.
struct Saved {
    case: &'static str,
    request: Value,
    id: String,
}

/// Save one named query and answer with the `NamedQueryId` the create minted.
async fn save(router: &MultiServiceRouter, request: &Value) -> String {
    let (status, created) = call(router, "CreateNamedQuery", request).await;
    assert_eq!(
        status, 200,
        "the named query {request} must be created, got {status} with {created:?}"
    );
    created["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNamedQuery must mint an id, got {created:?}"))
        .to_string()
}

/// The saved queries every table below deletes from, differing along the workgroup they
/// belong to, the optional members their create carried, and the script their text is
/// written in.
async fn given_named_queries(router: &MultiServiceRouter) -> Vec<Saved> {
    create_work_group(router, OTHER_WORK_GROUP).await;
    let requests: Vec<(&'static str, Value)> = vec![
        (
            "a query with no description, in the account's default workgroup",
            json!({
                "Name": "errors",
                "Database": "logs",
                "QueryString": "SELECT * FROM requests WHERE status >= 500",
            }),
        ),
        (
            "a query with a description, in the default workgroup named explicitly",
            json!({
                "Name": "slowest",
                "Database": "logs",
                "QueryString": "SELECT * FROM requests ORDER BY duration DESC",
                "Description": "the slowest requests",
                "WorkGroup": DEFAULT_WORK_GROUP,
            }),
        ),
        (
            "a query in a workgroup of its own",
            json!({
                "Name": "scoped",
                "Database": "analytics",
                "QueryString": "SELECT count(*) FROM events",
                "WorkGroup": OTHER_WORK_GROUP,
            }),
        ),
        (
            "a query whose name, description and text are not ASCII",
            json!({
                "Name": "ventas — año ✓",
                "Database": "analytics",
                "QueryString": "SELECT \"año\", count(*) FROM ventas WHERE región = 'münchen'",
                "Description": "ventas por región — 日本語 🙂",
                "WorkGroup": OTHER_WORK_GROUP,
            }),
        ),
    ];
    let mut saved = Vec::new();
    for (case, request) in requests {
        let id = save(router, &request).await;
        saved.push(Saved { case, request, id });
    }
    saved
}

/// One delete: `(status, body)`.
async fn delete(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "DeleteNamedQuery", &json!({ "NamedQueryId": id })).await
}

/// `GetNamedQuery`'s answer for one id: `(status, body)`.
async fn read(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await
}

/// The ids a batch read answers with as saved queries, and the ones it reports as
/// unprocessed.
async fn batch(router: &MultiServiceRouter, ids: &[String]) -> (Vec<String>, Vec<String>) {
    let (status, body) = call(
        router,
        "BatchGetNamedQuery",
        &json!({ "NamedQueryIds": ids }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the batch read must be served, got {status} with {body:?}"
    );
    let ids_of = |member: &str| -> Vec<String> {
        body[member]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|entry| {
                entry["NamedQueryId"]
                    .as_str()
                    .unwrap_or("<no NamedQueryId>")
                    .to_string()
            })
            .collect()
    };
    (ids_of("NamedQueries"), ids_of("UnprocessedNamedQueryIds"))
}

/// Assert that one delete was served with the empty success the model declares.
fn assert_served(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be served, got {status} with {body:?}"
    );
    assert_eq!(
        body,
        &json!({}),
        "case {case:?}: the output models no members, got {body:?}"
    );
}

/// Assert that one delete of an id the registry holds no saved query under is refused as a
/// bad request, in Athena's words for that miss.
fn assert_refused(case: &str, status: u16, body: &Value, id: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        INVALID_REQUEST,
        "case {case:?}: expected {INVALID_REQUEST}, got {body:?}"
    );
    assert_eq!(
        body["ErrorCode"].as_str(),
        Some(ERROR_CODE_INVALID_INPUT),
        "case {case:?}: expected the class of a refusal the request caused, got {body:?}"
    );
    // The sentence itself, spelled out here rather than taken from the service's own helper:
    // the resource type, a colon, the id the request carried and "does not exist".
    assert_eq!(
        message(body),
        format!("NamedQuery: {id} does not exist"),
        "case {case:?}: expected Athena's wording for a missing saved query, got {:?}",
        message(body)
    );
}

/// Assert that `id` names no saved query, through the single read and the batch read alike.
async fn assert_gone(router: &MultiServiceRouter, case: &str, id: &str) {
    let (status, body) = read(router, id).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a deleted query to be unreadable, got {status} with {body:?}"
    );
    // Athena's wording for this miss, spelled out here rather than taken from the service's
    // own helper: the resource type, a colon, the id the request carried, "does not exist".
    assert_eq!(
        message(&body),
        format!("NamedQuery: {id} does not exist"),
        "case {case:?}: expected Athena's wording for a missing saved query, got {:?}",
        message(&body)
    );
    let (answered, unprocessed) = batch(router, &[id.to_string()]).await;
    assert!(
        answered.is_empty(),
        "case {case:?}: a batch read must answer with no query for a deleted id, got \
         {answered:?}"
    );
    assert_eq!(
        unprocessed,
        vec![id.to_string()],
        "case {case:?}: a batch read must report the deleted id as unprocessed, got \
         {unprocessed:?}"
    );
}

/// Assert that `saved` still reads back exactly as its create saved it.
async fn assert_still_saved(router: &MultiServiceRouter, case: &str, saved: &Saved) {
    let (status, body) = read(router, &saved.id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {:?} must still be readable, got {status} with {body:?}",
        saved.case
    );
    let found = &body["NamedQuery"];
    for member in ["Name", "Database", "QueryString", "Description"] {
        assert_eq!(
            found[member], saved.request[member],
            "case {case:?}: expected {:?} to keep its {member} {:?}, got {:?}",
            saved.case, saved.request[member], found[member]
        );
    }
    assert_eq!(
        found["NamedQueryId"].as_str(),
        Some(saved.id.as_str()),
        "case {case:?}: expected {:?} to keep its id, got {:?}",
        saved.case,
        found["NamedQueryId"]
    );
}

/// A delete takes exactly the query its id names: that id names no query afterwards and
/// every other saved query answers unchanged, whichever query of the table is deleted and in
/// whichever order.
#[tokio::test]
async fn a_delete_removes_exactly_the_query_its_id_names() {
    // The rows: which position of the table is deleted first, so each query is in turn the
    // one taken while the others must be untouched.
    for first in 0..4usize {
        let router = router();
        let saved = given_named_queries(&router).await;
        let case = saved[first].case;

        let (status, body) = delete(&router, &saved[first].id).await;
        assert_served(case, status, &body);

        assert_gone(&router, case, &saved[first].id).await;
        for (position, other) in saved.iter().enumerate() {
            if position != first {
                assert_still_saved(&router, case, other).await;
            }
        }

        // The rest of the table is then deleted in turn: every saved query can be taken, and
        // the registry ends up holding none of them.
        for (position, other) in saved.iter().enumerate() {
            if position == first {
                continue;
            }
            let (status, body) = delete(&router, &other.id).await;
            assert_served(other.case, status, &body);
            assert_gone(&router, other.case, &other.id).await;
        }
        let every_id: Vec<String> = saved.iter().map(|saved| saved.id.clone()).collect();
        let (answered, unprocessed) = batch(&router, &every_id).await;
        assert!(
            answered.is_empty(),
            "case {case:?}: no query of the table may be left, got {answered:?}"
        );
        assert_eq!(
            unprocessed.len(),
            every_id.len(),
            "case {case:?}: every id of the table must name no query, got {unprocessed:?}"
        );
    }
}

/// A delete of an id this account holds no saved query under is refused as a bad request, and
/// the refusal takes nothing away: an id an earlier delete already took, and an id no create
/// ever minted.
#[tokio::test]
async fn an_id_the_registry_does_not_hold_is_refused() {
    let router = router();
    let saved = given_named_queries(&router).await;
    let taken = saved[0].id.clone();
    let (status, body) = delete(&router, &taken).await;
    assert_served(saved[0].case, status, &body);

    // The rows differ along the FORM of an id that names no saved query, each satisfying the
    // modelled `NamedQueryId` constraints (1 to 128 characters, `^\S+$`) so the only thing
    // true of it is that the registry holds nothing under it: an id an earlier delete took,
    // an id shaped like a minted one, the shortest admissible id, the longest admissible id,
    // and one that is not ASCII.
    let rows: Vec<(&str, String)> = vec![
        ("an id an earlier delete took", taken.clone()),
        (
            "an id no create minted",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("the shortest admissible id", "q".to_string()),
        ("the longest admissible id", "q".repeat(128)),
        ("an id that is not ASCII", "consulta-año-✓".to_string()),
    ];

    for (case, id) in rows {
        let (status, body) = delete(&router, &id).await;
        assert_refused(case, status, &body, &id);
    }

    // Nothing those refusals touched: every query the table still holds reads back as it was
    // saved, and the one that was taken is still the only one gone.
    for other in saved.iter().skip(1) {
        assert_still_saved(&router, "after the refusals", other).await;
    }
    assert_gone(&router, "after the refusals", &taken).await;
}

/// However many callers delete one saved query at once, exactly one of them takes it: the
/// rest find the registry no longer holding that id and are refused the way a delete of an id
/// no create ever minted is, and none of the other saved queries is touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_deletes_of_one_id_leave_exactly_one_success() {
    // The rows differ in how many callers race for the same id at once: two, and two larger
    // counts.
    for racers in [2usize, 4, 9] {
        let router = Arc::new(router());
        let saved = given_named_queries(&router).await;
        let id = saved[0].id.clone();
        let case = format!("{racers} racing deletes");

        let mut tasks = Vec::with_capacity(racers);
        for _ in 0..racers {
            let router = Arc::clone(&router);
            let id = id.clone();
            tasks.push(tokio::spawn(async move { delete(&router, &id).await }));
        }
        let mut answers = Vec::with_capacity(racers);
        for task in tasks {
            answers.push(task.await.expect("the delete task runs to completion"));
        }

        let served = answers.iter().filter(|(status, _)| *status == 200).count();
        assert_eq!(
            served, 1,
            "case {case:?}: expected exactly one delete to take the query, got {answers:?}"
        );
        for (status, body) in answers.iter().filter(|(status, _)| *status != 200) {
            assert_refused(&case, *status, body, &id);
        }
        assert_gone(&router, &case, &id).await;
        for other in saved.iter().skip(1) {
            assert_still_saved(&router, &case, other).await;
        }
    }
}
