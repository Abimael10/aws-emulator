// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DECISION side of a DataZone subscription request: the
//! status a created request resolves to, and `AcceptSubscriptionRequest` /
//! `RejectSubscriptionRequest` taken on it.
//!
//! Every assertion is made on the HTTP answer only — a handler that answered from a
//! hardcoded value instead of from the store fails here. The one thing a FIXTURE reaches
//! into the store for is a subscription target: it is created by
//! `CreateSubscriptionTarget`, which is addressed by an `environmentIdentifier` and is
//! still a scaffold (no environment resource exists yet), so a case that needs a listing
//! WITH a target behind it has no wire call to make one.
//!
//! The rules under test, each asserted over a TABLE of inputs:
//!
//! 1. A created request awaits a decision exactly when its subscribed listings resolve to
//!    a subscription target of the domain — one whose applicable asset types include the
//!    published entity's type. A listing no target applies to has no fulfilment path, so
//!    the service resolves the ask at once: `ACCEPTED`, with no `existingSubscriptionId`
//!    (nothing materialises a subscription) and no reviewer.
//! 2. A decision may be taken only on a request still awaiting one: `Accept` moves it to
//!    `ACCEPTED` and `Reject` to `REJECTED`, each recording the caller as `reviewerId` and
//!    `updatedBy`, the comment it carried, and a later `updatedAt` —
//!    `GetSubscriptionRequestDetails` reads back exactly that.
//! 3. A request that is NOT awaiting a decision refuses either decision with 400
//!    `ValidationException` and leaves its stored record untouched.
//! 4. A decision addressing a domain this store serves none for is denied, and one
//!    addressing a request the domain does not hold is a miss.

use crate::harness::Emulator;
use serde_json::{json, Value};
// The messages and enum members as the service states them, so a drift between these and
// what goes on the wire fails here.
use service_com_amazonaws_datazone_datazone::state::{
    cannot_reject_message, service_state, SubscriptionTarget,
    SUBSCRIPTION_ALREADY_DECIDED_MESSAGE, SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
    SUBSCRIPTION_REQUEST_STATUS_PENDING, SUBSCRIPTION_REQUEST_STATUS_REJECTED,
};
use std::sync::Arc;

/// The principal the harness's SigV4 scope names, which is what the reviewer of every
/// decision taken here must be.
const CALLER: &str = "arn:aws:iam::000000000000:user/AKIAEXAMPLETESTKEY";

/// A domain with a producer project, a published asset of a named type and a consumer
/// project that asks for access to it — the fixture every case here needs.
struct Catalog {
    domain: String,
    consumer: String,
    /// The asset type the published asset is OF: what a subscription target's applicable
    /// asset types have to include for a request against this listing to await a
    /// decision.
    asset_type: String,
    listing: String,
    producer: String,
}

/// Build the fixture: a settled domain, two projects, a custom asset type with the empty
/// `formsInput` the docs allow, an asset of that type and the listing publishing it.
async fn catalog(dz: &Emulator, name: &str, asset_type: &str) -> Catalog {
    let domain = dz.available_domain(name).await;
    let producer = dz.project(&domain, "producer").await;
    let consumer = dz.project(&domain, "consumer").await;

    let created_type = dz
        .call(
            "POST",
            &format!("/v2/domains/{domain}/asset-types"),
            Some(json!({
                "name": asset_type,
                "owningProjectIdentifier": producer,
                "formsInput": {},
            })),
        )
        .await;
    assert_eq!(
        created_type.status, 201,
        "creating asset type {asset_type:?}: expected 201, got {} {:?}",
        created_type.status, created_type.body
    );

    let created_asset = dz
        .call(
            "POST",
            &format!("/v2/domains/{domain}/assets"),
            Some(json!({
                "name": format!("{asset_type}-asset"),
                "typeIdentifier": asset_type,
                "owningProjectIdentifier": producer,
            })),
        )
        .await;
    assert_eq!(
        created_asset.status, 201,
        "creating the asset of {asset_type:?}: expected 201, got {} {:?}",
        created_asset.status, created_asset.body
    );

    let published = dz
        .call(
            "POST",
            &format!("/v2/domains/{domain}/listings/change-set"),
            Some(json!({
                "entityIdentifier": created_asset.s("id"),
                "entityType": "ASSET",
                "action": "PUBLISH",
            })),
        )
        .await;
    assert_eq!(
        published.status, 200,
        "publishing the asset of {asset_type:?}: expected 200, got {} {:?}",
        published.status, published.body
    );

    Catalog {
        domain,
        consumer,
        asset_type: asset_type.to_string(),
        listing: published.s("listingId"),
        producer,
    }
}

