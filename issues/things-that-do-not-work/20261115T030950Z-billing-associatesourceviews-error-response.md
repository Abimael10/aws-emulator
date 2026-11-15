Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 70ff1500c355ed7fc4788da90f349959a442a9b5

# The sourceViews writers resolve a foreign-account billing-view ARN against the store, answering 404 where every other ARN-addressed path answers 403

_Filed: 2026-11-15T03:09:49Z_
_Operation: AssociateSourceViews | Class: error-response_

> _Note: this issue was identified against the code base at commit `70ff1500c355ed7fc4788da90f349959a442a9b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`AssociateSourceViews` applies the crate's account-boundary gate to `input.arn` as its very first act, but then resolves every entry of `input.source_views` straight against `billing_views` with no account check at all. A `sourceViews` entry that is a well-formed `BillingViewArn` of ANOTHER account is therefore answered with `ResourceNotFoundException` (404) — "sourceViews: billing view <arn> does not exist" — rather than the modeled `AccessDeniedException` (403) the operation declares, so the answer to a foreign-account ARN depends on whether a view exists in that account.

`CreateBillingView` has the identical omission on the identical member: it calls `state.unknown_source_view(&input.source_views)` and renders the miss through `source_view_not_found`, with no `is_caller_account_arn` call anywhere in the handler.

This contradicts the rule stated on the helper the rest of the crate routes through: `state::is_caller_account_arn`'s doc says EVERY operation that identifies a billing view by ARN checks it BEFORE the lookup, "so the answer to a foreign-account ARN never depends on whether a view exists there". A `sourceViews` entry identifies a billing view by ARN — the refusal even reports `resourceType: "billingview"` and the ARN as `resourceId` — and both writers of the field skip the gate the same handler already calls one line earlier for `arn`. This is the same divergence two already-fixed issues closed for `ListSourceViewsForBillingView$arn` and for the tagging/policy plane's `resourceArn`; the `sourceViews` member is the surface that was left behind.

## Evidence

The rule, stated on the helper — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1451-1458:
```
/// Whether `arn` is scoped to the caller's own account.
///
/// Every operation that identifies a billing view by ARN checks this BEFORE it
/// looks the ARN up, so the answer to a foreign-account ARN never depends on
/// whether a view exists there.
pub fn is_caller_account_arn(arn: &str) -> bool {
    arn_account_id(arn) == Some(caller_account_id())
}
```
The same handler obeying it for `arn` — services/service-com-amazonaws-billing-awsbilling/src/ops/associate_source_views.rs:198-212:
```
        // The account boundary comes first, as it does for every operation that
        // identifies a billing view by ARN: a well-formed ARN outside the
        // caller's own account is refused before the store is consulted, so the
        // answer never depends on what another account holds.
        if !crate::state::is_caller_account_arn(&input.arn) {
            return Err(AssociateSourceViewsErrorModeled::AccessDeniedException(
                crate::state::cross_account_access_denied(
```
...and skipping it for `sourceViews` — services/service-com-amazonaws-billing-awsbilling/src/ops/associate_source_views.rs:264-268:
```
        // Every source must name a view this account holds, or the association
        // would point at nothing: ...
        if let Some(missing) = state.unknown_source_view(&input.source_views) {
            return Err(not_found(missing, "sourceViews"));
        }
```
where `not_found` is the 404 — associate_source_views.rs:162-167:
```
fn not_found(arn: &str, member: &str) -> AssociateSourceViewsError {
    AssociateSourceViewsErrorModeled::ResourceNotFoundException(
        crate::state::billing_view_not_found(arn, member),
    )
    .into()
}
```
The second writer of the same field, with no account check anywhere in the handler — services/service-com-amazonaws-billing-awsbilling/src/ops/create_billing_view.rs:292-294:
```
        if let Some(missing) = state.unknown_source_view(&input.source_views) {
            return Err(source_view_not_found(missing));
        }
```
The model gives both operations the 403 this path never answers — OPERATIONS.md:32 (AssociateSourceViews) and :39 (CreateBillingView): "**Errors:** `AccessDeniedException (HTTP 403)`, ... `ResourceNotFoundException (HTTP 404)`, ...".

Test coverage: the boundary suite exercises AssociateSourceViews only through its `arn` member, holding `sourceViews` at the caller's own view — services/service-com-amazonaws-billing-awsbilling/tests/all/arn_account_boundary.rs:48-50:
```
    ("AssociateSourceViews", |arn| {
        json!({ "arn": arn, "sourceViews": [PRIMARY_ARN] })
```
and the cross-account table claims completeness while omitting both `sourceViews` writers — tests/all/rejection_fidelity.rs:334-341: "// Every implemented operation that identifies a billing view by ARN; the last one reaches the ARN through the `arns` filter." followed by only GetBillingView/UpdateBillingView/DeleteBillingView/ListBillingViews. The current 404 is pinned by tests/all/source_view_existence.rs:103-107 (row "a source in another account the store does not hold", `view_arn("111122223333", "primary")`) asserted at :201-207 as `(404, "ResourceNotFoundException")` — a row whose own name concedes the answer is decided by what the store holds, which is exactly what state.rs:1453-1455 says it must not be.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs::emulator_with_engine, which records the caller as an organization management account so CreateBillingView is reachable).
2. Call GetBillingView(arn="arn:aws:billing::111122223333:billingview/primary") and observe 403 AccessDeniedException (pinned by tests/all/rejection_fidelity.rs:343-357).
3. Call CreateBillingView(name="x", sourceViews=["arn:aws:billing::111122223333:billingview/primary"]) with that same foreign ARN.
4. Observe 404 ResourceNotFoundException with message "sourceViews: billing view arn:aws:billing::111122223333:billingview/primary does not exist", resourceId = that ARN and resourceType = "billingview", instead of the modeled 403 AccessDeniedException.
5. Create a CUSTOM view of the caller's own account, then call AssociateSourceViews(arn=<that view>, sourceViews=["arn:aws:billing::111122223333:billingview/primary"]) and observe the same 404 — the handler having already applied the 403 gate to `arn` one line earlier.
