Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: f8e20ae46548b9b285010d18fd783588395da7bd

# An own-account ARN that no view is stored under is refused 400 ValidationException, so the modelled ResourceNotFoundException UpdateBillingView declares is never reachable and the crate's one-form rule for that ARN is broken

_Filed: 2026-11-24T23:45:29Z_
_Operation: UpdateBillingView | Class: error-response_

> _Note: this issue was identified against the code base at commit `f8e20ae46548b9b285010d18fd783588395da7bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

For one and the same input — a well-formed `BillingViewArn` in the CALLER'S OWN account that no billing view is stored under — this crate answers two different modelled errors depending on which operation was asked:

* `GetBillingView`, `ListSourceViewsForBillingView`, `ListBillingViewSegments`, `AssociateSourceViews` and `DisassociateSourceViews` answer HTTP 404 `ResourceNotFoundException`, all built by the one shared payload `crate::state::unresolved_billing_view_arn`.
* `UpdateBillingView` answers HTTP 400 `ValidationException` (`crate::state::not_a_custom_view`), because it resolves the ARN through `custom_view_mut` and collapses "absent" into "not a CUSTOM view".

Two consequences:

1. `UpdateBillingView` DECLARES `ResourceNotFoundException` (HTTP 404), whose model documentation states its trigger — "The specified ARN in the request doesn't exist" — and the handler never constructs that variant at all (`UpdateBillingViewErrorModeled::ResourceNotFoundException` appears only in the generated enum, never in the `handle` body). The input class the declared error documents is the exact input the handler answers 400 for, so the declared error is unreachable. This is not the harmless case of a declared-but-unused variant: `ConflictException` and `ServiceQuotaExceededException` are also declared and unused by this handler, but neither has a documented trigger an update request can reach, while the 404's documented trigger — the ARN does not exist — is reachable on every call.
2. It breaks a rule this crate states about itself. The shared helper's own doc says the refusal for this case is ONE form "so an unresolvable ARN of the caller's own account draws the same refusal whichever operation resolved it, read or write" — and `UpdateBillingView` is a writer that resolves exactly that member. The two sibling WRITERS of the same resource (`AssociateSourceViews`, `DisassociateSourceViews`) do answer the shared 404.

Note for whoever fixes this: `DeleteBillingView` behaves the same way but is CORRECT, because its model declares no `ResourceNotFoundException` at all (OPERATIONS.md:46) — there the collapse into `ValidationException` is the only refusal it can make. Only `UpdateBillingView` declares the 404 and withholds it.

A test pins today's behaviour with a stated reason ("Only a CUSTOM view may be edited, and eligibility is decided before existence, so an absent ARN is the same 400 as the account's PRIMARY view"), and the handler carries the same reason ("the modeled `ResourceNotFoundException` would tell a caller which of the two it was"). That reason is contradicted on both sides: by the model, which declares the 404 for this very trigger on this very operation, and by the crate itself — the non-disclosure the comment aims for does not exist, because `GetBillingView` on the same ARN already tells any caller which of the two it is (404 for absent, 200 for the account's `PRIMARY` view), and both ARNs are in the caller's own account so no cross-account signal is involved. Fixing this means answering `state::unresolved_billing_view_arn` for an absent ARN and keeping `not_a_custom_view` for a stored non-CUSTOM view, and updating that one test row.

## Evidence

The model declares the error and documents its trigger for THIS operation — services/service-com-amazonaws-billing-awsbilling/OPERATIONS.md:182 lists for UpdateBillingView: `ResourceNotFoundException (HTTP 404)`, and the handler's own model-derived doc block, services/service-com-amazonaws-billing-awsbilling/src/ops/update_billing_view.rs:89:
```
//! * `ResourceNotFoundException` (HTTP 404) — The specified ARN in the request doesn't exist.
```
(same sentence from `smithy_query(query="shape", shape="ResourceNotFoundException")`).

The handler, which answers 400 for that trigger and never constructs the 404 — src/ops/update_billing_view.rs:220-229:
```
        // Eligibility is decided BEFORE existence: only a `CUSTOM` view may be
        // edited, so an ARN that names no `CUSTOM` view of this account — absent
        // or the account's `PRIMARY` view — is one refusal, and the modeled
        // `ResourceNotFoundException` would tell a caller which of the two it
        // was.
        let view = state.custom_view_mut(&input.arn).ok_or_else(|| {
            UpdateBillingViewErrorModeled::ValidationException(crate::state::not_a_custom_view(
                &input.arn, "edited",
            ))
        })?;
