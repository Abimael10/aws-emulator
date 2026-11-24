Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 8ed8c28d7ced8b3283a18b17191a631681e35604

# GetRegionOptStatus decides its own unknown-RegionName 400 before the account-target composition, so both account-target 403s are outranked by a refusal the handler makes of its own

_Filed: 2026-11-23T11:44:44Z_
_Operation: GetRegionOptStatus | Class: error-response_

> _Note: this issue was identified against the code base at commit `8ed8c28d7ced8b3283a18b17191a631681e35604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetRegionOptStatus` is a reporting operation on an account that must already exist, and it does reach the one composition the state module states once (`ComAmazonawsAccountAccountState::target_account`). But it decides its OWN member refusal first: the `RegionName` catalog check (`state::region_kind` -> `state::unknown_region`, a 400 `ValidationException` naming `RegionName`) runs at the top of `handle`, and `store.target_account(...)` runs only afterwards.

That inverts the order this crate states for exactly this situation. `tests/all/account_target_composition_is_shared.rs` says the account-target rule is "ONE decision taken WHOLE: it is decided before any refusal a handler makes of its own", and the two handlers whose identical inversion was already filed against this crate now both resolve the account FIRST and decide their own member refusal second: `AcceptPrimaryEmailUpdate` (the `PrimaryEmail` syntax check) and `ListRegions` (the `NextToken` check). `GetRegionOptStatus` is the remaining reader that does the opposite.

Observable effect: for a request that is both an account-target refusal and carries a `RegionName` the catalog does not hold, the handler answers 400 instead of the 403 its siblings answer.

- `{"AccountId": "<id the store holds no record of>", "RegionName": "not-a-region"}` -> 400 `ValidationException` naming `RegionName`; the `account_not_accessible` 403 is never reached.
- `{"AccountId": "<the caller's own id>", "RegionName": "not-a-region"}` -> 400 as well; the `cannot_target_own_account` 403 is never reached.
- The same two account ids with a catalogued `RegionName` -> 403 (what the suites pin).

So for this one operation a member refusal outranks BOTH causes of the account-target rule, while on `AcceptPrimaryEmailUpdate` and `ListRegions` it outranks neither. The defect is the order, not the unknown-Region refusal itself.

## Evidence

The rule this crate states for the combination, in its own suite — services/service-com-amazonaws-account-account/tests/all/account_target_composition_is_shared.rs:17-21:

```
//! The rule is also ONE decision taken WHOLE: it is decided before any refusal
//! a handler makes of its own, so a request that is both an account-target
//! refusal and carries a member such a handler would refuse answers that same
//! 403 — for either cause — while the identical request on an account the store
//! holds is refused for its member.
```

and the test that drives it, account_target_composition_is_shared.rs:448-452:

```
/// The account-target rule is ONE decision, taken whole before any refusal the
/// handler itself decides: a request that is both an account-target refusal and
/// carries a member the handler would refuse answers the IDENTICAL 403 — the
/// same payload the bare request for that account earns — for BOTH causes, so
/// neither cause can be outranked by a member refusal the other one outranks.
```

GetRegionOptStatus decides its own member refusal FIRST — src/ops/get_region_opt_status.rs:171-179:

```
        // `RegionName`'s modelled `@length(min: 1, max: 50)` and `AccountId`'s
        // `^\d{12}$` were already enforced at the boundary. WHICH strings name a
        // Region is not modelled, so that is refused here.
        let Some(kind) = crate::state::region_kind(&input.region_name) else {
            return Err(GetRegionOptStatusErrorModeled::ValidationException(
                crate::state::unknown_region(&input.region_name),
            )
            .into());
        };
```

and only then resolves the account — get_region_opt_status.rs:188-191:

```
        let store = state.lock();
        let account = store
            .target_account(input.account_id.as_deref())
            .map_err(GetRegionOptStatusErrorModeled::AccessDeniedException)?;
```

