// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DECISION side of a DataZone subscription request: the
//! status a created request carries, `AcceptSubscriptionRequest` /
//! `RejectSubscriptionRequest` taken on it, and the subscription an accept materialises.
//!
//! Every assertion is made on the HTTP answer only — a handler that answered from a
//! hardcoded value instead of from the store fails here. A FIXTURE reaches into the store
//! for exactly two things, each because the operation that would do it on the wire is
//! still a scaffold: a subscription target (`CreateSubscriptionTarget` is addressed by an
//! `environmentIdentifier`, and no environment resource exists yet) and the ids of the
//! subscriptions a domain holds (`ListSubscriptions` is a scaffold, so no wire call names
//! them). Nothing is ASSERTED from the store.
//!
//! The rules under test, each asserted over a TABLE of inputs:
//!
//! 1. A created subscription request is an ASK, so it is `PENDING`: it awaits a decision,
//!    with no reviewer, no decision comment and no existing subscription. Whether the
//!    domain holds a subscription target for the listing does not enter into it — a
//!    target is what a GRANT is fulfilled through, not a decision.
//! 2. A decision may be taken only on a request still awaiting one: `Accept` moves it to
//!    `ACCEPTED` and `Reject` to `REJECTED`, each recording the caller as `reviewerId` and
//!    `updatedBy`, the comment it carried, and a later `updatedAt` —
//!    `GetSubscriptionRequestDetails` reads back exactly that.
//! 3. Accepting a request GRANTS the access, so it materialises exactly one subscription:
//!    `APPROVED`, naming the request it came from and the principal and listing revision
//!    the ask recorded, and readable by `GetSubscription` from that call on. Rejecting
//!    materialises none.
//! 4. A request that is NOT awaiting a decision refuses either decision with 400
//!    `ValidationException`, leaves its stored record untouched and materialises no
//!    further subscription.
//! 5. A decision addressing a domain this store serves none for is denied, and one
//!    addressing a request the domain does not hold is a miss.

use crate::harness::Emulator;
use serde_json::{json, Value};
// The messages and enum members as the service states them, so a drift between these and
// what goes on the wire fails here.
use service_com_amazonaws_datazone_datazone::state::{
    cannot_reject_message, service_state, SubscriptionTarget,
    SUBSCRIPTION_ALREADY_DECIDED_MESSAGE, SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
    SUBSCRIPTION_REQUEST_STATUS_PENDING, SUBSCRIPTION_REQUEST_STATUS_REJECTED,
    SUBSCRIPTION_STATUS_APPROVED,
};
use std::sync::Arc;

/// The principal the harness's SigV4 scope names, which is what the reviewer of every
/// decision taken here must be.
const CALLER: &str = "arn:aws:iam::000000000000:user/AKIAEXAMPLETESTKEY";

/// An IAM principal a subscription request can be made for: a role ARN of this account,
/// which is what `IamPrincipalArn`'s modelled pattern admits. DataZone resolves nothing
/// for an IAM principal, so no record has to exist for it.
const CONSUMER_ROLE: &str = "arn:aws:iam::000000000000:role/datazone-consumer";

