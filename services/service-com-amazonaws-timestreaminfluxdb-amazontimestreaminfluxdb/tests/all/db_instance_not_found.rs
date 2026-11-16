// SPDX-License-Identifier: CC0-1.0

//! A DB-instance `identifier` that resolves to nothing, over the wire.
//!
//! RULE: every operation that names a single DB instance by `identifier` refuses an
//! identifier that resolves to no record this account and Region holds with HTTP 404
//! `ResourceNotFoundException`, whose `message` is the one fixed sentence this service
//! answers a DB-instance lookup miss with — it interpolates no id, name or time — beside
//! the `resourceId` the request asked for and the `DB_INSTANCE` resource type. An id whose
//! instance was deleted resolves to nothing like any other, so it is refused the same way
//! for a read and for a mutation alike: a dead id stays unusable.
//!
//! The operation set is every one whose input models `identifier` as a
//! `DbInstanceIdentifier` (`smithy_query(query="digest")`): `GetDbInstance`,
//! `UpdateDbInstance`, `DeleteDbInstance` and `RebootDbInstance`. The identifier rows span
//! what the rule generalises over: both published length bounds of the modelled
//! `DbInstanceIdentifier` (len 3..64, `^[a-zA-Z0-9]+$`), the character classes that
//! pattern allows, an id nothing ever minted, a live instance's NAME (a name is not an
//! id), and the id of an instance that has been deleted.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_lifecycle::{call_in, ok, router, REGION};

/// The whole message AWS answers a DB-instance lookup miss with: fixed text for the error
/// class, carrying no request- or state-derived part.
const NOT_FOUND_MESSAGE: &str = "The DB instance with db id does not exist";

/// The `resourceType` a DB-instance refusal reports, a member of the modelled
/// `ResourceType` enum (`smithy_query(query="shape", shape="ResourceType")`).
const RESOURCE_TYPE: &str = "DB_INSTANCE";

/// One operation that names a DB instance: its wire name, and the request body it takes
/// for an identifier.
type LookupOperation = (&'static str, fn(&str) -> Value);

/// Every operation whose input names one DB instance by `identifier`, with the rest of the
/// request body it needs beside that member (`UpdateDbInstance` takes a partial update, so
/// one changed member is enough to make it a real reconfiguration request).
const LOOKUP_OPERATIONS: &[LookupOperation] = &[
    ("GetDbInstance", |id| json!({ "identifier": id })),
    ("UpdateDbInstance", |id| {
        json!({ "identifier": id, "port": 8086 })
    }),
    ("DeleteDbInstance", |id| json!({ "identifier": id })),
    ("RebootDbInstance", |id| json!({ "identifier": id })),
];

/// How long a test waits for a clock-owed settle before giving up: far above the span the
/// service settles a transient status after, so a loaded host does not fail the test.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The minimum a `CreateDbInstance` request must carry, per the model's `@required`
/// members (`name`, `password`, `dbInstanceType`, `vpcSubnetIds`, `vpcSecurityGroupIds`,
/// `allocatedStorage`).
fn minimal_create(name: &str) -> Value {
    json!({
        "name": name,
        "password": "supersecret1",
        "dbInstanceType": "db.influx.medium",
        "allocatedStorage": 20,
        "vpcSubnetIds": ["subnet-aaa"],
        "vpcSecurityGroupIds": ["sg-aaa"],
    })
}

/// Create one instance and hand back the service-generated `id`.
async fn create(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = ok(
        case,
        call_in(router, REGION, "CreateDbInstance", minimal_create(name)).await,
    );
    created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string `id`, got {created}"))
        .to_string()
}

/// Read the record back until it reports `AVAILABLE`.
///
/// Waits for the CONDITION rather than for a span of time: the settle the clock owes is
/// materialised when a reporting operation reads the record, so the test looks again until
/// the record is running or the generous deadline above passes.
async fn poll_until_available(router: &MultiServiceRouter, identifier: &str, case: &str) {
    let started = Instant::now();
    loop {
        let body = ok(
            case,
            call_in(
                router,
                REGION,
                "GetDbInstance",
                json!({ "identifier": identifier }),
            )
            .await,
        );
        if body["status"].as_str() == Some("AVAILABLE") {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the record never became AVAILABLE, it reports {:?}",
            body["status"]
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert one answer is the modelled not-found refusal for `identifier`.
fn assert_not_found(case: &str, identifier: &str, answer: (u16, Value)) {
    let (status, body) = answer;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {body}"
    );
    assert!(
        body["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("ResourceNotFoundException"),
        "case {case:?}: expected a ResourceNotFoundException, got {body}"
    );
    // The sentence is fixed text for this error class: the same for every miss, with no
    // part of it taken from the identifier that was asked for.
    assert_eq!(
        body["message"].as_str(),
        Some(NOT_FOUND_MESSAGE),
        "case {case:?}: expected the fixed lookup-miss message, got {:?}",
        body["message"]
    );
    assert_eq!(
        body["resourceId"].as_str(),
        Some(identifier),
        "case {case:?}: the refusal must name the identifier asked for, got {body}"
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some(RESOURCE_TYPE),
        "case {case:?}: unexpected resourceType in {body}"
    );
}

#[tokio::test]
async fn every_db_instance_lookup_refuses_an_unresolvable_identifier_the_same_way() {
    let router = router();
    // A live instance exists throughout, so each refusal is about resolution and not
    // about empty state.
    // The name satisfies BOTH the modelled `DbInstanceName` pattern
    // (`^[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*$`) and the `DbInstanceIdentifier` pattern
    // (`^[a-zA-Z0-9]+$`), so aiming an operation at it reaches the lookup instead of
    // being refused by input validation.
    let live_name = "notfoundliveone";
    let live_id = create(&router, "setup: live", live_name).await;

    let shortest = "abc";
    let longest = "z".repeat(64);
    let rows: Vec<(&str, &str)> = vec![
        ("the shortest identifier the model allows", shortest),
        ("the longest identifier the model allows", &longest),
        ("an identifier nothing ever minted", "doesnotexist"),
        ("a digits-only identifier", "0000000000"),
        ("a mixed-case identifier", "NoSuchDbInstance9"),
        // An identifier is the service-generated `id`, never the customer-supplied
        // `name`, so a live instance's name resolves to nothing.
        ("the name of a live instance", live_name),
    ];

    for (label, identifier) in rows {
        for (operation, body) in LOOKUP_OPERATIONS {
            let case = format!("{operation} on {label}");
            assert_not_found(
                &case,
                identifier,
                call_in(&router, REGION, operation, body(identifier)).await,
            );
        }
    }

    // None of those refusals disturbed the live record.
    poll_until_available(&router, &live_id, "live").await;
}

#[tokio::test]
async fn a_deleted_identifier_stays_unusable_for_reads_and_for_mutations() {
    // One deleted instance per operation under test: each row deletes its own instance and
    // then aims that operation at the dead id, so the refusal is not the one a previous
    // row already produced.
    for (operation, body) in LOOKUP_OPERATIONS {
        let router = router();
        let case = format!("{operation} on a deleted id");
        let id = create(&router, &case, "notfound-deleted-one").await;
        poll_until_available(&router, &id, &case).await;
        ok(
            &case,
            call_in(
                &router,
                REGION,
                "DeleteDbInstance",
                json!({ "identifier": &id }),
            )
            .await,
        );

        assert_not_found(
            &case,
            &id,
            call_in(&router, REGION, operation, body(&id)).await,
        );
    }
}
