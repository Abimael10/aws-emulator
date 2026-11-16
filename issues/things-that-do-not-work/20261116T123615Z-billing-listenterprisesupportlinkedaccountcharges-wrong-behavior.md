Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 6da3155c33a76acb7439be1ec6d6d9dad4c485c0

# billableSeconds sums overlapping subscription windows instead of the part of the period they cover, so the prorated spend can exceed the account's total spend

_Filed: 2026-11-16T12:36:15Z_
_Operation: ListEnterpriseSupportLinkedAccountCharges | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `6da3155c33a76acb7439be1ec6d6d9dad4c485c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`linked_account_charge` counts an account's clipped Enterprise Support subscription windows with `crate::state::covered_seconds`, which folds each window's length into a running sum WITHOUT merging windows that overlap. Two recorded subscription periods that overlap inside the requested billing period are therefore counted twice over:

* `billableSeconds` comes out LARGER than `totalSeconds` — i.e. more billable seconds than the billing period has — and
* `proratedTotalSupportEligibleSpend`, computed as `prorate(total_spend_minor, billable_seconds, total_seconds)`, comes out larger than the `totalSupportEligibleSpend` reported beside it: a share of a period that is greater than the whole.

Both the handler's own doc above the function and the suite that pins the rule state the opposite rule — `billableSeconds` is "the part of the period covered by the account's recorded subscription periods" — and `covered_seconds`' own doc says it answers "the whole seconds the windows in `clipped` COVER", which for overlapping windows is their union, not their sum. Nothing merges the windows on read, and nothing refuses them on write: `record_enterprise_support_linked_account` guards only the NUMBER of periods, with the explicit reason that "no stored record can make a response break the model's own bound" — exactly the invariant that is missing here.

The same unguarded-record class shows in a second member of this response: `linked_account_charge` projects every `by_service` line of the stored spend record 1:1 into `supportEligibleSpendByService`, whose modelled `@length` maximum is 200 here (and 50 on `BusinessSupportAccountCharge`, which the sibling read builds from the same stored vector), while `record_support_eligible_spend` bounds only the number of spend RECORDS and never the number of lines inside one — unlike the three projections in the same crate (`additional_charge_list`, `pricing_plans_for_period`, `enterprise_support_contract_accounts`) that each truncate to the modelled list bound.

The fix is local to the reader (count the union of the clipped windows) or to the recorder (reject/normalise overlapping periods the way it already rejects over-long lists); for every non-overlapping record — every row the existing suite covers — the response is unchanged.

## Evidence

Model (smithy_query shape=LinkedAccountCharge):
* `billableSeconds` — "The number of billable seconds in the billing period based on when the account was subscribed to Enterprise Support."
* `totalSeconds` — "The total number of seconds in the billing period."
* `proratedTotalSupportEligibleSpend` — "The prorated total support-eligible spend based on when the account was subscribed to Enterprise Support."
* `supportEligibleSpendByService` — target `ServiceLevelAccountUsageList`, `"length": {"max": 200}`; `BusinessSupportAccountCharge$supportEligibleSpendByService` — `"length": {"max": 50}`.

The rule the crate states, services/service-com-amazonaws-billing-awsbilling/src/ops/list_enterprise_support_linked_account_charges.rs:249-253:
```
/// `billableSeconds` is "the number of billable seconds in the billing period
/// based on when the account was subscribed to Enterprise Support": the part of
/// the period covered by the account's recorded subscription periods, or — for
/// an account whose subscription intervals are not recorded — the whole period,
```

The code under it, same file:264-284:
```
    let total_seconds = period_end_secs.saturating_sub(period_start_secs);
...
    let billable_seconds = subscription_windows
        .as_deref()
        .map_or(total_seconds, covered_seconds);
...
        prorated_total_support_eligible_spend: amount(prorate(
            total_spend_minor,
            billable_seconds,
            total_seconds,
        )),
```

and the counter it uses, services/service-com-amazonaws-billing-awsbilling/src/state/linked_accounts.rs:186-193 — a SUM, with no union:
```
/// The whole seconds the windows in `clipped` cover.
///
/// Saturating, so no set of stored periods can overflow the count.
pub fn covered_seconds(clipped: &[(i64, i64)]) -> i64 {
    clipped.iter().fold(0_i64, |total, (begin, end)| {
        total.saturating_add(end.saturating_sub(*begin))
    })
}
```

The sibling writer that DOES refuse a stored record a response cannot report, linked_accounts.rs:122-125 and :209-213:
```
    /// The most periods either list may hold: the length the modeled
    /// `TimePeriodList` carries (`len ..500`), so no stored record can make a
    /// response break the model's own bound.
    pub const MAX_PERIODS: usize = 500;
...
        if account.linked_periods.len() > EnterpriseSupportLinkedAccount::MAX_PERIODS
            || account.subscription_periods.len() > EnterpriseSupportLinkedAccount::MAX_PERIODS
        {
            return false;
        }
```
Nothing in that writer constrains the periods' VALUES, so overlapping windows are stored unchanged.

Second symptom — unbounded `by_service` projection, list_enterprise_support_linked_account_charges.rs:294-303:
```
        support_eligible_spend_by_service: Some(
            spend
                .by_service
                .iter()
                .map(|line| ServiceLevelAccountUsage { ... })
                .collect(),
        ),
```
against the only writer of that vector, src/state/support_charges.rs:212-221, which checks residency alone:
```
    pub fn record_support_eligible_spend(&mut self, spend: SupportEligibleSpend) -> bool {
        let key = spend.key();
        if !self.support_eligible_spend.contains_key(&key)
            && self.support_eligible_spend.len() >= SupportEligibleSpend::MAX_RESIDENT
        {
            return false;
        }
```

Test coverage: tests/all/enterprise_support_linked_account_charges.rs:439-442 states the rule the code breaks — "`billableSeconds` is the part of the billing period the account's recorded subscription periods cover ... and `proratedTotalSupportEligibleSpend` is the account's spend in that share of the period" — and its rows (file:457-517) use only DISJOINT windows ("two separate subscription periods" => 10 billable days), so the overlapping case is pinned nowhere.

## Steps to Reproduce

1. Build the emulator with its engine and reach this service's state as tests/all/enterprise_support_linked_account_charges.rs does.
2. Record a linked account whose `subscription_periods` hold two OVERLAPPING windows that each cover the whole of a past billing month, e.g. `vec![SupportTimePeriod::between(day_of_past_month(1), day_of_past_month(PAST_MONTH_DAYS + 1)), SupportTimePeriod::between(day_of_past_month(1), day_of_past_month(PAST_MONTH_DAYS + 1))]`, through `record_enterprise_support_linked_account` (it answers `true`: only the period COUNT is bounded).
3. Record that account's Support-eligible spend for the same month on the `AWSSupportEnterprise` plan through `record_support_eligible_spend`, with a total of 1000.00.
4. Call `ListEnterpriseSupportLinkedAccountCharges` with that `billingMonth` as the payer account.
5. Observe the entry reports `billableSeconds` = twice `totalSeconds`, and `proratedTotalSupportEligibleSpend` = "2000.00" beside `totalSupportEligibleSpend` = "1000.00" — a prorated share larger than the whole, where the union of the recorded windows is exactly one month and the prorated spend should equal the total.
6. For the secondary symptom: record one spend record whose `by_service` holds 60 lines and call the same read (and `ListBusinessSupportAccountCharges`); every line is projected, so `supportEligibleSpendByService` exceeds the modelled `@length` maximum of 50 that `BusinessSupportAccountCharge` declares.
