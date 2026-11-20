// SPDX-License-Identifier: CC0-1.0

//! The ONE idempotency-token reclamation rule of this crate, driven through a delete of
//! EVERY resource kind this service removes — `DeleteBillingGroup`,
//! `DeleteCustomLineItem`, `DeletePricingRule` and `DeletePricingPlan` — over the real
//! `restJson1` wire through the harness in `tests/all/payer_account_gate.rs`, whose
//! `call_with_headers` is the seam the `@httpHeader`-bound `ClientToken` arrives through.
//!
//! RULE: a `ClientToken` names the resource its first call minted, and "an API request
//! completes no more than one time" only while that resource exists
//! (`smithy_query(query="shape",shape="ClientToken")`), so removing a resource forgets
//! every token entry that named it: a repeated create carrying the same token then does
//! the work again and mints a NEW resource instead of answering with the ARN of one that
//! is gone, while a token naming a resource the delete did NOT remove goes on replaying
//! into that resource — whichever operation removed the resource, because every removal
//! reclaims its tokens through the store's one reclamation path.
//!
//! The rows differ along the resource KIND removed (all four the store removes, one per
//! removal method) and, in the second suite, along which kind's delete acted while the
//! others' tokens stayed live — never along a renaming. A re-create asks for a name of
//! its own: the name a deleted resource occupied is a rule of its own
//! (`tests/all/billing_group_period_occupancy.rs`), so a name nothing else claims is what
//! leaves these rows about the token alone.

use crate::payer_account_gate::Emulator;
use serde_json::{json, Value};

/// An AWS-managed pricing plan the billing groups here compute with: the
/// `PricingPlanArn` pattern enumerates it, so a group needs no plan of this account's
/// own and the plan rows below are free to delete the plan they created
/// (`smithy_query(query="shape",shape="PricingPlanArn")`).
const MANAGED_PRICING_PLAN: &str = "arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan";

/// The header the model binds every `@idempotencyToken ClientToken` of this service to.
const CLIENT_TOKEN_HEADER: &str = "x-amzn-client-token";

/// POST `body` to `path` carrying `token` in the `ClientToken` header, and report the
/// status, the body and the ARN the answer named (empty when it named none).
async fn create(em: &Emulator, path: &str, body: &Value, token: &str) -> (u16, Value, String) {
    let (status, answered, _) = em
        .call_with_headers(
            "POST",
            path,
            Some(body),
            &[(CLIENT_TOKEN_HEADER, token)],
        )
        .await;
    let arn = answered
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, answered, arn)
}

/// POST a delete of `arn` to `path`, carrying no `ClientToken` of its own.
async fn delete(em: &Emulator, path: &str, arn: &str) -> (u16, Value) {
    let (status, answered, _) = em.call("POST", path, Some(&json!({ "Arn": arn }))).await;
    (status, answered)
}

/// One removal: the create that mints the resource a token names, the delete that removes
/// it, and the create that asks for the same token again afterwards.
struct Removal {
    /// What this row removes, named in every assertion of it.
    label: &'static str,
    create_path: &'static str,
    delete_path: &'static str,
    /// The create the token's first call carries.
    create: Value,
    /// The create the repeated token carries after the delete: the same request but for a
    /// name of its own, so only the token decides the answer.
    re_create: Value,
    /// This row's token, distinct per row so no row can pass on another's entry.
    token: &'static str,
}

/// A billing group create for `name`, holding `account` and computing with the managed
/// plan, so no plan of this account's own is involved.
fn billing_group(name: &str, account: &str) -> Value {
    json!({
        "Name": name,
        "AccountGrouping": { "LinkedAccountIds": [account] },
        "ComputationPreference": { "PricingPlanArn": MANAGED_PRICING_PLAN },
        "PrimaryAccountId": account,
    })
}

/// A flat-fee custom line item create for `name`, applying to `group_arn`.
fn custom_line_item(name: &str, group_arn: &str) -> Value {
    json!({
        "Name": name,
        "Description": "a custom line item whose ClientToken a delete reclaims",
        "BillingGroupArn": group_arn,
        "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 7.5 } },
    })
}

/// A global markup pricing rule create for `name`.
fn pricing_rule(name: &str) -> Value {
    json!({
        "Name": name,
        "Scope": "GLOBAL",
        "Type": "MARKUP",
        "ModifierPercentage": 2.5,
    })
}

/// A pricing plan create for `name`, naming no rule, so nothing holds it back from being
/// deleted.
fn pricing_plan(name: &str) -> Value {
    json!({ "Name": name })
}

/// The rows: one per removal method of the store, each with the host `group_arn` the
/// custom line item applies to.
fn rows(group_arn: &str) -> Vec<Removal> {
    vec![
        Removal {
            label: "a deleted billing group",
            create_path: "/create-billing-group",
            delete_path: "/delete-billing-group",
            create: billing_group("ReclaimedGroup", "222233334444"),
            re_create: billing_group("ReclaimedGroupAgain", "222233334444"),
            token: "group-token",
        },
        Removal {
            label: "a deleted custom line item",
            create_path: "/create-custom-line-item",
            delete_path: "/delete-custom-line-item",
            create: custom_line_item("ReclaimedItem", group_arn),
            re_create: custom_line_item("ReclaimedItemAgain", group_arn),
            token: "item-token",
        },
        Removal {
            label: "a deleted pricing rule",
            create_path: "/create-pricing-rule",
            delete_path: "/delete-pricing-rule",
            create: pricing_rule("ReclaimedRule"),
            re_create: pricing_rule("ReclaimedRuleAgain"),
            token: "rule-token",
        },
        Removal {
            label: "a deleted pricing plan",
            create_path: "/create-pricing-plan",
            delete_path: "/delete-pricing-plan",
            create: pricing_plan("ReclaimedPlan"),
            re_create: pricing_plan("ReclaimedPlanAgain"),
            token: "plan-token",
        },
    ]
}

