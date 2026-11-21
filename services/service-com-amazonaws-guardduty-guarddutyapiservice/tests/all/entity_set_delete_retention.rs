// SPDX-License-Identifier: CC0-1.0

//! What repeated create-and-delete CHURN leaves behind on both entity-set families,
//! end to end over the wire: `DeleteThreatEntitySet` and `DeleteTrustedEntitySet`
//! share one deletion helper, so the rule is asserted on each of them.
//!
//! `threat_entity_set_delete.rs` and `trusted_entity_set_delete.rs` own one delete
//! each and `entity_set_delete_observability.rs` the one read rule a served delete
//! leaves behind; this suite owns what MANY of them do. A delete takes the set off the
//! detector and leaves NOTHING on its id, so the rule a caller can observe under churn
//! is that every id the churn deleted is refused with the same absence refusal an id no
//! create ever minted gets, however many cycles ago it was deleted, and no churn ever
//! brings a deleted set back into use or makes the store keep what it no longer serves.
//! Every request goes through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`] (the harness lives in the
//! `detector_lifecycle` suite), so routing, restJson1 (de)serialization and the
//! handlers are exercised together.
//!
//! The rules under test, each asserted over a SET of inputs rather than one call:
//!
//! 1. Every id a delete took off the detector is refused as absent afterwards — for
//!    every cycle of the churn, the oldest as much as the newest — and never reports a
//!    live status again.
//! 2. The listing names exactly the sets that are live: the churn leaves no ghost in
//!    the collection, and the set the last cycle created is readable.
//!
//! The rows differ along the family, the number of cycles and whether a read lands
//! between a delete and the next create.

use serde_json::{json, Value};

use crate::detector_lifecycle::{call, enabled_detector, new_router, Router};
use crate::set_delete_terminality::assert_entity_set_absent;
use service_com_amazonaws_guardduty_guarddutyapiservice::state::{
    ENTITY_SET_INACTIVE, THREAT_ENTITY_SET_ID_MEMBER, TRUSTED_ENTITY_SET_ID_MEMBER,
};

/// Assert one read of an id a delete has taken off the detector: the set is gone, so
/// the read is the entity-set families' absence refusal — the same one an id no create
/// ever minted gets — and never a body reporting a set.
fn assert_never_live_again(case: &str, status: u16, body: &Value, id: &str) {
    assert_entity_set_absent(case, status, body, id);
}

/// One entity-set family: the collection segment of its URI under
/// `/detector/{DetectorId}/`, the member its create reports the minted id in, and the
/// member its listing reports ids in.
struct Family {
    collection: &'static str,
    id_member: &'static str,
    ids_member: &'static str,
}

/// Both families, so neither can drift from the shared deletion rule.
const FAMILIES: [Family; 2] = [
    Family {
        collection: "threatentityset",
        id_member: THREAT_ENTITY_SET_ID_MEMBER,
        ids_member: "threatEntitySetIds",
    },
    Family {
        collection: "trustedentityset",
        id_member: TRUSTED_ENTITY_SET_ID_MEMBER,
        ids_member: "trustedEntitySetIds",
    },
];

/// Create one entity set in `family`, answering the id the create minted.
async fn create_set(
    router: &Router,
    detector_id: &str,
    family: &Family,
    index: usize,
    case: &str,
) -> String {
    let collection = family.collection;
    let (status, created) = call(
        router,
        "POST",
        &format!("/detector/{detector_id}/{collection}"),
        Some(json!({
            "name": format!("churn-{index}"),
            "format": "TXT",
            "location": format!("s3://guardduty-lists/churn-{index}.txt"),
            "activate": false,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create of set {index} to answer 200, got {status} {created:?}"
    );
    created[family.id_member]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a {} from the create of set {index}, got {created:?}",
                family.id_member
            )
        })
        .to_string()
}

/// Read one set of `family` back.
async fn get_set(router: &Router, detector_id: &str, family: &Family, id: &str) -> (u16, Value) {
    let collection = family.collection;
    call(
        router,
        "GET",
        &format!("/detector/{detector_id}/{collection}/{id}"),
        None,
    )
    .await
}

/// Delete one set of `family`, failing the test if the call did not succeed.
async fn delete_set(router: &Router, detector_id: &str, family: &Family, id: &str, case: &str) {
    let collection = family.collection;
    let (status, body) = call(
        router,
        "DELETE",
        &format!("/detector/{detector_id}/{collection}/{id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete of {id} to answer 200, got {status} {body:?}"
    );
}

/// The ids `family`'s listing names for a detector. The whole collection is bounded
/// well under the modelled page size, so it arrives in one page.
async fn listed(router: &Router, detector_id: &str, family: &Family, case: &str) -> Vec<String> {
    let collection = family.collection;
    let (status, got) = call(
        router,
        "GET",
        &format!("/detector/{detector_id}/{collection}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the listing to answer 200, got {status} {got:?}"
    );
    got[family.ids_member]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {} in {got:?}",
                family.ids_member
            )
        })
        .iter()
        .map(|id| id.as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn churn_leaves_no_deleted_entity_set_live() {
    // The rows differ along the number of create-and-delete cycles — one, a few and
    // many — so each row spans a deletion that has just happened and deletions many
    // cycles old.
    for cycles in [1_usize, 4, 9] {
        for family in &FAMILIES {
            let case = format!("{} churned {cycles} times", family.collection);
            let case = case.as_str();
            let router = new_router();
            let detector_id = enabled_detector(&router).await;

            let mut deleted_ids: Vec<String> = Vec::new();
            for index in 0..cycles {
                let id = create_set(&router, &detector_id, family, index, case).await;
                // Every id this cycle minted is live until its delete: one set at a
                // time is enough for both families, the one-at-a-time trusted set
                // included.
                let listing = listed(&router, &detector_id, family, case).await;
                assert_eq!(
                    listing,
                    vec![id.clone()],
                    "case {case:?}: cycle {index} must leave exactly its own set listed"
                );
                delete_set(&router, &detector_id, family, &id, case).await;
                // A read between the delete and the next create: the set is gone, so
                // the read is refused and resurrects nothing.
                if index % 2 == 0 {
                    let (status, body) = get_set(&router, &detector_id, family, &id).await;
                    assert_never_live_again(case, status, &body, &id);
                }
                deleted_ids.push(id);
            }

            // Rule 1: every id the churn deleted is refused the way an id no create
            // ever minted is, the oldest deletion of the row as much as the newest,
            // and whatever the clock has done since.
            for (index, id) in deleted_ids.iter().enumerate() {
                let case = format!("{case}: deletion {index}");
                let (status, body) = get_set(&router, &detector_id, family, id).await;
                assert_never_live_again(&case, status, &body, id);
            }

            // Rule 2: the collection is empty, and a fresh create is served and
            // readable — the churn left the family usable.
            let empty = listed(&router, &detector_id, family, case).await;
            assert!(
                empty.is_empty(),
                "case {case:?}: every set was deleted, so the listing must name none, got \
                 {empty:?}"
            );
            let fresh = create_set(&router, &detector_id, family, cycles, case).await;
            assert!(
                !deleted_ids.contains(&fresh),
                "case {case:?}: a create after the churn must mint an unused id, got {fresh}"
            );
            let (status, body) = get_set(&router, &detector_id, family, &fresh).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fresh set must be readable, got {status} {body:?}"
            );
            assert_eq!(
                body["status"].as_str().unwrap_or_default(),
                ENTITY_SET_INACTIVE,
                "case {case:?}: the fresh set was created unused, got {body:?}"
            );
        }
    }
}
