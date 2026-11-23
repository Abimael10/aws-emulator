// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the flow a saved-object migration out of an Amazon OpenSearch
//! Serverless COLLECTION is reached through: `aoss:CreateCollection`, `AttachDataSource` of
//! the ARN that create minted, and then `StartMigration` out of that same ARN, with
//! `GetMigration` and `ListMigrations` reporting the job it started.
//!
//! Both services are mounted on ONE shared engine ([`Emulator::with_collections`]), so every
//! collection a case names is created through OpenSearch Serverless' own API — the way a
//! client creates one — rather than written into this service's store by hand, and every call
//! goes over the real wire path, so what is asserted is what a client sees.
//!
//! The rules under test:
//!
//! * a collection of the caller's own account is a KIND of resource an application attaches as
//!   a data source, so the attach is ACCEPTED whatever the collection's own provisioning state
//!   and the attachment converges to `ATTACHED`, after which the collection is a data source
//!   OF the application — the set `StartMigration` resolves `migrationOptions.source` against
//!   — so the whole migration surface behind it is reachable. The rows differ along the
//!   collection's published type and along where the collection stands when the attach arrives
//!   (already active, still being created);
//! * that resolution is backed by the ATTACHMENT rather than by the ARN's value, so a start
//!   out of the same collection after it has been detached is the modelled missing-resource
//!   refusal.
//!
//! The harness comes from [`super::domain_lifecycle`], the collection and attachment helpers
//! from [`super::data_source_collection_attachments`] and [`super::data_source_attachments`],
//! and the migration calls from [`super::saved_object_migrations`] and
//! [`super::migration_listing`] — the start, the two reads and the listing's own reading of
//! the jobs it reports; this suite adds no second copy of any of them.

use serde_json::json;
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    ATTACHMENT_STATUS_ATTACHED, ATTACHMENT_STATUS_PENDING, MIGRATION_STATUS_PENDING,
};
use service_com_amazonaws_opensearchserverless_opensearchserverless::ops::types::COLLECTION_TYPE_VALUES;

use super::data_source_attachments::detach;
use super::data_source_collection_attachments::{
    attached_status, collection_became_active, created_collection, created_encryption_policy,
    settled_status,
};
use super::domain_lifecycle::{error_code, string_member, Emulator, RECORDED_NOT_FOUND_STATUS};
use super::migration_listing::{list_migrations, listed_ids};
use super::saved_object_migrations::{active_application, get_migration, start_from};

/// The `migrationId`s the application `id` holds, as `ListMigrations` reports them.
///
/// Asserted to be the WHOLE set: the listing is read without a page size, so an answer
/// carrying a continuation token would mean the jobs this reports are only part of what the
/// application holds, which no caller of this could tell from a short page.
async fn held_jobs(emulator: &Emulator, id: &str, case: &str) -> Vec<String> {
    let answer = list_migrations(emulator, id, "").await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from ListMigrations, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.body["nextToken"].is_null(),
        "case {case}: expected the listing to report every job the application holds on one \
         page, got {:?}",
        answer.body
    );
    listed_ids(&answer, case)
}