The refusal it decides first is a 400 on an input member — src/state.rs:1780-1785:

```
pub fn unknown_region(region_name: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "{REGION_NAME_MEMBER} {region_name:?} is not an Amazon Web Services Region code"
        ),
```

The two siblings that do the opposite. src/ops/accept_primary_email_update.rs:204-209 (composition first):

```
        let mut store = state.lock();
        let account_id = store
            .target_account_mut(Some(&input.account_id))
            .map_err(AcceptPrimaryEmailUpdateErrorModeled::AccessDeniedException)?
            .account_id
            .clone();
```

then its own member refusal, accept_primary_email_update.rs:218-223:

```
        if let Err(refusal) = crate::state::validated_email_address(
            crate::state::PRIMARY_EMAIL_MEMBER,
            &input.primary_email,
        ) {
            return Err(AcceptPrimaryEmailUpdateErrorModeled::ValidationException(refusal).into());
        }
```

and src/ops/list_regions.rs:251-263, composition first, token refusal second:

```
        let store = state.lock();
        let account = store
            .target_account(input.account_id.as_deref())
            .map_err(ListRegionsErrorModeled::AccessDeniedException)?;

        let page_size = usize::try_from(input.max_results.unwrap_or(MAX_PAGE_SIZE))
            .unwrap_or(MAX_PAGE_SIZE as usize);
        let resume_after = match input.next_token.as_deref() {
            None => None,
            Some(token) => Some(decode_page_token(token).ok_or_else(|| {
                ListRegionsErrorModeled::ValidationException(invalid_next_token())
            })?),
        };
```

Test coverage: no suite exercises the combination. account_target_composition_is_shared.rs:159-161 drives GetRegionOptStatus only with a catalogued Region:

```
        ("GetRegionOptStatus", "/getRegionOptStatus", |id| {
            json!({"AccountId": id, "RegionName": CATALOGUED_REGION})
        }),
```

and its `refusable_rows()` table (account_target_composition_is_shared.rs:363-367, "Every operation of [`rows`] that decides a refusal of its OWN inside the handler") lists only AcceptPrimaryEmailUpdate's `PrimaryEmail` and ListRegions' `NextToken` — GetRegionOptStatus, which also decides a refusal of its own, has no row. tests/all/unknown_account_refusal.rs:38-40 likewise picks a catalogued Region on purpose (`const CATALOGUED_REGION: &str = "af-south-1"; // A Region code this service's catalog holds, needed by the Region reads`), and tests/all/own_account_target_refusal.rs:50-53 does the same (`const DEFAULT_ON_REGION`/`OPT_IN_REGION`), so nothing pins the current order.

## Steps to Reproduce

1. Register `EmulatedService` for `service-com-amazonaws-account-account` on a `MultiServiceRouter` over a fresh `engine::InMemoryEngine` (as tests/all/account_target_composition_is_shared.rs:66-72 does).
2. POST /getRegionOptStatus with {"AccountId": "400000000002", "RegionName": "eu-north-1"} — observe 403 AccessDeniedException, the refusal account_target_composition_is_shared.rs:159-161 pins for an account the store holds no record of.
3. POST /getRegionOptStatus with {"AccountId": "400000000002", "RegionName": "not-a-region"} — observe 400 ValidationException naming RegionName; the unknown-account 403 is never reached because `target_account` runs after `region_kind`.
4. POST /getRegionOptStatus with {"AccountId": <state::default_account_id()>, "RegionName": "not-a-region"} — observe the same 400; the own-account 403 is not reached either.
5. POST /listRegions with {"AccountId": "400000000002", "NextToken": "6e6f742061207265616c20746f6b656e"} and POST /acceptPrimaryEmailUpdate with {"AccountId": "400000000002", "PrimaryEmail": "not-an-email-address", "Otp": "Q7r4Tz"} — observe 403 for both: the order the two sibling handlers apply for the same combination.
