Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 6c4ddbfafc8219d6aa6558c1dbbd2baee1b70bbe

# EnableRegion decides its own Region-opt-target 400 before the account-target rule, so the own-account 403 is outranked

_Filed: 2026-11-23T15:02:44Z_
_Operation: EnableRegion | Class: error-response_

> _Note: this issue was identified against the code base at commit `6c4ddbfafc8219d6aa6558c1dbbd2baee1b70bbe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states the account-target rule as ONE decision that is taken WHOLE, before any refusal a handler makes about its own members: an account-id member naming the caller's own account is refused 403 `AccessDeniedException` (`state::target_account_id` / `state::cannot_target_own_account`), and the siblings that also carry a handler-decided member refusal (`StartPrimaryEmailUpdate`, `AcceptPrimaryEmailUpdate`) resolve the account FIRST and only then look at the member they refuse themselves.

`EnableRegion` inverts that order. It calls `crate::state::region_opt_target(&region_name)` at the top of `handle` and only afterwards calls `crate::state::target_account_id(...)`. So the same `AccountId` member earns two different answers depending on which `RegionName` happens to travel with it:

- `{"AccountId": <the caller's own id>, "RegionName": "ap-east-1"}` (an opt-in Region) -> 403 `AccessDeniedException`, the refusal `tests/all/own_account_target_refusal.rs` pins for this operation.
- `{"AccountId": <the caller's own id>, "RegionName": "us-east-1"}` (a Region the catalog turns on by default) -> 400 `ValidationException` "us-east-1 is not a valid region for opt-in or opt-out."; the 403 is never reached.

A client therefore learns "that Region cannot be opted into" for a request whose real fault is that it named the management account's own id — the exact classification the 403 exists to give. The defect is the ordering, not either refusal: resolving `target_account_id` (a pure function of the member, which touches no store) before `region_opt_target` would make both cases answer alike, and would change no response for the standalone call that every Region-opt test drives.

## Evidence

The rule as this crate's own suite states it — services/service-com-amazonaws-account-account/tests/all/account_target_composition_is_shared.rs:17-21:

```
//! The rule is also ONE decision taken WHOLE: it is decided before any refusal
//! a handler makes of its own, so a request that is both an account-target
//! refusal and carries a member such a handler would refuse answers that same
//! 403 — for either cause — while the identical request on an account the store
//! holds is refused for its member.
```

The sibling that applies it in that order — services/service-com-amazonaws-account-account/src/ops/start_primary_email_update.rs:186-195:

```
        // The authorization rule comes FIRST, before the proposed address is
        // looked at or stored: this operation updates a member account from the
...
        let account_id = match crate::state::target_account_id(Some(&input.account_id)) {
```

followed only afterwards by its own member check, start_primary_email_update.rs:212-217:

```
        if let Err(refusal) = crate::state::validated_email_address(
            crate::state::PRIMARY_EMAIL_MEMBER,
            &input.primary_email,
        ) {
            return Err(StartPrimaryEmailUpdateErrorModeled::ValidationException(refusal).into());
        }
```

and the second sibling, services/service-com-amazonaws-account-account/src/ops/accept_primary_email_update.rs:189-190 and :205-209:

```
        // The authorization rule comes FIRST, as it does on
        // `StartPrimaryEmailUpdate`, and it comes WHOLE: ...
        let account_id = store
            .target_account_mut(Some(&input.account_id))
            .map_err(AcceptPrimaryEmailUpdateErrorModeled::AccessDeniedException)?
```

EnableRegion inverts it — services/service-com-amazonaws-account-account/src/ops/enable_region.rs:202-205:

```
        let region_name = input.region_name;
        if let Err(invalid) = crate::state::region_opt_target(&region_name) {
            return Err(EnableRegionErrorModeled::ValidationException(invalid).into());
        }
```

and only then enable_region.rs:212-217:

```
        let account_id = match crate::state::target_account_id(input.account_id.as_deref()) {
            Ok(account_id) => account_id,
            Err(denied) => {
                return Err(EnableRegionErrorModeled::AccessDeniedException(denied).into())
            }
        };
```

The 403 that is pinned for this operation — services/service-com-amazonaws-account-account/tests/all/own_account_target_refusal.rs:200-202:

```
        ("EnableRegion", "/enableRegion", |account| {
            with_account(json!({"RegionName": OPT_IN_REGION}), "AccountId", account)
        }),
```

with own_account_target_refusal.rs:52-53 naming the Regions used (`DEFAULT_ON_REGION = "eu-west-1"`, `OPT_IN_REGION = "ap-east-1"`), so that row only ever drives a VALID opt target and never exercises the split.

Nothing pins the inverted order: the opt-target suite drives both opt calls with no account-id member at all — services/service-com-amazonaws-account-account/tests/all/region_opt_target_rule.rs:161-167:

```
    for (operation, uri) in OPT_OPERATIONS {
        answers.push((
            operation,
            call(router, uri, json!({ "RegionName": region_name })).await,
        ));
    }
```

and the catalog of default-on Regions that earn the 400 is state.rs:452-470 (`REGIONS_ENABLED_BY_DEFAULT`, which includes `"us-east-1"`).

## Steps to Reproduce

1. Register `EmulatedService` for service-com-amazonaws-account-account on a `MultiServiceRouter` over a fresh `engine::InMemoryEngine` with `set_request_authentication(false)`, as tests/all/own_account_target_refusal.rs:68-74 does.
2. POST /enableRegion with body {"AccountId": <state::default_account_id()>, "RegionName": "ap-east-1"} — observe 403 AccessDeniedException ("the management account can't specify its own AccountId"), the refusal tests/all/own_account_target_refusal.rs pins for EnableRegion.
3. POST /enableRegion with body {"AccountId": <state::default_account_id()>, "RegionName": "us-east-1"} — observe 400 ValidationException with message "us-east-1 is not a valid region for opt-in or opt-out." and fieldList naming RegionName, instead of the identical 403 from step 2.
4. For contrast, POST /startPrimaryEmailUpdate with body {"AccountId": <state::default_account_id()>, "PrimaryEmail": "not-an-email-address"} — observe 403, because that sibling resolves the account-id member before refusing its own member.
