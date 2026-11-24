// SPDX-License-Identifier: CC0-1.0

//! A resource the subscription's PLAN TIER does not cover is refused as a statement about
//! the PLAN.
//!
//! RULE: eligibility is refused in the words of the question that refused it — a resource
//! the plan tier does not cover is answered with the fixed plan-tier sentence quoting the
//! tier the subscription is on and naming no resource, because the remedy is to change the
//! resource set or the tier; a resource refused because the association its owning service
//! must already hold is absent keeps the sentence that lists those resources, because the
//! remedy is to fix each one. Both travel in the modelled business-rule error
//! (`ValidationException`, HTTP 400), so the message is the only thing that tells a client
//! which rule it broke.
//!
//! The other class needs the service that OWNS the resource to answer, so it is asserted
//! where that service is mounted beside this one
//! (`core/router/tests/all/pricing_plan_subscription_resource_eligibility.rs`); what this
//! suite pins is the tier class and the fact that the two are worded apart.
//!
//! A tier refusal is also INERT, which is the rest of the rule a client sees: the
//! subscription keeps exactly the resources it covered, keeps its status, and still
//! publishes the entity tag it had — so that same tag still admits the next association of
//! a resource the tier DOES cover.
//!
//! The rows differ along what the rule generalises over: which resource kinds one request
//! names, how many of them the tier does not cover and where they sit in the request, what
//! the subscription already covers, and — for the coverage half — the tier itself, every
//! published one.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The HTTP harness, the body builder, the ARN builders and the settle poll are the sibling
// suites' — the ONE harness of this crate's integration tests — so this suite adds no
// second copy of any of them.
use super::subscription_lifecycle::{
    create_body_naming, distribution, hosted_zone, key_value_store, poll_until_active, web_acl,
};
use super::subscription_wire_form::{call, create, router, Answer};

/// The plan family whose resources the model places by tier: "For subscriptions in the
/// CloudFront plan family, the resources must include exactly one Amazon CloudFront
/// distribution and exactly one WAF web ACL. You can also include other supported
/// resources, such as Amazon Route 53 hosted zones, and CloudFront KeyValueStores"
/// (`smithy_query(query="doc",shape="CreateSubscriptionInput",member="resourceArns")`).
const FAMILY: &str = "CloudFront";

/// The tier that does not cover a KeyValueStore: the lowest of the published tiers
/// "`FREE`, `PRO`, `BUSINESS`, or `PREMIUM`"
/// (`smithy_query(query="doc",shape="Subscription",member="planTier")`).
const FREE_TIER: &str = "FREE";

/// Every published tier that is not the free one, from the same member doc.
const PAID_TIERS: [&str; 3] = ["PRO", "BUSINESS", "PREMIUM"];

/// The fixed text AWS states a plan-tier incompatibility with; its only variable part is
/// the tier the subscription is on, quoted.
///
/// Written out here rather than taken from the handler's own formatter — an assertion
/// against the implementation's own text could not fail — and visible to the sibling suites
/// of this test crate, so the expected sentence is written once.
pub(crate) fn incompatible_with_tier(tier: &str) -> String {
    format!(
        "Resources are incompatible with plan tier '{tier}': modify resources or select a \
         different tier."
    )
}

/// The fixed opening of the OTHER refusal class, the one that lists the resources whose
/// prerequisite association is absent: a tier refusal must not be worded like it.
const PREREQUISITE_PREFIX: &str = "The following resources are not eligible for this";

/// `AssociateResourcesToSubscription(arn, resources)` under `if_match`.
async fn associate(
    router: &MultiServiceRouter,
    arn: &str,
    resources: &[String],
    if_match: &str,
) -> Answer {
    call(
        router,
        "AssociateResourcesToSubscription",
        json!({"arn": arn, "resourceArns": resources}),
        Some(if_match),
    )
    .await
}

/// What `GetSubscription` reports for `arn`: `(the body, the ETag)`.
async fn read_back(router: &MultiServiceRouter, arn: &str, case: &str) -> (Value, String) {
    let read = call(router, "GetSubscription", json!({"arn": arn}), None).await;
    assert_eq!(
        read.status, 200,
        "case {case}: GetSubscription must answer the stored subscription, got {} {:?}",
        read.status, read.body
    );
    let etag = read.require_etag(case);
    (read.body, etag)
}