/// Create the billing group the custom line item rows apply to.
async fn host_group(em: &Emulator) -> String {
    let (status, body, arn) = create(
        em,
        "/create-billing-group",
        &billing_group("HostGroup", "111122223333"),
        "host-group-token",
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateBillingGroup: expected 200, got {status} with {body}"
    );
    arn
}

/// RULE: while the resource a `ClientToken` minted exists the token replays into it, and
/// once a delete has removed that resource the token names nothing — so the same create,
/// carrying the same token, does the work again and mints a NEW resource. Asserted
/// through a delete of every kind this store removes.
#[tokio::test]
async fn every_delete_reclaims_the_client_token_that_named_the_removed_resource() {
    let em = Emulator::payer();
    let group = host_group(&em).await;

    for row in rows(&group) {
        let label = row.label;

        let (status, body, first) = create(&em, row.create_path, &row.create, row.token).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the create expected 200, got {status} with {body}"
        );
        assert!(
            !first.is_empty(),
            "case {label:?}: expected the create to answer an Arn, got {body}"
        );

        // While the resource exists the token replays: the repeated request completes
        // successfully with the ARN of the first call and mints nothing.
        let (status, body, replayed) = create(&em, row.create_path, &row.create, row.token).await;
        assert_eq!(
            status, 200,
            "case {label:?}: a replayed ClientToken completes successfully, got {status} with \
             {body}"
        );
        assert_eq!(
            replayed, first,
            "case {label:?}: a replayed ClientToken answers with the first ARN, got {body}"
        );

        let (status, body) = delete(&em, row.delete_path, &first).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the delete expected 200, got {status} with {body}"
        );

        // The token named a resource that no longer exists, so it names nothing: the
        // repeated create does the work again.
        let (status, body, second) = create(&em, row.create_path, &row.re_create, row.token).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the create after the delete expected 200, got {status} with {body}"
        );
        assert_ne!(
            second, first,
            "case {label:?}: expected a NEW resource, got the deleted {first:?}"
        );
        assert!(
            !second.is_empty(),
            "case {label:?}: expected the re-create to answer an Arn, got {body}"
        );
    }
}

/// RULE: a delete reclaims the token entries that named the resource it removed and no
/// others — every token still naming a stored resource goes on replaying into it.
///
/// The rows differ along which kind's delete acted: a custom line item first, then a
/// pricing plan, each time leaving the tokens of the resources that are still there live.
#[tokio::test]
async fn a_delete_reclaims_only_the_tokens_that_named_the_removed_resource() {
    let em = Emulator::payer();
    let group = host_group(&em).await;

    // One token per kind, each naming a resource that exists right now.
    let live = rows(&group);

    let mut minted: Vec<String> = Vec::new();
    for row in &live {
        let (status, answered, arn) = create(&em, row.create_path, &row.create, row.token).await;
        assert_eq!(
            status, 200,
            "setup for {:?}: expected 200, got {status} with {answered}",
            row.label
        );
        minted.push(arn);
    }

    // Which kinds' deletes act, and in which order: two different removal methods of the
    // store, named by the row's own label so the cases cannot drift from `rows()`.
    let acting = ["a deleted custom line item", "a deleted pricing plan"];
    let mut removed: Vec<usize> = Vec::new();

    for label in acting {
        let index = live
            .iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| panic!("case {label:?}: no such row in rows()"));
        let (status, body) = delete(&em, live[index].delete_path, &minted[index]).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the delete expected 200, got {status} with {body}"
        );
        removed.push(index);

        for (row, live_row) in live.iter().enumerate() {
            let held = live_row.label;
            if removed.contains(&row) {
                // The resource this token named is gone, so the token names nothing: the
                // create does its work again and mints a new resource.
                let (status, answered, arn) =
                    create(&em, live_row.create_path, &live_row.re_create, live_row.token).await;
                assert_eq!(
                    status, 200,
                    "case {label:?}: the reclaimed token of {held:?} is served, got {status} \
                     with {answered}"
                );
                assert_ne!(
                    arn, minted[row],
                    "case {label:?}: the token of {held:?} must stop naming the removed \
                     resource, got {answered}"
                );
            } else {
                // The resource this token named is still stored, so the token still
                // replays into exactly it.
                let (status, answered, arn) =
                    create(&em, live_row.create_path, &live_row.create, live_row.token).await;
                assert_eq!(
                    status, 200,
                    "case {label:?}: the token of {held:?} still replays, got {status} with \
                     {answered}"
                );
                assert_eq!(
                    arn, minted[row],
                    "case {label:?}: the token of {held:?} must still answer with {:?}, got \
                     {answered}",
                    minted[row]
                );
            }
        }
    }
}
