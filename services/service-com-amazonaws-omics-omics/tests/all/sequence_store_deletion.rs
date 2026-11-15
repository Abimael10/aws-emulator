// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteSequenceStore`.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes. The HTTP harness is the one
//! `private_workflow_runs` owns and the sequence-store helpers are `sequence_stores`'.
//!
//! The rule under test: deleting a sequence store RECLAIMS it — the store and everything
//! held inside it (its tags, and the multipart read set uploads initiated against it,
//! which are reachable only through the store) leave the service's state, so every later
//! call that names the id answers the modelled `ResourceNotFoundException` (HTTP 404),
//! while no other store is touched. It holds whatever the store carried, whatever status
//! the clock has left it in and however much it holds, and an id the registry does not
//! hold is that same 404 however the id is shaped.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_omics_omics::state::{
    service_state, MAX_TAGS, SEQUENCE_STORE_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::multipart_read_set_uploads::{
    create_upload, created_upload_id, emulator_holding_its_engine,
};
use crate::private_workflow_runs::{call, error_code, tag_map};
use crate::sequence_stores::{await_store_status, create_store, created_store_id, get_store};

/// The published bounds of the modelled `SequenceStoreId` length (10..36).
/// Source: the `id` member of `DeleteSequenceStoreRequest`.
const SEQUENCE_STORE_ID_WIDTH: (usize, usize) = (10, 36);

/// A KMS key ARN satisfying the modelled `SseConfig$keyArn` (len 20..2048).
const KEY_ARN: &str = "arn:aws:kms:us-west-2:000000000000:key/omics-emulator-key";

/// `DELETE /sequencestore/{id}`.
async fn delete_store(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/sequencestore/{id}"), None).await
}

/// Assert a refusal is the modelled sequence-store 404, carrying the service's own
/// wording for it.
fn assert_store_not_found(case: &str, operation: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected 404 from {operation}, got {body:?}"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException from {operation}, got {body:?}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(SEQUENCE_STORE_NOT_FOUND_MESSAGE),
        "case {case:?}: expected the service's own wording from {operation}, got {body:?}"
    );
}

/// Whether the service still holds a sequence store record for `id`.
fn store_is_held(engine: &Arc<engine::InMemoryEngine>, id: &str) -> bool {
    let state = service_state(&**engine).expect("the omics service state");
    // Bound rather than returned directly: the guard the read borrows from would
    // otherwise outlive `state`, which the borrow checker refuses.
    let held = state.lock().sequence_store(id).is_some();
    held
}

/// How many multipart read set uploads the service still holds for `sequence_store_id`.
fn stored_uploads_for(engine: &Arc<engine::InMemoryEngine>, sequence_store_id: &str) -> usize {
    let state = service_state(&**engine).expect("the omics service state");
    // Bound for the same reason as above: the guard must be dropped before `state` is.
    let held = state
        .lock()
        .multipart_read_set_uploads_for(sequence_store_id)
        .count();
    held
}

/// Initiate `count` multipart read set uploads against `sequence_store_id`, answering
/// their upload ids.
async fn fill_with_uploads(
    router: &MultiServiceRouter,
    case: &str,
    sequence_store_id: &str,
    count: usize,
) -> Vec<String> {
    let mut ids = Vec::new();
    for n in 0..count {
        let (status, body) = create_upload(
            router,
            sequence_store_id,
            &json!({
                "name": format!("{case}-upload-{n}"),
                "sourceFileType": "FASTQ",
                "subjectId": format!("subject-{n}"),
                "sampleId": format!("sample-{n}"),
            }),
        )
        .await;
        ids.push(created_upload_id(case, status, &body));
    }
    ids
}

/// A row of the delete table: a store to build, how much it holds, and whether the clock
/// has settled its `CREATING` -> `ACTIVE` move before the delete is issued.
struct DeleteCase {
    /// How the row is reported in a failure, and the store's `name`/`clientToken`.
    case: &'static str,
    /// The create body, without the `name`/`clientToken` the row's `case` fills in.
    request: Value,
    /// Multipart read set uploads initiated against the store before it is deleted.
    uploads: usize,
    /// Whether to wait for the store to reach `ACTIVE` before deleting it.
    settled: bool,
}

