// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CLASS every operation addressing ONE saved query states a miss
//! of a `NamedQueryId` under, driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a request naming a resource this account holds none of is refused
//! under the class of that MISS — for a saved query, `NAMED_QUERY_NOT_FOUND`, the same class
//! the per-id entry a batch read carries — and not under the class of a malformed request
//! (`INVALID_INPUT`), which is what an id outside the modelled constraints is refused with
//! before any handler sees it. The envelope is `InvalidRequestException` (HTTP 400), the
//! class travels under both of the names the wire gives it (`AthenaErrorCode` and the
//! `ErrorCode` alias), and the sentence is `NamedQuery: <id> does not exist`, whose one
//! variable part is the id the request carried.
//!
//! Each test asserts that over a TABLE rather than one request: the rows differ along HOW an
//! id comes to name nothing (never minted, taken by a delete, taken by an earlier delete of
//! the same id) and along the FORM of the id (a UUID, a non-UUID token, the shortest id the
//! model allows and one of the longest), and the operations differ along what they would have
//! done with the query — read it, revise it, or take it.
//!
//! Sources: `smithy_query(query="operation",operation="GetNamedQuery,UpdateNamedQuery,DeleteNamedQuery,BatchGetNamedQuery")`
//! for the inputs and the `InvalidRequestException` (HTTP 400) each models, and
//! `smithy_query(query="shape",shape="NamedQueryId,ErrorCode")` for the id constraints
//! (`@length` 1 to 128, pattern `^\S+$`) the bound rows below sit on and just past.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_INVALID_INPUT, ERROR_CODE_NAMED_QUERY_NOT_FOUND,
};

use crate::harness::{assert_refusal, call, router};

/// The lower bound of the modelled `NamedQueryId` `@length` (1 to 128 characters).
const MIN_NAMED_QUERY_ID_CHARS: usize = 1;

/// Its upper bound.
const MAX_NAMED_QUERY_ID_CHARS: usize = 128;

/// An id of `chars` characters of non-whitespace, so a row can sit exactly on a published
/// bound of `NamedQueryId` — or one past it — without quoting any id a run minted.
fn id_of_length(chars: usize) -> String {
    "q".repeat(chars)
}

/// Save one query and answer with the `NamedQueryId` the create minted.
async fn save(router: &MultiServiceRouter, name: &str, query_string: &str) -> String {
    let request = json!({
        "Name": name,
        "Database": "logs",
        "QueryString": query_string,
    });
    let (status, body) = call(router, "CreateNamedQuery", &request).await;
    assert_eq!(
        status, 200,
        "CreateNamedQuery({request}) must be accepted, got {status} with {body:?}"
    );
    body["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNamedQuery must answer with an id, got {body:?}"))
        .to_string()
}

/// Every operation that addresses ONE saved query by its id, with a request body that is
/// well formed for that operation — so what each row exercises is the id resolving to
/// nothing and nothing else.
fn addressing_operations(id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("GetNamedQuery", json!({ "NamedQueryId": id })),
        (
            "UpdateNamedQuery",
            json!({
                "NamedQueryId": id,
                "Name": "written-by-a-refused-update",
                "QueryString": "SELECT 1 FROM events",
            }),
        ),
        ("DeleteNamedQuery", json!({ "NamedQueryId": id })),
    ]
}