/// RULE: an OpenSearch Serverless collection of the caller's own account is a data source an
/// application attaches — the attach is accepted and the attachment converges to ATTACHED once
/// the collection is active — and the attached collection is then a data source OF that
/// application, which is what a start resolves its source against, so the job it mints is
/// reported by both reads of the migration surface.
#[tokio::test]
async fn a_collection_attached_to_an_application_is_a_migration_source_it_holds() {
    let emulator = Emulator::with_collections();
    created_encryption_policy(&emulator).await;
    let id = active_application(&emulator, "collectionflow").await;

    // One row per PUBLISHED collection type — OpenSearch Serverless' own `CollectionType`
    // value set (`smithy_query(query="shape",shape="CollectionType")` of that service's
    // model), reached as the constant that service publishes rather than a list written here.
    // The first row attaches a collection that has already become active and every other one a
    // collection still being created, so the attach is asserted on both sides of the
    // collection's own provisioning.
    let mut started: Vec<(String, String)> = Vec::new();
    for (at, collection_type) in COLLECTION_TYPE_VALUES.iter().copied().enumerate() {
        let waited = if at == 0 {
            "once it is active"
        } else {
            "while it is still being created"
        };
        let case = format!("a {collection_type} collection, attached {waited}");
        let name = format!("flow-{}", collection_type.to_lowercase());
        let arn = created_collection(&emulator, &name, Some(collection_type)).await;
        if at == 0 {
            collection_became_active(&emulator, &name, &case).await;
        }

        // The attach is accepted: whether the collection is up yet is the attachment's own
        // status to carry, never a verdict on the ARN. "If both the application and the data
        // source are active, the attachment completes immediately with a status of ATTACHED",
        // so the row whose collection has already become active — against an application that
        // is active too — is held to exactly that, and only a row attaching a collection that
        // is still being created may answer PENDING.
        let answered = attached_status(&emulator, &id, &arn, &case).await;
        if at == 0 {
            assert_eq!(
                answered, ATTACHMENT_STATUS_ATTACHED,
                "case {case}: expected the attach of an active collection to an active \
                 application to complete at once, got {answered:?}"
            );
        } else {
            assert!(
                answered == ATTACHMENT_STATUS_ATTACHED || answered == ATTACHMENT_STATUS_PENDING,
                "case {case}: expected the attach to answer {ATTACHMENT_STATUS_PENDING} or \
                 {ATTACHMENT_STATUS_ATTACHED}, got {answered:?}"
            );
        }
        let settled = settled_status(&emulator, &id, &arn, &case).await;
        assert_eq!(
            settled, ATTACHMENT_STATUS_ATTACHED,
            "case {case}: expected the attachment of a collection this account holds to \
             converge to {ATTACHMENT_STATUS_ATTACHED}, got {settled:?}"
        );

        // ... and the attached collection is one of the application's data sources, so a start
        // out of that same ARN is served rather than refused.
        let answer = start_from(&emulator, &id, &arn).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200 from StartMigration out of the attached collection, got \
             {} with {:?}",
            answer.status, answer.body
        );
        let migration_id = string_member(&answer.body, "migrationId", &case);
        assert_eq!(
            answer.body["status"],
            json!(MIGRATION_STATUS_PENDING),
            "case {case}: a job is {MIGRATION_STATUS_PENDING} when it is first started, got {:?}",
            answer.body
        );

        // The read round-trips the job: the source it reports is the collection the start
        // named, and the application is the one it was started for.
        let read = get_migration(&emulator, &migration_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetMigration({migration_id}), got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["source"]["datasourceArn"],
            json!(arn),
            "case {case}: expected the job to report the collection it migrates out of, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["applicationId"],
            json!(id),
            "case {case}: expected the job to report the application it migrates into, got {:?}",
            read.body
        );
        started.push((case, migration_id));
    }

    // Every job the rows started is one the application holds, so nothing behind the
    // attachment is unreachable.
    let listed = held_jobs(&emulator, &id, "the jobs the rows started").await;
    for (case, migration_id) in &started {
        assert!(
            listed.contains(migration_id),
            "case {case}: expected the listing to report the job {migration_id}, got {listed:?}"
        );
    }
}

/// RULE: a start resolves its source against the data sources the application HOLDS, so the
/// same collection ARN that was served while it was attached names nothing to export from once
/// it has been detached — the modelled missing-resource refusal, and no job. The sentence that
/// refusal states is [`super::saved_object_migrations`]'s to assert; what is asserted here is
/// that the resolution follows the attachment rather than the ARN's value.
#[tokio::test]
async fn a_start_out_of_a_collection_the_application_has_detached_is_refused() {
    let emulator = Emulator::with_collections();
    created_encryption_policy(&emulator).await;
    let id = active_application(&emulator, "collectiondetached").await;
    let case = "a collection attached and then detached";

    let arn = created_collection(&emulator, "flow-detached", None).await;
    let answered = attached_status(&emulator, &id, &arn, case).await;
    assert!(
        answered == ATTACHMENT_STATUS_ATTACHED || answered == ATTACHMENT_STATUS_PENDING,
        "case {case}: expected the attach to be accepted, got {answered:?}"
    );
    let settled = settled_status(&emulator, &id, &arn, case).await;
    assert_eq!(
        settled, ATTACHMENT_STATUS_ATTACHED,
        "case {case}: expected the attachment to converge to {ATTACHMENT_STATUS_ATTACHED} before \
         the detach, got {settled:?}"
    );

    // While it is attached, the start is served...
    let served = start_from(&emulator, &id, &arn).await;
    assert_eq!(
        served.status, 200,
        "case {case}: expected 200 from StartMigration while the collection is attached, got {} \
         with {:?}",
        served.status, served.body
    );
    let served_id = string_member(&served.body, "migrationId", case);

    let removed = detach(&emulator, &id, &arn).await;
    assert_eq!(
        removed.status, 200,
        "case {case}: expected 200 from DetachDataSource, got {} with {:?}",
        removed.status, removed.body
    );

    // ... and once it is not, the very same request names no data source of the application.
    let refused = start_from(&emulator, &id, &arn).await;
    assert_eq!(
        (refused.status, error_code(&refused).as_str()),
        (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
        "case {case}: expected {RECORDED_NOT_FOUND_STATUS} ResourceNotFoundException after the \
         detach, got {} with {:?}",
        refused.status,
        refused.body
    );

    // The refusal minted nothing: the application still holds only the job started before the
    // detach.
    let listed = held_jobs(&emulator, &id, case).await;
    assert_eq!(
        listed,
        vec![served_id],
        "case {case}: expected the refused start to mint no job, got {listed:?}"
    );
}
