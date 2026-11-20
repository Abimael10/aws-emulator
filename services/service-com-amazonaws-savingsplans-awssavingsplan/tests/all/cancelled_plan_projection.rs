// SPDX-License-Identifier: CC0-1.0

//! What `DescribeSavingsPlans` reports for a plan whose queued purchase was
//! cancelled: the record this store holds for it, projected as it is stored.
//!
//! The rules asserted here, each over a table of plan types and of tag
//! spellings rather than over the one plan a recording happened to carry:
//!
//! * the described plan's `tags` are the stored tag set projected VERBATIM —
//!   every key as it was supplied, including one carrying the `aws:` prefix AWS
//!   reserves elsewhere, and exactly the keys the plan still carries after a
//!   selective removal — and `ListTagsForResource` reports the same set;
//! * the tag set survives the lifecycle: cancelling the queued purchase changes
//!   the plan's `state` and NOTHING else about the record, so a tag written
//!   before the cancellation is still reported after it;
//! * the plan's dimension members come from the scope of the offering it was
//!   bought from: a commitment sold for one region and instance family reports
//!   exactly those, and one sold for neither reports neither (no invented
//!   region, no invented family).
//!
//! The write paths are pinned by [`crate::tag_key_round_trip`] and
//! [`crate::tag_key_removal_round_trip`], and the cancellation's own refusals by
//! [`crate::queued_purchase_deletion`]; what this suite adds is the PROJECTION
//! of the stored record across the plan's life, which is what a client reading a
//! cancelled plan back sees. Everything goes through the same
//! `MultiServiceRouter` the emulator boots, reusing the harness in
//! [`crate::savings_plans_lifecycle`].

use serde_json::{json, Map, Value};
use service_com_amazonaws_savingsplans_awssavingsplan::state;

use crate::savings_plans_lifecycle::{text, Emulator, ONE_YEAR};

/// How far ahead a purchase is dated so that its term has not begun: a queued
/// purchase is the only one that can be cancelled.
const QUEUED_AHEAD_SECONDS: i64 = 86_400;

/// Now, in whole seconds since the epoch: what a `purchaseTime` carries.
fn now_seconds() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs(),
    )
    .expect("a clock this century")
}

/// The tag map a reader reported — `{}` when it reported no tags at all.
fn tags(body: &Value) -> Map<String, Value> {
    body.get("tags")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The value of an offering property (`SavingsPlanOfferingPropertyKey`), or
/// `None` when the offering is not scoped to it.
fn offering_property(offering: &Value, name: &str) -> Option<String> {
    offering
        .get("properties")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().find(|entry| text(entry, "name") == name))
        .map(|entry| text(entry, "value"))
}

/// A member of the described plan, or `None` when the plan does not carry a
/// value for it (an absent member and an explicit null are one answer: no
/// value).
fn member(plan: &Value, name: &str) -> Option<String> {
    plan.get(name)
        .filter(|value| !value.is_null())
        .map(|value| value.as_str().unwrap_or_default().to_string())
}

