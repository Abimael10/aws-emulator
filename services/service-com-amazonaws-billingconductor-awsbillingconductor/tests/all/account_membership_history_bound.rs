// SPDX-License-Identifier: CC0-1.0

//! The bounded HISTORY of billing-period records, over the two stores that keep one: an
//! account's billing group memberships and a pricing plan's rule associations.
//!
//! RULE: a half-open billing-period record that is in force in no billing period at all
//! — one ended in the very period it began in — is reportable for no period, so it is
//! not kept; and the bounded history a resource keeps counts and evicts only the ENDED
//! records, oldest first, so the record in force and the records an earlier period still
//! reports are never dropped to make room for records that report nothing.
//!
//! The rows differ along what the rule generalises over: which store keeps the history
//! (an account's memberships, a pricing plan's rule associations), how many times a
//! caller cycles one resource inside ONE billing period (once, twice, a handful, as many
//! as the bound and more than it), and how far past the bound a genuine ENDED history is
//! pushed (exactly at it, one over, many over).
//!
//! The observable end of the same rule is driven over the service's real `restJson1`
//! wire at the bottom of this file: `DisassociateAccounts` followed by
//! `AssociateAccounts` inside one billing period, repeated, leaves what an earlier
//! billing period reports — the account's `BillingGroupArn` and the group's `Size` —
//! exactly as it was.

use serde_json::json;
use service_com_amazonaws_billingconductor_awsbillingconductor::state::{
    billing_group_arn, current_billing_period, pricing_rule_arn, AccountAssociationRecord,
    PricingPlanRecord, PricingRuleAssociationRecord,
    MAX_ENDED_ACCOUNT_ASSOCIATIONS_PER_ACCOUNT as MAX_ENDED_MEMBERSHIPS,
    MAX_ENDED_PRICING_RULE_ASSOCIATIONS_PER_PRICING_PLAN as MAX_ENDED_RULE_ASSOCIATIONS,
};
use service_com_amazonaws_billingconductor_awsbillingconductor::ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState;

/// The harness every account-association suite in this crate drives, and the month
/// arithmetic the period-scoped suites share.
use crate::account_association_billing_periods::{current_period, Emulator};
use crate::pricing_rule_association_periods::shift_period;

/// A billing group ARN of the `[a-zA-Z0-9]{10}` shape the modelled `BillingGroupArn`
/// pattern allows, one per index.
fn group_arn(index: usize) -> String {
    billing_group_arn(&format!("G{index:09}"))
}

/// The membership of `group_arn` that ran over the half-open period range
/// `[start, end)` — the world no request can ask for, since an associating call happens
/// now.
fn membership(group_arn: &str, start: &str, end: Option<&str>) -> AccountAssociationRecord {
    AccountAssociationRecord {
        billing_group_arn: group_arn.to_string(),
        inclusive_start_billing_period: start.to_string(),
        exclusive_end_billing_period: end.map(str::to_string),
    }
}

/// The `BillingGroupArn` the store reports for `account_id` in `billing_period`, through
/// the one index every period-scoped read answers from.
fn reported_group(
    state: &BillingConductorState,
    account_id: &str,
    billing_period: &str,
) -> Option<String> {
    state
        .linked_accounts(billing_period, None)
        .get(account_id)
        .cloned()
        .flatten()
}

/// RULE, over an account's memberships: a membership released in the very billing period
/// it began in covers no period, so it is not kept — a caller cycling one account inside
/// one billing period leaves the stored history exactly as it was, and the membership an
/// earlier period reports stays reported.
#[test]
fn cycling_a_membership_inside_one_billing_period_keeps_no_record_and_loses_none() {
    let account = "111122223333";
    let past_group = group_arn(1);
    let cycled_group = group_arn(2);
    let now = current_billing_period();
    let past_start = shift_period(&now, -12);
    let past_end = shift_period(&past_start, 1);

    // Rows differ along how many associate/disassociate cycles the caller makes inside
    // the current billing period: one, two, a handful, as many as the bound on the kept
    // history, and more than it.
    for cycles in [
        1,
        2,
        7,
        MAX_ENDED_MEMBERSHIPS,
        MAX_ENDED_MEMBERSHIPS + 5,
    ] {
        let case = format!("{cycles} cycles inside the billing period {now}");
        let mut state = BillingConductorState::default();
        state.account_associations.insert(
            account.to_string(),
            vec![membership(&past_group, &past_start, Some(&past_end))],
        );

        for _ in 0..cycles {
            state.associate_account(account, &cycled_group);
            state.release_account(account, &cycled_group);
        }

        // The genuine past membership still reports its own period, in the listing and
        // in the group's size.
        let reported = reported_group(&state, account, &past_start);
        assert_eq!(
            reported.as_deref(),
            Some(past_group.as_str()),
            "case {case}: the membership of {past_start} must still be reported, got \
             {reported:?}"
        );
        let sizes = state.billing_group_sizes_in(std::slice::from_ref(&past_group), &past_start);
        assert_eq!(
            sizes.get(&past_group).copied(),
            Some(1),
            "case {case}: the group's Size in {past_start} must still count the account, got \
             {sizes:?}"
        );

        // Nothing the cycles left reports any period, so nothing of them is kept: the
        // stored history is the one past membership it started as.
        let stored = state
            .account_associations
            .get(account)
            .map(Vec::as_slice)
            .unwrap_or_default();
        assert_eq!(
            stored.len(),
            1,
            "case {case}: a cycle inside one billing period must leave no record, got \
             {stored:?}"
        );

        // And the account really is in no group now — the release stands.
        let current = reported_group(&state, account, &now);
        assert_eq!(
            current, None,
            "case {case}: the released account belongs to no group in {now}, got {current:?}"
        );
    }
}

