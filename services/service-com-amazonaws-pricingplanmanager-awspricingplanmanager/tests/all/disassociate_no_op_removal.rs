// SPDX-License-Identifier: CC0-1.0

//! Removing a resource a subscription does not cover, and refusing a removal the resulting
//! set does not admit.
//!
//! RULE: `DisassociateResourcesFromSubscription` takes off exactly those of the named
//! resources the subscription covers, so a name the subscription does not cover takes
//! nothing off it and is no refusal — the call is answered with the resource set the removal
//! leaves behind, and a call that leaves that set equal to the stored one writes NOTHING:
//! the record keeps its status, its modification time and the concurrency token the caller
//! already holds. And when the set the removal would leave behind is one the subscription's
//! plan does not admit, the refusal is a statement about that plan — the modelled
//! business-rule failure (`ValidationException`, HTTP 400) whose sentence quotes the plan
//! tier read from the STORED subscription and names no resource, with nothing written
//! either.
//!
//! The rows differ along what the rule generalises over: which kinds of resource a request
//! names and how many, whether each of them is covered, where in the request list it sits,
//! how many the subscription holds, the plan family, and the plan tier — every published one
//! for the refusal half.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The HTTP harness, the body builder, the ARN builders and the settle poll are the sibling
// suites' — the ONE harness of this crate's integration tests — so this suite adds no
// second copy of any of them, and the expected plan-tier sentence is the one the suite that
// owns that wording writes.
use super::plan_tier_incompatibility_refusal::incompatible_with_tier;
use super::subscription_lifecycle::{
    create_body_naming, distribution, hosted_zone, key_value_store, poll_until_active, web_acl,
};
use super::subscription_wire_form::{call, create, router, Answer};

/// Every published plan tier, "`FREE`, `PRO`, `BUSINESS`, or `PREMIUM`"
/// (`smithy_query(query="doc", shape="Subscription", member="planTier")`).
const PUBLISHED_TIERS: [&str; 4] = ["FREE", "PRO", "BUSINESS", "PREMIUM"];

/// `DisassociateResourcesFromSubscription(arn, resources)` under `if_match`.
async fn disassociate(
    router: &MultiServiceRouter,
    arn: &str,
    resources: &[String],
    if_match: &str,
) -> Answer {
    call(
        router,
        "DisassociateResourcesFromSubscription",
        json!({"arn": arn, "resourceArns": resources}),
        Some(if_match),
    )
    .await
}

