Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 70ff1500c355ed7fc4788da90f349959a442a9b5

# The "billingMonth must be a month in the past" gate (and the payer-account gate beside it) is written out three times, once per Enterprise Support read

_Filed: 2026-11-15T03:09:49Z_
_Operation: GetEnterpriseSupportChargeSummary | Class: maintainability_

> _Note: this issue was identified against the code base at commit `70ff1500c355ed7fc4788da90f349959a442a9b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The three Enterprise Support reads each carry their own verbatim copy of the same two rules, so the copies can drift even though all three answer the same modeled `EnterpriseSupportBillingMonth` contract.

Copy 1 — the past-month gate: `parse_month(&input.billing_month)` -> `crate::state::billing_period(now)` -> `if month_index(requested) >= month_index(current) { invalid_field("billingMonth", "must be a month in the past") }`, with the same quoted doc comment above it, appears identically in `get_enterprise_support_charge_summary.rs`, `get_enterprise_support_contract_details.rs` and `list_enterprise_support_linked_account_charges.rs`.

Copy 2 — the payer-account gate: the same "only the payer account of the Enterprise Support profile may view its Enterprise Support data" decision is spelled two different ways across the same three handlers: two of them clone `state.billing_family_management_account_id` and compare it with `!=` against `caller_account_id()`, while the third calls the `state::is_billing_family_management_account` predicate written for exactly that purpose.

The crate already keeps shared rules in one place — `state/billing_month.rs` hosts the `parse_month`/`month_index`/`month_period` arithmetic these three share, and `state.rs` hosts the shared refusals (`billing_view_not_healthy`, `not_a_custom_view`, `billing_view_not_found`, `requested_tag_set`, `decode_page_token`/`PAGE_TOKEN_REFUSAL`) precisely so sibling handlers cannot answer one rule two ways. These two gates are the rules that were left inline. Folding each onto one shared predicate in `state` leaves every response byte-identical (each handler keeps building its own modeled `ValidationException`/`AccessDeniedException` from the shared decision, with the same member name and the same `must be a month in the past` constraint text) and leaves all three existing tests unchanged.

## Evidence

Copy A — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs:262-272:
```
        let requested = parse_month(&input.billing_month)
            .ok_or_else(|| invalid_field("billingMonth", "must name a calendar month"))?;
        let current = crate::state::billing_period(::std::time::SystemTime::now())
            .ok_or_else(|| unavailable("the current billing month"))?;
        // The `EnterpriseSupportBillingMonth` doc: "This must be a month in the
        // past." ...
        if month_index(requested) >= month_index(current) {
            return Err(invalid_field("billingMonth", "must be a month in the past"));
        }
```
Copy B — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_contract_details.rs:269-279:
```
        let requested = parse_month(&input.billing_month)
            .ok_or_else(|| invalid_field("billingMonth", "must name a calendar month"))?;
        let current = crate::state::billing_period(::std::time::SystemTime::now())
            .ok_or_else(|| unavailable("the current billing month"))?;
        // The `EnterpriseSupportBillingMonth` doc: "This must be a month in the
        // past." ...
        if month_index(requested) >= month_index(current) {
            return Err(invalid_field("billingMonth", "must be a month in the past"));
        }
```
Copy C — services/service-com-amazonaws-billing-awsbilling/src/ops/list_enterprise_support_linked_account_charges.rs:356-366:
```
        let requested = parse_month(&input.billing_month)
            .ok_or_else(|| invalid_field("billingMonth", "must name a calendar month"))?;
        let current = crate::state::billing_period(::std::time::SystemTime::now())
            .ok_or_else(|| unavailable("the current billing month"))?;
        // The `EnterpriseSupportBillingMonth` doc: "This must be a month in the
        // past." ...
        if month_index(requested) >= month_index(current) {
            return Err(invalid_field("billingMonth", "must be a month in the past"));
        }
```
The payer gate, spelled two ways for one rule — get_enterprise_support_charge_summary.rs:247-249:
```
        let payer_account_id = state.billing_family_management_account_id.clone();
        let caller_account_id = crate::state::caller_account_id();
        if payer_account_id != caller_account_id {
```
list_enterprise_support_linked_account_charges.rs:341-343 (identical), versus get_enterprise_support_contract_details.rs:260, which uses the predicate written for it:
```
        if !state.is_billing_family_management_account(crate::state::caller_account_id()) {
```
The crate's own precedent for hoisting a shared rule — state.rs:1451-1455 (`is_caller_account_arn`: "Every operation that identifies a billing view by ARN checks this BEFORE it looks the ARN up") and state.rs:1552-1564 (`requested_tag_set`: "Every writer that takes a `ResourceTagList` collects it through here, so the creator and the updater of a view's tag set answer identical input identically.").

Test coverage that must stay green and unchanged: three same-named tests, one per suite — tests/all/enterprise_support_charge_summary.rs:830 `refuses_a_month_that_is_not_in_the_past`, tests/all/enterprise_support_contract_details.rs:714 `refuses_a_month_that_is_not_in_the_past`, tests/all/enterprise_support_linked_account_charges.rs:784 `refuses_a_month_that_is_not_in_the_past` (whose doc comment at :780 quotes the same model sentence: "The `EnterpriseSupportBillingMonth` doc: \"This must be a month in the past.\"").

## Steps to Reproduce

1. grep -n 'must be a month in the past' services/service-com-amazonaws-billing-awsbilling/src/ops/*.rs — three identical refusals in three handlers.
2. Read get_enterprise_support_charge_summary.rs:262-272, get_enterprise_support_contract_details.rs:269-279 and list_enterprise_support_linked_account_charges.rs:356-366: the parse / current-month / `month_index(requested) >= month_index(current)` sequence and its doc comment are the same text three times.
3. grep -n 'billing_family_management_account' services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_contract_details.rs services/service-com-amazonaws-billing-awsbilling/src/ops/list_enterprise_support_linked_account_charges.rs — one gate, two spellings.
4. Observe that state/billing_month.rs already owns parse_month/month_index/month_period and state.rs already owns every other refusal these three handlers share, so a change to either rule has to be made in three places today.