/// RULE, over an account's memberships: the kept history is bounded by the ENDED
/// memberships alone, oldest dropped first, so the membership in force survives however
/// long the history is and every evicted period reads back the way it read before
/// anybody recorded it.
#[test]
fn the_kept_membership_history_is_bounded_and_never_evicts_the_one_in_force() {
    let account = "444455556666";
    let live_group = group_arn(999);
    let now = current_billing_period();

    // Rows differ along how far past the bound the ended history is pushed: exactly at
    // it, one over, and many over.
    for ended in [
        MAX_ENDED_MEMBERSHIPS,
        MAX_ENDED_MEMBERSHIPS + 1,
        MAX_ENDED_MEMBERSHIPS + 10,
    ] {
        let case = format!("{ended} ended memberships against a bound of {MAX_ENDED_MEMBERSHIPS}");
        let mut state = BillingConductorState::default();

        // One ended membership per period, oldest first, each in force in exactly the
        // period it began in and each of a different group, so which one a period
        // reports says which one survived.
        let records: Vec<AccountAssociationRecord> = (0..ended)
            .map(|index| {
                let start = shift_period("2000-01", i64::try_from(index).expect("a small count"));
                let end = shift_period(&start, 1);
                membership(&group_arn(index), &start, Some(&end))
            })
            .collect();
        state
            .account_associations
            .insert(account.to_string(), records);

        // The write that enforces the bound: the account joins a group now.
        state.associate_account(account, &live_group);

        let stored = state
            .account_associations
            .get(account)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let kept_ended = stored
            .iter()
            .filter(|record| record.exclusive_end_billing_period.is_some())
            .count();
        assert_eq!(
            kept_ended, MAX_ENDED_MEMBERSHIPS,
            "case {case}: expected the ended history capped at {MAX_ENDED_MEMBERSHIPS}, got \
             {kept_ended}"
        );

        // The membership in force is untouched, whatever was evicted.
        let in_force = state.account_billing_group(account);
        assert_eq!(
            in_force,
            Some(live_group.as_str()),
            "case {case}: the membership in force must survive, got {in_force:?}"
        );
        let current = reported_group(&state, account, &now);
        assert_eq!(
            current.as_deref(),
            Some(live_group.as_str()),
            "case {case}: the current period reports the group in force, got {current:?}"
        );

        // The evicted ones are the OLDEST: the newest ended membership is still reported
        // for the period it ran in, and every membership beyond the bound is reported
        // nowhere.
        let newest = ended - 1;
        let newest_period =
            shift_period("2000-01", i64::try_from(newest).expect("a small count"));
        let reported = reported_group(&state, account, &newest_period);
        assert_eq!(
            reported.as_deref(),
            Some(group_arn(newest).as_str()),
            "case {case}: the newest ended membership must still be reported for \
             {newest_period}, got {reported:?}"
        );
        for evicted in 0..ended.saturating_sub(MAX_ENDED_MEMBERSHIPS) {
            let period = shift_period("2000-01", i64::try_from(evicted).expect("a small count"));
            let reported = reported_group(&state, account, &period);
            assert_eq!(
                reported, None,
                "case {case}: the evicted membership {evicted} must be reported for no period, \
                 got {reported:?} at {period}"
            );
        }
    }
}

