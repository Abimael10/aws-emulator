Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4

# CreateBillingView persists a duplicated sourceViews entry, although AssociateSourceViews keeps that same field a SET and the crate refuses the same duplication in the create's other non-unique list

_Filed: 2026-11-15T15:11:49Z_
_Operation: CreateBillingView | Class: validation_

> _Note: this issue was identified against the code base at commit `1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The modelled `BillingViewSourceViewsList` carries no `@uniqueItems` (only `@length(min:1,max:10)`), so a request may name the same billing-view ARN twice and the rule is the service's to apply. `CreateBillingView` applies none: after resolving every entry against the store it moves the request's list into the record verbatim, duplicates and all.

The other writer of exactly this field refuses to create that state: `AssociateSourceViews` treats the association as a SET and says so, so a source the view already draws from adds nothing. The crate also already has a rule for the OTHER list `CreateBillingView` takes that the model leaves non-unique — `resourceTags` — and that rule is to refuse the request rather than let the stored record depend on arrival order (`state::requested_tag_set`, which the handler calls four lines earlier).

Three reads then report the duplicate as if it were two associations:

1. `sourceViewCount` is documented as "The number of source views associated with this billing view" and is derived as `view.source_views.len()`, so a view created with `sourceViews:[A,A]` reports 2 associations to one view.
2. `ListSourceViewsForBillingView` hands the same ARN back twice (its own cursor code already acknowledges this state: "a view holding the same source ARN twice reports both copies, once each").
3. The per-view source quota `AssociateSourceViews` enforces counts the duplicates, so a view created with ten copies of one ARN is refused `ServiceQuotaExceededException` ("A billing view can draw from at most 10 source views") on the next association although it draws from a single distinct view.

No test in the crate covers a duplicated `sourceViews` entry (tests/all/source_view_existence.rs builds its rows from ten DISTINCT held sources; tests/all/tag_set_duplicate_keys.rs covers only the tag-key rule), so nothing justifies the divergence from the sibling writer.

## Evidence

The create stores the request's list unchanged — services/service-com-amazonaws-billing-awsbilling/src/ops/create_billing_view.rs:315-322:
```
        let arn = state.mint_view_arn();
        let view = crate::state::BillingView {
            arn: arn.clone(),
            name: input.name,
            description: input.description,
            billing_view_type: crate::state::BILLING_VIEW_TYPE_CUSTOM.to_string(),
            owner_account_id: ::engine::emulator_account_id().to_string(),
            source_views: input.source_views,
```
The sibling writer of the same field keeps it a set — services/service-com-amazonaws-billing-awsbilling/src/ops/associate_source_views.rs:285-291:
```
        // The association is a set, so a source already present adds nothing and
        // the request stays idempotent.
        for source in input.source_views {
            if !source_views.contains(&source) {
                source_views.push(source);
            }
        }
```
and enforces the per-view bound on the resulting list — associate_source_views.rs:292-298:
```
        if source_views.len() > crate::state::MAX_SOURCE_VIEWS_PER_VIEW {
            return Err(AssociateSourceViewsErrorModeled::ServiceQuotaExceededException(
                ServiceQuotaExceededException {
                    message: format!(
                        "A billing view can draw from at most {} source views",
                        crate::state::MAX_SOURCE_VIEWS_PER_VIEW
```
The crate's stated rule for the create's OTHER non-unique list — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1555-1563 (called at create_billing_view.rs:253-254):
```
/// A tag set is keyed by key, so one request may not carry a key twice: the two
/// entries would decide the stored value by arrival order, and collecting them
/// into the keyed set would silently drop one of the two values. The modelled
/// `ResourceTagList` carries no `@uniqueItems`, so duplicates reach a handler
/// and the rule is the service's to apply. Every writer that takes a
/// `ResourceTagList` collects it through here ...
```
Smithy: `BillingViewSourceViewsList` is `{"type": "list", "length": {"min": 1, "max": 10}, "member": {"target": "BillingViewArn"}}` — no `@uniqueItems`; `BillingViewElement$sourceViewCount` is documented "The number of source views associated with this billing view."

The count that goes wrong is derived from the stored list — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_view.rs:204-205:
```
        let source_view_count = caller_created
            .then(|| i32::try_from(view.source_views.len()).unwrap_or(i32::MAX));
```
and state.rs:430-434 states the invariant it is meant to serve:
```
/// `sourceViewCount`, `derivedViewCount` and `sourceAccountId` are NOT fields:
/// the first is `source_views.len()`, the second is the number of views whose
/// `source_views` name this one ... all three derived on
/// read so none of them can go stale.
```
The listing already knows this state exists — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:286-288:
```
/// Each ARN the token names accounts for exactly ONE stored occurrence, so a
/// view holding the same source ARN twice reports both copies, once each.
```
Test coverage today: tests/all/source_view_existence.rs:46-50 builds its `sourceViews` rows from ten distinct `held-source-N` ARNs, and tests/all/tag_set_duplicate_keys.rs:264 (`create_billing_view_applies_the_duplicate_tag_key_rule`) pins the duplicate rule for `resourceTags` only — no row sends the same source ARN twice.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs::emulator_with_engine).
2. Call CreateBillingView with {"name":"dup","sourceViews":["arn:aws:billing::000000000000:billingview/primary","arn:aws:billing::000000000000:billingview/primary"]} (the account's PRIMARY ARN twice) — it answers 200 and mints the view.
3. Call GetBillingView on the new ARN: `billingView.sourceViewCount` is 2, although the view is associated with ONE source view.
4. Call ListSourceViewsForBillingView on the new ARN: `sourceViews` carries the same ARN twice.
5. Call AssociateSourceViews(arn=<the new ARN>, sourceViews=[<the same PRIMARY ARN>]): it adds nothing, because that writer keeps the field a set (associate_source_views.rs:285-291) — the state the create just persisted is one its sibling refuses to produce.
6. Repeat the create with ten copies of one ARN, then AssociateSourceViews with a second, distinct source: the request is refused with ServiceQuotaExceededException "A billing view can draw from at most 10 source views" although the view draws from one distinct view.