/// Put a subscription target into `catalog`'s domain, applicable to the asset types
/// `applicable_asset_types`.
///
/// The fixture seam this suite documents at the top: `CreateSubscriptionTarget` is still a
/// scaffold, so there is no wire call that records one. The record is exactly what that
/// handler will write.
fn put_subscription_target(
    engine: &Arc<engine::InMemoryEngine>,
    catalog: &Catalog,
    id: &str,
    applicable_asset_types: &[&str],
) {
    let state = service_state(&**engine).expect("the DataZone store is reachable");
    let mut store = state.lock();
    let domain = store
        .domains
        .get_mut(&catalog.domain)
        .expect("the fixture's domain is stored");
    domain.subscription_targets.insert(
        id.to_string(),
        SubscriptionTarget {
            id: id.to_string(),
            domain_id: catalog.domain.clone(),
            environment_id: format!("{id}-environment"),
            project_id: catalog.producer.clone(),
            applicable_asset_types: applicable_asset_types
                .iter()
                .map(|asset_type| (*asset_type).to_string())
                .collect(),
        },
    );
}

/// Ask for access to `catalog`'s listing for its consumer project.
async fn ask(dz: &Emulator, catalog: &Catalog, reason: &str) -> Value {
    let created = dz
        .call(
            "POST",
            &format!("/v2/domains/{}/subscription-requests", catalog.domain),
            Some(json!({
                "subscribedPrincipals": [{ "project": { "identifier": catalog.consumer } }],
                "subscribedListings": [{ "identifier": catalog.listing }],
                "requestReason": reason,
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "asking for access ({reason:?}): expected 200, got {} {:?}",
        created.status, created.body
    );
    created.body
}

/// `GetSubscriptionRequestDetails` on one stored request.
async fn read_back(dz: &Emulator, domain: &str, id: &str) -> Value {
    let got = dz
        .call(
            "GET",
            &format!("/v2/domains/{domain}/subscription-requests/{id}"),
            None,
        )
        .await;
    assert_eq!(
        got.status, 200,
        "reading request {id}: expected 200, got {} {:?}",
        got.status, got.body
    );
    got.body
}

/// A member of a response body as a string, or `"<absent>"` so a message says which
/// member was missing.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<absent>")
        .to_string()
}

#[tokio::test]
async fn a_request_for_a_listing_no_subscription_target_applies_to_is_resolved_at_once() {
    // The rows differ along what the rule generalises over: whether the domain holds no
    // subscription target at all, one applicable to a DIFFERENT asset type, or several
    // inapplicable ones — in each case nothing the subscription could be granted through.
    let rows: Vec<(&str, Vec<Vec<&str>>)> = vec![
        ("a domain holding no subscription target", vec![]),
        (
            "a target applicable to another asset type only",
            vec![vec!["amazon.datazone.RelationalTable"]],
        ),
        (
            "several targets, none applicable to the published type",
            vec![
                vec!["amazon.datazone.RelationalTable", "OtherCustomType"],
                vec![],
            ],
        ),
    ];

    for (index, (label, targets)) in rows.into_iter().enumerate() {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("auto-resolve-{index}"), "PlainCustomType").await;
        for (slot, applicable) in targets.iter().enumerate() {
            put_subscription_target(&engine, &catalog, &format!("target-{slot}"), applicable);
        }

        let created = ask(&dz, &catalog, "no approval step to await").await;
        assert_eq!(
            member(&created, "status"),
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            "case {label:?}: a listing no subscription target applies to has nothing to \
             await, got {created:?}"
        );
        assert_eq!(
            created.get("existingSubscriptionId"),
            None,
            "case {label:?}: no subscription materialises for such a listing, got {created:?}"
        );
        assert_eq!(
            created.get("reviewerId"),
            None,
            "case {label:?}: nobody reviewed the request, got {created:?}"
        );
        assert_eq!(
            created.get("decisionComment"),
            None,
            "case {label:?}: no decision carried a comment, got {created:?}"
        );

        let read = read_back(&dz, &catalog.domain, &member(&created, "id")).await;
        assert_eq!(
            member(&read, "status"),
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            "case {label:?}: the reader reports the stored status, got {read:?}"
        );
        assert_eq!(
            read.get("existingSubscriptionId"),
            None,
            "case {label:?}: the reader reports no subscription either, got {read:?}"
        );
    }
}

#[tokio::test]
async fn a_request_for_a_listing_a_subscription_target_applies_to_awaits_a_decision() {
    // The rows differ along how the applicable target names the published type: as its
    // only applicable type, or among others.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("the published type is the target's only one", vec!["Fulfillable"]),
        (
            "the published type is one of the target's several",
            vec!["amazon.datazone.RelationalTable", "Fulfillable", "Another"],
        ),
    ];

    for (index, (label, applicable)) in rows.into_iter().enumerate() {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("awaits-decision-{index}"), "Fulfillable").await;
        put_subscription_target(&engine, &catalog, "fulfilling-target", &applicable);
        assert!(
            applicable.contains(&catalog.asset_type.as_str()),
            "case {label:?}: the row's target must apply to the published type {:?}",
            catalog.asset_type
        );

        let created = ask(&dz, &catalog, "awaiting the owner's decision").await;
        assert_eq!(
            member(&created, "status"),
            SUBSCRIPTION_REQUEST_STATUS_PENDING,
            "case {label:?}: a listing with a target behind it awaits a decision, got \
             {created:?}"
        );
        assert_eq!(
            created.get("reviewerId"),
            None,
            "case {label:?}: nobody has decided it yet, got {created:?}"
        );
    }
}

