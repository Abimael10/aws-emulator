// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteExport`, and for the state a delete removes: the
//! export job `CreateExport` recorded, the archive that job assembled, and the address
//! `DescribeExport` announced for it.
//!
//! Every request goes through the same `MultiServiceRouter::dispatch` the emulator serves
//! on its socket — the modelled operations and the archive's own pre-signed address alike —
//! so the routing, the statuses and the error envelopes are exercised exactly as a client
//! sees them. The harness, the bot, the locales and the export helpers are the other
//! suites', reused rather than copied.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one case:
//!
//! * an export whose `resourceSpecification` resolves settles on the status in which the
//!   export is ready to download, reporting no reason for a failure that did not happen and
//!   announcing the address its archive is served at;
//! * a delete of a stored export removes the job together with the archive it handed out
//!   and reports the status of the deletion for the id it removed, so a later read of that
//!   id answers the resource this service does not hold, a listing no longer carries it and
//!   the address stops serving;
//! * a delete is scoped to the one id it names: every other stored export is untouched;
//! * an `exportId` no stored job carries — one nothing was recorded under, and one whose
//!   job was already deleted — is refused with the 412 this service answers a request path
//!   naming a resource it does not hold, naming the identifier that failed.

use serde_json::{json, Value};
use service_com_amazonaws_lexmodelsv2_lexmodelbuildingservicev2::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::bot_lifecycle::{call, emulator, error_code, member, text};
use crate::export_archive::fetch;
use crate::exports::{
    create_export, export_body, export_uri, exportable_bot, is_export_id, list_exports,
    listed_ids, settled_export,
};

/// An `exportId` of the modelled `Id` shape (len 10..10, pattern `^[0-9a-zA-Z]+$`) that no
/// create ever minted: the syntactically valid id of an export that is not there.
const UNRECORDED_EXPORT_ID: &str = "Nx3Vb8Qd2L";

/// One row of the deletion table: what the deleted job exports, and whether it is deleted
/// once its archive is ready or while the job is still in flight.
struct Case {
    case: &'static str,
    /// The `CreateExport` request body.
    request: Value,
    /// Whether the test waits for the job to reach the status in which the archive is
    /// ready before deleting it. A job is deletable in either status, so the table carries
    /// both.
    delete_when_ready: bool,
}

/// The table every assertion runs over: rows differ in the kind of resource exported (a
/// whole bot and one locale of one), in the bot version it is taken from, in the file
/// format (members of the modelled `ImportExportFileFormat` enum,
/// `smithy_query(query="shape",shape="ImportExportFileFormat")`: CSV | LexJson | TSV) and
/// in the status the job is in when it is deleted.
fn cases(bot_id: &str, bot_version: &str) -> Vec<Case> {
    vec![
        Case {
            case: "a whole bot at its draft version, deleted once ready",
            request: export_body(
                json!({ "botExportSpecification": { "botId": bot_id, "botVersion": "DRAFT" } }),
                "LexJson",
                json!({}),
            ),
            delete_when_ready: true,
        },
        Case {
            case: "a whole bot at a numbered version, deleted while still in flight",
            request: export_body(
                json!({
                    "botExportSpecification": { "botId": bot_id, "botVersion": bot_version },
                }),
                "TSV",
                json!({}),
            ),
            delete_when_ready: false,
        },
        Case {
            case: "one locale of the draft, deleted once ready",
            request: export_body(
                json!({
                    "botLocaleExportSpecification": {
                        "botId": bot_id,
                        "botVersion": "DRAFT",
                        "localeId": "en_US",
                    },
                }),
                "CSV",
                json!({}),
            ),
            delete_when_ready: true,
        },
        Case {
            case: "another locale of the draft, deleted once ready",
            request: export_body(
                json!({
                    "botLocaleExportSpecification": {
                        "botId": bot_id,
                        "botVersion": "DRAFT",
                        "localeId": "de_DE",
                    },
                }),
                "LexJson",
                json!({}),
            ),
            delete_when_ready: true,
        },
    ]
}

/// Delete one export and answer the response the delete gave.
async fn delete_export(router: &MultiServiceRouter, export_id: &str) -> (u16, Value) {
    call(router, "DELETE", &export_uri(export_id), None).await
}

