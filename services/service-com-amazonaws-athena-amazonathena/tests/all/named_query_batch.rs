// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `BatchGetNamedQuery`, driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a batch read partitions the ids it was given — each id this account
//! holds a named query under is answered from that query, exactly as a single read of it
//! answers, and each id it holds none under is reported under `UnprocessedNamedQueryIds`
//! with the reason, in the order the request listed them. Nothing is created or removed by
//! reading, and the batch is never partly processed: a list longer than the model admits is
//! refused outright.
//!
//! The bounds are the model's, read with
//! `smithy_query(query="operation",operation="BatchGetNamedQuery")` and
//! `smithy_query(query="shape",shape="NamedQueryIdList,UnprocessedNamedQueryId")`:
//! `NamedQueryIds` is required and holds 1 to 50 ids, and an unprocessed id carries the id,
//! an error code and an error message.
//!
//! Each test asserts the rule over a TABLE rather than one request: the rows differ along
//! how many ids the batch carries, how many of them resolve, their order, and whether an id
//! is repeated.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    named_query_not_found, DEFAULT_WORK_GROUP, ERROR_CODE_NAMED_QUERY_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_work_group, error_code, message, router};

/// The error shape a refusal of this operation carries: the client error it models
/// (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The most ids one batch may carry: the maximum of the modelled `NamedQueryIdList`
/// `@length` (1 to 50).
const MAX_BATCH_IDS: usize = 50;

/// The workgroup of its own one of the saved queries below belongs to, so a row covers a
/// query outside the account's default workgroup as well as inside it.
const OTHER_WORK_GROUP: &str = "batch-work-group";

/// Save one named query and answer with the `NamedQueryId` the create minted.
async fn save(
    router: &MultiServiceRouter,
    name: &str,
    database: &str,
    query_string: &str,
    description: Option<&str>,
    work_group: Option<&str>,
) -> String {
    let mut request = json!({
        "Name": name,
        "Database": database,
        "QueryString": query_string,
    });
    if let Some(description) = description {
        request["Description"] = json!(description);
    }
    if let Some(work_group) = work_group {
        request["WorkGroup"] = json!(work_group);
    }
    let (status, created) = call(router, "CreateNamedQuery", &request).await;
    assert_eq!(
        status, 200,
        "the named query {name} must be created, got {status} with {created:?}"
    );
    created["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNamedQuery must mint an id, got {created:?}"))
        .to_string()
}

/// Three saved queries, differing along the members a create may carry and the workgroup
/// they belong to: `(case, NamedQueryId)`.
async fn given_named_queries(router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    create_work_group(router, OTHER_WORK_GROUP).await;
    vec![
        (
            "a query with no description, in the default workgroup",
            save(
                router,
                "errors",
                "logs",
                "SELECT * FROM requests WHERE status >= 500",
                None,
                None,
            )
            .await,
        ),
        (
            "a query with a description, in the default workgroup",
            save(
                router,
                "slowest",
                "logs",
                "SELECT * FROM requests ORDER BY duration DESC",
                Some("the slowest requests"),
                Some(DEFAULT_WORK_GROUP),
            )
            .await,
        ),
        (
            "a query in a workgroup of its own",
            save(
                router,
                "scoped",
                "analytics",
                "SELECT count(*) FROM events",
                None,
                Some(OTHER_WORK_GROUP),
            )
            .await,
        ),
    ]
}

/// One batch read: `(status, NamedQueries, UnprocessedNamedQueryIds)`.
async fn batch(router: &MultiServiceRouter, ids: &[String]) -> (u16, Vec<Value>, Vec<Value>) {
    let (status, body) = call(
        router,
        "BatchGetNamedQuery",
        &json!({ "NamedQueryIds": ids }),
    )
    .await;
    let queries = body["NamedQueries"].as_array().cloned().unwrap_or_default();
    let unprocessed = body["UnprocessedNamedQueryIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (status, queries, unprocessed)
}

/// `GetNamedQuery`'s view of one id.
async fn read(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
    assert_eq!(
        status, 200,
        "the named query {id} must be readable, got {status} with {body:?}"
    );
    body["NamedQuery"].clone()
}

/// An id a saved query was never minted under, satisfying the element constraints of
/// `NamedQueryIdList` (1 to 128 characters, `^\S+$`) so the only thing wrong with it is that
/// the registry holds nothing under it.
fn unknown_id(seed: usize) -> String {
    format!("00000000-0000-4000-8000-{seed:012}")
}

/// Every id the registry holds a query under is answered from that query, in the order the
/// request listed them, and each entry is what a single read of that id answers with.
#[tokio::test]
async fn every_held_id_is_answered_as_a_read_answers_it() {
    let router = router();
    let saved = given_named_queries(&router).await;

    // The rows: one id, every id, and every id in the reverse order — the answer follows
    // the request's order, not the registry's.
    let all: Vec<String> = saved.iter().map(|(_, id)| id.clone()).collect();
    let mut reversed = all.clone();
    reversed.reverse();
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("one id", vec![all[0].clone()]),
        ("every id", all.clone()),
        ("every id in reverse", reversed),
        ("one id repeated", vec![all[1].clone(), all[1].clone()]),
    ];

    for (case, ids) in rows {
        let (status, queries, unprocessed) = batch(&router, &ids).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the batch to be served, got {status}"
        );
        assert!(
            unprocessed.is_empty(),
            "case {case:?}: every id was held, so none is unprocessed, got {unprocessed:?}"
        );
        assert_eq!(
            queries.len(),
            ids.len(),
            "case {case:?}: expected one entry per id listed, got {queries:?}"
        );
        for (position, id) in ids.iter().enumerate() {
            let reported = &queries[position];
            assert_eq!(
                reported["NamedQueryId"].as_str(),
                Some(id.as_str()),
                "case {case:?}: entry {position} must be the id at that position, got \
                 {reported:?}"
            );
            assert_eq!(
                reported, &read(&router, id).await,
                "case {case:?}: entry {position} must be what a read of it answers, got \
                 {reported:?}"
            );
        }
    }
}

