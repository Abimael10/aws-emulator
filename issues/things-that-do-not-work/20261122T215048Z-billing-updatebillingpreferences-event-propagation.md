Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: 98adcde486cb4192ced2f9bf692c49eb4f63966b

# Nothing in src/ ever activates a gated billing feature, so BILLING_ALERTS / RI_SHARING / RI_SHARING_HISTORY preferences are 403 forever

_Filed: 2026-11-22T21:50:48Z_
_Operation: UpdateBillingPreferences | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `98adcde486cb4192ced2f9bf692c49eb4f63966b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both preference operations gate on `ComAmazonawsBillingAwsbillingState::feature_access`, which the store seeds with every `BillingFeature` EXCEPT the three in `ACCESS_ACTIVATION_FEATURES` (`BILLING_ALERTS`, `RI_SHARING`, `RI_SHARING_HISTORY`). state.rs says that set is only a DEFAULT and that `grant_feature_access` is what moves it — but no production path in this crate (and none in any other crate of the workspace) ever calls `grant_feature_access`: the only callers are four files under `tests/all/`, which reach into the engine in-process.

So over the wire the gate is unsatisfiable: `UpdateBillingPreferences` for `RI_SHARING` and `GetBillingPreferences` for `BILLING_ALERTS` / `RI_SHARING` / `RI_SHARING_HISTORY` answer 403 `AccessDeniedException` for every request, in every order, forever. Three of the model's seven features therefore have no reachable preference plane at all, and because every `RI_SHARING` write is refused before `set_preference` runs, the `RI_SHARING_HISTORY` sequence that `preference_entries` reads off `preference_history` can never hold a single change either.

This is the same defect this crate already fixed once for the other out-of-service activation fact: `CreateBillingView`'s management-account gate used to be unsatisfiable until it was made to read AWS Organizations' own store (`state::is_organization_management_account`), which state.rs calls "the fact read from the Organizations store". The billing-console activation has no such reader, and the helper that would record it is dead outside the test suite.

## Evidence

The crate states the field is movable state, not a constant — services/service-com-amazonaws-billing-awsbilling/src/state.rs:284-289:
```
/// What an ACCOUNT has activated is state, not a constant: this is only the
/// default, which
/// [`ComAmazonawsBillingAwsbillingState::feature_access`] is seeded from and
/// [`ComAmazonawsBillingAwsbillingState::grant_feature_access`] moves.
pub const ACCESS_ACTIVATION_FEATURES: &[&str] =
    &["BILLING_ALERTS", "RI_SHARING", "RI_SHARING_HISTORY"];
```
The seed closes exactly those three — src/state.rs:870-876:
```
            // Every feature except the ones behind the Billing-console access
            // activation this account does not have.
            feature_access: BILLING_FEATURE_VALUES
                .iter()
                .filter(|feature| !ACCESS_ACTIVATION_FEATURES.contains(*feature))
```
The mover exists — src/state.rs:1237-1241:
```
    /// Activate `feature` for this account, so its preferences become
    /// reachable. Idempotent.
    pub fn grant_feature_access(&mut self, feature: &str) {
        self.feature_access.insert(feature.to_string());
```
…and a repo-wide grep for `grant_feature_access` finds ONLY that declaration plus test call sites: `tests/all/billing_feature_access.rs:100`, `tests/all/billing_preferences.rs:88`, `tests/all/billing_preference_pagination.rs:52`, `tests/all/token_scope.rs:606`. No `src/` caller anywhere in the workspace.

The gates that are therefore permanently closed — src/ops/update_billing_preferences.rs:221-226:
```
            if !state.lock().has_feature_access(feature) {
                return Err(UpdateBillingPreferencesErrorModeled::AccessDeniedException(
                    crate::state::feature_access_denied(Self::NAME, feature),
                )
                .into());
```
and src/ops/get_billing_preferences.rs:422-427 (same predicate, same refusal).

The crate's own precedent for an out-of-service activation fact — src/ops/create_billing_view.rs:239-240:
```
        let caller = crate::state::caller_account_id();
        if !crate::state::is_organization_management_account(&*self.engine, caller) {
```
with src/state.rs:817-819 calling it "`organization_management_account_id`, the fact read from the Organizations store".

Test coverage: tests/all/billing_feature_access.rs asserts the gate both ways, but only by flipping the field in-process (`set_access`, line 96-104) — no test (and no API) reaches the granted state over the wire, so nothing pins the current unreachability as intended.

## Steps to Reproduce

1. Boot the emulator and POST `X-Amz-Target: AWSBilling.UpdateBillingPreferences` with `{"feature":"RI_SHARING","billingPreferencesPerKey":[{"key":"sharing","value":"ENABLED"}]}` — observe 403 AccessDeniedException ([Message=No access to feature RI_SHARING]).
2. Call every other operation this service models, in any order, then retry the request — the answer is unchanged, because nothing in src/ writes `feature_access`.
3. grep -rn 'grant_feature_access' services/ — the only hits are the declaration in src/state.rs:1239 and four call sites under tests/all/.
4. POST `AWSBilling.GetBillingPreferences` with `{"features":["RI_SHARING_HISTORY"]}` and with `{"features":["BILLING_ALERTS"]}` — both 403 forever, so neither feature's preferences can ever be read or written through the API.
