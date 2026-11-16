Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 19b22e1e25079acdfeabe3d5ebd7d9ab5a440fb6

# A dataFilterExpression timeRange whose inclusive begin is later than its inclusive end is stored on the view, although the crate refuses that exact relation for every other date range a request carries

_Filed: 2026-11-16T13:15:32Z_
_Operation: UpdateBillingView | Class: validation_

> _Note: this issue was identified against the code base at commit `19b22e1e25079acdfeabe3d5ebd7d9ab5a440fb6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`Expression` (the shape `dataFilterExpression` targets) carries a `timeRange` member of type `TimeRange`, whose two bounds (`beginDateInclusive`, `endDateInclusive`) are both optional `Timestamp`s. Neither writer of that field inspects it: `UpdateBillingView` assigns `view.data_filter_expression = Some(expression)` and `CreateBillingView` stores `input.data_filter_expression` verbatim, so a request carrying `dataFilterExpression.timeRange = { beginDateInclusive: T, endDateInclusive: T-1 }` succeeds, the view is persisted with a data filter whose period selects no instant at all, and `GetBillingView` hands that filter straight back.

The relation between two bounds of one range is exactly the class of rule this crate states it enforces in the handler, because the boundary validator cannot see it: `check_active_time_range` in `ListBillingViews` refuses `activeAfterInclusive > activeBeforeInclusive` under a doc comment that says so in as many words, and the crate applies the same refusal to every other date range a request carries — `ListBillingViewSegments` (`begin >= end`, pinned by three test rows), `ListBusinessSupportSubscriptionHistory` (`start >= end`), `GetCreditAllocationHistory` (`startDate >= endDate`) and `GetCredits` (`endDate` before `startDate`). `ActiveTimeRange` has the same shape as `TimeRange` — two inclusive bounds — and is refused when inverted; the `timeRange` a billing view's data filter carries is the one range in this service that is accepted inverted and then persisted.

Both writers' handler comments assert that "Every modeled constraint ... is refused by the boundary validator before this runs, so reaching here means the input satisfies all of them", which is true and is precisely the point: the ordering of the two `timeRange` bounds is NOT a modeled constraint, so nothing refuses it, and unlike every sibling range in the crate no handler check stands in for it.

## Evidence

The rule, stated on the sibling check of a two-inclusive-bound range — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_views.rs:227-250:
```
/// Refuse every documented relation between the two bounds of the
/// `activeTimeRange` filter — the two rules the boundary validator cannot see,
/// because each is a relation between members rather than a constraint on one.
///
/// 1. The range runs forwards: its inclusive start is not later than its
///    inclusive end.
...
fn check_active_time_range(range: &ActiveTimeRange) -> Result<(), ListBillingViewsError> {
    let after = crate::state::date_time_secs(&range.active_after_inclusive);
    let before = crate::state::date_time_secs(&range.active_before_inclusive);
    if after > before {
        return Err(invalid_field(
            "activeTimeRange",
            "activeAfterInclusive must not be later than activeBeforeInclusive",
        ));
```
The same refusal in the read that takes a `timeRange` — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_view_segments.rs:246-254:
```
    // The period is half-open, so a range whose end does not come after its
    // begin selects no instant at all: that is a request a caller cannot have
    // meant, refused rather than answered with an empty page.
    if begin >= end {
        return Err(invalid_field(
            "timeRange",
            "beginDateInclusive must be earlier than endDateExclusive",
        ));
```
...and it is pinned — services/service-com-amazonaws-billing-awsbilling/tests/all/billing_view_segments.rs:740-760 asserts `400 ValidationException` over the member `timeRange` for "endDateExclusive equal to beginDateInclusive", "one second before" and "a month before".

The writer that skips it — services/service-com-amazonaws-billing-awsbilling/src/ops/update_billing_view.rs:233-239:
```
        if let Some(expression) = input.data_filter_expression {
            view.data_filter_expression = Some(expression);
            // `viewDefinitionLastUpdatedAt` is "the timestamp of when the billing
            // view definition was last updated": the data filter expression IS
            // that definition, so only a request that changes it moves this.
            view.view_definition_last_updated_at = Some(updated_at);
        }
```
and the other writer of the same field — services/service-com-amazonaws-billing-awsbilling/src/ops/create_billing_view.rs:322-323:
```
            source_views: input.source_views,
            data_filter_expression: input.data_filter_expression,
```
under its own claim at create_billing_view.rs:214-217 / update_billing_view.rs:172-173 that "Every modeled constraint ... is refused by the boundary validator before this runs" — which the bound ordering is not.

The model, `smithy_query(query="shape", shape="TimeRange")`: "Specifies a time range with inclusive begin and end dates." with members `beginDateInclusive` ("The inclusive start date of the time range.") and `endDateInclusive` ("The inclusive end date of the time range."); `Expression$timeRange`: "Specifies a time range filter for the billing view data." Neither member carries any trait the boundary validator could use to relate them, and the generated input type keeps both (src/ops/types/shared_types.rs:207-209: `pub begin_date_inclusive: Option<::aws_smithy_types::DateTime>, pub end_date_inclusive: Option<::aws_smithy_types::DateTime>`).

Test coverage today: nothing exercises `Expression.timeRange` at all. Every `dataFilterExpression` a test sends uses `dimensions` or `tags` only — tests/all/billing_views.rs:263-276, tests/all/refusal_message_text.rs:386-389, tests/all/rejection_fidelity.rs:409, tests/all/billing_view_health_gate.rs:52-54 — so no test pins the acceptance of an inverted one, and no comment in the crate justifies it.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs::emulator_with_engine), and make the caller the organization management account the way tests/all/refusal_message_text.rs:393-395 does so CreateBillingView is permitted.
2. CreateBillingView(name="inverted", sourceViews=[the account's PRIMARY view ARN]) and note the minted ARN.
3. Call UpdateBillingView(arn=<that ARN>, dataFilterExpression={"timeRange": {"beginDateInclusive": 1709251200, "endDateInclusive": 1706745600}}) — an inclusive begin a month after the inclusive end.
4. The call answers 200 with `updatedAt`, and GetBillingView(arn=<that ARN>) reports `dataFilterExpression.timeRange` back with those same inverted bounds: a stored filter whose period contains no instant.
5. For contrast, send the same inverted relation to the read that takes a time range — ListBillingViewSegments(arn=<PRIMARY view ARN>, timeRange={"beginDateInclusive": 1709251200, "endDateExclusive": 1706745600}) — and it is refused 400 ValidationException naming `timeRange` (tests/all/billing_view_segments.rs:754-760), as is ListBillingViews(activeTimeRange={activeAfterInclusive: 1709251200, activeBeforeInclusive: 1706745600}).
