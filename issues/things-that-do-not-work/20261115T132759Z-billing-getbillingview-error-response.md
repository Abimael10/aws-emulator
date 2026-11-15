Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: cea69faefe0a03548889d0b13bc59c200d97bf70

# GetBillingView hand-rolls its own 404 sentence, breaking the crate's stated one-form rule for a billing-view ARN that resolves to nothing

_Filed: 2026-11-15T13:27:59Z_
_Operation: GetBillingView | Class: error-response_

> _Note: this issue was identified against the code base at commit `cea69faefe0a03548889d0b13bc59c200d97bf70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states, on the helper that builds this message, that there is ONE form for "a billing-view ARN that names no view of this account" and that the billing-view reads which resolve a single `arn` all answer with it. `ListSourceViewsForBillingView` routes through that helper (and says so in a comment). `GetBillingView` does not: it builds `format!("Billing view {} does not exist", input.arn)` inline, and `ListBillingViewSegments` builds the same hand-rolled sentence for the same situation. The result is that one unresolvable ARN of the caller's own account is refused with three different `ResourceNotFoundException` messages depending on which single-ARN read was asked — `[<arn>] is an invalid billing view arn` (ListSourceViewsForBillingView), `Billing view <arn> does not exist` (GetBillingView, ListBillingViewSegments), and `arn: billing view <arn> does not exist` (AssociateSourceViews, via `state::billing_view_not_found`, whose own doc claims it is "Shared by every operation that resolves a billing-view ARN a request names ... so the same unresolvable ARN is refused in one wording whichever operation was asked"). Two helpers in state.rs each claim to be the single form, and the two reads cited here use neither, so nothing in the crate decides what a single-ARN read answers. This is a text-only divergence (status 404, code ResourceNotFoundException, `resourceId`/`resourceType` are already identical across all of them), and a fix is to route GetBillingView's (and ListBillingViewSegments') miss through the helper the sibling read already uses.

## Evidence

The rule, stated on the helper — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1577-1586:
```
/// The message this service's ARN validation answers for a billing-view ARN
/// that names no view of this account: the requested ARN, in brackets, reported
/// as an invalid billing view ARN.
///
/// One form, stated once: `ListBillingViews` already refuses an `arns` filter
/// entry with it, and the billing-view reads that resolve a single `arn` answer
/// the same way.
pub fn invalid_billing_view_arn(arn: &str) -> String {
    format!("[{arn}] is an invalid billing view arn")
}
```
The sibling read that obeys it — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:370-379:
```
        let view = state.billing_views.get(&input.arn).ok_or_else(|| {
            ListSourceViewsForBillingViewErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    // A read that resolves one `BillingViewArn` reports the ARN
                    // it could not resolve, in this service's own ARN-validation
                    // form (`state::invalid_billing_view_arn`).
                    message: crate::state::invalid_billing_view_arn(&input.arn),
```
The read that breaks it — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_view.rs:182-188:
```
        let view = state.billing_views.get(&input.arn).ok_or_else(|| {
            GetBillingViewErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
                message: format!("Billing view {} does not exist", input.arn),
                resource_id: input.arn.clone(),
                resource_type: crate::state::BILLING_VIEW_RESOURCE_TYPE.to_string(),
            })
        })?;
```
The same hand-rolled sentence in the other single-ARN read — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_view_segments.rs:269-277:
```
    let view = match &input.arn {
        Some(arn) => state.billing_views.get(arn).ok_or_else(|| {
            ListBillingViewSegmentsErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: format!("Billing view {arn} does not exist"),
```
And the third form, whose doc ALSO claims to be the single one — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1612-1625:
```
/// The modeled `ResourceNotFoundException` payload (HTTP 404) for a billing-view
/// ARN no stored view answers to, named by the request member that carried it.
///
/// Shared by every operation that resolves a billing-view ARN a request names —
/// the view it acts on and the source views it draws from ... — so the same
/// unresolvable ARN is refused in one wording whichever operation was asked.
pub fn billing_view_not_found(arn: &str, member: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: format!("{member}: billing view {arn} does not exist"),
```
Test coverage today: the shared form is pinned for the sibling read only — services/service-com-amazonaws-billing-awsbilling/tests/all/refusal_and_projection_fidelity.rs:292-312 asserts `message(&body) == format!("[{arn}] is an invalid billing view arn")` for `ListSourceViewsForBillingView`, and tests/all/source_view_disassociation.rs:391-398 and tests/all/cross_account_existence_signal.rs:260-268 assert the same sentence for DisassociateSourceViews and ListSourceViewsForBillingView. The very loop that checks it (cross_account_existence_signal.rs:246-268) then calls `GetBillingView` on the SAME absent ARN at :284-294 and asserts only `(404, "ResourceNotFoundException")`, never the message — so no test pins GetBillingView's third wording, and nothing in the crate justifies it.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs::emulator_with_engine).
2. Pick an ARN of the caller's own account that no view is stored under, e.g. arn:aws:billing::000000000000:billingview/absent.
3. Call ListSourceViewsForBillingView(arn=<that ARN>): 404 ResourceNotFoundException with message "[arn:aws:billing::000000000000:billingview/absent] is an invalid billing view arn" (pinned by tests/all/refusal_and_projection_fidelity.rs:307-312).
4. Call GetBillingView(arn=<the same ARN>): 404 ResourceNotFoundException with message "Billing view arn:aws:billing::000000000000:billingview/absent does not exist" — a second form, although state.rs:1581-1583 says the billing-view reads that resolve a single `arn` answer the same way.
5. Call ListBillingViewSegments(arn=<the same ARN>) for the same hand-rolled form, and AssociateSourceViews(arn=<the same ARN>, sourceViews=[<the account's PRIMARY view>]) for the third form "arn: billing view <arn> does not exist" built by state::billing_view_not_found, whose doc at state.rs:1615-1618 claims it is the one wording every operation resolving such an ARN shares.
