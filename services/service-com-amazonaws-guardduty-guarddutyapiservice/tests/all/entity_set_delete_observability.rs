// SPDX-License-Identifier: CC0-1.0

//! What a client that DELETES an entity set and then polls its read observes, on both
//! entity-set families, end to end over the wire.
//!
//! `threat_entity_set_delete.rs` and `trusted_entity_set_delete.rs` own one family's
//! delete each and `entity_set_delete_retention.rs` what churn leaves behind; this
//! suite owns the one rule both reads follow once a delete has been served.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test, asserted over a TABLE spanning both families and everything
//! the rule generalises over — the status the set was in when the delete arrived
//! (including the transitional one), the `*EntitySetFormat` the create stored, whether
//! the caller activated the set, whether it carried an `ExpectedBucketOwner`, the
//! number of tags, the size of the name and the number of sets deleted under one
//! detector:
//!
//! 1. A served delete starts an ASYNCHRONOUS deletion, so the id it names stays
//!    addressable while the service still holds its record: the family's own read
//!    answers HTTP 200 and reports the deletion through its status enum
//!    (`smithy_query(shape="ThreatEntitySetStatus")` and its trusted sibling carry
//!    `DELETE_PENDING` and the terminal `DELETED`), never the absence refusal an id
//!    the detector holds no record for gets.
//! 2. The deletion runs to `DELETED` and stays there while the service holds the
//!    record, and every other member the read reports is still the one the create
//!    stored — so a client can poll a delete to completion.
//! 3. Once the deletion has run out its retention
//!    (`state::ENTITY_SET_DELETED_RETENTION_MILLIS`) the service no longer holds the
//!    set, so the id is refused the way one no create ever minted is: a client that
//!    polls a delete to the end reaches that refusal instead of polling for ever, and
//!    can tell "the deletion is running" from "already gone".
//! 4. Leaving the detector's collection is not the same as leaving the reader: the
//!    listing stops naming a deleted set while its own id keeps answering.
//! 5. The 200 is a LOOKUP: an id of the same form that no create minted is refused
//!    with the entity-set families' absence refusal under the same detector, at the
//!    same time as the deleted one answers.

use serde_json::{json, Value};

use crate::detector_lifecycle::{call, enabled_detector, new_router, Router};
use crate::set_delete_terminality::{assert_entity_set_absent, ABSENT_SET_ID};
use service_com_amazonaws_guardduty_guarddutyapiservice::state::{
    ENTITY_SET_ACTIVE, ENTITY_SET_DELETED, ENTITY_SET_DELETED_RETENTION_MILLIS,
    ENTITY_SET_DELETE_PENDING, ENTITY_SET_INACTIVE,
};

/// How long a poll may wait for the clock to owe a deletion its terminal status
/// before the test gives up. Generous on purpose: the wait ends on the CONDITION, and
/// this is only the deadline past which the emulator is considered broken.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The gap between two reads of a poll. Only the pause between attempts; no loop
/// below ever ends on it.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// The statuses a read may report for an entity set whose delete has been served:
/// the transitional one, and the terminal one the clock settles it to.
const DELETION_STATUSES: [&str; 2] = [ENTITY_SET_DELETE_PENDING, ENTITY_SET_DELETED];

