Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 41fa950ddd6915a1c40615232d76af513e3c005f

# The three Enterprise Support reads resolve the caller's identity (locking another service's store) while holding this service's own state lock, which two sibling handlers state must never happen

_Filed: 2026-11-15T23:32:41Z_
_Operation: GetEnterpriseSupportContractDetails | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `41fa950ddd6915a1c40615232d76af513e3c005f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::enterprise_support_access_denied` builds its message through `crate::state::caller_identity_arn`, which takes the STS service's store and locks it mutably. All three Enterprise Support reads — `GetEnterpriseSupportContractDetails`, `GetEnterpriseSupportChargeSummary` and `ListEnterpriseSupportLinkedAccountCharges` — call that helper AFTER they have taken this service's own state guard, so the refusal path holds the billing lock and then acquires another service's lock.

Two sibling handlers of this same crate explicitly arrange the opposite for the structurally identical refusal (`cross_account_access_denied`, which reaches `caller_identity_arn` the same way): both decide the boundary and build the refusal BEFORE the store is locked, and their comments state the reason — resolving the caller's identity "reaches another service's store, which this way never happens while this service's own lock is held". `RedeemCredits` states the same discipline as an explicit lock ORDER for the one other cross-service path in the crate. The Enterprise Support reads break that discipline: the payer-account comparison does need billing state, but nothing requires the STS lookup to happen under the billing guard — the refusal could be composed after the guard is dropped, as every other ARN-addressed handler does.

## Evidence

The rule, stated by a sibling handler — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_views.rs:178-196:
```
/// Enforce the account boundary on every entry of the `arns` filter, BEFORE the
/// store is locked: the decision needs no billing state, and resolving the
/// caller's identity for the refusal reaches another service's store, which this
/// way never happens while this service's own lock is held.
fn check_arns_filter_accounts<Eng: Send + Sync + 'static>(
```
...and by a second sibling — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_view_segments.rs:340-346:
```
        // The account boundary is enforced BEFORE any existence lookup, ...
        // It is decided first, outside the store's lock, because it
        // needs no billing state and resolving the caller's identity for the
        // refusal reaches another service's store.
```
...and as an explicit cross-service lock order — services/service-com-amazonaws-billing-awsbilling/src/ops/redeem_credits.rs:161-166:
```
        // LOCK ORDER, where this handler is the only holder of both: the billing ledger
        // FIRST, then the credit-code registry below. Taking them in that order is what
        // makes the redemption atomic ... and a future reader of billing
        // state from the other service must take them in the same order.
```
What the helper does — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1492-1508:
```
pub fn caller_identity_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    sts_state::service_state(engine)
        .and_then(|sts| {
            sts_state::resolve_caller(
                &mut sts.lock(),
```
The three handlers that call it under their own guard — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_contract_details.rs:250-267:
```
        let state = crate::state::service_state(&*self.engine)
            .ok_or_else(|| unavailable("the Enterprise Support contract store"))?;
        let state = state.lock();
...
        if !state.is_billing_family_management_account(crate::state::caller_account_id()) {
            return Err(
                GetEnterpriseSupportContractDetailsErrorModeled::AccessDeniedException(
                    crate::state::enterprise_support_access_denied(Self::NAME, &*self.engine, ctx),
```
services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs:240-256 (`let state = state.lock();` … `crate::state::enterprise_support_access_denied(Self::NAME, &*self.engine, ctx,)`) and services/service-com-amazonaws-billing-awsbilling/src/ops/list_enterprise_support_linked_account_charges.rs:334-350 (same shape).

Test coverage: the cross-account refusal of these reads is exercised in tests/all/enterprise_support_contract_details.rs and tests/all/enterprise_support_charge_summary.rs by setting `billing_family_management_account_id` to another account, so the path that takes both locks is reached, but no test asserts anything about lock discipline.

## Steps to Reproduce

1. Read services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_views.rs:178-181 and list_billing_view_segments.rs:340-346: the crate states that resolving the caller's identity for a refusal must not happen while this service's own lock is held.
2. Read src/state.rs:1492-1508: `caller_identity_arn` locks the STS service's store (`&mut sts.lock()`).
3. Read src/ops/get_enterprise_support_contract_details.rs:250-267 (and the same shape in get_enterprise_support_charge_summary.rs:240-256 and list_enterprise_support_linked_account_charges.rs:334-350): the billing state guard is taken first and `enterprise_support_access_denied(..., &*self.engine, ctx)` — hence the STS lock — is acquired underneath it.
4. Set `billing_family_management_account_id` to an account other than the caller and call GetEnterpriseSupportContractDetails(billingMonth=<a past month>): the refusal is composed while the billing guard is held, acquiring the STS store's lock inside this service's critical section.