#[tokio::test]
async fn a_decision_on_a_request_that_awaits_one_is_recorded() {
    // Both decisions, over the same rule: the decision moves the request to its own
    // terminal status and records who took it; the rows differ along which decision is
    // taken and whether it carried a comment.
    let rows: Vec<(&str, &str, &str, Option<&str>)> = vec![
        (
            "an accept with a comment",
            "accept",
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            Some("approved for the quarterly report"),
        ),
        (
            "an accept with no comment",
            "accept",
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            None,
        ),
        (
            "a reject with a comment",
            "reject",
            SUBSCRIPTION_REQUEST_STATUS_REJECTED,
            Some("the consumer project is out of scope"),
        ),
        (
            "a reject with no comment",
            "reject",
            SUBSCRIPTION_REQUEST_STATUS_REJECTED,
            None,
        ),
    ];

    for (index, (label, decision, expect_status, comment)) in rows.into_iter().enumerate() {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("decision-{index}"), "Fulfillable").await;
        put_subscription_target(&engine, &catalog, "fulfilling-target", &["Fulfillable"]);
        let created = ask(&dz, &catalog, "please grant access").await;
        let id = member(&created, "id");
        assert_eq!(
            member(&created, "status"),
            SUBSCRIPTION_REQUEST_STATUS_PENDING,
            "case {label:?}: the fixture's request must await a decision, got {created:?}"
        );

        let body = comment.map(|comment| json!({ "decisionComment": comment }));
        let decided = dz
            .call(
                "PUT",
                &format!(
                    "/v2/domains/{}/subscription-requests/{id}/{decision}",
                    catalog.domain
                ),
                body,
            )
            .await;
        assert_eq!(
            decided.status, 200,
            "case {label:?}: expected 200, got {} {:?}",
            decided.status, decided.body
        );
        assert_eq!(
            member(&decided.body, "status"),
            expect_status,
            "case {label:?}: the decision moves the request, got {:?}",
            decided.body
        );
        assert_eq!(
            member(&decided.body, "reviewerId"),
            CALLER,
            "case {label:?}: the reviewer is the caller that decided, got {:?}",
            decided.body
        );
        assert_eq!(
            member(&decided.body, "updatedBy"),
            CALLER,
            "case {label:?}: the decision is what updated the request, got {:?}",
            decided.body
        );
        assert_eq!(
            decided.body.get("decisionComment").and_then(Value::as_str),
            comment,
            "case {label:?}: the comment the decision carried, got {:?}",
            decided.body
        );
        assert_eq!(
            member(&decided.body, "id"),
            id,
            "case {label:?}: the decision is reported on the request it addressed, got {:?}",
            decided.body
        );
        assert_eq!(
            member(&decided.body, "requestReason"),
            "please grant access",
            "case {label:?}: the ask itself is unchanged, got {:?}",
            decided.body
        );

        // The round trip: the reader reports the decided record, not the ask.
        let read = read_back(&dz, &catalog.domain, &id).await;
        assert_eq!(
            member(&read, "status"),
            expect_status,
            "case {label:?}: the reader reports the decided status, got {read:?}"
        );
        assert_eq!(
            read.get("decisionComment").and_then(Value::as_str),
            comment,
            "case {label:?}: the reader reports the stored comment, got {read:?}"
        );
        assert_eq!(
            member(&read, "reviewerId"),
            CALLER,
            "case {label:?}: the reader reports the stored reviewer, got {read:?}"
        );
        assert_eq!(
            read.get("createdAt"),
            created.get("createdAt"),
            "case {label:?}: a decision does not move createdAt, got {read:?}"
        );
    }
}

