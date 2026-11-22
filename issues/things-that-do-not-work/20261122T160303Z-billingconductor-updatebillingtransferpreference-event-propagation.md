Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: 33eed1d27fdea9bbe5c40c61631ec81c0850a581

# No production path ever records a billing transfer, so every transfer-addressing operation answers 404 forever and TRANSFER_BILLING billing groups are unreachable

_Filed: 2026-11-22T16:03:03Z_
_Operation: UpdateBillingTransferPreference | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `33eed1d27fdea9bbe5c40c61631ec81c0850a581`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate resolves every billing-transfer request against a private `billing_transfers` map whose documented entry point, `ComAmazonawsBillingconductorAwsbillingconductorState::record_billing_transfer`, is called from NO handler and from no other crate in the repository — only from a test helper. The map is therefore empty for the whole life of a served process, and `billing_transfer_exists` fails closed, so:

* `GetBillingTransferPreference` answers HTTP 404 `ResourceNotFoundException` for EVERY `ResponsibilityTransferArn`, so its documented "never set" answer (absent `LastModifiedTime`) is unreachable;
* `UpdateBillingTransferPreference` answers HTTP 404 for every ARN, so a preference can never be set, and the `AutoTransferBillingGroupCreationPreference` rule the handler enforces below the gate (`MISSING_PRICING_PLAN_ARN`, the plan-exists check) is dead code;
* `CreateBillingGroup` refuses every request naming `AccountGrouping.ResponsibilityTransferArn` with `ValidationException`/`INVALID_ARN`, so `BillingGroupRecord::billing_group_type()` can never return `TRANSFER_BILLING`, `has_billing_owner()` can never be satisfied by a transfer, and `ListBillingGroupsFilter.ResponsibilityTransferArns` can never select anything;
* `DeletePricingPlan`'s `PRICING_PLAN_REFERENCED_BY_PREFERENCE_DELETE_CONFLICT` branch is unreachable.

The inconsistency is inside this crate: it already propagates the other two AWS-Organizations-owned facts it needs by READING the Organizations store at request time — the management account through `organization_management_account_id` (state.rs:4720) and the consolidated-family roster through `organization_linked_accounts` (state.rs:4652) — and both are documented as exactly that kind of one-directional cross-store read. The transfer universe alone is left to a local write seam that nothing on the request path calls, so the fact never propagates. (For completeness: the Organizations crate's own transfer surface is still scaffolded, e.g. `services/service-com-amazonaws-organizations-awsorganizationsv20161128/src/ops/invite_organization_to_transfer_responsibility.rs:4` carries `//! SCAFFOLDED, NOT IMPLEMENTED:`, so there is nothing to read from there yet either — the wiring is what is missing on both ends.)

## Evidence

The seam, documented as the only way transfers enter the store — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state.rs:3266-3278`:
```
/// Record a billing transfer the organization HAS, answering whether the store holds
/// it afterwards.
///
/// This is the seam the transfer universe enters the store through: Billing Conductor
/// creates no billing transfer — an Organizations surface does — so the transfers this
/// store can answer for are the ones recorded here, ...
pub fn record_billing_transfer(&mut self, arn: &str) -> bool {
```
The field it fills — `src/state.rs:2978-2990`:
```
/// The billing transfers THIS organization has, keyed by full `ResponsibilityTransferArn`,
/// ... This is the set every operation that addresses a
/// billing transfer resolves one in, so a transfer no entry here names does not exist
/// as far as this store is concerned.
/// ... so this map is empty until something records one, the same way
/// `payer_account_id` holds no account until something designates one.
billing_transfers: BTreeMap<String, BillingTransferRecord>,
```
Nothing on the request path calls it. `grep -rn 'record_billing_transfer' services` yields only the declaration/doc sites in `src/state.rs` (2989, 3278, 4402) and ONE caller, a test helper — `tests/all/billing_transfer_identity.rs:166-185`:
```
/// Record a billing transfer the organization HAS in `engine`'s store, ...
/// ... This stands in for the
/// Organizations surface that mints them, exactly as the suites stand in for the payer
/// designation.
pub(crate) fn designate_transfer(engine: &engine::InMemoryEngine, arn: &str) -> String {
```
That analogy is where the two sides disagree: the payer designation does NOT need a stand-in on the request path, because this crate reads it from the Organizations store itself — `src/state.rs:4720`, `pub fn organization_management_account_id<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<String>`, documented "it is READ from that store through the one shared engine rather than copied into this service" — and the same for the family roster at `src/state.rs:4652`, `pub fn organization_linked_accounts<Eng: Send + Sync + 'static>(...)`. The transfers have no such read.

The three refusals that therefore always fire:
* `src/ops/get_billing_transfer_preference.rs:184-193`: `if !state.billing_transfer_exists(&transfer_arn, organization_payer.as_deref()) { return Err(... ResourceNotFoundException { message: "the billing transfer does not exist" ...`
* `src/ops/update_billing_transfer_preference.rs:199-208`: the identical gate, with the handler's own comment at 192-198 — "only a transfer the store holds a record of exists. Any other ARN is this operation's modelled 404".
* `src/ops/create_billing_group.rs:376-382`: `if !state.billing_transfer_exists(transfer_arn, organization_payer.as_deref()) { return Err(validation("INVALID_ARN", "AccountGrouping.ResponsibilityTransferArn", format!("the billing transfer {transfer_arn} does not exist"))); }`

Test coverage: `tests/all/billing_transfer_identity.rs`, `tests/all/billing_transfer_preference.rs` and `tests/all/billing_transfer_preference_unchanged_update.rs` all reach a 200 only by calling the test-only `designate_transfer` helper directly against the engine's store — no test (and no client) reaches a 200 over HTTP.

## Steps to Reproduce

1. Start the emulator and make the emulated account the organization's management account (Organizations `CreateOrganization`) so the payer-gated billingconductor control plane is reachable.
2. Call every HTTP operation the emulator offers; none of them records a billing transfer (`grep -rn 'record_billing_transfer' services` shows the only caller is `tests/all/billing_transfer_identity.rs:181`).
3. POST /get-billing-transfer-preference with {"ResponsibilityTransferArn":"arn:aws:organizations::<emulated account id>:transfer/o-aaaaaaaaaa/billing/inbound/rt-aaaaaaaa"} — observe HTTP 404 ResourceNotFoundException "the billing transfer does not exist".
4. PUT /update-billing-transfer-preference with the same ARN and {"AutoBillingTransferBillingGroupCreation":{"Enabled":false}} — observe HTTP 404, so no preference can ever be stored and the operation's 200 path is unreachable.
5. POST /create-billing-group with {"Name":"G1","AccountGrouping":{"ResponsibilityTransferArn":"<the same ARN>"},"ComputationPreference":{"PricingPlanArn":"<a created plan>"}} — observe HTTP 400 ValidationException with reason INVALID_ARN on AccountGrouping.ResponsibilityTransferArn.
6. POST /list-billing-groups and observe that no group can ever carry BillingGroupType=TRANSFER_BILLING or an AccountGrouping.ResponsibilityTransferArn, although src/state.rs:1736-1744 computes that type and ListBillingGroupsFilter models a ResponsibilityTransferArns filter.