/// A removal naming nothing the subscription covers is answered with the stored
/// subscription, unchanged: the same resource set, the same status, the same modification
/// time and the same entity tag — which is why the call can be repeated under that same tag
/// and a real removal still follows it.
#[tokio::test]
async fn a_removal_naming_nothing_the_subscription_covers_writes_nothing() {
    struct Case {
        label: &'static str,
        family: &'static str,
        tier: &'static str,
        /// The resources the subscription is created over.
        held: Vec<String>,
        /// The resources one request names, none of which the subscription covers.
        named: Vec<String>,
        /// A resource the subscription DOES cover, removed afterwards under the tag the
        /// no-op left in place; empty for a subscription that may lose nothing.
        then_removed: Vec<String>,
    }

    let cases = vec![
        Case {
            label: "a hosted zone a CloudFront subscription never covered",
            family: "CloudFront",
            tier: "FREE",
            held: vec![
                distribution("E1NOOP"),
                web_acl("acl-noop"),
                hosted_zone("Z1NOOP"),
            ],
            named: vec![hosted_zone("ZNEVERHELD")],
            then_removed: vec![hosted_zone("Z1NOOP")],
        },
        Case {
            label: "resources of the very kinds the family requires, none of them covered",
            family: "CloudFront",
            tier: "PRO",
            held: vec![
                distribution("E2NOOP"),
                web_acl("acl-noop-2"),
                key_value_store("kvs-noop"),
            ],
            named: vec![distribution("E9NEVERHELD"), web_acl("acl-never-held")],
            then_removed: vec![key_value_store("kvs-noop")],
        },
        Case {
            label: "two kinds at once against a family with no required composition",
            family: "Route53",
            tier: "BUSINESS",
            held: vec![hosted_zone("Z2NOOP"), hosted_zone("Z3NOOP")],
            named: vec![key_value_store("kvs-never-held"), distribution("E8NEVERHELD")],
            then_removed: vec![hosted_zone("Z3NOOP")],
        },
        Case {
            label: "a subscription covering one resource, which the request does not name",
            family: "Route53",
            tier: "PREMIUM",
            held: vec![hosted_zone("Z4NOOP")],
            named: vec![hosted_zone("Z5NEVERHELD")],
            then_removed: Vec::new(),
        },
    ];

    for case in &cases {
        // A router of its own per row: a resource is covered by at most one subscription,
        // so rows naming the same resources are independent accounts rather than one.
        let router = router();
        let label = case.label;
        let created = create(
            &router,
            create_body_naming(case.family, case.tier, &case.held, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let etag = created.require_etag(label);

        let answered = disassociate(&router, &arn, &case.named, &etag).await;
        assert_eq!(
            answered.status, 200,
            "case {label:?}: a removal naming nothing covered must be answered 200, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body["resourceArns"],
            json!(case.held),
            "case {label:?}: expected the stored set {:?}, got {:?}",
            case.held,
            answered.body["resourceArns"]
        );
        assert_eq!(
            answered.string("status"),
            "ACTIVE",
            "case {label:?}: nothing is being applied, so the status must stay ACTIVE, got {:?}",
            answered.body
        );
        assert_eq!(
            answered.require_etag(label),
            etag,
            "case {label:?}: a call that wrote nothing must answer the tag the caller held"
        );
        assert_eq!(
            answered.body, created.body,
            "case {label:?}: the answer must be the record as it stood, got {:?}",
            answered.body
        );

        // The STORE agrees, down to the modification time: nothing was written.
        let fetched = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            fetched.status, 200,
            "case {label:?}: the subscription must still be readable, got {} {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.body, created.body,
            "case {label:?}: a removal that took nothing off must leave the record alone, got {:?}",
            fetched.body
        );
        assert_eq!(
            fetched.require_etag(label),
            etag,
            "case {label:?}: the reader must still report the token the caller held"
        );

        // So the same call under the same tag is answered the same way, as many times as a
        // convergence loop makes it.
        let again = disassociate(&router, &arn, &case.named, &etag).await;
        assert_eq!(
            again.status, 200,
            "case {label:?}: the same no-op must be answered again, got {} {:?}",
            again.status, again.body
        );
        assert_eq!(
            again.body, created.body,
            "case {label:?}: the repeated no-op must answer the same record, got {:?}",
            again.body
        );

        // ...and the unmoved tag still admits a removal of something the subscription DOES
        // cover, which is what proves the no-op consumed no concurrency token.
        if !case.then_removed.is_empty() {
            let removed = disassociate(&router, &arn, &case.then_removed, &etag).await;
            assert_eq!(
                removed.status, 200,
                "case {label:?}: the unmoved tag must still admit a real removal, got {} {:?}",
                removed.status, removed.body
            );
            let retained: Vec<String> = case
                .held
                .iter()
                .filter(|held| !case.then_removed.contains(held))
                .cloned()
                .collect();
            assert_eq!(
                removed.body["resourceArns"],
                json!(retained),
                "case {label:?}: expected the retained set {retained:?}, got {:?}",
                removed.body["resourceArns"]
            );
            assert_ne!(
                removed.require_etag(label),
                etag,
                "case {label:?}: a removal that wrote must hand out a new token"
            );
        }
    }
}

/// One request may name both: the resources the subscription covers come off, the ones it
/// does not are ignored, and that IS a write — the change is reported as being applied and
/// the tag moves.
#[tokio::test]
async fn a_removal_takes_off_the_covered_names_and_ignores_the_rest() {
    struct Case {
        label: &'static str,
        family: &'static str,
        tier: &'static str,
        held: Vec<String>,
        /// What one request names: covered and uncovered resources mixed.
        removed: Vec<String>,
        /// The set the subscription is left covering.
        retained: Vec<String>,
    }

    let cases = vec![
        Case {
            label: "an uncovered name behind a covered one",
            family: "CloudFront",
            tier: "FREE",
            held: vec![
                distribution("E1MIXED"),
                web_acl("acl-mixed"),
                hosted_zone("Z1MIXED"),
            ],
            removed: vec![hosted_zone("Z1MIXED"), hosted_zone("ZNOTMIXED")],
            retained: vec![distribution("E1MIXED"), web_acl("acl-mixed")],
        },
        Case {
            label: "an uncovered name between two covered ones",
            family: "Route53",
            tier: "PRO",
            held: vec![
                hosted_zone("Z2MIXED"),
                hosted_zone("Z3MIXED"),
                hosted_zone("Z4MIXED"),
            ],
            removed: vec![
                hosted_zone("Z4MIXED"),
                key_value_store("kvs-not-mixed"),
                hosted_zone("Z2MIXED"),
            ],
            retained: vec![hosted_zone("Z3MIXED")],
        },
        Case {
            label: "an uncovered name ahead of the covered one",
            family: "Route53",
            tier: "PREMIUM",
            held: vec![hosted_zone("Z5MIXED"), key_value_store("kvs-mixed")],
            removed: vec![distribution("E7NOTMIXED"), key_value_store("kvs-mixed")],
            retained: vec![hosted_zone("Z5MIXED")],
        },
    ];

    for case in &cases {
        let router = router();
        let label = case.label;
        let created = create(
            &router,
            create_body_naming(case.family, case.tier, &case.held, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let etag = created.require_etag(label);

        let answered = disassociate(&router, &arn, &case.removed, &etag).await;
        assert_eq!(
            answered.status, 200,
            "case {label:?}: the removal must succeed, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body["resourceArns"],
            json!(case.retained),
            "case {label:?}: expected the retained set {:?}, got {:?}",
            case.retained,
            answered.body["resourceArns"]
        );
        assert_eq!(
            answered.string("status"),
            "SYNC_IN_PROGRESS",
            "case {label:?}: a change that is being applied says so, got {:?}",
            answered.body
        );
        assert_ne!(
            answered.require_etag(label),
            etag,
            "case {label:?}: a write must hand out a new concurrency token"
        );

        // The ROUND TRIP: the reader reports what was stored, and the change settles on the
        // clock with exactly the retained set.
        let fetched = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            fetched.body, answered.body,
            "case {label:?}: the removal must report what it stored, got {:?}",
            fetched.body
        );
        let settled = poll_until_active(&router, &arn, label).await;
        assert_eq!(
            settled.body["resourceArns"],
            json!(case.retained),
            "case {label:?}: the settled subscription must keep {:?}, got {:?}",
            case.retained,
            settled.body["resourceArns"]
        );
    }
}

/// A removal the resulting set does not admit is refused as a statement about the PLAN: the
/// fixed sentence quoting the tier the STORED subscription is on, naming no resource and
/// identifying none — on every published tier, and inert.
#[tokio::test]
async fn a_removal_the_resulting_set_does_not_admit_states_the_stored_plan_tier() {
    for tier in PUBLISHED_TIERS {
        // Which of the two resources the CloudFront family requires the request would take
        // the last of: the rule is about the set left behind, so either one refuses.
        for (kind, index) in [("the distribution", 0), ("the web ACL", 1)] {
            let router = router();
            let label = format!("{tier} losing {kind}");
            let case = label.as_str();
            let held = vec![
                distribution(&format!("E1REQ{tier}{index}")),
                web_acl(&format!("acl-req-{tier}-{index}")),
                hosted_zone(&format!("Z1REQ{tier}{index}")),
            ];
            let created = create(
                &router,
                create_body_naming("CloudFront", tier, &held, Some("IMMEDIATE")),
            )
            .await;
            let arn = created.string("arn");
            let etag = created.require_etag(case);

            let refused = disassociate(&router, &arn, &held[index..=index], &etag).await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}: the removal must be refused 400, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("ValidationException"),
                "case {case:?}: unexpected error class for {:?}",
                refused.body
            );
            let expected = incompatible_with_tier(tier);
            let message = refused.message(case);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            for resource in &held {
                assert!(
                    !message.contains(resource),
                    "case {case:?}: the refusal names no resource, got {message:?}"
                );
            }
            assert_eq!(
                refused.opt_string("resourceId"),
                None,
                "case {case:?}: the refusal identifies no single resource, got {:?}",
                refused.body
            );

            // INERT: the stored subscription is exactly as it was.
            let fetched = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
            assert_eq!(
                fetched.body, created.body,
                "case {case:?}: a refused removal must change nothing, got {:?}",
                fetched.body
            );
            assert_eq!(
                fetched.require_etag(case),
                etag,
                "case {case:?}: a refused removal must not mint a new token"
            );
        }
    }
}
