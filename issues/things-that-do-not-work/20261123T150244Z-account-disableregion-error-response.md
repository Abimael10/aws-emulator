Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 6c4ddbfafc8219d6aa6558c1dbbd2baee1b70bbe

# DisableRegion decides its own Region-opt-target 400 before the account-target rule, so the own-account 403 is outranked

_Filed: 2026-11-23T15:02:44Z_
_Operation: DisableRegion | Class: error-response_

> _Note: this issue was identified against the code base at commit `6c4ddbfafc8219d6aa6558c1dbbd2baee1b70bbe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Same inversion as on `EnableRegion`, in the second copy of the opt path. `DisableRegion` calls `crate::state::region_opt_target(&region_name)` at the top of `handle` and only afterwards calls `crate::state::target_account_id(...)`, although this crate states the account-target rule as one decision taken whole before any refusal a handler makes of its own, and its siblings `StartPrimaryEmailUpdate` / `AcceptPrimaryEmailUpdate` resolve the account id first.

Consequence: the same `AccountId` member earns two different answers depending on which `RegionName` travels with it.

- `{"AccountId": <the caller's own id>, "RegionName": "ap-east-1"}` -> 403 `AccessDeniedException`, the answer tests/all/own_account_target_refusal.rs pins for this operation.
- `{"AccountId": <the caller's own id>, "RegionName": "us-east-1"}` -> 400 `ValidationException` "us-east-1 is not a valid region for opt-in or opt-out."; the 403 is never reached.

The defect is the ordering, not either refusal. `target_account_id` is a pure function of the member and touches no store, so deciding it before `region_opt_target` leaves every standalone call (the only shape the Region-opt suites drive) answering exactly as it does today.

## Evidence

The rule as this crate's own suite states it — services/service-com-amazonaws-account-account/tests/all/account_target_composition_is_shared.rs:17-21:

```
//! The rule is also ONE decision taken WHOLE: it is decided before any refusal
//! a handler makes of its own, so a request that is both an account-target
//! refusal and carries a member such a handler would refuse answers that same
//! 403 — for either cause — while the identical request on an account the store
//! holds is refused for its member.
```

The sibling that applies it in that order — services/service-com-amazonaws-account-account/src/ops/accept_primary_email_update.rs:189-190, :205-209 and :218-223:

```
        // The authorization rule comes FIRST, as it does on
        // `StartPrimaryEmailUpdate`, and it comes WHOLE: ...
        let account_id = store
            .target_account_mut(Some(&input.account_id))
            .map_err(AcceptPrimaryEmailUpdateErrorModeled::AccessDeniedException)?
...
        if let Err(refusal) = crate::state::validated_email_address(
            crate::state::PRIMARY_EMAIL_MEMBER,
            &input.primary_email,
        ) {
            return Err(AcceptPrimaryEmailUpdateErrorModeled::ValidationException(refusal).into());
        }
```

DisableRegion inverts it — services/service-com-amazonaws-account-account/src/ops/disable_region.rs:203-206:

```
        let region_name = input.region_name;
        if let Err(invalid) = crate::state::region_opt_target(&region_name) {
            return Err(DisableRegionErrorModeled::ValidationException(invalid).into());
        }
```

and only then disable_region.rs:213-218:

```
        let account_id = match crate::state::target_account_id(input.account_id.as_deref()) {
            Ok(account_id) => account_id,
            Err(denied) => {
                return Err(DisableRegionErrorModeled::AccessDeniedException(denied).into())
            }
        };
```

The 403 pinned for this operation — services/service-com-amazonaws-account-account/tests/all/own_account_target_refusal.rs:203-205:

```
        ("DisableRegion", "/disableRegion", |account| {
            with_account(json!({"RegionName": OPT_IN_REGION}), "AccountId", account)
        }),
```

which only ever drives a valid opt target (`OPT_IN_REGION = "ap-east-1"`, own_account_target_refusal.rs:53), so the split is unexercised; and the opt-target suite sends no account-id member at all (tests/all/region_opt_target_rule.rs:161-167), so nothing pins the inverted order.

## Steps to Reproduce

1. Register `EmulatedService` for service-com-amazonaws-account-account on a `MultiServiceRouter` over a fresh `engine::InMemoryEngine` with `set_request_authentication(false)`.
2. POST /disableRegion with body {"AccountId": <state::default_account_id()>, "RegionName": "ap-east-1"} — observe 403 AccessDeniedException, the refusal tests/all/own_account_target_refusal.rs pins for DisableRegion.
3. POST /disableRegion with body {"AccountId": <state::default_account_id()>, "RegionName": "us-east-1"} — observe 400 ValidationException "us-east-1 is not a valid region for opt-in or opt-out." instead of the identical 403 from step 2.
4. For contrast, POST /acceptPrimaryEmailUpdate with body {"AccountId": <state::default_account_id()>, "PrimaryEmail": "not-an-email-address", "Otp": "A1b2C3"} — observe 403, because that sibling decides the account-target rule before refusing its own member.
