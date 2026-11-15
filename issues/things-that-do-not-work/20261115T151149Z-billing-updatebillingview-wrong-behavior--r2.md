Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4

# UpdateBillingView moves viewDefinitionLastUpdatedAt for a dataFilterExpression that is unchanged, contrary to the comment above the write and to both sibling writers of the same pair of instants

_Filed: 2026-11-15T15:11:49Z_
_Operation: UpdateBillingView | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateBillingView` stamps `viewDefinitionLastUpdatedAt` whenever the request CARRIES `dataFilterExpression`, without comparing it with the expression the view already holds. The comment immediately above that assignment states the opposite rule — "so only a request that changes it moves this" — and the two sibling writers that move the same pair of instants (`updatedAt` and `viewDefinitionLastUpdatedAt`) both compare the stored value first and say that they do, one of them naming this very handler as the one that moves the member "when the data filter expression changes". `state::forget_view`, the third writer of the pair, also states that "a view whose set did not contain `arn` is left untouched".

So a caller that re-sends the view's current `dataFilterExpression` (an SDK read-modify-write that resubmits the definition it just read, or a retry of an update that already landed) is told that the billing view's DEFINITION was last updated at the instant of that no-op call, which `BillingViewElement$viewDefinitionLastUpdatedAt` documents as "The timestamp of when the billing view definition was last updated". Nothing in the crate justifies the divergence: no test sends an unchanged expression (the only row that sends one, tests/all/billing_views.rs:766-771, updates a view created from `view_rows()[0]`, whose `data_filter_expression` is `None` — a genuine change).

Note for a fixer: the same handler also stamps `updatedAt` unconditionally at update_billing_view.rs:240, including for a request that carries no mutating member at all, but that no-op case IS asserted today (tests/all/billing_views.rs:765 row "no member at all" + the `updated["updatedAt"].is_number()` assertion at :796-800), so this issue is about the definition instant, which no test pins.

## Evidence

The write, with the rule stated directly above it — services/service-com-amazonaws-billing-awsbilling/src/ops/update_billing_view.rs:226-240:
```
        let updated_at = std::time::SystemTime::now();
        if let Some(name) = input.name {
            view.name = name;
        }
        if let Some(description) = input.description {
            view.description = Some(description);
        }
        if let Some(expression) = input.data_filter_expression {
            view.data_filter_expression = Some(expression);
            // `viewDefinitionLastUpdatedAt` is "the timestamp of when the billing
            // view definition was last updated": the data filter expression IS
            // that definition, so only a request that changes it moves this.
            view.view_definition_last_updated_at = Some(updated_at);
        }
        view.updated_at = Some(updated_at);
```
There is no comparison with `view.data_filter_expression` anywhere in the handler, so "only a request that changes it" is not what the code does.

Sibling writer 1 — services/service-com-amazonaws-billing-awsbilling/src/ops/associate_source_views.rs:316-324 (and it names this handler's rule):
```
        let changed = view.source_views != source_views;
        view.source_views = source_views;
        if changed {
            view.updated_at = Some(now);
            // The set of source views is part of the view definition, so a change
            // to it moves `viewDefinitionLastUpdatedAt` — the same member
            // `UpdateBillingView` moves when the data filter expression changes.
            view.view_definition_last_updated_at = Some(now);
        }
```
Sibling writer 2 — services/service-com-amazonaws-billing-awsbilling/src/ops/disassociate_source_views.rs:274-281:
```
        if view.source_views != remaining {
            view.source_views = remaining;
            view.updated_at = Some(now);
            // The set of source views is part of the view definition, so a change
            // to it moves `viewDefinitionLastUpdatedAt` — the same member
            // `AssociateSourceViews` moves when it widens that set.
            view.view_definition_last_updated_at = Some(now);
        }
```
Sibling writer 3 — services/service-com-amazonaws-billing-awsbilling/src/state.rs:944-962:
```
    /// A view that drew from `arn` loses that one source and keeps the rest in
    /// the order it held them. Its source set is part of its definition, so the
    /// two instants a definition change moves — `updatedAt` and
    /// `viewDefinitionLastUpdatedAt` ... move to `at`, and a view whose set did not contain `arn` is left
    /// untouched.
    pub fn forget_view(&mut self, arn: &str, at: SystemTime) -> Option<BillingView> {
...
            if !view.source_views.iter().any(|source| source == arn) {
                continue;
            }
```
Smithy: `BillingViewElement$viewDefinitionLastUpdatedAt` — "The timestamp of when the billing view definition was last updated."

Test coverage today: tests/all/billing_views.rs:766-771 is the only row that sends `dataFilterExpression` to UpdateBillingView, and the view it updates is built from `view_rows()[0]`, whose `data_filter_expression: None` (tests/all/billing_views.rs:247) — so the expression genuinely changes there and no test pins the unchanged case. tests/all/billing_view_delete_cascade.rs:111-116 and tests/all/billing_view_health_gate.rs:222 assert only that a view no write touched carries NEITHER instant.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs::emulator()).
2. CreateBillingView with {"name":"demo","sourceViews":[<the account's PRIMARY ARN>],"dataFilterExpression":{"dimensions":{"key":"LINKED_ACCOUNT","values":["000000000000"]}}}.
3. GetBillingView on the new ARN and note that `viewDefinitionLastUpdatedAt` is absent (the create records none).
4. UpdateBillingView with {"arn":<the new ARN>,"dataFilterExpression":{"dimensions":{"key":"LINKED_ACCOUNT","values":["000000000000"]}}} — byte-for-byte the expression the view already holds.
5. GetBillingView again: `viewDefinitionLastUpdatedAt` is now set to the instant of that call, although the view definition did not change — while the same no-op through AssociateSourceViews (re-associating a source the view already draws from) leaves both instants exactly as they were (associate_source_views.rs:316-324).