/// The table: one row per way a store can differ when it is deleted, so the rule is
/// asserted over the class and not over one request. The rows vary along the members the
/// store carries, the number of tags, how much the store holds, and the status the clock
/// has left it in.
fn delete_cases() -> Vec<DeleteCase> {
    vec![
        DeleteCase {
            case: "a bare store, still creating",
            request: json!({}),
            uploads: 0,
            settled: false,
        },
        DeleteCase {
            case: "a bare store, settled to active",
            request: json!({}),
            uploads: 0,
            settled: true,
        },
        DeleteCase {
            case: "every optional member at once",
            request: json!({
                "description": "everything named",
                "sseConfig": { "type": "KMS", "keyArn": KEY_ARN },
                "fallbackLocation": "s3://omics-emulator-fallback/all/",
                "eTagAlgorithmFamily": "SHA512up",
                "propagatedSetLevelTags": ["project", "cohort"],
                "s3AccessConfig": { "accessLogLocation": "s3://omics-emulator-logs/all/" },
                "tags": tag_map(3),
            }),
            uploads: 0,
            settled: true,
        },
        DeleteCase {
            // "When a sequence store is deleted, all tags associated with the store are
            // also deleted": a store at the documented tag bound goes the same way.
            case: "tags at the documented bound",
            request: json!({ "tags": tag_map(MAX_TAGS) }),
            uploads: 0,
            settled: false,
        },
        DeleteCase {
            case: "one upload in flight",
            request: json!({}),
            uploads: 1,
            settled: true,
        },
        DeleteCase {
            case: "several uploads in flight",
            request: json!({}),
            uploads: 3,
            settled: true,
        },
    ]
}

/// The create body for a row: its own members plus the `name` and `clientToken` the row's
/// `case` names it by.
fn request_for(case: &DeleteCase) -> Value {
    let mut body = case.request.as_object().cloned().unwrap_or_default();
    body.insert("name".to_string(), Value::String(case.case.to_string()));
    body.insert(
        "clientToken".to_string(),
        Value::String(format!("delete-{}", case.case)),
    );
    Value::Object(body)
}

#[tokio::test]
async fn a_deleted_sequence_store_and_everything_it_holds_are_reclaimed() {
    let (router, engine) = emulator_holding_its_engine();

    // A bystander store, holding an upload of its own: every row below must leave it, and
    // its upload, exactly as they were — a delete reclaims ONE store.
    let (status, body) = create_store(&router, &json!({ "name": "the bystander" })).await;
    let bystander = created_store_id("the bystander", status, &body);
    let bystander_upload = fill_with_uploads(&router, "the bystander", &bystander, 1).await;

    for row in delete_cases() {
        let case = row.case;
        let (status, body) = create_store(&router, &request_for(&row)).await;
        let id = created_store_id(case, status, &body);
        fill_with_uploads(&router, case, &id, row.uploads).await;
        if row.settled {
            await_store_status(&router, &id, "ACTIVE").await;
        }
        assert_eq!(
            stored_uploads_for(&engine, &id),
            row.uploads,
            "case {case:?}: expected {} stored uploads before the delete, got {}",
            row.uploads,
            stored_uploads_for(&engine, &id)
        );

        let (status, body) = delete_store(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DeleteSequenceStore, got {body:?}"
        );

        // The record really left the service's state, with everything it held.
        assert!(
            !store_is_held(&engine, &id),
            "case {case:?}: sequence store {id} is still in the service's state after its \
             delete"
        );
        assert_eq!(
            stored_uploads_for(&engine, &id),
            0,
            "case {case:?}: expected no upload left for the deleted store {id}, got {}",
            stored_uploads_for(&engine, &id)
        );

        // And every implemented reader of the id now answers the modelled 404.
        let (status, body) = get_store(&router, &id).await;
        assert_store_not_found(case, "GetSequenceStore", status, &body);
        let (status, body) = create_upload(
            &router,
            &id,
            &json!({
                "name": "after the delete",
                "sourceFileType": "FASTQ",
                "subjectId": "subject-z",
                "sampleId": "sample-z",
            }),
        )
        .await;
        assert_store_not_found(case, "CreateMultipartReadSetUpload", status, &body);
        // The bystander is untouched, store and upload alike.
        let body = await_store_status(&router, &bystander, "ACTIVE").await;
        assert_eq!(
            body.get("id").and_then(Value::as_str),
            Some(bystander.as_str()),
            "case {case:?}: the bystander store must still read back, got {body:?}"
        );
        assert_eq!(
            stored_uploads_for(&engine, &bystander),
            bystander_upload.len(),
            "case {case:?}: the bystander store must still hold its {} upload(s), got {}",
            bystander_upload.len(),
            stored_uploads_for(&engine, &bystander)
        );
    }
}