/// Assert that every operation addressing the saved query `id` reports the miss the same
/// way: HTTP 400 `InvalidRequestException`, the class of the miss under both wire names, and
/// Athena's sentence for it.
async fn assert_every_operation_reports_the_miss(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) {
    // Spelled out here rather than taken only from the service's own helper: the fixed text
    // is `NamedQuery: ` and ` does not exist`, and the id the CALLER carried is the one
    // variable part.
    let expected = format!("NamedQuery: {id} does not exist");
    for (operation, request) in addressing_operations(id) {
        let (status, body) = call(router, operation, &request).await;
        let row = format!("{case} / {operation}");
        assert_refusal(
            &row,
            status,
            &body,
            ERROR_CODE_NAMED_QUERY_NOT_FOUND,
            &expected,
        );
        assert_eq!(
            crate::harness::message(&body),
            state::named_query_not_found(id),
            "case {row:?}: expected the one wording every miss of a saved query answers \
             with, got {body:?}"
        );
    }

    // The batch read states the SAME class about the same id, in its per-id entry rather
    // than as a refusal of the whole request: one id resolving to nothing is one fact,
    // however the request asked about it.
    let (status, body) = call(
        router,
        "BatchGetNamedQuery",
        &json!({ "NamedQueryIds": [id] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a batch read answers a partial success, got {status} with {body:?}"
    );
    let entries = body["UnprocessedNamedQueryIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected exactly one unprocessed entry, got {body:?}"
    );
    let entry = &entries[0];
    assert_eq!(
        entry["NamedQueryId"].as_str(),
        Some(id),
        "case {case:?}: the entry must name the id the request carried, got {entry:?}"
    );
    assert_eq!(
        entry["ErrorCode"].as_str(),
        Some(ERROR_CODE_NAMED_QUERY_NOT_FOUND),
        "case {case:?}: expected the class {ERROR_CODE_NAMED_QUERY_NOT_FOUND:?}, got {entry:?}"
    );
    assert_eq!(
        entry["ErrorMessage"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the message {expected:?}, got {entry:?}"
    );
}

#[tokio::test]
async fn every_operation_states_a_missing_saved_query_in_one_class() {
    let router = router();

    // Ids that resolve to nothing because no create ever minted them, differing in form and
    // sitting on each published bound of the modelled `NamedQueryId`.
    let unminted: Vec<(&str, String)> = vec![
        (
            "a UUID no create minted",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("an id that is no UUID at all", "saved-query-7".to_string()),
        (
            "the shortest id the model allows",
            id_of_length(MIN_NAMED_QUERY_ID_CHARS),
        ),
        (
            "the longest id the model allows",
            id_of_length(MAX_NAMED_QUERY_ID_CHARS),
        ),
    ];
    for (case, id) in &unminted {
        assert_every_operation_reports_the_miss(&router, case, id).await;
    }

    // An id a delete TOOK resolves to nothing for the same reason, so it is stated the same
    // way — and a second delete of it finds it gone and says so once more.
    let taken = save(&router, "taken-by-its-delete", "SELECT 1 FROM events").await;
    let (status, removed) = call(
        &router,
        "DeleteNamedQuery",
        &json!({ "NamedQueryId": taken }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete of {taken} must be served, got {status} with {removed:?}"
    );
    assert_every_operation_reports_the_miss(&router, "an id its own delete took", &taken).await;
    assert_every_operation_reports_the_miss(&router, "an id an earlier delete took", &taken).await;

    // A saved query the test did not touch is unaffected by any of the refusals above.
    let standing = save(&router, "left-standing", "SELECT 2 FROM events").await;
    let (status, body) = call(
        &router,
        "GetNamedQuery",
        &json!({ "NamedQueryId": standing }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the saved query {standing} must still read back, got {status} with {body:?}"
    );
}

#[tokio::test]
async fn an_id_outside_the_modelled_constraints_is_a_bad_request_not_a_miss() {
    let router = router();

    // One past the published upper bound, and the one form the pattern `^\S+$` excludes:
    // the request itself is wrong, so it is refused with the class of a bad request rather
    // than with the class of a saved query this account does not hold.
    let malformed: Vec<(&str, String)> = vec![
        (
            "an id one character past the modelled maximum",
            id_of_length(MAX_NAMED_QUERY_ID_CHARS + 1),
        ),
        (
            "an id of no characters at all",
            id_of_length(MIN_NAMED_QUERY_ID_CHARS - 1),
        ),
        ("an id carrying whitespace", "saved query".to_string()),
    ];
    for (case, id) in &malformed {
        for (operation, request) in addressing_operations(id) {
            let (status, body) = call(&router, operation, &request).await;
            let row = format!("{case} / {operation}");
            assert_eq!(
                status, 400,
                "case {row:?}: expected HTTP 400, got {status} with {body:?}"
            );
            for member in ["AthenaErrorCode", "ErrorCode"] {
                assert_eq!(
                    body[member].as_str(),
                    Some(ERROR_CODE_INVALID_INPUT),
                    "case {row:?}: expected {member} {ERROR_CODE_INVALID_INPUT:?}, got {body:?}"
                );
            }
        }
    }
}

/// One revision a row drives: the members the update carries, and what the query must report
/// once it is applied.
struct Revision {
    case: &'static str,
    name: String,
    query_string: String,
    description: Option<String>,
}

#[tokio::test]
async fn a_revision_is_read_back_and_a_delete_makes_the_id_miss() {
    let router = router();

    // Rows differing along the VALUE, the SIZE and the number of statements the revision
    // writes, and along whether it carries a description — never along the recorded case
    // alone.
    let revisions = vec![
        Revision {
            case: "a new name and a new single statement",
            name: "revised-one".to_string(),
            query_string: "SELECT 100".to_string(),
            description: None,
        },
        Revision {
            case: "a name of the longest the model allows",
            // `NameString` is modelled `@length` 1 to 128.
            name: "n".repeat(128),
            query_string: "SELECT 1 FROM events; SELECT 2 FROM events".to_string(),
            description: Some("revised twice over".to_string()),
        },
        Revision {
            case: "a long query text and an empty description",
            name: "revised-long".to_string(),
            query_string: (0..200)
                .map(|position| format!("SELECT {position} FROM events"))
                .collect::<Vec<_>>()
                .join("; "),
            // `NamedQueryDescriptionString` is modelled `@length` 0 to 1024, so the empty
            // description is a value a client may write.
            description: Some(String::new()),
        },
    ];

    for revision in &revisions {
        let case = revision.case;
        let id = save(&router, "created-as", "SELECT 1").await;

        let mut update = json!({
            "NamedQueryId": id,
            "Name": revision.name,
            "QueryString": revision.query_string,
        });
        if let Some(description) = &revision.description {
            update["Description"] = json!(description);
        }
        let (status, applied) = call(&router, "UpdateNamedQuery", &update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be served, got {status} with {applied:?}"
        );

        let (status, body) = call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the revised query to read back, got {status} with {body:?}"
        );
        let found = &body["NamedQuery"];
        assert_eq!(
            found["Name"].as_str(),
            Some(revision.name.as_str()),
            "case {case:?}: expected the Name the update wrote, got {found:?}"
        );
        assert_eq!(
            found["QueryString"].as_str(),
            Some(revision.query_string.as_str()),
            "case {case:?}: expected the QueryString the update wrote, got {:?}",
            found["QueryString"].as_str().map(|text| text.len())
        );
        assert_eq!(
            found["Description"].as_str(),
            revision.description.as_deref(),
            "case {case:?}: expected the Description the update wrote, got {found:?}"
        );
        assert_eq!(
            found["NamedQueryId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the revision keeps the id the create minted, got {found:?}"
        );

        // The delete is observable: every operation addressing the id now states the miss.
        let (status, removed) = call(&router, "DeleteNamedQuery", &json!({ "NamedQueryId": id }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to be served, got {status} with {removed:?}"
        );
        assert_every_operation_reports_the_miss(&router, case, &id).await;
    }
}