/// One row of the partition table: the case, the ids the batch sends, the ids it must
/// answer with as queries, and the ids it must report as unprocessed.
struct PartitionCase {
    case: &'static str,
    sent: Vec<String>,
    answered: Vec<String>,
    unprocessed: Vec<String>,
}

/// An id the registry holds nothing under is reported as unprocessed, with the class of a
/// refusal the request itself caused and the sentence a single read of that id states.
#[tokio::test]
async fn an_id_the_registry_does_not_hold_is_reported_as_unprocessed() {
    let router = router();
    let saved = given_named_queries(&router).await;
    let held: Vec<String> = saved.iter().map(|(_, id)| id.clone()).collect();

    let rows: Vec<PartitionCase> = vec![
        PartitionCase {
            case: "nothing but unheld ids",
            sent: vec![unknown_id(1), unknown_id(2)],
            answered: Vec::new(),
            unprocessed: vec![unknown_id(1), unknown_id(2)],
        },
        PartitionCase {
            case: "an unheld id between two held ones",
            sent: vec![held[0].clone(), unknown_id(3), held[1].clone()],
            answered: vec![held[0].clone(), held[1].clone()],
            unprocessed: vec![unknown_id(3)],
        },
        PartitionCase {
            case: "an unheld id repeated",
            sent: vec![unknown_id(4), unknown_id(4)],
            answered: Vec::new(),
            unprocessed: vec![unknown_id(4), unknown_id(4)],
        },
    ];

    for row in rows {
        let PartitionCase {
            case,
            sent,
            answered: expected_queries,
            unprocessed: expected_unprocessed,
        } = row;
        let (status, queries, unprocessed) = batch(&router, &sent).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an unheld id is reported, not refused, got {status}"
        );
        let answered: Vec<String> = queries
            .iter()
            .map(|query| {
                query["NamedQueryId"]
                    .as_str()
                    .unwrap_or("<no NamedQueryId>")
                    .to_string()
            })
            .collect();
        assert_eq!(
            answered, expected_queries,
            "case {case:?}: expected the held ids answered in order, got {queries:?}"
        );
        let missed: Vec<String> = unprocessed
            .iter()
            .map(|entry| {
                entry["NamedQueryId"]
                    .as_str()
                    .unwrap_or("<no NamedQueryId>")
                    .to_string()
            })
            .collect();
        assert_eq!(
            missed, expected_unprocessed,
            "case {case:?}: expected the unheld ids reported in order, got {unprocessed:?}"
        );
        for entry in &unprocessed {
            let id = entry["NamedQueryId"].as_str().unwrap_or_default();
            assert_eq!(
                entry["ErrorCode"].as_str(),
                Some(ERROR_CODE_NAMED_QUERY_NOT_FOUND),
                "case {case:?}: expected the class of a saved query that does not exist, got \
                 {entry:?}"
            );
            assert_eq!(
                entry["ErrorMessage"].as_str(),
                Some(named_query_not_found(id).as_str()),
                "case {case:?}: expected the sentence a read of that id states, got {entry:?}"
            );
        }
    }
}

/// The batch is bounded by the modelled list length: as many ids as it admits are served,
/// and one more is refused outright — no part of such a batch is processed.
#[tokio::test]
async fn a_batch_longer_than_the_list_admits_is_refused_outright() {
    let router = router();
    let saved = given_named_queries(&router).await;
    let held = saved[0].1.clone();

    // A full batch: the held id, then unheld ones up to the bound the model publishes.
    let mut full: Vec<String> = vec![held.clone()];
    full.extend((1..MAX_BATCH_IDS).map(unknown_id));
    let (status, queries, unprocessed) = batch(&router, &full).await;
    assert_eq!(
        status, 200,
        "a batch of the most ids the list admits must be served, got {status}"
    );
    assert_eq!(
        queries.len(),
        1,
        "exactly the one held id must be answered, got {queries:?}"
    );
    assert_eq!(
        unprocessed.len(),
        MAX_BATCH_IDS - 1,
        "every other id must be reported as unprocessed, got {unprocessed:?}"
    );

    // One id past the bound: refused as a bad request, naming the member and the limit.
    let mut past = full.clone();
    past.push(unknown_id(MAX_BATCH_IDS));
    let (status, body) = call(
        &router,
        "BatchGetNamedQuery",
        &json!({ "NamedQueryIds": past }),
    )
    .await;
    assert_eq!(status, 400, "expected HTTP 400, got {status} with {body:?}");
    assert_eq!(
        error_code(&body),
        INVALID_REQUEST,
        "expected {INVALID_REQUEST}, got {body:?}"
    );
    assert_eq!(
        message(&body),
        format!(
            "1 validation error detected: Value at 'namedQueryIds' failed to satisfy \
             constraint: Member must have length less than or equal to {MAX_BATCH_IDS}"
        ),
        "expected AWS's validation wording for the list's length, got {:?}",
        message(&body)
    );

    // Nothing was processed: the one held id is still readable and the registry answers the
    // same batch as before.
    let (status, queries, unprocessed) = batch(&router, &full).await;
    assert_eq!(
        status, 200,
        "the batch within the bound must still be served, got {status}"
    );
    assert_eq!(
        queries.len(),
        1,
        "the refused batch must have changed nothing, got {queries:?}"
    );
    assert_eq!(
        unprocessed.len(),
        MAX_BATCH_IDS - 1,
        "the refused batch must have changed nothing, got {unprocessed:?}"
    );
}