/// RULE, over a pricing plan's rule associations — the SAME pruning, reached from the
/// other store that keeps a billing-period history: an association ended in the very
/// period it began in is not kept, so cycling one rule inside one billing period leaves
/// the record as it was and the association an earlier period reports stays reported.
#[test]
fn cycling_a_rule_association_inside_one_billing_period_keeps_no_record_and_loses_none() {
    let now = current_billing_period();
    let past_start = shift_period(&now, -12);
    let past_end = shift_period(&past_start, 1);
    let past_rule = pricing_rule_arn("PASTRULE01");
    let cycled_rule = pricing_rule_arn("CYCLERULE1");

    // The same rows as the membership history above: a caller cycling one rule inside
    // one billing period, up to and past the bound on the kept history.
    for cycles in [
        1,
        2,
        7,
        MAX_ENDED_RULE_ASSOCIATIONS,
        MAX_ENDED_RULE_ASSOCIATIONS + 5,
    ] {
        let case = format!("{cycles} rule association cycles inside the billing period {now}");
        let mut plan = PricingPlanRecord::default();
        plan.pricing_rule_associations
            .push(PricingRuleAssociationRecord {
                arn: past_rule.clone(),
                inclusive_start_billing_period: past_start.clone(),
                exclusive_end_billing_period: Some(past_end.clone()),
            });

        for _ in 0..cycles {
            plan.pricing_rule_associations
                .push(PricingRuleAssociationRecord::started_now(&cycled_rule));
            plan.end_rule_associations(std::slice::from_ref(&cycled_rule), &now);
        }

        let reported = plan.associated_rule_arns(&past_start);
        assert_eq!(
            reported,
            vec![past_rule.clone()],
            "case {case}: the association of {past_start} must still be reported, got \
             {reported:?}"
        );
        assert_eq!(
            plan.pricing_rule_associations.len(),
            1,
            "case {case}: a cycle inside one billing period must leave no record, got {:?}",
            plan.pricing_rule_associations
        );
        let current = plan.associated_rule_arns(&now);
        assert!(
            current.is_empty(),
            "case {case}: the ended association is in force in no period, got {current:?}"
        );
    }
}

/// RULE over the WIRE, the way a caller reaches it: `DisassociateAccounts` followed by
/// `AssociateAccounts` inside one billing period, repeated, changes nothing about what an
/// earlier billing period reports — the account is still in the group it was in then and
/// the group's `Size` still counts it.
#[tokio::test]
async fn account_cycles_inside_one_billing_period_keep_an_earlier_period_membership() {
    let em = Emulator::new();
    let plan = em.create_plan("CyclePlan").await;
    let now = current_period();

    // Rows differ along how many disassociate/associate cycles the caller makes inside
    // the current billing period: one, two, as many as the bound on the kept history,
    // and more than it.
    for (index, cycles) in [1, 2, MAX_ENDED_MEMBERSHIPS, MAX_ENDED_MEMBERSHIPS + 5]
        .into_iter()
        .enumerate()
    {
        let account = format!("{:012}", 700 + index);
        let group = em
            .create_group(&format!("CycleGroup{index}"), &plan, &[])
            .await;
        let case = format!("{cycles} cycles of account {account} inside {now}");

        // The account was a member for one month a year ago: made through the real wire
        // and then backdated, since an associating call happens now.
        em.associate(&group, &[account.as_str()]).await;
        let past_start = shift_period(&now, -12);
        let past_end = shift_period(&past_start, 1);
        em.membership_ran(&account, &past_start, Some(&past_end));
        // The group has to exist in every period this row reads.
        em.group_created_in(&group, &shift_period(&past_start, -1));

        let before = em.association_in(&case, &account, Some(&past_start)).await;
        assert_eq!(
            before.as_deref(),
            Some(group.as_str()),
            "case {case}: the backdated membership must be reported for {past_start}, got \
             {before:?}"
        );

        for _ in 0..cycles {
            em.disassociate(&group, &[account.as_str()]).await;
            em.associate(&group, &[account.as_str()]).await;
        }

        // The earlier period reports exactly what it reported before the cycles.
        let after = em.association_in(&case, &account, Some(&past_start)).await;
        assert_eq!(
            after, before,
            "case {case}: the membership of {past_start} must be untouched, expected \
             {before:?}, got {after:?}"
        );

        // And so does that period's `Size`, the one rule a billing group's membership
        // count is reported from.
        let (status, body) = em
            .post(
                "/list-billing-groups",
                &json!({"Filters": {"Arns": [group.clone()]}, "BillingPeriod": past_start}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: ListBillingGroups expected 200, got {status} with {body:?}"
        );
        let size = body["BillingGroups"][0]["Size"].as_i64();
        assert_eq!(
            size,
            Some(1),
            "case {case}: the group's Size in {past_start} must still count the account, got \
             {body:?}"
        );

        // The cycles left the account where the last one put it: in the group, now.
        let current = em.association_in(&case, &account, Some(&now)).await;
        assert_eq!(
            current.as_deref(),
            Some(group.as_str()),
            "case {case}: the last associate stands in {now}, got {current:?}"
        );
    }
}
