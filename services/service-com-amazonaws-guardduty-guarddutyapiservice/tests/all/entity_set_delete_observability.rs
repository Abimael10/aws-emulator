// SPDX-License-Identifier: CC0-1.0

//! What a client that DELETES an entity set and then reads it back observes, on both
//! entity-set families, end to end over the wire.
//!
//! `threat_entity_set_delete.rs` and `trusted_entity_set_delete.rs` own one family's
//! delete each; this suite owns the one rule both reads follow once a delete has been
//! served, and what repeated create-and-delete CHURN leaves behind.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rules under test, asserted over a TABLE spanning both families and everything
//! the rule generalises over — the status the set was in when the delete arrived
//! (including the transitional one), the `*EntitySetFormat` the create stored, whether
//! the caller activated the set, whether it carried an `ExpectedBucketOwner`, the
//! number of tags, the size of the name and the number of sets deleted under one
//! detector:
//!
//! 1. A served delete leaves NOTHING on the id it named: the set is gone, so the
//!    family's own read refuses that id exactly as it refuses one no create ever
//!    minted — HTTP 404 under the `ResourceNotFoundException` class with the
//!    entity-set families' fixed sentence — from the very next read on, and on every
//!    read after that. A client that deletes and then confirms the deletion by
//!    reading the id reaches its not-found branch at once, with no dependence on how
//!    long it waits.
//! 2. The refusal is a LOOKUP over the store, not a blanket answer: a live sibling of
//!    the same family under the same detector still reads back 200 while the deleted
//!    id is refused, and the listing names exactly the live sets.
//!
//! `entity_set_delete_retention.rs` owns what repeated create-and-delete CHURN leaves
//! behind, and `entity_set_delete_absence.rs` the other ways an entity-set id fails to
//! resolve.

use serde_json::{json, Value};

use crate::detector_lifecycle::{call, enabled_detector, new_router, Router};
use crate::set_delete_terminality::{assert_entity_set_absent, ABSENT_SET_ID};
use service_com_amazonaws_guardduty_guarddutyapiservice::state::{
    ENTITY_SET_ACTIVE, ENTITY_SET_INACTIVE,
};

/// How long a poll may wait for a status the clock owes a LIVE set before the test
/// gives up. Generous on purpose: the wait ends on the CONDITION, and this is only the
/// deadline past which the emulator is considered broken.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The gap between two reads of a poll. Only the pause between attempts; no loop
/// below ever ends on it.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// How many times a read of a deleted id is repeated to show the refusal is not a
/// phase the caller can wait out: every one of them answers the same way.
const REPEATED_READS: usize = 3;

/// One entity-set family: the collection segment under `/detector/{DetectorId}`, the
/// member its create reports the minted id in and the member its listing names ids
/// in.
struct Family {
    case: &'static str,
    segment: &'static str,
    id_member: &'static str,
    ids_member: &'static str,
}

/// Both families, so neither can drift from the shared rule.
const FAMILIES: [Family; 2] = [
    Family {
        case: "threat entity set",
        segment: "threatentityset",
        id_member: "threatEntitySetId",
        ids_member: "threatEntitySetIds",
    },
    Family {
        case: "trusted entity set",
        segment: "trustedentityset",
        id_member: "trustedEntitySetId",
        ids_member: "trustedEntitySetIds",
    },
];

/// One case: the create body, and whether the set's own transition is waited out
/// before the delete arrives.
struct Row {
    case: &'static str,
    create: Value,
    /// Whether to wait for the create's transition to settle before deleting, so the
    /// delete arrives at a settled set in one row and mid-transition in another.
    settle_first: bool,
}