/// How long the poll for the end of a deletion may run before the test gives up: the
/// published retention window plus half a minute of slack. Generous on purpose — the
/// loop ends when the id is refused, and this is only the deadline past which the
/// emulator is considered broken.
fn removal_deadline() -> std::time::Duration {
    let window = u64::try_from(ENTITY_SET_DELETED_RETENTION_MILLIS).unwrap_or_default();
    std::time::Duration::from_millis(window) + std::time::Duration::from_secs(30)
}

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
    /// — the CONDITION ends the wait, never a fixed pause. Answers the read that saw
    /// it, or `None` once the service has stopped holding the record, which is how a
    /// deletion ends: that read is the family's absence refusal, so a wait for a
    /// deletion status cannot outlive the record it waits on.
    async fn read_until_status(
        &self,
        router: &Router,
        detector_id: &str,
        set_id: &str,
        want: &str,
        case: &str,
    ) -> Option<Value> {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, body) = self.get(router, detector_id, set_id).await;
            if status != 200 {
                assert_entity_set_absent(case, status, &body, set_id);
                return None;
            }
            if body["status"].as_str() == Some(want) {
                return Some(body);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: {set_id} never reached {want}, last read {body:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// Assert one read of a set whose delete has been served: while the service holds the
/// record it answers and the status it reports is one of the two the deletion moves
/// through; once it has stopped holding it the id is refused as absent. What it never
/// reports is a live status.
fn assert_reports_a_deletion(case: &str, status: u16, body: &Value, id: &str) {
    if status != 200 {
        assert_entity_set_absent(case, status, body, id);
        return;
    }
    let reported = body["status"].as_str().unwrap_or_default();
    assert!(
        DELETION_STATUSES.contains(&reported),
        "case {case:?}: expected one of {DELETION_STATUSES:?}, got {reported:?} {body:?}"
    );
}

/// Assert the read reports the members the create stored, so the answer comes from the
/// record and not from the delete.
fn assert_reports_the_stored_set(case: &str, created: &Value, read: &Value) {
    for member in ["name", "format", "location", "tags", "expectedBucketOwner"] {
        assert_eq!(
            read[member], created[member],
            "case {case:?}: the deletion must leave {member} as the create stored it, expected \
             {:?}, got {:?}",
            created[member], read[member]
        );
    }
    assert_eq!(
        read["createdAt"], created["createdAt"],
        "case {case:?}: the deletion must not move createdAt, expected {:?}, got {:?}",
        created["createdAt"], read["createdAt"]
    );
    let created_at = created["createdAt"].as_f64().unwrap_or_default();
    let updated_at = read["updatedAt"].as_f64().unwrap_or_default();
    assert!(
        updated_at >= created_at,
        "case {case:?}: the deletion is a change, so updatedAt must not precede createdAt, got \
         {updated_at} before {created_at}"
    );
}

// ---------------------------------------------------------------------------
// Rules 1, 2 and 5: the deleted id keeps answering, and the deletion completes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_deleted_entity_set_reports_its_deletion_until_it_completes() {
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
                let settled_create = family
                    .read_until_status(&router, &detector_id, &set_id, want, case)
                    .await;
                assert!(
                    settled_create.is_some(),
                    "case {case:?}: a set nothing deleted must stay readable until it settles"
                );
            }
            let (status, created) = family.get(&router, &detector_id, &set_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the created set to be readable, got {status} {created:?}"
            );

            family.delete(&router, &detector_id, &set_id, case).await;

            // Rule 1: the very next read answers, reporting the deletion.
            let (status, straight_after) = family.get(&router, &detector_id, &set_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the read straight after the delete still answers, got {status} \
                 {straight_after:?}"
            );
            assert_reports_a_deletion(case, status, &straight_after, &set_id);
            assert_reports_the_stored_set(case, &created, &straight_after);

            // Rule 2: polling it reaches the terminal status, which does not move
            // again while the record is held, and the record is still the one the
            // create stored.
            if let Some(settled) = family
                .read_until_status(&router, &detector_id, &set_id, ENTITY_SET_DELETED, case)
                .await
            {
                assert_reports_the_stored_set(case, &created, &settled);
                let (status, again) = family.get(&router, &detector_id, &set_id).await;
                assert_reports_a_deletion(case, status, &again, &set_id);
                if status == 200 {
                    assert_eq!(
                        again["status"].as_str().unwrap_or_default(),
                        ENTITY_SET_DELETED,
                        "case {case:?}: {ENTITY_SET_DELETED} is terminal, got {again:?}"
                    );
                }
            }

            // Rule 5: the 200 above is a lookup over the store — an id of the same
            // form no create minted is still the absence refusal, under this very
            // detector.
            let (status, body) = family.get(&router, &detector_id, ABSENT_SET_ID).await;
            assert_entity_set_absent(case, status, &body, ABSENT_SET_ID);
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the deletion ends in the absence refusal, so a poll for it terminates.
// ---------------------------------------------------------------------------

/// Polling a served delete to its end reaches the absence refusal: the service stops
/// holding the set once the deletion has run out its retention, so a client waiting
/// for "already gone" is not left polling a record that never goes. One row per
/// family, and the loop ends on the CONDITION — a refused read — never on a fixed
/// wait.
#[tokio::test]
async fn polling_a_deleted_entity_set_reaches_the_absence_refusal() {
    for family in &FAMILIES {
        let case = format!("{}: polled to the end", family.case);
        let case = case.as_str();
        let router = new_router();
        let detector_id = enabled_detector(&router).await;
        let set_id = family
            .create(
                &router,
                &detector_id,
                json!({
                    "name": "polled_to_the_end-1",
                    "format": "ALIEN_VAULT",
                    "location": "s3://guardduty-lists/polled.csv",
                    "activate": true,
                }),
                case,
            )
            .await;
        family.delete(&router, &detector_id, &set_id, case).await;

        let deadline = std::time::Instant::now() + removal_deadline();
        let mut reads = 0_u32;
        let mut saw_settled = false;
        loop {
            let (status, body) = family.get(&router, &detector_id, &set_id).await;
            if status != 200 {
                // The end of the deletion: the id is refused exactly like one no
                // create ever minted.
                assert_entity_set_absent(case, status, &body, &set_id);
                break;
            }
            // Every answering read in between reports the deletion, never a status
            // the set held while it was in use.
            assert_reports_a_deletion(case, status, &body, &set_id);
            reads += 1;
            saw_settled |= body["status"].as_str() == Some(ENTITY_SET_DELETED);
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the deletion of {set_id} never ended, last read {body:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        // The terminal status is what the poll sees before the id is refused — unless
        // the record was already released by the time the first read landed, which is
        // the same deletion seen later rather than a different one.
        assert!(
            saw_settled || reads == 0,
            "case {case:?}: the poll read the set {reads} time(s) and never saw the terminal \
             {ENTITY_SET_DELETED}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: the collection stops naming the set its own read still reports.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_deleted_entity_set_leaves_the_collection_but_not_its_own_read() {
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

        // Each deleted set is still answered on its own id, reporting its deletion
        // while the service holds the record, and a second delete of it is refused:
        // the detector has nothing left to delete.
        for id in &deleted {
            let (status, body) = family.get(&router, &detector_id, id).await;
            assert_reports_a_deletion(case, status, &body, id);

            let (status, body) = family.delete_raw(&router, &detector_id, id).await;
            assert_entity_set_absent(case, status, &body, id);
        }
    }
}