#[tokio::test]
async fn a_decision_on_a_request_that_is_already_decided_is_refused() {
    // The rows span the statuses a request can already carry — one the service resolved
    // at create, one an accept took, one a reject took — and both decision operations
    // against each. Each refusal names its own message, and the stored record is
    // untouched afterwards.
    let already: Vec<(&str, Option<&str>, &str, Option<&str>)> = vec![
        (
            "a request the service resolved at create",
            None,
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            None,
        ),
        (
            "a request an accept decided",
            Some("accept"),
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            Some("already approved"),
        ),
        (
            "a request a reject decided",
            Some("reject"),
            SUBSCRIPTION_REQUEST_STATUS_REJECTED,
            Some("already refused"),
        ),
    ];

    for (index, (label, decided_by, expect_status, comment)) in already.into_iter().enumerate() {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("already-decided-{index}"), "Fulfillable").await;
        // A request only awaits a decision when a target applies to its listing, so the
        // two rows that take one need the target and the auto-resolved row must not have
        // it.
        if decided_by.is_some() {
            put_subscription_target(&engine, &catalog, "fulfilling-target", &["Fulfillable"]);
        }
        let created = ask(&dz, &catalog, "the original ask").await;
        let id = member(&created, "id");
        if let Some(decision) = decided_by {
            let decided = dz
                .call(
                    "PUT",
                    &format!(
                        "/v2/domains/{}/subscription-requests/{id}/{decision}",
                        catalog.domain
                    ),
                    comment.map(|comment| json!({ "decisionComment": comment })),
                )
                .await;
            assert_eq!(
                decided.status, 200,
                "case {label:?}: the fixture's {decision}: got {} {:?}",
                decided.status, decided.body
            );
        }

        let refusals: Vec<(&str, String)> = vec![
            ("accept", SUBSCRIPTION_ALREADY_DECIDED_MESSAGE.to_string()),
            ("reject", cannot_reject_message(expect_status)),
        ];
        for (decision, expect_message) in refusals {
            let refused = dz
                .call(
                    "PUT",
                    &format!(
                        "/v2/domains/{}/subscription-requests/{id}/{decision}",
                        catalog.domain
                    ),
                    Some(json!({ "decisionComment": "a second opinion" })),
                )
                .await;
            assert_eq!(
                (refused.status, refused.error_code.as_str()),
                (400, "ValidationException"),
                "case {label:?}: a {decision} on it: got {} {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                member(&refused.body, "message"),
                expect_message,
                "case {label:?}: the {decision}'s refusal message, got {:?}",
                refused.body
            );

            // The refusal wrote nothing: the status and the comment are as they were.
            let read = read_back(&dz, &catalog.domain, &id).await;
            assert_eq!(
                member(&read, "status"),
                expect_status,
                "case {label:?}: the refused {decision} left the status alone, got {read:?}"
            );
            assert_eq!(
                read.get("decisionComment").and_then(Value::as_str),
                comment,
                "case {label:?}: the refused {decision} left the comment alone, got {read:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_decision_addressing_nothing_this_store_holds_is_refused() {
    let (dz, engine) = Emulator::with_engine();
    let catalog = catalog(&dz, "decision-refusals", "Fulfillable").await;
    put_subscription_target(&engine, &catalog, "fulfilling-target", &["Fulfillable"]);
    let pending = member(&ask(&dz, &catalog, "a pending ask").await, "id");

    // (label, domain, request id, expected status, expected error code)
    let rows: Vec<(&str, String, String, u16, &str)> = vec![
        (
            // A permission is held ON a domain, so the caller holds none on an identifier
            // the store serves no domain for: authorization decides the call before any
            // lookup.
            "a domain nothing created",
            "dzd_doesnotexist".to_string(),
            pending.clone(),
            403,
            "AccessDeniedException",
        ),
        (
            "a request id nothing recorded",
            catalog.domain.clone(),
            "nosuchrequest".to_string(),
            404,
            "ResourceNotFoundException",
        ),
    ];

    for (label, domain, id, status, code) in rows {
        for decision in ["accept", "reject"] {
            let refused = dz
                .call(
                    "PUT",
                    &format!("/v2/domains/{domain}/subscription-requests/{id}/{decision}"),
                    None,
                )
                .await;
            assert_eq!(
                (refused.status, refused.error_code.as_str()),
                (status, code),
                "case {label:?} on {decision}: got {} {:?}",
                refused.status,
                refused.body
            );
        }
    }

    // Every refusal left the request alone: it still awaits a decision, and still takes
    // one.
    let read = read_back(&dz, &catalog.domain, &pending).await;
    assert_eq!(
        member(&read, "status"),
        SUBSCRIPTION_REQUEST_STATUS_PENDING,
        "the refusals left the request awaiting a decision, got {read:?}"
    );
    let decided = dz
        .call(
            "PUT",
            &format!(
                "/v2/domains/{}/subscription-requests/{pending}/accept",
                catalog.domain
            ),
            None,
        )
        .await;
    assert_eq!(
        decided.status, 200,
        "an accept after the refusals: got {} {:?}",
        decided.status, decided.body
    );
}