#[tokio::test]
async fn a_cancelled_queued_plan_reads_back_as_the_record_the_store_holds() {
    let emulator = Emulator::new();

    // (case, the tag key written at purchase) — the key set a caller may use,
    // spanning the spellings a tag key can take: a plain one, the prefix AWS
    // reserves elsewhere, that prefix in another case, a key that is itself an
    // ARN and a non-ASCII one. `TagMap` constrains neither key nor value
    // (smithy_query shape=TagMap,TagKey,TagValue), so every one of them is
    // stored and reported as supplied.
    let spellings: [(&str, &str); 5] = [
        ("a plain key", "team"),
        ("the prefix AWS reserves elsewhere", "aws:cost-centre:owner"),
        ("that prefix in mixed case", "Aws:Allocation"),
        ("an ARN as a key", "arn:aws:iam::000000000000:role/Billing"),
        ("non-ASCII", "équipe"),
    ];

    // (case, plan type) — the plan types differ in the SCOPE their offerings
    // are sold for, which is what the plan's dimension members report.
    for (scope_case, plan_type) in [
        ("a commitment sold for one region and instance family", "EC2Instance"),
        ("a commitment sold for neither", "Compute"),
    ] {
        let offering = emulator
            .offering(plan_type, state::PAYMENT_NO_UPFRONT, ONE_YEAR)
            .await;
        let offering_id = text(&offering, "offeringId");

        for (tag_case, key) in spellings {
            let case = format!("{scope_case} / {tag_case} ({key})");
            // Written at purchase: the key under test and a plainly spelled
            // sibling that is removed again below.
            let savings_plan_id = emulator
                .purchase(json!({
                    "savingsPlanOfferingId": offering_id,
                    "commitment": "0.1",
                    "purchaseTime": now_seconds() + QUEUED_AHEAD_SECONDS,
                    "tags": { key: "purchased", "Sibling": "removed later" },
                }))
                .await;
            let before = emulator.plan(&savings_plan_id).await;
            let arn = text(&before, "savingsPlanArn");
            assert_eq!(
                text(&before, "state"),
                "queued",
                "case {case:?}: a purchase dated ahead of now has not begun, got {:?}",
                text(&before, "state")
            );

            // A second key arrives through `TagResource`, and the sibling is
            // removed again, so the stored set is one the service assembled
            // from three writes rather than one request's map.
            for (operation, request) in [
                (
                    "TagResource",
                    json!({ "resourceArn": arn, "tags": { "Owner": "tagged" } }),
                ),
                (
                    "UntagResource",
                    json!({ "resourceArn": arn, "tagKeys": ["Sibling"] }),
                ),
            ] {
                let answer = emulator.call(operation, request.clone()).await;
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: {operation} {request} must be accepted, got {} {:?}",
                    answer.status, answer.body
                );
            }

            // The set the store now holds, as the service assembled it.
            let stored = tags(&json!({
                "tags": { key: "purchased", "Owner": "tagged" }
            }));

            let described = emulator.plan(&savings_plan_id).await;
            assert_eq!(
                tags(&described),
                stored,
                "case {case:?}: the described plan must carry exactly the stored tag set, got {:?}",
                tags(&described)
            );

            // Cancel the queued purchase: the state moves, the record does not.
            let cancelled = emulator
                .call(
                    "DeleteQueuedSavingsPlan",
                    json!({ "savingsPlanId": savings_plan_id }),
                )
                .await;
            assert_eq!(
                cancelled.status, 200,
                "case {case:?}: a queued purchase can be cancelled, got {} {:?}",
                cancelled.status, cancelled.body
            );

            let after = emulator.plan(&savings_plan_id).await;
            assert_eq!(
                text(&after, "state"),
                "queued-deleted",
                "case {case:?}: a cancelled purchase reads back as queued-deleted, got {:?}",
                text(&after, "state")
            );
            assert_eq!(
                tags(&after),
                stored,
                "case {case:?}: the cancellation must not touch the tag set, got {:?}",
                tags(&after)
            );
            let listed = emulator
                .call("ListTagsForResource", json!({ "resourceArn": arn }))
                .await;
            assert_eq!(
                tags(&listed.body),
                stored,
                "case {case:?}: both readers report the one stored set, got {:?}",
                listed.body
            );

            // Everything else about the record is what it was before the
            // cancellation: the state member is the only difference.
            let without_state = |plan: &Value| {
                let mut object = plan.as_object().cloned().unwrap_or_default();
                object.remove("state");
                object
            };
            assert_eq!(
                without_state(&after),
                without_state(&described),
                "case {case:?}: cancelling a queued purchase changes the plan's state and nothing \
                 else"
            );

            // The dimension members are the offering's scope, reported for a
            // commitment that has one and left unset for a commitment that
            // does not — never filled in with a value no offering sold.
            for (name, scope) in [
                (
                    "region",
                    offering_property(&offering, state::PROPERTY_REGION),
                ),
                (
                    "ec2InstanceFamily",
                    offering_property(&offering, state::PROPERTY_INSTANCE_FAMILY),
                ),
            ] {
                assert_eq!(
                    member(&after, name),
                    scope,
                    "case {case:?}: {name} must be the scope the offering was sold for, got {:?}",
                    member(&after, name)
                );
            }
        }
    }
}