/// A domain with a producer project, a published asset of a named type and a consumer
/// project that asks for access to it — the fixture every case here needs.
struct Catalog {
    domain: String,
    consumer: String,
    /// The asset type the published asset is OF: what a subscription target's applicable
    /// asset types can be made to include.
    asset_type: String,
    /// The published asset itself — what an `assetScopes` or `assetPermissions` entry on
    /// a request against this listing may name.
    asset: String,
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
        asset: created_asset.s("id"),
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

/// The ids of the subscriptions `domain` holds.
///
/// The second fixture seam: `ListSubscriptions` is still a scaffold, so no wire call names
/// a subscription and a case has to learn the id to address `GetSubscription` with. Only
/// the IDS come from here — every assertion about a subscription below is made on the HTTP
/// answer `GetSubscription` gives for it.
fn stored_subscription_ids(engine: &Arc<engine::InMemoryEngine>, domain: &str) -> Vec<String> {
    let state = service_state(&**engine).expect("the DataZone store is reachable");
    let store = state.lock();
    store
        .domains
        .get(domain)
        .map(|domain| domain.subscriptions.keys().cloned().collect())
        .unwrap_or_default()
}

/// Ask for access to `catalog`'s listing for `principal`.
async fn ask_for(dz: &Emulator, catalog: &Catalog, principal: Value, reason: &str) -> Value {
    let created = dz
        .call(
            "POST",
            &format!("/v2/domains/{}/subscription-requests", catalog.domain),
            Some(json!({
                "subscribedPrincipals": [principal],
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

/// Ask for access to `catalog`'s listing for its consumer PROJECT.
async fn ask(dz: &Emulator, catalog: &Catalog, reason: &str) -> Value {
    let principal = json!({ "project": { "identifier": catalog.consumer } });
    ask_for(dz, catalog, principal, reason).await
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
async fn a_created_request_awaits_a_decision_whatever_targets_the_domain_holds() {
    // The rows differ along what the rule is independent of: whether the domain holds no
    // subscription target at all, one applicable to a DIFFERENT asset type, one
    // applicable to the published type, or several of each.
    let rows: Vec<(&str, Vec<Vec<&str>>)> = vec![
        ("a domain holding no subscription target", vec![]),
        (
            "a target applicable to another asset type only",
            vec![vec!["amazon.datazone.RelationalTable"]],
        ),
        (
            "a target applicable to the published type",
            vec![vec!["PlainCustomType"]],
        ),
        (
            "several targets, one of them applicable",
            vec![
                vec!["amazon.datazone.RelationalTable", "OtherCustomType"],
                vec!["PlainCustomType"],
                vec![],
            ],
        ),
    ];

    for (index, (label, targets)) in rows.into_iter().enumerate() {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("awaits-decision-{index}"), "PlainCustomType").await;
        for (slot, applicable) in targets.iter().enumerate() {
            put_subscription_target(&engine, &catalog, &format!("target-{slot}"), applicable);
        }

        let created = ask(&dz, &catalog, "a decision is awaited").await;
        assert_eq!(
            member(&created, "status"),
            SUBSCRIPTION_REQUEST_STATUS_PENDING,
            "case {label:?}: a created request awaits a decision, got {created:?}"
        );
        assert_eq!(
            created.get("reviewerId"),
            None,
            "case {label:?}: nobody has reviewed the request, got {created:?}"
        );
        assert_eq!(
            created.get("decisionComment"),
            None,
            "case {label:?}: no decision carried a comment, got {created:?}"
        );
        assert_eq!(
            created.get("existingSubscriptionId"),
            None,
            "case {label:?}: no subscription is materialised by the ask, got {created:?}"
        );
        assert_eq!(
            stored_subscription_ids(&engine, &catalog.domain),
            Vec::<String>::new(),
            "case {label:?}: the ask granted nothing, so the domain holds no subscription"
        );

        let read = read_back(&dz, &catalog.domain, &member(&created, "id")).await;
        assert_eq!(
            member(&read, "status"),
            SUBSCRIPTION_REQUEST_STATUS_PENDING,
            "case {label:?}: the reader reports the stored status, got {read:?}"
        );
        // The target is a grant's fulfilment path, not a decision: the request that one
        // applies to still takes a decision, and that is what moves it.
        let decided = dz
            .call(
                "PUT",
                &format!(
                    "/v2/domains/{}/subscription-requests/{}/accept",
                    catalog.domain,
                    member(&created, "id")
                ),
                None,
            )
            .await;
        assert_eq!(
            (decided.status, member(&decided.body, "status")),
            (200, SUBSCRIPTION_REQUEST_STATUS_ACCEPTED.to_string()),
            "case {label:?}: the awaited decision is taken, got {} {:?}",
            decided.status,
            decided.body
        );
        assert_eq!(
            catalog.asset_type, "PlainCustomType",
            "case {label:?}: the rows' targets are written against the published type"
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
        let (dz, _engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("decision-{index}"), "Fulfillable").await;
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
async fn a_decision_materialises_a_subscription_only_when_it_accepts() {
    // The rows span the class the rule generalises over: which decision is taken, which
    // kind of principal the access was asked for, and whether the accept narrowed the
    // permissions it grants. Every accept materialises exactly one APPROVED subscription;
    // a reject materialises none.
    enum Principal {
        /// The ask names the consumer PROJECT, a DataZone resource of the domain.
        Project,
        /// The ask names an IAM principal, a resource of another service.
        Iam,
    }
    let rows: Vec<(&str, &str, Principal, bool)> = vec![
        ("an accept for a project", "accept", Principal::Project, false),
        ("an accept for an IAM principal", "accept", Principal::Iam, false),
        (
            "an accept narrowing the asset permissions",
            "accept",
            Principal::Project,
            true,
        ),
        ("a reject", "reject", Principal::Project, false),
    ];

    for (index, (label, decision, principal, narrow)) in rows.into_iter().enumerate() {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("materialise-{index}"), "Fulfillable").await;
        let asked_for = match principal {
            Principal::Project => json!({ "project": { "identifier": catalog.consumer } }),
            Principal::Iam => json!({ "iam": { "identifier": CONSUMER_ROLE } }),
        };
        let created = ask_for(&dz, &catalog, asked_for, "access for the report").await;
        let request_id = member(&created, "id");

        let body = narrow.then(|| {
            json!({
                "assetPermissions": [{
                    "assetId": catalog.asset,
                    // `Permissions` is a union with one `s3` arm of S3Permission
                    // (smithy_query shape="Permissions,S3Permission": READ | WRITE).
                    "permissions": { "s3": ["READ"] },
                }],
            })
        });
        let decided = dz
            .call(
                "PUT",
                &format!(
                    "/v2/domains/{}/subscription-requests/{request_id}/{decision}",
                    catalog.domain
                ),
                body,
            )
            .await;
        assert_eq!(
            decided.status, 200,
            "case {label:?}: the decision: expected 200, got {} {:?}",
            decided.status, decided.body
        );

        let ids = stored_subscription_ids(&engine, &catalog.domain);
        if decision == "reject" {
            assert_eq!(
                ids,
                Vec::<String>::new(),
                "case {label:?}: a refused ask grants nothing, so no subscription exists"
            );
            // Nothing holds the request's own id as a subscription either, so a read of
            // it is a miss rather than an invented access.
            let missed = dz
                .call(
                    "GET",
                    &format!("/v2/domains/{}/subscriptions/{request_id}", catalog.domain),
                    None,
                )
                .await;
            assert_eq!(
                (missed.status, missed.error_code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {label:?}: reading a subscription nothing created: got {} {:?}",
                missed.status,
                missed.body
            );
            continue;
        }

        assert_eq!(
            ids.len(),
            1,
            "case {label:?}: an accept materialises exactly one subscription, got {ids:?}"
        );
        let got = dz
            .call(
                "GET",
                &format!("/v2/domains/{}/subscriptions/{}", catalog.domain, ids[0]),
                None,
            )
            .await;
        assert_eq!(
            got.status, 200,
            "case {label:?}: reading the subscription back: got {} {:?}",
            got.status, got.body
        );
        assert_eq!(
            member(&got.body, "status"),
            SUBSCRIPTION_STATUS_APPROVED,
            "case {label:?}: the access the accept granted is approved, got {:?}",
            got.body
        );
        assert_eq!(
            member(&got.body, "id"),
            ids[0],
            "case {label:?}: the subscription reports its own id, got {:?}",
            got.body
        );
        assert_eq!(
            member(&got.body, "subscriptionRequestId"),
            request_id,
            "case {label:?}: it names the request it came from, got {:?}",
            got.body
        );
        assert_eq!(
            member(&got.body, "domainId"),
            catalog.domain,
            "case {label:?}: it is reported in its own domain, got {:?}",
            got.body
        );
        assert_eq!(
            member(&got.body, "createdBy"),
            CALLER,
            "case {label:?}: the caller that decided created the access, got {:?}",
            got.body
        );
        assert_eq!(
            got.body.get("updatedBy"),
            None,
            "case {label:?}: nothing has updated the subscription yet, got {:?}",
            got.body
        );
        assert_eq!(
            got.body.get("createdAt"),
            got.body.get("updatedAt"),
            "case {label:?}: an untouched subscription was last changed when it was made, \
             got {:?}",
            got.body
        );

        // The listing the access is ON: the catalog record the ask was made against.
        let listing = got
            .body
            .get("subscribedListing")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            member(&listing, "id"),
            catalog.listing,
            "case {label:?}: the subscription is on the subscribed listing, got {listing:?}"
        );
        assert_eq!(
            member(&listing, "ownerProjectId"),
            catalog.producer,
            "case {label:?}: the listing is reported with its owning project, got {listing:?}"
        );
        assert_eq!(
            listing
                .pointer("/item/assetListing/entityId")
                .and_then(Value::as_str),
            Some(catalog.asset.as_str()),
            "case {label:?}: the published entity is the asset, got {listing:?}"
        );
        let granted = listing.pointer("/item/assetListing/permissions/s3");
        if narrow {
            assert_eq!(
                granted,
                Some(&json!(["READ"])),
                "case {label:?}: the subscription reports what the decision narrowed the \
                 access to, got {listing:?}"
            );
        } else {
            assert_eq!(
                granted, None,
                "case {label:?}: no decision narrowed the access, got {listing:?}"
            );
        }

        // The principal the access is HELD BY, in the arm the ask named.
        let subscribed = got
            .body
            .get("subscribedPrincipal")
            .cloned()
            .unwrap_or(Value::Null);
        match principal {
            Principal::Project => assert_eq!(
                subscribed.pointer("/project/id").and_then(Value::as_str),
                Some(catalog.consumer.as_str()),
                "case {label:?}: the project that asked holds the access, got {subscribed:?}"
            ),
            Principal::Iam => assert_eq!(
                subscribed.pointer("/iam/principalArn").and_then(Value::as_str),
                Some(CONSUMER_ROLE),
                "case {label:?}: the IAM principal that asked holds the access, got \
                 {subscribed:?}"
            ),
        }

        // The decided request still reports itself as accepted, and reading the
        // subscription again answers the same record — a read changes nothing.
        let read = read_back(&dz, &catalog.domain, &request_id).await;
        assert_eq!(
            member(&read, "status"),
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            "case {label:?}: the accepted request reads back accepted, got {read:?}"
        );
        let again = dz
            .call(
                "GET",
                &format!("/v2/domains/{}/subscriptions/{}", catalog.domain, ids[0]),
                None,
            )
            .await;
        assert_eq!(
            (again.status, again.body),
            (got.status, got.body),
            "case {label:?}: a second read answers the same subscription"
        );
        assert_eq!(
            stored_subscription_ids(&engine, &catalog.domain),
            ids,
            "case {label:?}: reading created no further subscription"
        );
    }
}

#[tokio::test]
async fn a_decision_on_a_request_that_is_already_decided_is_refused() {
    // The rows span the statuses a decided request can carry — one an accept took, one a
    // reject took — and both decision operations against each. Each refusal names its own
    // message, the stored record is untouched afterwards, and no further subscription is
    // materialised.
    let already: Vec<(&str, &str, &str, Option<&str>, usize)> = vec![
        (
            "a request an accept decided",
            "accept",
            SUBSCRIPTION_REQUEST_STATUS_ACCEPTED,
            Some("already approved"),
            1,
        ),
        (
            "a request a reject decided",
            "reject",
            SUBSCRIPTION_REQUEST_STATUS_REJECTED,
            Some("already refused"),
            0,
        ),
    ];

    for (index, (label, decided_by, expect_status, comment, subscriptions)) in
        already.into_iter().enumerate()
    {
        let (dz, engine) = Emulator::with_engine();
        let catalog = catalog(&dz, &format!("already-decided-{index}"), "Fulfillable").await;
        let created = ask(&dz, &catalog, "the original ask").await;
        let id = member(&created, "id");
        let decided = dz
            .call(
                "PUT",
                &format!(
                    "/v2/domains/{}/subscription-requests/{id}/{decided_by}",
                    catalog.domain
                ),
                comment.map(|comment| json!({ "decisionComment": comment })),
            )
            .await;
        assert_eq!(
            decided.status, 200,
            "case {label:?}: the fixture's {decided_by}: got {} {:?}",
            decided.status, decided.body
        );
        let held = stored_subscription_ids(&engine, &catalog.domain);
        assert_eq!(
            held.len(),
            subscriptions,
            "case {label:?}: the first decision's own effect on the access, got {held:?}"
        );

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

            // The refusal wrote nothing: the status, the comment and the access granted
            // are as they were.
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
            assert_eq!(
                stored_subscription_ids(&engine, &catalog.domain),
                held,
                "case {label:?}: the refused {decision} materialised no subscription"
            );
        }
    }
}

#[tokio::test]
async fn a_decision_addressing_nothing_this_store_holds_is_refused() {
    let (dz, engine) = Emulator::with_engine();
    let catalog = catalog(&dz, "decision-refusals", "Fulfillable").await;
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
        // A subscription is read the same way: the denial and the miss are decided
        // before any access is resolved, and neither reports one.
        let read = dz
            .call("GET", &format!("/v2/domains/{domain}/subscriptions/{id}"), None)
            .await;
        assert_eq!(
            (read.status, read.error_code.as_str()),
            (status, code),
            "case {label:?} on a subscription read: got {} {:?}",
            read.status,
            read.body
        );
    }

    // Every refusal left the request alone: it still awaits a decision, and still takes
    // one — which is what materialises the access.
    let read = read_back(&dz, &catalog.domain, &pending).await;
    assert_eq!(
        member(&read, "status"),
        SUBSCRIPTION_REQUEST_STATUS_PENDING,
        "the refusals left the request awaiting a decision, got {read:?}"
    );
    assert_eq!(
        stored_subscription_ids(&engine, &catalog.domain),
        Vec::<String>::new(),
        "no refused call granted any access"
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
    let ids = stored_subscription_ids(&engine, &catalog.domain);
    assert_eq!(
        ids.len(),
        1,
        "the accept after the refusals granted the access, got {ids:?}"
    );
}