```
`grep ResourceNotFound src/ops/update_billing_view.rs` matches only the doc line (:89) and the generated enum variant (:117) — nothing in `handle` builds it.

The crate's own stated rule for this exact case — src/state.rs:1694-1706 (doc of `unresolved_billing_view_arn`):
```
/// The modeled `ResourceNotFoundException` payload (HTTP 404) for the billing
/// view a request identifies by its own `arn` member when this account holds no
/// view under that ARN — "The specified ARN in the request doesn't exist".
///
/// ONE form for that case, stated once, so an unresolvable ARN of the caller's
/// own account draws the same refusal whichever operation resolved it, read or
/// write: ...
pub fn unresolved_billing_view_arn(arn: &str) -> ResourceNotFoundException {
```

The two sibling WRITERS that do apply it — src/ops/disassociate_source_views.rs:228-230:
```
        if !state.billing_views.contains_key(&input.arn) {
            return Err(not_found(&input.arn));
        }
```
with src/ops/disassociate_source_views.rs:139-144:
```
fn not_found(arn: &str) -> DisassociateSourceViewsError {
    DisassociateSourceViewsErrorModeled::ResourceNotFoundException(
        crate::state::unresolved_billing_view_arn(arn),
    )
    .into()
}
```
and src/ops/associate_source_views.rs:231-233 (`let Some(target) = state.billing_views.get(&input.arn) else { return Err(not_found(&input.arn)); };`) with its `not_found` at associate_source_views.rs:163-168.

Test coverage today — services/service-com-amazonaws-billing-awsbilling/tests/all/billing_views.rs:968-997 pins both answers side by side for the SAME ARN (`ABSENT_ARN`, which is in the caller's account: tests/all/billing_views.rs:22 `pub(crate) const ACCOUNT_ID: &str = "000000000000";` and :30 `pub(crate) const ABSENT_ARN: &str = "arn:aws:billing::000000000000:billingview/does-not-exist";`):
```
        ("get an absent view", "GetBillingView", json!({ "arn": ABSENT_ARN }), 404, "ResourceNotFoundException"),
        (
            // Only a CUSTOM view may be edited, and eligibility is decided
            // before existence, so an absent ARN is the same 400 as the
            // account's PRIMARY view.
            "update an absent view", "UpdateBillingView",
            json!({ "arn": ABSENT_ARN, "name": "whatever" }), 400, "ValidationException",
        ),
```
and the opposite answer for the same ARN on a sibling writer — tests/all/source_view_disassociation.rs:359-365:
```
        (
            "an ARN no view is stored under",
            ABSENT_ARN.to_string(),
            404,
            "ResourceNotFoundException",
        ),
```

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs).
2. Pick an ARN in the caller's own account that no view is stored under: `arn:aws:billing::000000000000:billingview/does-not-exist` (tests/all/billing_views.rs:30).
3. Call GetBillingView(arn=<that ARN>): HTTP 404 `ResourceNotFoundException`, message `[arn:aws:billing::000000000000:billingview/does-not-exist] is an invalid billing view arn`.
4. Call DisassociateSourceViews(arn=<the same ARN>, sourceViews=[<any stored view>]): HTTP 404 `ResourceNotFoundException`, the same message — the shared `state::unresolved_billing_view_arn` payload.
5. Call UpdateBillingView(arn=<the same ARN>, name="whatever"): HTTP 400 `ValidationException` with the not-a-CUSTOM-view sentence, although the operation declares `ResourceNotFoundException` (HTTP 404) for "The specified ARN in the request doesn't exist" and that declared error is never emitted by the handler for any input.
