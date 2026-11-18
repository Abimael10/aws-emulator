// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a BATCH read of query executions classifies an id it cannot
//! answer with an execution.
//!
//! `BatchGetQueryExecution` answers in two parts — `QueryExecutions` for every id this
//! account holds an execution for, `UnprocessedQueryExecutionIds` for every id it does not
//! — and the entry an id is reported under carries the CLASS of that id's failure under
//! `ErrorCode`, which is what a client branches on for a partial answer. The two classes
//! are decided one before the other:
//!
//! * an id that is not a query execution identifier at all — one outside the modelled
//!   `QueryExecutionId` (1 to 128 characters matching `^\S+$`, read with
//!   `smithy_query(query="shape",shape="QueryExecutionIdList,QueryExecutionId")`) — makes
//!   the request itself wrong, so the whole request is refused with the class a bad request
//!   carries (`INVALID_INPUT`) and nothing is partitioned;
//! * an id that IS one and that resolves to no execution failed only at resolving, which is
//!   its own class (`QUERY_EXECUTION_NOT_FOUND`), reported in band beside the sentence a
//!   single read of that id is refused with.
//!
//! The rule is asserted over TABLES rather than one id: the rows differ along the FORM of
//! the id (the shortest and the longest the model admits, a hyphenated UUID in either case,
//! a word, an id carrying punctuation, and a real execution's id with one character
//! changed) and, for the refused half, along the constraint the id breaks (each published
//! bound and one step past it, and the pattern). The model constrains the id to nothing
//! narrower than those two constraints, so no row expects a well-formed id to be reported
//! as a bad request.
//!
//! Every call goes over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_INVALID_INPUT, ERROR_CODE_QUERY_EXECUTION_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, router, start};

/// Where the queries in this suite write their results: what matters is that a query HAS a
/// location, so a start is accepted and really mints an id.
const OUTPUT_LOCATION: &str = "s3://athena-results/batch-unprocessed-suite/";