/// Assert that `export_id` names nothing this service holds: a read of it answers the
/// resource it does not hold, a delete of it is refused naming that id, and a listing of
/// `bot_id`'s exports does not carry it.
async fn assert_gone(router: &MultiServiceRouter, case: &str, bot_id: &str, export_id: &str) {
    let (status, read) = call(router, "GET", &export_uri(export_id), None).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected a read of the deleted export to answer 404, got {status} \
         {read:?}"
    );
    assert_eq!(
        error_code(&read),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {read:?}"
    );
    assert_eq!(
        text(&read, "message"),
        state::RESOURCE_NOT_FOUND_MESSAGE,
        "case {case:?}: expected the sentence this service states for a resource it does \
         not hold, got {read:?}"
    );

    let (status, refused) = delete_export(router, export_id).await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected a second delete to be refused with 412, got {status} \
         {refused:?}"
    );
    assert_eq!(
        error_code(&refused),
        "PreconditionFailedException",
        "case {case:?}: expected PreconditionFailedException, got {refused:?}"
    );
    assert_eq!(
        text(&refused, "message"),
        format!("Export with id {export_id} in the request path does not exist."),
        "case {case:?}: expected the sentence naming the identifier that did not resolve, \
         got {refused:?}"
    );

    let listed = list_exports(router, &json!({ "botId": bot_id, "maxResults": 100 })).await;
    assert!(
        !listed_ids(&listed).contains(&export_id.to_string()),
        "case {case:?}: a deleted export must not be in a listing of its bot, got {listed:?}"
    );
}

#[tokio::test]
async fn a_deleted_export_is_removed_with_the_archive_it_handed_out() {
    let router = emulator();
    let (bot_id, bot_version) =
        exportable_bot(&router, "ExportDeletionBot", &["en_US", "de_DE"]).await;
    let cases = cases(&bot_id, &bot_version);

    // Every case is submitted first, so the span the clock owes each one runs alongside
    // the others rather than one after another.
    let mut submitted: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        let created = create_export(&router, &case.request).await;
        let export_id = text(&created, "exportId");
        assert!(
            is_export_id(&export_id),
            "case {:?}: expected a 10-character alphanumeric exportId, got {created:?}",
            case.case
        );
        submitted.push((case, export_id));
    }

    // The address each ready job announces, kept so a fetch of it after the delete shows
    // the archive went with the record.
    let mut announced: Vec<(&str, String)> = Vec::new();
    for (case, export_id) in &submitted {
        if !case.delete_when_ready {
            // A job is deletable in whatever status it is in, so this row deletes one that
            // was not waited for. Which status it is in by then is the clock's business —
            // the span may already have fallen due — so the read below accepts either of
            // the two a recorded job can be in and the row's point is that the delete does
            // not depend on it.
            let (status, in_flight) = call(&router, "GET", &export_uri(export_id), None).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected DescribeExport to answer 200, got {status} {in_flight:?}",
                case.case
            );
            let reported = text(&in_flight, "exportStatus");
            assert!(
                reported == state::EXPORT_STATUS_IN_PROGRESS
                    || reported == state::EXPORT_STATUS_COMPLETED,
                "case {:?}: a recorded job is in progress or ready, got {reported:?} in \
                 {in_flight:?}",
                case.case
            );
            continue;
        }
        let ready = settled_export(&router, case.case, export_id).await;
        assert_eq!(
            text(&ready, "exportStatus"),
            state::EXPORT_STATUS_COMPLETED,
            "case {:?}: a job whose archive was assembled must report the status in which \
             the export is ready to download, got {ready:?}",
            case.case
        );
        assert_eq!(
            member(&ready, "failureReasons"),
            Value::Null,
            "case {:?}: a job that did not fail reports no failure reasons, got {ready:?}",
            case.case
        );
        let url = text(&ready, "downloadUrl");
        assert!(
            url.starts_with("https://"),
            "case {:?}: a ready job must announce the address its archive is served at, \
             got {ready:?}",
            case.case
        );
        let fetched = fetch(&router, &url).await;
        assert_eq!(
            fetched.status, 200,
            "case {:?}: expected the announced address to serve the archive, got {} {:?}",
            case.case,
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        assert_eq!(
            fetched.media_type,
            state::EXPORT_ARCHIVE_MEDIA_TYPE,
            "case {:?}: expected the archive to be announced as {}, got {:?}",
            case.case,
            state::EXPORT_ARCHIVE_MEDIA_TYPE,
            fetched.media_type
        );
        announced.push((case.case, url));
    }

    // Each delete removes the job it names, reports the status of the deletion for that
    // id, and leaves every job it did not name exactly where it was.
    for index in 0..submitted.len() {
        let (case, export_id) = &submitted[index];
        let (status, deleted) = delete_export(&router, export_id).await;
        assert_eq!(
            status, 202,
            "case {:?}: expected DeleteExport to answer 202, got {status} {deleted:?}",
            case.case
        );
        assert_eq!(
            text(&deleted, "exportId"),
            *export_id,
            "case {:?}: DeleteExport must report the export it removed, got {deleted:?}",
            case.case
        );
        assert_eq!(
            text(&deleted, "exportStatus"),
            state::EXPORT_STATUS_DELETING,
            "case {:?}: DeleteExport must report the status of the deletion, got {deleted:?}",
            case.case
        );

        assert_gone(&router, case.case, &bot_id, export_id).await;

        // The delete was scoped to the one id it named: every job submitted after it is
        // still readable under its own id.
        for (other, other_id) in submitted.iter().skip(index + 1) {
            let (status, read) = call(&router, "GET", &export_uri(other_id), None).await;
            assert_eq!(
                status, 200,
                "case {:?}: deleting {export_id} must leave {other_id} ({:?}) readable, got \
                 {status} {read:?}",
                case.case, other.case
            );
        }
    }

    // The archive went with the record, so the address a ready job announced serves
    // nothing once that job is gone.
    for (case, url) in &announced {
        let fetched = fetch(&router, url).await;
        assert_eq!(
            fetched.status, 404,
            "case {case:?}: the address of a deleted export's archive must serve nothing, \
             got {} {:?}",
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
    }

    // Nothing of this bot is left to list.
    let listed = list_exports(&router, &json!({ "botId": bot_id, "maxResults": 100 })).await;
    assert_eq!(
        listed_ids(&listed),
        Vec::<String>::new(),
        "every export of the bot was deleted, so a listing of it is empty, got {listed:?}"
    );
}