/// One request the tier refuses, and the association that must still be admitted after it.
struct Case {
    label: &'static str,
    /// The resources the subscription is created over.
    held: Vec<String>,
    /// The resources one request adds; at least one of them the free tier does not cover.
    named: Vec<String>,
    /// A resource the tier DOES cover, added after the refusal under the tag the refusal
    /// left in place.
    then_admitted: Vec<String>,
}

/// On the free tier, an association naming a resource the tier does not cover is refused
/// with the plan-tier sentence, changes nothing, and leaves the subscription as
/// associable as it was.
#[tokio::test]
async fn an_association_the_tier_refuses_states_the_plan_tier_and_changes_nothing() {
    let cases = vec![
        Case {
            label: "one KeyValueStore added to the required pair",
            held: vec![distribution("E1TIERMSG"), web_acl("acl-tier-msg")],
            named: vec![key_value_store("kvs-alone")],
            then_admitted: vec![hosted_zone("Z1TIERMSG")],
        },
        Case {
            label: "a KeyValueStore behind a resource the tier does cover",
            held: vec![distribution("E2TIERMSG"), web_acl("acl-tier-msg-2")],
            named: vec![hosted_zone("Z2TIERMSG"), key_value_store("kvs-behind")],
            then_admitted: vec![hosted_zone("Z3TIERMSG")],
        },
        Case {
            label: "two KeyValueStores, nothing else",
            held: vec![distribution("E3TIERMSG")],
            named: vec![key_value_store("kvs-one"), key_value_store("kvs-two")],
            then_admitted: vec![hosted_zone("Z4TIERMSG"), web_acl("acl-tier-msg-3")],
        },
    ];

    for case in &cases {
        // A router of its own per row: a resource is covered by at most one subscription,
        // so rows naming the same resources are independent accounts rather than one.
        let router = router();
        let label = case.label;
        let created = create(
            &router,
            create_body_naming(FAMILY, FREE_TIER, &case.held, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let etag_before = created.require_etag(label);

        let refused = associate(&router, &arn, &case.named, &etag_before).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: a resource the tier does not cover must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ValidationException"),
            "case {label:?}: unexpected error class for {:?}",
            refused.body
        );
        let expected = incompatible_with_tier(FREE_TIER);
        let message = refused.message(label);
        assert_eq!(
            message, expected,
            "case {label:?}: expected the message {expected:?}, got {message:?}"
        );
        assert!(
            !message.starts_with(PREREQUISITE_PREFIX),
            "case {label:?}: the tier refusal must not be worded as the prerequisite one, \
             got {message:?}"
        );
        for named in case.named.iter().chain(case.held.iter()) {
            assert!(
                !message.contains(named),
                "case {label:?}: the tier refusal names no resource, got {message:?}"
            );
        }
        assert_eq!(
            refused.opt_string("resourceId"),
            None,
            "case {label:?}: the tier refusal identifies no single resource, got {:?}",
            refused.body
        );

        // INERT: the refusal wrote nothing — the same resources, the same status, the same
        // tag.
        let (read, etag_after) = read_back(&router, &arn, label).await;
        assert_eq!(
            read["resourceArns"],
            json!(case.held),
            "case {label:?}: a refused association must keep the resources it covered, got {read:?}"
        );
        assert_eq!(
            read["status"].as_str(),
            Some("ACTIVE"),
            "case {label:?}: a refused association must leave the status alone, got {read:?}"
        );
        assert_eq!(
            etag_after, etag_before,
            "case {label:?}: a refused association must not move the ETag"
        );

        // ...so the tag the refusal left in place still admits a resource the tier covers.
        let admitted = associate(&router, &arn, &case.then_admitted, &etag_before).await;
        assert_eq!(
            admitted.status, 200,
            "case {label:?}: the unmoved tag must still admit a covered resource, got {} {:?}",
            admitted.status, admitted.body
        );
        let covering: Vec<String> = case
            .held
            .iter()
            .chain(case.then_admitted.iter())
            .cloned()
            .collect();
        assert_eq!(
            admitted.body["resourceArns"],
            json!(covering),
            "case {label:?}: expected the subscription to cover {covering:?}, got {:?}",
            admitted.body["resourceArns"]
        );

        // The change settles on the clock, and what was refused never joined.
        let settled = poll_until_active(&router, &arn, label).await;
        assert_eq!(
            settled.body["resourceArns"],
            json!(covering),
            "case {label:?}: the settled subscription must cover {covering:?}, got {:?}",
            settled.body["resourceArns"]
        );
    }
}

/// The same request the free tier refuses is ADMITTED on every published paid tier, which
/// is what makes the refusal a statement about the plan: the resource is the same, only the
/// tier the subscription is on differs.
#[tokio::test]
async fn the_same_resource_joins_on_every_tier_that_covers_it() {
    for tier in PAID_TIERS {
        let router = router();
        let held = vec![distribution("E1TIERCOVER"), web_acl("acl-tier-cover")];
        let created = create(
            &router,
            create_body_naming(FAMILY, tier, &held, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let etag = created.require_etag(tier);

        let store = key_value_store("kvs-covered");
        let answered = associate(&router, &arn, std::slice::from_ref(&store), &etag).await;
        assert_eq!(
            answered.status, 200,
            "case {tier:?}: a tier that covers the resource must admit it, got {} {:?}",
            answered.status, answered.body
        );
        let covering: Vec<String> = held.iter().cloned().chain([store.clone()]).collect();
        assert_eq!(
            answered.body["resourceArns"],
            json!(covering),
            "case {tier:?}: expected the subscription to cover {covering:?}, got {:?}",
            answered.body["resourceArns"]
        );

        // The ROUND TRIP: what the association stored is what a read reports, once the
        // change has settled on the clock.
        let settled = poll_until_active(&router, &arn, tier).await;
        assert_eq!(
            settled.body["resourceArns"],
            json!(covering),
            "case {tier:?}: the settled subscription must cover {covering:?}, got {:?}",
            settled.body["resourceArns"]
        );
    }
}

/// A `CreateSubscription` the tier refuses states the SAME sentence — the wording belongs
/// to the question, not to the operation — and stores nothing.
///
/// The rows differ along the tier asked for and the resource kinds the request names.
#[tokio::test]
async fn a_create_the_tier_refuses_states_the_same_plan_tier_sentence() {
    // (case, the resources the request names)
    let rows: [(&str, Vec<String>); 3] = [
        (
            "a KeyValueStore alone",
            vec![key_value_store("kvs-create-alone")],
        ),
        (
            "a KeyValueStore beside the required pair",
            vec![
                distribution("E1TIERCREATE"),
                web_acl("acl-tier-create"),
                key_value_store("kvs-create-beside"),
            ],
        ),
        (
            "a KeyValueStore before a resource the tier covers",
            vec![
                key_value_store("kvs-create-first"),
                hosted_zone("Z1TIERCREATE"),
            ],
        ),
    ];

    for (case, named) in &rows {
        let router = router();
        let answered = call(
            &router,
            "CreateSubscription",
            create_body_naming(FAMILY, FREE_TIER, named, Some("IMMEDIATE")),
            None,
        )
        .await;
        assert_eq!(
            answered.status, 400,
            "case {case:?}: creating over a resource the tier does not cover must be \
             refused, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.error_code.as_deref(),
            Some("ValidationException"),
            "case {case:?}: unexpected error class for {:?}",
            answered.body
        );
        let expected = incompatible_with_tier(FREE_TIER);
        let message = answered.message(case);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );

        // A refused create stores no subscription at all.
        let listed = call(&router, "ListSubscriptions", json!({}), None).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListSubscriptions must answer, got {} {:?}",
            listed.status, listed.body
        );
        let subscriptions = listed.body["subscriptions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            subscriptions.is_empty(),
            "case {case:?}: a refused create must store no subscription, got {subscriptions:?}"
        );
    }
}
