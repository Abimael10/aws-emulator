// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for an application data source that ANOTHER service holds: an Amazon
//! OpenSearch Serverless COLLECTION, named by `AttachDataSource`'s `dataSourceArn` and
//! resolved in the store `aoss:CreateCollection` writes.
//!
//! Both services are mounted on ONE shared engine ([`Emulator::with_collections`]), so the
//! collection a case attaches is created through OpenSearch Serverless' own API — the way a
//! client creates one — rather than written into this service's store by hand. Every call
//! goes over the real wire path, so what is asserted is what a client sees.
//!
//! The rules under test:
//!
//! * a collection is a data source an application can attach: the attach is ACCEPTED and the
//!   attachment converges to `ATTACHED` once the collection is active, which both reads then
//!   report — over rows differing along where the collection stands when the attach arrives
//!   (already active, still being created) and along the ARN's partition;
//! * an ARN of that kind the serverless store holds no collection under names a data source
//!   that never becomes active, so the attachment the clock lands is the non-retryable
//!   `FAILED` — which is also what a collection whose deletion has been accepted leaves
//!   behind;
//! * removing a data source from an application is work the service does, so a SECOND detach
//!   of the same attachment arriving while the first is still in flight is refused the
//!   modelled `ConflictException`, while the data source is detached as far as every read is
//!   concerned and can be attached again.

use serde_json::json;
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    ATTACHMENT_STATUS_ATTACHED, ATTACHMENT_STATUS_FAILED,
};
use service_com_amazonaws_opensearchserverless_opensearchserverless::state::COLLECTION_ACTIVE;

use super::data_source_attachments::{application, attach, describe_attachment, detach};
use super::domain_lifecycle::{error_code, poll_until, string_member, Emulator, REGION};

