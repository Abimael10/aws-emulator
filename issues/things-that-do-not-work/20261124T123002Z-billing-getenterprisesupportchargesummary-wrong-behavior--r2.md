Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: b9efb47143cf6ff4809e1d58bc20d5c480ad9c3b

# The charge summary adds Reserved Instance and Savings Plan fees to Support-eligible spend whatever the contract's treatment method is, including the documented "None" that excludes them

_Filed: 2026-11-24T12:30:02Z_
_Operation: GetEnterpriseSupportChargeSummary | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b9efb47143cf6ff4809e1d58bc20d5c480ad9c3b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The Enterprise Support contract this crate persists carries the two treatment methods the model documents — `reserved_instance_treatment_method` and `savings_plans_treatment_method` (src/state/enterprise_support.rs:404, :412) — and `GetEnterpriseSupportContractDetails` reports both and even gates the two amortization start dates on them. `GetEnterpriseSupportChargeSummary`, the read that actually PRICES the profile's spend, never consults either one: it sums `spend.reserved_instance_spend_minor` and `spend.savings_plan_spend_minor` into `total_support_eligible_spend` unconditionally, and that total is what `plan.charge(total_spend_minor)` prices and what `totalSupportCharge`, `supportCharge` and `supportChargePercentage` are derived from.

The model states what the method means in the charge calculation: `None` is "RI fees excluded from Support-eligible spend" (and, for the twin member, "Savings Plan fees excluded from Support-eligible spend"). So for a contract whose stored method is `None`, this emulator reports fees in `totalSupportEligibleSpend` / `totalSupportEligibleReservedInstanceSpend` / `totalSupportEligibleSavingsPlanSpend` that the model says are not Support-eligible at all, and charges the profile (and allocates to the payer) a Support charge computed from them. The crate is aware of the distinction: the `SUPPORT_TREATMENT_UPFRONT` constant's own doc says the Upfront reading — "the record holds one billing month's Reserved Instance and Savings Plan spend and the month's charge is computed from the whole of it" — is what a stored spend record carries, and the contract's `Default` hard-codes `Upfront` for exactly that reason. The handler then applies that one reading to every contract, so the stored method is write-and-report-only and the two `None` values the crate itself enumerates in `SUPPORT_TREATMENT_METHODS` change nothing about the charge.

The sibling report `ListEnterpriseSupportLinkedAccountCharges` reads the same records the same way (`total_support_eligible_spend_minor()` plus the separate RI/SP members), so the same unfiltered fees appear in the per-account breakdown; the charge summary is where the divergence decides money.

## Evidence

The model sentence that states the behaviour — `smithy_query(query="doc", shape="GetEnterpriseSupportContractDetailsResponse", member="supportReservedInstanceTreatmentMethod")`:
"The method used to include Reserved Instance (RI) fees in the Enterprise Support charge calculation. Valid values: None (RI fees excluded from Support-eligible spend), Upfront (full upfront RI fees included in month of purchase), Amortized (...), AmortizedCustom (...), AmortizedAll (...)."
and its twin, `member="supportSavingsPlansTreatmentMethod"`: "Valid values: None (Savings Plan fees excluded from Support-eligible spend), Upfront (...)".

The crate restates that same sentence on its own constant — services/service-com-amazonaws-billing-awsbilling/src/state/enterprise_support.rs:62-72:
```
/// `None`, a Reserved Instance or Savings Plans treatment method.
///
/// From the model's `supportReservedInstanceTreatmentMethod` doc, which lists
/// the values both treatment methods take: "None (RI fees excluded from
/// Support-eligible spend), Upfront (full upfront RI fees included in month of
/// purchase), ..."
pub const SUPPORT_TREATMENT_NONE: &str = "None";
```