#[tokio::test]
async fn an_export_id_no_export_carries_cannot_be_deleted() {
    let router = emulator();
    let (bot_id, _) = exportable_bot(&router, "ExportDeletionRefusalBot", &["en_US"]).await;

    // A create whose specification resolves is recorded under its own id, so the id of a
    // job that is not there stays unresolvable however many jobs the store holds.
    let created = create_export(
        &router,
        &export_body(
            json!({ "botExportSpecification": { "botId": &bot_id, "botVersion": "DRAFT" } }),
            "LexJson",
            json!({}),
        ),
    )
    .await;
    let recorded = text(&created, "exportId");

    // Rows differ in how the id is formed, inside the one shape the model admits
    // (`Id`, len 10..10, pattern `^[0-9a-zA-Z]+$`): an id of mixed case, one of digits
    // alone, one of letters alone, and one a single character away from the id a create
    // did mint.
    let rows: Vec<(&str, String)> = vec![
        (
            "an id nothing was ever recorded under",
            UNRECORDED_EXPORT_ID.to_string(),
        ),
        ("an id of digits alone", "1029384756".to_string()),
        ("an id of letters alone", "AbCdEfGhIj".to_string()),
        (
            "an id differing from a recorded one in its last character",
            format!(
                "{}{}",
                &recorded[..recorded.len() - 1],
                if recorded.ends_with('0') { '1' } else { '0' }
            ),
        ),
    ];
    for (case, export_id) in rows {
        if export_id == recorded {
            continue;
        }
        let (status, refused) = delete_export(&router, &export_id).await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected DeleteExport to refuse with 412, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "PreconditionFailedException",
            "case {case:?}: expected PreconditionFailedException, got {refused:?}"
        );
        assert_eq!(
            text(&refused, "message"),
            format!("Export with id {export_id} in the request path does not exist."),
            "case {case:?}: expected the sentence naming the identifier that did not \
             resolve, got {refused:?}"
        );
    }

    // No refusal touched the job that IS there.
    let (status, read) = call(&router, "GET", &export_uri(&recorded), None).await;
    assert_eq!(
        status, 200,
        "a refused delete must leave the recorded export readable, got {status} {read:?}"
    );
}