/// The `encryption` security policy a collection cannot be created without: OpenSearch
/// Serverless requires one covering the collection's name before `CreateCollection` is
/// served, so this suite creates one covering every collection it makes — the shape AWS's
/// "Encryption policies for Amazon OpenSearch Serverless" documents.
///
/// `pub(crate)` because the sibling suite that drives a collection all the way through to a
/// migration ([`super::collection_backed_migration_flow`]) needs the same precondition rather
/// than a second copy of it.
pub(crate) async fn created_encryption_policy(emulator: &Emulator) {
    let policy = json!({
        "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
        "AWSOwnedKey": true,
    });
    let answer = emulator
        .collections_call(
            "CreateSecurityPolicy",
            json!({
                "type": "encryption",
                "name": "attachment-probe-encryption",
                "policy": policy.to_string(),
            }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case CreateSecurityPolicy(encryption): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
}

/// `aoss:CreateCollection` for `name`, through OpenSearch Serverless' own API, answering the
/// ARN of the collection it minted.
///
/// `collection_type` names the collection's `type` where a case is about one kind of
/// collection, and is left out otherwise — a request naming none takes that service's own
/// default. `pub(crate)` for the same reason as [`created_encryption_policy`].
pub(crate) async fn created_collection(
    emulator: &Emulator,
    name: &str,
    collection_type: Option<&str>,
) -> String {
    let mut body = json!({ "name": name });
    if let Some(collection_type) = collection_type {
        body["type"] = json!(collection_type);
    }
    let answer = emulator.collections_call("CreateCollection", body).await;
    assert_eq!(
        answer.status, 200,
        "case CreateCollection({name:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body["createCollectionDetail"], "arn", name)
}

/// `aoss:DeleteCollection` for the collection `id`, so a case can name a collection whose
/// deletion the serverless service has accepted.
async fn deleted_collection(emulator: &Emulator, arn: &str) {
    let id = arn
        .rsplit_once('/')
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| panic!("case DeleteCollection({arn:?}): the ARN names a collection id"));
    let answer = emulator
        .collections_call("DeleteCollection", json!({ "id": id }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case DeleteCollection({id:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
}

/// The status the attach of `data_source_arn` answered with, asserting it was accepted.
///
/// `pub(crate)` for the same reason as [`created_encryption_policy`].
pub(crate) async fn attached_status(
    emulator: &Emulator,
    id: &str,
    data_source_arn: &str,
    case: &str,
) -> String {
    let answer = attach(emulator, id, json!({ "dataSourceArn": data_source_arn })).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from AttachDataSource, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["dataSourceArn"],
        json!(data_source_arn),
        "case {case}: the attach reports the data source it was given, got {:?}",
        answer.body
    );
    string_member(&answer.body, "status", case)
}

/// The status the single-attachment read reports for `data_source_arn` right now.
async fn read_status(emulator: &Emulator, id: &str, data_source_arn: &str, case: &str) -> String {
    let answer = describe_attachment(emulator, id, data_source_arn).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from DescribeDataSourceAttachment, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "status", case)
}

/// The TERMINAL status the attachment of `data_source_arn` settles in: the read is repeated
/// until the status the clock owes the record has landed, so nothing here waits a fixed time.
///
/// `pub(crate)` for the same reason as [`created_encryption_policy`].
pub(crate) async fn settled_status(
    emulator: &Emulator,
    id: &str,
    data_source_arn: &str,
    case: &str,
) -> String {
    poll_until(case, || async {
        let status = read_status(emulator, id, data_source_arn, case).await;
        status == ATTACHMENT_STATUS_ATTACHED || status == ATTACHMENT_STATUS_FAILED
    })
    .await;
    read_status(emulator, id, data_source_arn, case).await
}

/// Look again until the collection `name` reports ACTIVE through its OWN service, the way a
/// client watches a collection come up: the CONDITION is polled for rather than waited out,
/// so nothing here depends on how fast the host runs.
///
/// `pub(crate)` for the same reason as [`created_encryption_policy`].
pub(crate) async fn collection_became_active(emulator: &Emulator, name: &str, case: &str) {
    poll_until(case, || async {
        let answer = emulator
            .collections_call("BatchGetCollection", json!({ "names": [name] }))
            .await;
        // The read itself has to be served: a collection that is merely not active yet is what
        // this waits for, so a read that failed is reported as the failure it is rather than
        // waited out to the deadline.
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200 from BatchGetCollection({name:?}), got {} with {:?}",
            answer.status, answer.body
        );
        answer.body["collectionDetails"][0]["status"] == json!(COLLECTION_ACTIVE)
    })
    .await;
}

/// One attach to make: where the collection it names stands, and the status the clock must
/// leave the attachment in.
struct Row {
    case: &'static str,
    /// Whether the collection the attach names is one the serverless store holds.
    held: bool,
    /// Whether the case waits for the collection to be active before attaching.
    active_first: bool,
    /// Whether the collection's deletion is accepted before the attach.
    deleted_first: bool,
    settles_in: &'static str,
}

/// RULE: a collection of this account is a data source an application attaches, and the
/// attachment converges to ATTACHED once that collection is active; an ARN of the same kind
/// the serverless store holds no live collection under names a data source that never
/// becomes active, so the attachment settles FAILED.
#[tokio::test]
async fn an_attachment_of_a_serverless_collection_follows_that_collection() {
    let emulator = Emulator::with_collections();
    created_encryption_policy(&emulator).await;
    let id = application(&emulator, "collections-ui").await;
    let account = ::engine::emulator_account_id();

    let rows = [
        Row {
            case: "a collection that is already active",
            held: true,
            active_first: true,
            deleted_first: false,
            settles_in: ATTACHMENT_STATUS_ATTACHED,
        },
        Row {
            case: "a collection that is still being created",
            held: true,
            active_first: false,
            deleted_first: false,
            settles_in: ATTACHMENT_STATUS_ATTACHED,
        },
        Row {
            case: "a collection whose deletion has been accepted",
            held: true,
            active_first: true,
            deleted_first: true,
            settles_in: ATTACHMENT_STATUS_FAILED,
        },
        Row {
            case: "an ARN naming no collection of this account",
            held: false,
            active_first: false,
            deleted_first: false,
            settles_in: ATTACHMENT_STATUS_FAILED,
        },
    ];

    for (at, row) in rows.iter().enumerate() {
        let name = format!("probe-collection-{at}");
        let arn = if row.held {
            created_collection(&emulator, &name, None).await
        } else {
            format!("arn:aws:aoss:{REGION}:{account}:collection/{name}")
        };
        if row.active_first {
            collection_became_active(&emulator, &name, row.case).await;
        }
        if row.deleted_first {
            deleted_collection(&emulator, &arn).await;
        }

        let answered = attached_status(&emulator, &id, &arn, row.case).await;
        assert!(
            answered == ATTACHMENT_STATUS_ATTACHED || answered == "PENDING",
            "case {}: expected the attach to answer PENDING or ATTACHED, got {answered:?}",
            row.case
        );
        let settled = settled_status(&emulator, &id, &arn, row.case).await;
        assert_eq!(
            settled, row.settles_in,
            "case {}: expected the attachment to settle in {}, got {settled:?}",
            row.case, row.settles_in
        );
    }
}

/// RULE: removing a data source from an application is work the service does, so a second
/// detach of the same attachment arriving while the first is still in flight is refused the
/// modelled `ConflictException` — while every read already reports the data source as
/// detached, and attaching it again is a fresh attachment rather than a conflict.
#[tokio::test]
async fn a_second_detach_of_one_attachment_meets_the_removal_already_in_flight() {
    let emulator = Emulator::with_collections();
    created_encryption_policy(&emulator).await;
    let id = application(&emulator, "collections-detach").await;
    let arn = created_collection(&emulator, "detach-race-collection", None).await;

    let attached = attached_status(&emulator, &id, &arn, "the attachment to detach").await;
    assert!(
        attached == ATTACHMENT_STATUS_ATTACHED || attached == "PENDING",
        "case the attachment to detach: expected the attach to be accepted, got {attached:?}"
    );
    let settled = settled_status(&emulator, &id, &arn, "the attachment to detach").await;
    assert_eq!(
        settled, ATTACHMENT_STATUS_ATTACHED,
        "case the attachment to detach: expected it to be ATTACHED before the detaches, got \
         {settled:?}"
    );

    let first = detach(&emulator, &id, &arn).await;
    assert_eq!(
        first.status, 200,
        "case the detach that wins: expected 200, got {} with {:?}",
        first.status, first.body
    );

    // Every read already reports the data source as detached...
    let read = describe_attachment(&emulator, &id, &arn).await;
    assert_eq!(
        error_code(&read),
        "ResourceNotFoundException",
        "case a read after the detach: expected the attachment to be gone, got {:?}",
        read.body
    );

    // ... while a second detach of it meets the removal in flight.
    let loser = detach(&emulator, &id, &arn).await;
    assert_eq!(
        loser.status, 409,
        "case the detach that loses: expected 409, got {} with {:?}",
        loser.status, loser.body
    );
    assert_eq!(
        error_code(&loser),
        "ConflictException",
        "case the detach that loses: expected the modelled conflict, got {:?}",
        loser.body
    );

    // And the data source can be attached again: the detach took the attachment with it, so
    // this is a fresh attach rather than a repeat of one still held.
    let again = attached_status(&emulator, &id, &arn, "attached again after the detach").await;
    assert!(
        again == ATTACHMENT_STATUS_ATTACHED || again == "PENDING",
        "case attached again after the detach: expected the attach to be accepted, got {again:?}"
    );
    let settled = settled_status(&emulator, &id, &arn, "attached again after the detach").await;
    assert_eq!(
        settled, ATTACHMENT_STATUS_ATTACHED,
        "case attached again after the detach: expected it to converge again, got {settled:?}"
    );
}