/// The rows differ along what the rule generalises over: the status the set held when
/// the delete arrived, the FORMAT (members of `ThreatEntitySetFormat`, which the
/// trusted family models identically — `smithy_query(shape="ThreatEntitySetFormat")`
/// lists TXT, STIX, OTX_CSV, ALIEN_VAULT, PROOF_POINT and FIRE_EYE), the S3 URL form
/// of the location, whether the set was activated, whether it recorded an expected
/// bucket owner, the number of tags and the size of the name (both ends of the
/// modelled `len 1..300`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an ACTIVE set, settled before the delete",
            create: json!({
                "name": "deletion_probe-1",
                "format": "TXT",
                "location": "s3://guardduty-lists/probe.txt",
                "activate": true,
                "tags": {"team": "secops"},
            }),
            settle_first: true,
        },
        Row {
            case: "an ACTIVATING set, deleted mid-transition",
            create: json!({
                "name": "a",
                "format": "OTX_CSV",
                "location": "https://guardduty-lists.s3.us-west-2.amazonaws.com/otx.csv",
                "activate": true,
            }),
            settle_first: false,
        },
        Row {
            case: "an INACTIVE set the caller never activated",
            create: json!({
                "name": "f".repeat(300),
                "format": "FIRE_EYE",
                "location": "https://s3.amazonaws.com/guardduty-lists/deep/nested/fire-eye.txt",
                "activate": false,
                "expectedBucketOwner": "000000000000",
                "tags": {"team": "secops", "env": "test"},
            }),
            settle_first: true,
        },
        Row {
            case: "a STIX set with an expected bucket owner",
            create: json!({
                "name": "STIX_set-2",
                "format": "STIX",
                "location": "s3://guardduty-lists/threat.stix",
                "activate": false,
                "expectedBucketOwner": "123456789012",
            }),
            settle_first: false,
        },
    ]
}