The handler that prices the month, with no reference to either stored method — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs:299-318:
```
        for spend in
            state.enterprise_support_spend_for_month(&payer_account_id, &input.billing_month)
        {
            usage_spend_minor = usage_spend_minor.saturating_add(spend.usage_basis_minor());
            reserved_instance_spend_minor =
                reserved_instance_spend_minor.saturating_add(spend.reserved_instance_spend_minor);
            savings_plan_spend_minor =
                savings_plan_spend_minor.saturating_add(spend.savings_plan_spend_minor);
...
        let total_spend_minor = usage_spend_minor
            .saturating_add(reserved_instance_spend_minor)
            .saturating_add(savings_plan_spend_minor);
```
and that total is what the charge and every reported money member come from — get_enterprise_support_charge_summary.rs:360 `plan.charge(total_spend_minor)`, :365-370 `allocated_support_charge_minor(...)`, :388-391 `total_support_eligible_spend: amount(total_spend_minor)`, `total_support_eligible_reserved_instance_spend: amount(reserved_instance_spend_minor)`, `total_support_eligible_savings_plan_spend: amount(savings_plan_spend_minor)`, :395-398 `support_charge_percentage: rate_decimal(effective_rate_basis_points(charge.total_minor, total_spend_minor))`.

The sibling read of the SAME contract that does consult the methods — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_contract_details.rs:316-338:
```
            support_reserved_instance_treatment_method: Some(
                contract.reserved_instance_treatment_method.clone(),
            ),
            // "When supportReservedInstanceTreatmentMethod = AmortizedCustom,
            //  only amortized fees for Reserved Instances purchased on or after
            //  this date are included in the calculation. ..."
            support_reserved_instance_amortization_start_date: contract
                .reserved_instance_amortization_start_date
                .filter(|_| {
                    contract.reserved_instance_treatment_method
                        == SUPPORT_TREATMENT_AMORTIZED_CUSTOM
                })
```

The crate's own admission that it implements exactly one reading — src/state/enterprise_support.rs:74-81 (`SUPPORT_TREATMENT_UPFRONT`: "It is what a stored [`SupportEligibleSpend`] record carries: the record holds one billing month's Reserved Instance and Savings Plan spend and the month's charge is computed from the whole of it") and the contract default at :438-445:
```
            // A stored spend record holds one month's Reserved Instance and
            // Savings Plan spend and the month's charge is computed from the
            // whole of it, which is the `Upfront` treatment of both: no part of
            // a fee is spread over another month.
            reserved_instance_treatment_method: SUPPORT_TREATMENT_UPFRONT.to_string(),
```

Test coverage today: the methods ARE exercised as stored state, but only against the contract projection — services/service-com-amazonaws-billing-awsbilling/tests/all/enterprise_support_contract_details.rs:356-368 sets `contract.reserved_instance_treatment_method` and `contract.savings_plans_treatment_method` to each documented value including `SUPPORT_TREATMENT_NONE` and asserts only which amortization date is reported. Nothing in tests/all/enterprise_support_charge_summary.rs sets a treatment method at all (no `SUPPORT_TREATMENT_*` symbol appears in that file), so no test pins the summary's behaviour for a contract whose fees are excluded, and no comment in the handler justifies ignoring the stored method.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/enterprise_support_charge_summary.rs).
2. Record one Enterprise Support account-month of spend for a past billing month through `ComAmazonawsBillingAwsbillingState::record_support_eligible_spend` with `support_plan_name = AWSSupportEnterprise`, some `by_service` usage spend, and a non-zero `reserved_instance_spend_minor` and `savings_plan_spend_minor` (the way tests/all/enterprise_support_charge_summary.rs plants spend).
3. Set the contract's treatment methods to the documented exclusion: `contract.reserved_instance_treatment_method = "None"` and `contract.savings_plans_treatment_method = "None"` (the same direct state write tests/all/enterprise_support_contract_details.rs:366-368 performs).
4. Call GetEnterpriseSupportContractDetails(billingMonth=<that month>): it reports `supportReservedInstanceTreatmentMethod: "None"` and `supportSavingsPlansTreatmentMethod: "None"`, i.e. RI and Savings Plan fees are excluded from Support-eligible spend.
5. Call GetEnterpriseSupportChargeSummary(billingMonth=<the same month>): `totalSupportEligibleReservedInstanceSpend` and `totalSupportEligibleSavingsPlanSpend` are the planted fees, `totalSupportEligibleSpend` is usage + RI + SP, and `totalSupportCharge`/`supportCharge` are priced from that sum — identical to the answer for a contract whose methods are `Upfront`, so the method the previous call reported changed nothing.
