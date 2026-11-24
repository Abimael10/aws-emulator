Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 71bc1c80105f531149a45b246b8955e941e566c6

# costCategoryArn and ruleName are reported for any stored credit, although the model documents both as present only when creditSharingType is COST_CATEGORY_RULE

_Filed: 2026-11-24T01:28:23Z_
_Operation: GetCredits | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `71bc1c80105f531149a45b246b8955e941e566c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetCredits` projects a stored credit into the modelled `CreditData` with `credit_data`, copying `costCategoryArn` and `ruleName` straight out of the record with no reference to the credit's `creditSharingType`. The model documents both members as conditionally present: they exist only for a credit whose sharing scope is controlled by a Cost Category rule. A stored credit that carries either field while its `creditSharingType` is `DEFAULT`, `DISABLED`, `CUSTOM` or absent is therefore reported with members the model says are null in that case.

This crate does apply exactly this class of documented conditional presence elsewhere: `GetEnterpriseSupportContractDetails` quotes the "Null for all other methods" sentences of its two amortization-start-date members and gates each one with a `.filter(|_| ...)` on the treatment method it belongs to. `credit_data` is the one projection in the crate that reports a conditionally present member unconditionally, and the stored `Credit` fields carry no documented invariant that would make the condition hold at write time (`state.rs:659-661` documents neither field).

## Evidence

Smithy, `CreditData$costCategoryArn`: "The Amazon Resource Name (ARN) of the Cost Category controlling the credit's sharing scope. Present only when `creditSharingType` is `COST_CATEGORY_RULE`." and `CreditData$ruleName`: "The rule name within the Cost Category. Present only when `creditSharingType` is `COST_CATEGORY_RULE`."

The projection copies both unconditionally — services/service-com-amazonaws-billing-awsbilling/src/ops/get_credits.rs:302-305:
```
        credit_console_visibility: credit.credit_console_visibility.clone(),
        credit_sharing_type: credit.credit_sharing_type.clone(),
        cost_category_arn: credit.cost_category_arn.clone(),
        rule_name: credit.rule_name.clone(),
```

The sibling read that DOES apply the same kind of documented condition — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_contract_details.rs:319-338:
```
            // "When supportReservedInstanceTreatmentMethod = AmortizedCustom,
            // only amortized fees for Reserved Instances purchased on or after
            // this date are included in the calculation. This field is Null for
            // all other treatment methods."
            support_reserved_instance_amortization_start_date: contract
                .reserved_instance_amortization_start_date
                .filter(|_| {
                    contract.reserved_instance_treatment_method
                        == SUPPORT_TREATMENT_AMORTIZED_CUSTOM
                })
                .map(timestamp),
...
            // "This is applicable when supportSavingsPlansTreatmentMethod =
            // Amortized and is Null for all other methods."
            support_savings_plans_amortization_start_date: contract
                .savings_plans_amortization_start_date
                .filter(|_| contract.savings_plans_treatment_method == SUPPORT_TREATMENT_AMORTIZED)
                .map(timestamp),
```

The stored fields carry no invariant of their own — services/service-com-amazonaws-billing-awsbilling/src/state.rs:658-661:
```
    /// A `CreditSharingType` value, e.g. `DEFAULT`.
    pub credit_sharing_type: Option<String>,
    pub cost_category_arn: Option<String>,
    pub rule_name: Option<String>,
```

Test coverage: tests/all/credits.rs:248-250 asserts only that the reported `costCategoryArn` equals `stored.cost_category_arn.as_deref()`, and every row that sets it also sets `credit_sharing_type = Some("COST_CATEGORY_RULE")` (tests/all/credits.rs:390-393); the `DISABLED`, `DEFAULT` and `CUSTOM` rows (tests/all/credits.rs:407, 415, 423) leave it `None` (tests/all/credits.rs:147). So no existing assertion pins the divergent combination, and gating the two members on `COST_CATEGORY_RULE` leaves every row's expected answer unchanged.

## Steps to Reproduce

1. Seed the billing store with a credit for the caller's account whose `credit_sharing_type` is `Some("CUSTOM")` (or `DEFAULT`/`DISABLED`/`None`) and whose `cost_category_arn` and `rule_name` are both set.
2. Call `GetCredits` with that `accountId` and a `startDate` inside the credit's life.
3. The reported `CreditData` carries `costCategoryArn` and `ruleName` beside `creditSharingType: "CUSTOM"`, although the model documents both as present only when `creditSharingType` is `COST_CATEGORY_RULE`.
4. Compare with `GetEnterpriseSupportContractDetails`, which drops `supportSavingsPlansAmortizationStartDate` whenever the treatment method is not `Amortized` for the identically worded rule.