#[tokio::test]
async fn deleting_a_sequence_store_the_registry_does_not_hold_is_not_found() {
    let (router, _engine) = emulator_holding_its_engine();
    // One real store exists, so the 404s below are about the id, not about an empty
    // registry.
    let (status, body) = create_store(&router, &json!({ "name": "the only store" })).await;
    let existing = created_store_id("the only store", status, &body);

    // The rule generalises over the id's width and value, across the whole modelled
    // `SequenceStoreId` length (10..36): anything the registry does not hold is not
    // found, however it is shaped.
    let (min, max) = SEQUENCE_STORE_ID_WIDTH;
    let ids = [
        "0".repeat(min),
        "9".repeat(min),
        "1234567890".to_string(),
        "1".repeat(max),
        "9".repeat(max),
    ];
    for id in &ids {
        assert_ne!(
            id, &existing,
            "the fixture must not collide with the real id"
        );
        let (status, body) = delete_store(&router, id).await;
        assert_store_not_found(id, "DeleteSequenceStore", status, &body);
    }

    // The real store is still there: a refused delete reclaimed nothing.
    let (status, body) = get_store(&router, &existing).await;
    assert_eq!(
        status, 200,
        "a refused delete must leave the store it did not name, got {body:?}"
    );

    // Deleting it twice: the first call reclaims it, so the second names an id the
    // registry no longer holds and is the same 404.
    let (status, body) = delete_store(&router, &existing).await;
    assert_eq!(status, 200, "expected 200 from the first delete, got {body:?}");
    let (status, body) = delete_store(&router, &existing).await;
    assert_store_not_found(
        "a second delete of the same store",
        "DeleteSequenceStore",
        status,
        &body,
    );
}

#[tokio::test]
async fn a_client_token_whose_store_was_deleted_creates_a_new_one() {
    let (router, _engine) = emulator_holding_its_engine();
    // A replay can only answer with a store the service still holds, so once the store a
    // `clientToken` created is reclaimed, that token mints a fresh one. The rule
    // generalises over the token's text.
    for token in ["store-1", "::store-2::", "Ω-store-3"] {
        let body = json!({ "name": "recreated", "clientToken": token });
        let (status, response) = create_store(&router, &body).await;
        let first = created_store_id(token, status, &response);

        let (status, response) = delete_store(&router, &first).await;
        assert_eq!(
            status, 200,
            "case {token:?}: expected 200 from DeleteSequenceStore, got {response:?}"
        );

        let (status, response) = create_store(&router, &body).await;
        let second = created_store_id(token, status, &response);
        assert_ne!(
            first, second,
            "case {token:?}: a token whose store was deleted must mint a new store, \
             got {first} twice"
        );
        let (status, response) = get_store(&router, &second).await;
        assert_eq!(
            status, 200,
            "case {token:?}: the recreated store must read back, got {response:?}"
        );
    }
}