impl Family {
    async fn create(&self, router: &Router, detector_id: &str, body: Value, case: &str) -> String {
        let (status, got) = call(
            router,
            "POST",
            &format!("/detector/{detector_id}/{}", self.segment),
            Some(body),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to answer 200, got {status} {got:?}"
        );
        got[self.id_member]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a {} from the create, got {got:?}",
                    self.id_member
                )
            })
            .to_string()
    }

    async fn get(&self, router: &Router, detector_id: &str, set_id: &str) -> (u16, Value) {
        call(
            router,
            "GET",
            &format!("/detector/{detector_id}/{}/{set_id}", self.segment),
            None,
        )
        .await
    }

    async fn delete(&self, router: &Router, detector_id: &str, set_id: &str, case: &str) {
        let (status, got) = self.delete_raw(router, detector_id, set_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to answer 200, got {status} {got:?}"
        );
    }

    async fn delete_raw(&self, router: &Router, detector_id: &str, set_id: &str) -> (u16, Value) {
        call(
            router,
            "DELETE",
            &format!("/detector/{detector_id}/{}/{set_id}", self.segment),
            None,
        )
        .await
    }

    /// The ids this family's listing names. The collections here are bounded well
    /// under the modelled page size, so one page holds them all.
    async fn listed(&self, router: &Router, detector_id: &str, case: &str) -> Vec<String> {
        let (status, got) = call(
            router,
            "GET",
            &format!("/detector/{detector_id}/{}", self.segment),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the listing to answer 200, got {status} {got:?}"
        );
        got[self.ids_member]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|id| id.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Read `set_id` until it reports `want`, or until [`SETTLE_DEADLINE`] has passed
    /// — the CONDITION ends the wait, never a fixed pause. Only a LIVE set is waited
    /// on here, so a read that does not answer 200 is a failure of the wait.
    async fn read_until_status(
        &self,
        router: &Router,
        detector_id: &str,
        set_id: &str,
        want: &str,
        case: &str,
    ) -> Value {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, body) = self.get(router, detector_id, set_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a set nothing deleted must stay readable until it settles, got \
                 {status} {body:?}"
            );
            if body["status"].as_str() == Some(want) {
                return body;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: {set_id} never reached {want}, last read {body:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Assert every one of a handful of consecutive reads of `set_id` is the
    /// entity-set families' absence refusal, so the refusal is not a phase and the
    /// answer does not depend on when the caller looks.
    async fn assert_stays_absent(
        &self,
        router: &Router,
        detector_id: &str,
        set_id: &str,
        case: &str,
    ) {
        for attempt in 0..REPEATED_READS {
            let (status, body) = self.get(router, detector_id, set_id).await;
            assert_entity_set_absent(&format!("{case}: read {attempt}"), status, &body, set_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 1: the deleted id answers the absence refusal, on every read.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_deleted_entity_set_is_refused_as_absent_from_the_next_read_on() {
    for family in &FAMILIES {
        for row in rows() {
            let case = format!("{}: {}", family.case, row.case);
            let case = case.as_str();
            // One store per row: the trusted family keeps one set at a time, so a row
            // cannot share a detector with the next.
            let router = new_router();
            let detector_id = enabled_detector(&router).await;
            let set_id = family
                .create(&router, &detector_id, row.create.clone(), case)
                .await;
            if row.settle_first {
                // The status the create's own transition settles on: a set the caller
                // asked GuardDuty to use ends ACTIVE, one it did not is stored
                // INACTIVE, which is settled already.
                let want = if row.create["activate"] == json!(true) {
                    ENTITY_SET_ACTIVE
                } else {
                    ENTITY_SET_INACTIVE
                };
                family
                    .read_until_status(&router, &detector_id, &set_id, want, case)
                    .await;
            }
            let (status, created) = family.get(&router, &detector_id, &set_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the created set to be readable, got {status} {created:?}"
            );

            family.delete(&router, &detector_id, &set_id, case).await;

            // Rule 1: the very next read is the absence refusal, and so is every
            // read after it.
            family
                .assert_stays_absent(&router, &detector_id, &set_id, case)
                .await;

            // A second delete has nothing left to delete, so it is that same
            // refusal: the id names no entity set at all any more.
            let (status, body) = family.delete_raw(&router, &detector_id, &set_id).await;
            assert_entity_set_absent(case, status, &body, &set_id);

            // Rule 2: the refusal is a lookup — an id of the same form no create
            // minted is refused the same way, under this very detector.
            let (status, body) = family.get(&router, &detector_id, ABSENT_SET_ID).await;
            assert_entity_set_absent(case, status, &body, ABSENT_SET_ID);
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the listing names the live sets, and a live sibling still reads back.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_deleted_entity_set_leaves_the_collection_and_its_own_read() {
    // The rows differ along HOW MANY of a detector's sets are deleted: the trusted
    // family holds one at a time, so it is driven one set per cycle, while the threat
    // family holds several and has two deleted out from under a live sibling.
    for family in &FAMILIES {
        let case = format!("{}: deleted beside a live sibling", family.case);
        let case = case.as_str();
        let router = new_router();
        let detector_id = enabled_detector(&router).await;

        let mut deleted: Vec<String> = Vec::new();
        for index in 0..2 {
            let id = family
                .create(
                    &router,
                    &detector_id,
                    json!({
                        "name": format!("collection_probe-{index}"),
                        "format": "TXT",
                        "location": format!("s3://guardduty-lists/probe-{index}.txt"),
                        "activate": index % 2 == 0,
                    }),
                    case,
                )
                .await;
            family.delete(&router, &detector_id, &id, case).await;
            let listing = family.listed(&router, &detector_id, case).await;
            assert!(
                !listing.contains(&id),
                "case {case:?}: the listing must stop naming the deleted {id}, got {listing:?}"
            );
            deleted.push(id);
        }

        // A live sibling, to show the listing names what the detector really holds
        // rather than nothing at all.
        let live = family
            .create(
                &router,
                &detector_id,
                json!({
                    "name": "collection_probe-live",
                    "format": "STIX",
                    "location": "s3://guardduty-lists/live.txt",
                    "activate": false,
                }),
                case,
            )
            .await;
        let listing = family.listed(&router, &detector_id, case).await;
        assert_eq!(
            listing,
            vec![live.clone()],
            "case {case:?}: the listing must name exactly the live set, got {listing:?}"
        );

        // Each deleted id is refused, and a second delete of it is refused too: the
        // detector has nothing left to delete. The live sibling is untouched.
        for id in &deleted {
            family
                .assert_stays_absent(&router, &detector_id, id, case)
                .await;
            let (status, body) = family.delete_raw(&router, &detector_id, id).await;
            assert_entity_set_absent(case, status, &body, id);
        }
        let (status, body) = family.get(&router, &detector_id, &live).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the live sibling must still read back, got {status} {body:?}"
        );
    }
}
