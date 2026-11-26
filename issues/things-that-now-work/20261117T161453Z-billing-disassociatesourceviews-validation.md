Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 81ee6daca0ad8b587dff6c463aefc2067d56c432

# A disassociation that removes a view's LAST source view is accepted, so the view's source list falls outside the @length(min: 1) the crate itself cites when enforcing the other side of that bound

_Filed: 2026-11-17T16:14:53Z_
_Operation: DisassociateSourceViews | Class: validation_

> _Note: this issue was identified against the code base at commit `81ee6daca0ad8b587dff6c463aefc2067d56c432`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DisassociateSourceViews` computes the set difference of the stored `source_views` and the request's list and stores the result with no lower bound on what is left, so a request naming every source the view holds leaves the stored list EMPTY. `ListSourceViewsForBillingView` then serves `sourceViews: []` for that view, although its output member is `@required` and targets `BillingViewSourceViewsList`, whose `@length` is `min: 1, max: 10`; `GetBillingView` reports `sourceViewCount: 0` for the same record.

The crate treats that very trait as binding for its OTHER side: `state::MAX_SOURCE_VIEWS_PER_VIEW` is documented as "the model's own bound, not an emulator choice ... whose `@length` is 1..10", and `AssociateSourceViews` refuses a request that would push the stored list past 10 with the modeled `ServiceQuotaExceededException`. The same stored field is therefore guarded against exceeding the maximum of the modelled length and left unguarded against falling under its minimum, by the two handlers that are each other's inverse (both call the gate "the set" the association is). A view also cannot be BORN with no source: `CreateBillingView$sourceViews` is `@required` with the same `@length`, so zero sources is a state only this handler's write path (and the forced-delete cascade) can produce.

Full disclosure of two things a fixer must weigh: (1) the current outcome is pinned by two rows of `tests/all/source_view_disassociation.rs`, which assert `remaining: vec![]` and read it back as an empty `sourceViews` — those rows state a reason about set-difference semantics only, not about whether emptying the set is permitted; (2) `DeleteBillingView` with `force: true` reaches the same zero-source state through `state::forget_view`, which an already-fixed issue deliberately introduced to avoid dangling source ARNs — so a fix should refuse the unforced disassociation that empties the list without reversing that cascade.

## Evidence

Model (smithy_query query="shape" shape="BillingViewSourceViewsList,ListSourceViewsForBillingViewResponse"):
```
"BillingViewSourceViewsList": {"type": "list", "length": {"min": 1, "max": 10}, "member": {"target": "BillingViewArn", ...}}
"ListSourceViewsForBillingViewResponse": {"members": {"sourceViews": {"target": "BillingViewSourceViewsList", "required": true, "length": {"min": 1, "max": 10}, "type": "list", "doc": "<p>A list of billing views used as the data source for the custom billing view. </p>"}, ...}}
```

The crate citing that same trait as binding, and enforcing only its maximum — services/service-com-amazonaws-billing-awsbilling/src/state.rs:145-153:
```
/// The most source views one billing view can draw from.
///
/// This is the model's own bound, not an emulator choice: both
/// `AssociateSourceViews$sourceViews` and `ListSourceViewsForBillingView`'s
/// output target `BillingViewSourceViewsList`, whose `@length` is 1..10. A
/// request that would push a view past it is refused with the modeled
/// `ServiceQuotaExceededException` ("exceeded the size of an individual
/// resource").
pub const MAX_SOURCE_VIEWS_PER_VIEW: usize = 10;
```
enforced in services/service-com-amazonaws-billing-awsbilling/src/ops/associate_source_views.rs:292-306:
```
        if source_views.len() > crate::state::MAX_SOURCE_VIEWS_PER_VIEW {
            return Err(AssociateSourceViewsErrorModeled::ServiceQuotaExceededException(
                ServiceQuotaExceededException {
                    message: format!(
                        "A billing view can draw from at most {} source views",
                        crate::state::MAX_SOURCE_VIEWS_PER_VIEW
                    ),
```

The inverse handler, with no minimum check — services/service-com-amazonaws-billing-awsbilling/src/ops/disassociate_source_views.rs:268-281:
```
        let remaining: Vec<String> = view
            .source_views
            .iter()
            .filter(|source| !input.source_views.contains(source))
            .cloned()
            .collect();
        if view.source_views != remaining {
            view.source_views = remaining;
            view.updated_at = Some(now);
```

The read that then serves the empty list — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:425-431:
```
        Ok(ListSourceViewsForBillingViewOutput {
            // Derived on every read from the one stored copy of the view, so a
            // list cannot disagree with what `GetBillingView` reports as
            // `sourceViewCount`.
            source_views: page.iter().map(|arn| (*arn).to_string()).collect(),
            next_token,
        })
```

Test coverage that pins the current outcome (stating only the set-difference rule) — services/service-com-amazonaws-billing-awsbilling/tests/all/source_view_disassociation.rs:53-58:
```
        Row {
            case: "the only source view",
            held: vec![PRIMARY_ARN.to_string()],
            remove: vec![PRIMARY_ARN.to_string()],
            remaining: vec![],
        },
```
and :95-100 ("all ten, the modeled maximum of one request" → `remaining: vec![]`), read back at :215-224 as an empty `sourceViews` and `sourceViewCount` 0.

The other path into the same state, which a fix must not reverse — services/service-com-amazonaws-billing-awsbilling/src/state.rs:952-962 (`forget_view`): "A view that drew from `arn` loses that one source and keeps the rest in the order it held them."

## Steps to Reproduce

1. Boot the emulator with the harness in tests/all/billing_views.rs::emulator_with_engine (which records the caller as an organization management account so CreateBillingView is reachable).
2. Call CreateBillingView(name="only-one", sourceViews=["arn:aws:billing::000000000000:billingview/primary"]) and note the minted ARN — the model requires at least one source view here, so no view can exist with none.
3. Call DisassociateSourceViews(arn=<that ARN>, sourceViews=["arn:aws:billing::000000000000:billingview/primary"]).
4. Observe 200 and the view's source list stored EMPTY, rather than a refusal of a request that takes the list below the modelled minimum of 1 (contrast AssociateSourceViews, which refuses the same field going above the maximum of the same @length with ServiceQuotaExceededException).
5. Call ListSourceViewsForBillingView(arn=<that ARN>) and observe `sourceViews` served as an empty list, although the response member is @required and targets BillingViewSourceViewsList with @length(min: 1, max: 10); GetBillingView(arn=<that ARN>) reports sourceViewCount: 0 for the same record.
