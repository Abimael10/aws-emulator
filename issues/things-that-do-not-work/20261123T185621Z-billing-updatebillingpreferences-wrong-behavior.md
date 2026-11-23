Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: b182a47fe4222897ea5ff61fa3dd31f69a735d61

# A credit/{creditId}/{option} preference key resolves its creditId by exact string, so an id written with leading zeros is refused 403 although the crate reads the same id numerically elsewhere

_Filed: 2026-11-23T18:56:21Z_
_Operation: UpdateBillingPreferences | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b182a47fe4222897ea5ff61fa3dd31f69a735d61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

For `feature = CREDIT_PREFERENCE_OPTIONS` the handler takes the `{creditId}` segment of each key and resolves it through `state::credit_preference_authority`, which looks it up with `self.credits.get(credit_id)` — an exact string lookup against the store keyed by the id as it was minted (`"1"`, `"2"`, ...). The crate's own reader of a `creditId` admits leading zeros as the SAME id: `state::is_credit_id` accepts `0007` as a modelled `CreditId` (`^[0-9]+$`, 1..32), `GetCreditAllocationHistory` compares a requested `creditId` with the stored one as NUMBERS and says in its doc that "an id written with leading zeros names the same credit", and `CreditAllocation::key` left-pads the stored id to 32 digits precisely "so ids order numerically".

So one identifier is read two ways inside one crate. A caller that names a credit it genuinely holds as `credit/0007/status` — a `PreferenceKey` the modelled pattern `^[a-zA-Z0-9-\/\*: ]+$` admits, whose id segment this crate itself classifies as a valid `CreditId` naming credit 7 — is refused with 403 `AccessDeniedException` "Credit 0007 is not a credit of this consolidated billing family", while the identical credit named `credit/7/status` is accepted. The refusal is wrong twice over: the credit IS in the family, and the message tells the caller the opposite.

The same exact-string reading also means the stored preference key is whatever spelling arrived, so two spellings of one credit would be two independent preference slots rather than one — the opposite of the "a key that NAMES a resource is honoured only when that resource is one the family holds" rule the crate states, which is a rule about the resource, not about its spelling. This does not reverse the fixed issue that introduced the lookup (the id is still resolved against `state.credits`); it only asks the lookup to read the id the way the crate's other reader of a `creditId` reads it.

## Evidence

The write path that resolves the id — services/service-com-amazonaws-billing-awsbilling/src/ops/update_billing_preferences.rs:325-343:
```
        // A `credit/{creditId}/{option}` key names ONE credit (every key of the
        // request the same one), and that credit is the caller's to configure
        // only "for credits they own, or ... for any credit in the family": the
        // id is resolved against the stored credits exactly as an account-list
        // key's account is resolved against the family above, so no preference
        // is ever stored for a credit this family does not hold.
        if let Some(credit_id) = named_credit {
            if let Err(refusal) = state.credit_preference_authority(caller, credit_id) {
                return Err(access_denied(match refusal {
                    crate::state::CreditPreferenceRefusal::OutsideFamily => format!(
                        "Credit {credit_id} is not a credit of this consolidated billing family"
                    ),
```
and the exact-string lookup behind it — src/state.rs:1093-1100:
```
    pub fn credit_preference_authority(
        &self,
        account_id: &str,
        credit_id: &str,
    ) -> Result<(), CreditPreferenceRefusal> {
        let Some(credit) = self.credits.get(credit_id) else {
            return Err(CreditPreferenceRefusal::OutsideFamily);
        };
```

The crate accepts a leading-zero segment as a `CreditId` — src/state.rs:1393-1399:
```
/// Whether `value` is a `CreditId`: 1..[`CREDIT_ID_MAX_LEN`] characters of
/// `^[0-9]+$`, the modeled shape of the id a credit is stored under.
pub fn is_credit_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= CREDIT_ID_MAX_LEN
        && value.bytes().all(|byte| byte.is_ascii_digit())
}
```
(reached from `credit_key_parts`, src/state.rs:1377-1391, which is what `named_credit` comes from.)

The sibling reader of the same identifier, which states the opposite rule — src/ops/get_credit_allocation_history.rs:251-259:
```
/// Whether the stored `CreditId` names the same credit as the request's
/// `creditId` filter.
///
/// The stored id is the modeled digit string and the filter is a `Long`, so
/// they are compared as numbers: an id written with leading zeros names the
/// same credit, and an id too large for the filter's range names none.
fn is_filtered_credit(stored: &str, filter: i64) -> bool {
    stored.parse::<i128>().ok() == Some(i128::from(filter))
}
```
and the store key that encodes the same numeric reading — src/state.rs:598-608:
```
    /// The credit id is left-padded to the modeled `CreditId` maximum of 32
    /// digits so ids order numerically, and the parts are joined with the ASCII
    /// unit separator (`0x1F`) ...
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{:0>32}\u{1f}{}\u{1f}{}",
            self.billing_month, self.credit_id, self.account_id, self.applied_service_name,
        )
    }
```

Test coverage today: the credit-key suite enumerates the id-segment shapes it covers and leading zeros is not among them — tests/all/credit_preference_keys.rs:16-21: "The rows span what the rule generalises over: the id segment's shape (digits, letters, a mix, empty, and each modelled length bound plus one past the maximum), whether a credit is stored under it, ..." — and the only 403 it pins is for an id no credit is held under (credit_preference_keys.rs:376-384, `credit/123456/status`). The numeric reading IS pinned for the sibling read — tests/all/credit_allocation_history.rs:190-201 seeds a row with `credit_id: "0042"` under the comment "A credit id written with leading zeros: the `creditId` filter is a Long, so it names this credit as the number 42." So nothing in the crate justifies the two readings diverging.

## Steps to Reproduce

1. Boot the emulator with the harness in tests/all/credit_preference_keys.rs::emulator (which seeds credits of the caller's family, e.g. creditId "1", and grants CREDIT_PREFERENCE_OPTIONS access).
2. Call GetCredits(accountId="000000000000", startDate=<a recent past instant>) and confirm the response reports a credit whose `creditId` is "1".
3. Call UpdateBillingPreferences(feature="CREDIT_PREFERENCE_OPTIONS", billingPreferencesPerKey=[{"key":"credit/1/status","value":"ENABLED"}]) -> 200.
4. Call UpdateBillingPreferences(feature="CREDIT_PREFERENCE_OPTIONS", billingPreferencesPerKey=[{"key":"credit/0001/status","value":"ENABLED"}]) — the same credit, named with a leading zero, inside the modelled PreferenceKey pattern and accepted as a CreditId by state::is_credit_id — and observe 403 AccessDeniedException "Credit 0001 is not a credit of this consolidated billing family", for a credit the family demonstrably holds.
5. For contrast, call GetCreditAllocationHistory(accountId="000000000000", creditId=42, startDate=..., endDate=...) against an allocation seeded with credit_id "0042" (tests/all/credit_allocation_history.rs:190-201): the leading-zero id IS matched, because that read compares the ids as numbers and its doc says "an id written with leading zeros names the same credit".
