Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: b4093aae6d988631adde420a6428194a1a0d4714

# AcceptPrimaryEmailUpdate splits the account-target composition around its PrimaryEmail syntax check, so the unknown-account 403 loses to the address 400 while the own-account 403 wins

_Filed: 2026-11-20T07:15:50Z_
_Operation: AcceptPrimaryEmailUpdate | Class: error-response_

> _Note: this issue was identified against the code base at commit `b4093aae6d988631adde420a6428194a1a0d4714`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`AcceptPrimaryEmailUpdate` is a write on an account that must ALREADY exist (its own comment says it "creates no account record"), which is exactly the case the state module states once as `ComAmazonawsAccountAccountState::target_account_mut` — the composition of BOTH halves of the account-target rule (the caller's own account, and an id this emulator holds no record of), decided "before anything about that account's sub-resources is looked at". The handler does not call that composition. It calls the first half (`state::target_account_id`) at the top of `handle`, then refuses a `PrimaryEmail` that is not an email address (400 `ValidationException`), and only afterwards applies the second half (`store.accessible_account_mut`).

Because the two halves straddle the address check, the two refusals the handler's own comment calls one "account-target rule" land on opposite sides of the identical 400:

- `{"AccountId": "<unknown 12-digit id>", "PrimaryEmail": "not-an-email-address", "Otp": "Q7r4Tz"}` -> 400 `ValidationException` naming `PrimaryEmail`; the 403 the composition states is never reached.
- `{"AccountId": "<the caller's own id>", "PrimaryEmail": "not-an-email-address", "Otp": "Q7r4Tz"}` -> 403 `AccessDeniedException`.

So one half of one rule outranks the address refusal and the other does not. It also breaks the order the handler's own module doc states ("the authorization rule, then the address's syntax, then whether there is an update at all"): half of the authorization rule is decided third, after the syntax. Routing the handler through `store.target_account_mut(Some(&input.account_id))` the way `DeleteAlternateContact`, `SendPhoneNumberVerification` and `VerifyPhoneNumber` do would make both halves behave alike. The defect is the split, not the address refusal itself.

## Evidence

The rule the crate states once — services/service-com-amazonaws-account-account/src/state.rs:28-33:

```
//! is then changed), and the COMPOSITION of the two —
//! [`ComAmazonawsAccountAccountState::target_account`] for a read,
//! [`ComAmazonawsAccountAccountState::target_account_mut`] for a write that
//! must not create a record — is what those handlers call, keeping only the
//! mapping into their own operation's error enum.
```

and state.rs:1387-1398 on `target_account_mut`:

```
    /// The same composition as [`target_account`](Self::target_account), built
    /// on [`accessible_account_mut`](Self::accessible_account_mut): the record
    /// is handed back mutably, for a call that CHANGES an account which must
    /// already exist and so must not bring one into existence
```

The handler's own stated order — src/ops/accept_primary_email_update.rs:14-19:

```
//! The order the refusals are decided in: the authorization rule, then the
//! address's syntax, then whether there is an update at all (the modelled 400
//! `ValidationException`, as on the status read), then the update's status, then the input
//! members the request presents (the address, then the passcode), and last the
//! conflict with another account's hold on the address
```

First half — accept_primary_email_update.rs:197-198:

```
        let account_id = crate::state::target_account_id(Some(&input.account_id))
            .map_err(AcceptPrimaryEmailUpdateErrorModeled::AccessDeniedException)?;
```

The 400 decided in between — accept_primary_email_update.rs:206-211 (it returns before the store is even locked):

```
        if let Err(refusal) = crate::state::validated_email_address(
            crate::state::PRIMARY_EMAIL_MEMBER,
            &input.primary_email,
        ) {
            return Err(AcceptPrimaryEmailUpdateErrorModeled::ValidationException(refusal).into());
        }
```

Second half, whose own comment calls it the same rule — accept_primary_email_update.rs:229-236:

```
        // The other half of the account-target rule, stated once in
        // `state::ComAmazonawsAccountAccountState::accessible_account_mut`:
        // this operation acts on an update that already exists, so it creates
        // no account record, and an account this emulator has never been told
        // about is the modelled authorization refusal.
        let account = store
            .accessible_account_mut(&account_id)
            .map_err(AcceptPrimaryEmailUpdateErrorModeled::AccessDeniedException)?;
```

The single-call form the no-create siblings use — src/ops/verify_phone_number.rs:194-196 (and delete_alternate_contact.rs:210-212, send_phone_number_verification.rs:192-194):

```
        let account = store
            .target_account_mut(input.account_id.as_deref())
            .map_err(VerifyPhoneNumberErrorModeled::AccessDeniedException)?;
```

Test coverage: tests/all/unknown_account_refusal.rs:175-185 pins AcceptPrimaryEmailUpdate at 403 for an unknown account, but only with a well-formed address:

```
        (
            "AcceptPrimaryEmailUpdate".to_string(),
            "/acceptPrimaryEmailUpdate",
            |id| {
                json!({
                    "AccountId": id,
                    "PrimaryEmail": "ada@example.com",
```

and tests/all/account_target_composition_is_shared.rs:49-52 deliberately avoids the combination:

```
/// An address inside `PrimaryEmailAddress`'s modelled `@length(min: 5, max:
/// 64)` and this model's email expression, so the acceptance row is refused for
/// its account and never for a malformed member.
const WELL_FORMED_EMAIL: &str = "grace@example.com";
```

tests/all/primary_email_address_syntax.rs drives the malformed-address refusal on AcceptPrimaryEmailUpdate only after a `start` has created the account record (:447-458), so no test combines an unknown account with a malformed address.

## Steps to Reproduce

1. Register `EmulatedService` for `service-com-amazonaws-account-account` on a `MultiServiceRouter` over a fresh `engine::InMemoryEngine` (as tests/all/unknown_account_refusal.rs does).
2. POST /acceptPrimaryEmailUpdate with {"AccountId": "210987654321", "PrimaryEmail": "grace@example.com", "Otp": "Q7r4Tz"} — observe 403 AccessDeniedException, the refusal tests/all/unknown_account_refusal.rs:175-185 pins for an account the store holds no record of.
3. POST /acceptPrimaryEmailUpdate with {"AccountId": "210987654321", "PrimaryEmail": "not-an-email-address", "Otp": "Q7r4Tz"} — observe 400 ValidationException naming PrimaryEmail; the unknown-account 403 is never reached because `accessible_account_mut` runs after `validated_email_address`.
4. POST /acceptPrimaryEmailUpdate with {"AccountId": <state::default_account_id()>, "PrimaryEmail": "not-an-email-address", "Otp": "Q7r4Tz"} — observe 403 AccessDeniedException: the OTHER half of the same rule still outranks the identical 400.
5. POST /verifyPhoneNumber with {"AccountId": "210987654321", "Otp": "Q7r4Tz"} — observe the 403 the shared composition gives in one call, the form this handler does not use.
