Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 553a1d2e685367944e9839a399c7f24f7975bfe6

# The DUPLICATE_ACCOUNT refusal is written out three times instead of once beside its two sibling account-list refusals in state/refusals.rs

_Filed: 2026-11-20T11:59:18Z_
_Operation: AssociateAccounts | Class: maintainability_

> _Note: this issue was identified against the code base at commit `553a1d2e685367944e9839a399c7f24f7975bfe6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate has a module whose stated job is to build the account-list refusals that several operations share, exactly so one fault is worded the same way whichever operation carried it. Two of the three refusals an account list can earn live there and are called from the handlers (`account_not_in_billing_family`, `accounts_already_associated`, both taking the offending member path as a `field` argument). The third one — the `DUPLICATE_ACCOUNT` refusal for an account named twice in one request — is instead hand-written in full, three times: in `AssociateAccounts`, in `DisassociateAccounts` and in `CreateBillingGroup`. All three copies pick the same `ValidationExceptionReason`, build the same sentence from the same `crate::state::first_duplicate` answer, and differ only in the member path they name (`AccountIds` vs `AccountGrouping.LinkedAccountIds`) — which is precisely the one thing the two shared refusals already take as a parameter.

The three copies can therefore drift: a change to the reason or the wording in one handler silently leaves the other two behind, even though `DisassociateAccounts`'s own comment asserts the three are the same refusal. The fix is to add one `duplicate_account(field, account_id)` constructor to `src/state/refusals.rs` beside its two siblings and call it from the three handlers; the reason, the member path in `Fields` and the message text stay byte-for-byte what they are today, so every response and every existing test is unchanged.

## Evidence

The shared home and the two siblings that already live there:

- `src/state/refusals.rs:3-5`: "//! The modelled refusals several of this service's operations share, built from the request so one fault is reported the same way whichever operation carried it."
- `src/state/refusals.rs:42-54`: `pub fn accounts_already_associated(field: &str, account_id: &str, owner_group_arn: &str) -> crate::ops::types::ValidationException { validation_exception("ACCOUNTS_ALREADY_ASSOCIATED", field, format!(...)) }`, documented at `src/state/refusals.rs:38-41` as "built here so every operation that takes account ids answers it the same way; `field` names the input member that carried the id (`AccountIds`, `AccountGrouping.LinkedAccountIds`, …), which is what goes in `Fields`."
- `src/state/refusals.rs:74-83`: `pub fn account_not_in_billing_family(field: &str, account_id: &str)` — same shape, same `field` parameter.

The third refusal, written out three times instead:

- `src/ops/associate_accounts.rs:196-202`:
```
        if let Some(duplicate) = crate::state::first_duplicate(&input.account_ids) {
            return Err(validation(
                "DUPLICATE_ACCOUNT",
                "AccountIds",
                format!("account {duplicate} is listed more than once"),
            ));
        }
```
- `src/ops/disassociate_accounts.rs:193-199` (identical body, same member), under a comment at `src/ops/disassociate_accounts.rs:190-192` that asserts the sameness the copies have to maintain by hand: "// One request may not name the same account twice — the `DUPLICATE_ACCOUNT` // reason `ValidationExceptionReason` enumerates, refused the same way // `AssociateAccounts` and `CreateBillingGroup` refuse it."
- `src/ops/create_billing_group.rs:289-295`:
```
        if let Some(duplicate) = crate::state::first_duplicate(&linked_account_ids) {
            return Err(validation(
                "DUPLICATE_ACCOUNT",
                "AccountGrouping.LinkedAccountIds",
                format!("account {duplicate} is listed more than once"),
            ));
        }
```
The two handlers that hand-write this copy also call the SHARED constructors a few lines further down for the neighbouring checks — `src/ops/associate_accounts.rs:213-220` (`crate::state::account_not_in_billing_family("AccountIds", account_id)`) and `src/ops/create_billing_group.rs:322-336` (`crate::state::accounts_already_associated("AccountGrouping.LinkedAccountIds", account_id, &owner)`) — so the inconsistency is inside one loop body.

Existing tests that pin the responses the fix must leave unchanged:
- `tests/all/billing_group_core_loop.rs:817-871` `an_account_listed_twice_in_one_request_is_refused` — three rows: `AssociateAccounts names one account twice`, `AssociateAccounts names one account twice among others`, `CreateBillingGroup names one linked account twice`, each asserting HTTP 400, `ValidationException` and `body["Reason"] == Some("DUPLICATE_ACCOUNT")`.
- `tests/all/disassociate_accounts.rs:549-589` `a_duplicate_account_and_an_unknown_group_are_refused` — two rows asserting the same 400 / `DUPLICATE_ACCOUNT` for `DisassociateAccounts`.
- `tests/all/validation_exception_field_member_names.rs:22` names `DUPLICATE_ACCOUNT` among the reasons whose `Fields` member names it pins.

## Steps to Reproduce

1. Open `src/state/refusals.rs` and read the module header (lines 3-5) and the two account-list refusal constructors `accounts_already_associated` (lines 42-54) and `account_not_in_billing_family` (lines 74-83), each taking the offending input member as `field`.
2. Open `src/ops/associate_accounts.rs` at lines 196-202 and note the `DUPLICATE_ACCOUNT` refusal is built inline rather than through that module.
3. Open `src/ops/disassociate_accounts.rs` at lines 190-199 and `src/ops/create_billing_group.rs` at lines 289-295: the same reason, the same `first_duplicate` call and the same sentence, differing only in the member path.
4. Confirm the three copies are what keeps the comment at `src/ops/disassociate_accounts.rs:190-192` true — nothing in the crate enforces it.
5. Run `cargo test -p service-com-amazonaws-billingconductor-awsbillingconductor` and note `an_account_listed_twice_in_one_request_is_refused` and `a_duplicate_account_and_an_unknown_group_are_refused` pass; they must keep passing unchanged after the three copies are collapsed into one `duplicate_account(field, account_id)` in `src/state/refusals.rs`.