/// A `StartQueryExecution` request for `sql` that names where its results go.
fn query(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// The answer to one batch read of `ids`.
async fn batch(router: &MultiServiceRouter, ids: &[String]) -> (u16, Value) {
    call(
        router,
        "BatchGetQueryExecution",
        &json!({ "QueryExecutionIds": ids }),
    )
    .await
}

/// The `QueryExecutionId` of every execution a batch answer reports, in order.
fn reported(body: &Value) -> Vec<String> {
    body["QueryExecutions"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|found| found["QueryExecutionId"].as_str().map(str::to_string))
        .collect()
}

/// Every entry a batch answer reports under `UnprocessedQueryExecutionIds`, in order.
fn unprocessed(body: &Value) -> Vec<Value> {
    body["UnprocessedQueryExecutionIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// An id that satisfies the modelled `QueryExecutionId` and that this account holds no
/// execution under is reported IN BAND, under the class of a permanent miss, beside the
/// sentence a single read of that id is refused with — while a real id asked for in the
/// same request still comes back as an execution.
#[tokio::test]
async fn an_unresolvable_well_formed_id_is_reported_as_a_query_execution_miss() {
    let router = router();
    let held = start(&router, &query("SELECT 0 AS n")).await;

    // A real id with its last character replaced: an id of exactly the form this service
    // mints, which no execution answers to. Built from state rather than written down, so
    // the row is not a recorded id.
    let mut near_miss = held.clone();
    let last = near_miss.pop().expect("a minted id is not empty");
    near_miss.push(if last == 'z' { 'y' } else { 'z' });

    // The rows differ along the FORM and the SIZE of the id, inside the two constraints the
    // model declares on `QueryExecutionId` (1 to 128 characters, `^\S+$`).
    let cases: Vec<(&str, String)> = vec![
        ("the shortest id the model admits", "x".to_string()),
        ("the longest id the model admits", "x".repeat(128)),
        (
            "a hyphenated UUID no start minted",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
        ),
        (
            "the same UUID in upper case",
            "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE".to_string(),
        ),
        (
            "a word that is no UUID at all",
            "no-such-execution".to_string(),
        ),
        ("an id carrying punctuation", "not/a::query#id".to_string()),
        ("a real id with one character changed", near_miss.clone()),
    ];

    for (case, id) in &cases {
        // Asked for beside an id this account really holds, so the answer is a PARTITION
        // and not one class for everything.
        let (status, body) = batch(&router, &[held.clone(), id.clone()]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        assert_eq!(
            reported(&body),
            vec![held.clone()],
            "case {case:?}: expected the held execution alone under QueryExecutions, got \
             {body:?}"
        );
        let entries = unprocessed(&body);
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: expected exactly one unprocessed entry, got {entries:?}"
        );
        let entry = &entries[0];
        assert_eq!(
            entry["QueryExecutionId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the id it was asked for, got {entry:?}"
        );
        assert_eq!(
            entry["ErrorCode"].as_str(),
            Some(ERROR_CODE_QUERY_EXECUTION_NOT_FOUND),
            "case {case:?}: expected the class {ERROR_CODE_QUERY_EXECUTION_NOT_FOUND:?}, got \
             {entry:?}"
        );
        assert_eq!(
            entry["ErrorMessage"].as_str(),
            Some(state::query_execution_not_found(id).as_str()),
            "case {case:?}: expected the sentence for a miss of that id, got {entry:?}"
        );

        // The class and the sentence are the ones a SINGLE read of the same id is refused
        // with: one miss, stated the same way whichever read asked.
        let (refused_status, refused) = call(
            &router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            refused_status, 400,
            "case {case:?}: a single read of the same id must be refused, got \
             {refused_status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        for member in ["AthenaErrorCode", "ErrorCode"] {
            assert_eq!(
                refused[member].as_str(),
                entry["ErrorCode"].as_str(),
                "case {case:?}: the single read must carry the class the batch entry does, \
                 got {refused:?}"
            );
        }
        assert_eq!(
            message(&refused),
            entry["ErrorMessage"].as_str().unwrap_or_default(),
            "case {case:?}: the single read must state what the batch entry states, got \
             {refused:?}"
        );
    }

    // Every unknown id of the table asked for in ONE request is reported in the order the
    // request listed them, each with its own entry — a batch of 50 ids is the most the
    // model admits, and this is well inside it.
    let asked: Vec<String> = cases.iter().map(|(_, id)| id.clone()).collect();
    let (status, body) = batch(&router, &asked).await;
    assert_eq!(
        status, 200,
        "a batch of unknown ids must be answered, got {status} with {body:?}"
    );
    assert!(
        reported(&body).is_empty(),
        "no unknown id may be reported as an execution, got {body:?}"
    );
    let echoed: Vec<String> = unprocessed(&body)
        .iter()
        .filter_map(|entry| entry["QueryExecutionId"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        echoed, asked,
        "expected every unknown id in the order asked for, got {body:?}"
    );
}

/// An id that is NOT a query execution identifier makes the request itself wrong: the whole
/// batch is refused with the class a bad request carries, and nothing is partitioned.
#[tokio::test]
async fn an_id_outside_the_modelled_identifier_refuses_the_whole_batch_as_invalid_input() {
    let router = router();
    let held = start(&router, &query("SELECT 1 AS n")).await;

    // The rows differ along the constraint the id breaks: each published bound of
    // `QueryExecutionId` and one step past it, and its `^\S+$` pattern.
    let cases: Vec<(&str, String)> = vec![
        (
            "no characters at all, one step below the minimum",
            String::new(),
        ),
        ("one character past the maximum", "x".repeat(129)),
        ("an id carrying a space", "has a space".to_string()),
        ("an id that is whitespace alone", " ".to_string()),
        ("an id carrying a tab", "has\ta-tab".to_string()),
        (
            "a UUID-shaped id with a trailing newline",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n".to_string(),
        ),
    ];

    for (case, id) in &cases {
        // Asked for beside an id this account really holds: the refusal is of the REQUEST,
        // so the held execution is not answered either.
        let (status, body) = batch(&router, &[held.clone(), id.clone()]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {body:?}"
        );
        for member in ["AthenaErrorCode", "ErrorCode"] {
            assert_eq!(
                body[member].as_str(),
                Some(ERROR_CODE_INVALID_INPUT),
                "case {case:?}: expected {member} {ERROR_CODE_INVALID_INPUT:?}, got {body:?}"
            );
        }
        assert!(
            body["QueryExecutions"].is_null() && body["UnprocessedQueryExecutionIds"].is_null(),
            "case {case:?}: a refused request partitions nothing, got {body:?}"
        );
        // The clause names the member that broke its constraint — the element of the list,
        // at the position the request listed it.
        assert!(
            message(&body).contains("queryExecutionIds") && message(&body).contains(".member'"),
            "case {case:?}: the refusal must name the member that broke its constraint, got \
             {body:?}"
        );
        assert!(
            !message(&body).contains(id.as_str()) || id.trim().is_empty(),
            "case {case:?}: the refusal states the requirement, not the bytes the request \
             carried, got {body:?}"
        );
    }

    // The bounds themselves: the shortest and the longest id the model admits are inside
    // the constraint, so a batch carrying them is answered rather than refused.
    for (case, id) in [
        ("the minimum length", "x".to_string()),
        ("the maximum length", "x".repeat(128)),
    ] {
        let (status, body) = batch(&router, &[id]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an id at the bound must be answered, got {status} with {body:?}"
        );
    }
}
