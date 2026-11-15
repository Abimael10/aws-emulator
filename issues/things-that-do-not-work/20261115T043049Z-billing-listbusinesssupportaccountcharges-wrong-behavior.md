Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: a22dc19d5e32998b957368c08025d606fc68b0e9

# The Business Support charge report prices and reports every account-month bill of the month, including bills recording the Enterprise Support plan, which the modeled supportPlanName does not admit

_Filed: 2026-11-15T04:30:49Z_
_Operation: ListBusinessSupportAccountCharges | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `a22dc19d5e32998b957368c08025d606fc68b0e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListBusinessSupportAccountCharges` walks `support_eligible_spend_for_month` and prices EVERY record of the billing month, with no filter on the Support plan the bill records. It then reports each record as a `BusinessSupportAccountCharge` carrying the stored plan name verbatim, and counts every record in the month-wide `totalSupportCharge`, `totalSupportEligibleSpend` and `accountCount`.

The store deliberately holds account-months recording `AWSSupportEnterprise` — that is how this crate represents an account inside the Enterprise Support profile, and `enterprise_support_spend_entries_for_month` selects exactly those records for the two Enterprise reads. So for a month in which any account of the family was on Enterprise Support, this read answers with a `BusinessSupportAccountCharge` whose `supportPlanName` is `AWSSupportEnterprise` — a value the modeled member's documented value list does not contain — priced at 0.00 by the fall-through schedule, and it adds that account's spend and its head-count to the month's Business Support totals, which therefore double-count the spend `GetEnterpriseSupportChargeSummary` reports for the same month.

Every other reader of the same store filters by plan: `support_subscription_contracts` skips a bill whose plan is not one of the three documented Business/Developer/Basic names, with the reason stated in the crate ("a bill recording another plan, such as the Enterprise Support plan an Enterprise Support profile's accounts are billed under, is that profile's contract to report and not one of these"), and the Enterprise reads select only `AWSSupportEnterprise`. This read is the only one with no plan predicate. The comment on `price_schedule`'s fall-through arm states only how such a record is PRICED ("reported with its spend and no charge"); it does not address whether a bill on a plan the output member cannot name belongs in this report at all.

## Evidence

Smithy, `BusinessSupportAccountCharge$supportPlanName`: "The Support plan name for this account. Valid values: `AWSSupportBusiness` (Business Support plan), `AWSSupportDeveloper` (Developer Support plan), `AWSSupportEssential` (Basic Support plan)."

No plan filter, and the month totals count every record — services/service-com-amazonaws-billing-awsbilling/src/ops/list_business_support_account_charges.rs:495-519:
```
        // Every account the month holds a bill for is priced, whichever one the
        // request asked about: the month's `totalSupportCharge`, ...
        for (key, spend) in state
            .support_eligible_spend_for_month(crate::state::caller_account_id(), &input.billing_month)
        {
            let (charge, charge_minor, basis_minor) = account_charge(spend);
            total_support_charge_minor = total_support_charge_minor.saturating_add(charge_minor);
            ...
            account_count = account_count.saturating_add(1);
```
The stored plan name is echoed into the modeled member — same file:390-392:
```
        BusinessSupportAccountCharge {
            account_id: spend.account_id.clone(),
            support_plan_name: spend.support_plan_name.clone(),
```
and an unknown plan falls through to a zero schedule — same file:209-245:
```
/// A stored plan this service has no schedule for is reported with its spend
/// and no charge, rather than priced by another plan's schedule.
fn price_schedule(support_plan_name: &str) -> PriceSchedule {
...
        _ => PriceSchedule { minimum_charge_minor: 0, tiers: &[] },
```
The sibling reader of the same store DOES filter, with its reason stated — services/service-com-amazonaws-billing-awsbilling/src/state/support_subscriptions.rs:83-104:
```
    /// Only a plan the modeled `planName` documents is reported ... A bill recording another plan, such as
    /// the Enterprise Support plan an Enterprise Support profile's accounts are
    /// billed under, is that profile's contract to report and not one of these.
...
            if !super::SUPPORT_PLAN_NAMES.contains(&spend.support_plan_name.as_str()) {
                continue;
            }
```
and the Enterprise reads select exactly the records this one also prices — services/service-com-amazonaws-billing-awsbilling/src/state/enterprise_support.rs:729-736:
```
        self.support_eligible_spend_for_month(payer_account_id, billing_month)
            .filter(|(_, spend)| spend.support_plan_name == SUPPORT_PLAN_ENTERPRISE)
```
Test coverage: tests/all/business_support_account_charges.rs:277-300 exercises only `SUPPORT_PLAN_BUSINESS`, `SUPPORT_PLAN_DEVELOPER` and `SUPPORT_PLAN_BASIC`, so no row pins the Enterprise case; tests/all/token_scope.rs:153-156 seeds two `SUPPORT_PLAN_ENTERPRISE` and two `SUPPORT_PLAN_BUSINESS` bills in one month and pages this read, but only asserts the walk holds at least two entries (tests/all/token_scope.rs:337-340).

## Steps to Reproduce

1. Record Support-eligible spend for billing month 2024-03 for account A with `support_plan_name = "AWSSupportBusiness"` and for account B with `support_plan_name = "AWSSupportEnterprise"` (both in the caller's consolidated billing family).
2. Call `ListBusinessSupportAccountCharges` with `billingMonth: "2024-03"`.
3. The answer contains an `accountCharges` entry for account B with `supportPlanName: "AWSSupportEnterprise"` and `totalCharge: "0.00"`, a value the modeled member's documented value list does not admit.
4. `accountCount` is 2 and `totalSupportEligibleSpend` includes account B's spend, which `GetEnterpriseSupportChargeSummary` for the same month also reports as the Enterprise profile's spend.
