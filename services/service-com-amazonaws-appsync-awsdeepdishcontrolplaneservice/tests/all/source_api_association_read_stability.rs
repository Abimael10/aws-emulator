// SPDX-License-Identifier: CC0-1.0

//! A settled source-API association reports the SAME merge outcome to every later
//! caller, end to end over HTTP.
//!
//! The rule asserted here: reading an association reports what is stored — the only
//! write a read makes is the merge the CLOCK already made due, which was owed at that
//! instant whether or not anybody called. So once the merge has been performed, every
//! later read of the association answers the same `sourceApiAssociationStatus`, the same
//! `sourceApiAssociationStatusDetail` and the same `lastSuccessfulMergeDate`, and no read
//! of the Merged API's merged types moves any of them: polling an association can neither
//! re-run its merge nor re-date one.
//!
//! The reads are driven over a class rather than one call: the association is read
//! repeatedly, and between the reads the merged types the merge installed are read too —
//! a different reader of the same state. The Merged API is addressed by both forms
//! `mergedApiIdentifier` accepts (its `apiId` and its ARN, which an SDK percent-encodes
//! into the one path segment the label binds), so neither form reports a different merge.

use serde_json::json;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::resource_tags::encode_label;
use crate::schema_creation::{await_creation, get_type, start_creation};
use crate::source_api_association::{
    await_terminal_merge, call, create_api, read_association, router,
};

/// The source API's schema, whose types the merge carries onto the Merged API.
const SOURCE_SCHEMA: &str = "type Query {\n  orders: Order\n}\ntype Order {\n  id: ID!\n}\n";

#[tokio::test]
async fn reading_a_settled_association_reports_the_same_merge_to_every_caller() {
    let router = router();
    let source = create_api(&router, "read-stability-source", state::GRAPHQL_API_TYPE).await;
    let merged = create_api(&router, "read-stability-merged", state::MERGED_API_TYPE).await;

    let started = start_creation(&router, &source.id, SOURCE_SCHEMA.as_bytes()).await;
    assert_eq!(
        started.status, 200,
        "case fixture: expected StartSchemaCreation 200, got {} {:?}",
        started.status, started.body
    );
    await_creation(&router, &source.id, "fixture").await;

    let created = call(
        &router,
        "POST",
        &format!("/v1/mergedApis/{}/sourceApiAssociations", merged.id),
        Some(json!({"sourceApiIdentifier": source.id})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case fixture: expected the association to be created, got {} {:?}",
        created.status, created.body
    );
    let association_id = created.body["sourceApiAssociation"]["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Wait for the merge the create scheduled — on the CONDITION, not on a fixed time.
    let settled = await_terminal_merge(&router, "fixture", &merged.id, &association_id).await;

    // Every later read answers the same record, whichever form names the Merged API, and
    // the readers in between — the merged types the merge installed — move none of it.
    for round in 1..=3 {
        let merged_type = get_type(&router, &merged.id, "Query", state::SDL_FORMAT).await;
        assert_eq!(
            merged_type.status, 200,
            "case round {round}: expected the merged API to hold the merged type, got {} {:?}",
            merged_type.status, merged_type.body
        );
        for form in [merged.id.clone(), encode_label(&merged.arn)] {
            let again = read_association(&router, "read again", &form, &association_id).await;
            assert_eq!(
                again, settled,
                "case round {round} addressed by {form}: expected the settled association \
                 {settled:?}, got {again:?}"
            );
        }
    }
}
