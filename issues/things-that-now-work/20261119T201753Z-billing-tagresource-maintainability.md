Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 08a32c2b45cf1544e9455c75c90bbd9565d142c8

# The tagging plane's managed-then-existence decision is written out three times, once per tagging handler

_Filed: 2026-11-19T20:17:53Z_
_Operation: TagResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `08a32c2b45cf1544e9455c75c90bbd9565d142c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The three operations of this service's tagging plane — `TagResource`, `UntagResource` and `ListTagsForResource` — each re-implement the SAME two-step decision about the resource an ARN names, in the same order:

1. look the ARN up in `state.billing_views` and, if the stored view `is_managed()`, refuse with the modeled `ValidationException` (400) carrying `message`/`fieldList[0].message` = `state::MANAGED_VIEW_NOT_TAGGABLE` over the member `resourceArn` and `reason` = `state::VALIDATION_REASON_FIELD`;
2. look the SAME ARN up again and, if nothing is stored, refuse with the modeled `ResourceNotFoundException` (404) carrying `message` = `state::RESOURCE_NOT_FOUND_MESSAGE`, `resourceId` = the requested ARN and `resourceType` = `state::BILLING_VIEW_RESOURCE_TYPE`.

The two sentences are already constants in `state.rs`, but the DECISION — which of the two refusals applies, in which order, and with which `reason`/`fieldList`/`resourceId`/`resourceType` payload — is duplicated in all three handlers, so the three copies can drift (one could start deciding existence before taggability, or stop reporting `resourceType`) while each handler's own doc header asserts they draw the same two lines.

This crate already has the shape of the fix for exactly this situation, applied to the Enterprise Support reads: a shared decision in `crate::state` that answers a payload or a small refusal enum, which each handler lifts into the modeled error ITS operation declares (`state::enterprise_support_read_refusal`, and `billing_month::require_past_billing_month` returning `BillingMonthRefusal` — "The DECISION is shared, the exception is not: each handler turns one of these into the modeled error its own operation declares"). A `state`-level helper that resolves a tagging target to `Result<&BillingView, TaggingRefusal>` (with `Managed` and `Absent` variants, already-built `ValidationException`/`ResourceNotFoundException` payloads) and three call sites that map it into `TagResourceErrorModeled` / `UntagResourceErrorModeled` / `ListTagsForResourceErrorModeled` leaves every response byte and every existing test unchanged: the three payloads are already character-identical (all three use `message: why.to_string()` / `MANAGED_VIEW_NOT_TAGGABLE` and `RESOURCE_NOT_FOUND_MESSAGE`).

## Evidence

The rule each handler claims to share — services/service-com-amazonaws-billing-awsbilling/src/ops/list_tags_for_resource.rs:7-11:
```
//! Taggability is a property of the resource, not of the direction of the call, so
//! this read draws the same two lines `TagResource` does: a managed billing view is
//! refused with the modeled `ValidationException` (400) and an ARN naming no stored
//! resource with `ResourceNotFoundException` (404). A taggable resource with no tags
//! answers an empty list.
```
and services/service-com-amazonaws-billing-awsbilling/src/ops/untag_resource.rs:12-16:
```
//! Taggability is a property of the resource, so this write draws the same two
//! lines `TagResource` and `ListTagsForResource` do: a managed billing view is
//! refused with the modeled `ValidationException` (400) and an ARN naming no
//! stored resource with `ResourceNotFoundException` (404). Both refusals remove
//! nothing.
```

Copy 1 — services/service-com-amazonaws-billing-awsbilling/src/ops/tag_resource.rs:199-222:
```
        if state
            .billing_views
            .get(&input.resource_arn)
            .is_some_and(crate::state::BillingView::is_managed)
        {
            return Err(invalid_field(
                "resourceArn",
                crate::state::MANAGED_VIEW_NOT_TAGGABLE,
            ));
        }

        let Some(view) = state.billing_views.get_mut(&input.resource_arn) else {
            return Err(TagResourceErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(),
                    resource_id: input.resource_arn.clone(),
                    resource_type: crate::state::BILLING_VIEW_RESOURCE_TYPE.to_string(),
                },
            )
            .into());
        };
```
(its `invalid_field` at tag_resource.rs:132-142 builds `ValidationException { message: why.to_string(), reason: VALIDATION_REASON_FIELD, field_list: [{name: member, message: why}] }`.)

Copy 2 — services/service-com-amazonaws-billing-awsbilling/src/ops/untag_resource.rs:201-224:
```
        if state
            .billing_views
            .get(&input.resource_arn)
            .is_some_and(crate::state::BillingView::is_managed)
        {
            return Err(invalid_field(
                "resourceArn",
                crate::state::MANAGED_VIEW_NOT_TAGGABLE,
            ));
        }

        let Some(view) = state.billing_views.get_mut(&input.resource_arn) else {
            return Err(UntagResourceErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(),
                    resource_id: input.resource_arn.clone(),
                    resource_type: crate::state::BILLING_VIEW_RESOURCE_TYPE.to_string(),
                },
            )
            .into());
        };
```
(its `invalid_field` at untag_resource.rs:134-144 is the same function body as TagResource's.)

Copy 3 — services/service-com-amazonaws-billing-awsbilling/src/ops/list_tags_for_resource.rs:181-208:
```
        if state
            .billing_views
            .get(&input.resource_arn)
            .is_some_and(crate::state::BillingView::is_managed)
        {
            return Err(ListTagsForResourceErrorModeled::ValidationException(
                ValidationException {
                    message: crate::state::MANAGED_VIEW_NOT_TAGGABLE.to_string(),
                    reason: crate::state::VALIDATION_REASON_FIELD.to_string(),
                    field_list: Some(vec![ValidationExceptionField {
                        name: "resourceArn".to_string(),
                        message: crate::state::MANAGED_VIEW_NOT_TAGGABLE.to_string(),
                    }]),
                },
            )
            .into());
        }

        let view = state.billing_views.get(&input.resource_arn).ok_or_else(|| {
            ListTagsForResourceErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
                message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(),
                resource_id: input.resource_arn.clone(),
                resource_type: crate::state::BILLING_VIEW_RESOURCE_TYPE.to_string(),
            })
        })?;
```

The pattern the crate already uses for a shared decision with per-operation exceptions — services/service-com-amazonaws-billing-awsbilling/src/state/billing_month.rs:145-147:
```
/// The DECISION is shared, the exception is not: each handler turns one of
/// these into the modeled error its own operation declares, so one rule reaches
/// the wire in one wording from every read that applies it.
```
and services/service-com-amazonaws-billing-awsbilling/src/state/enterprise_support.rs:682-699, where the payer-account gate of three reads lives in one `enterprise_support_read_refusal` returning an `AccessDeniedException` each handler wraps in its own enum variant.

Test coverage that must stay byte-identical: tests/all/source_views_tags_and_policy.rs:766 `tagging_refuses_a_managed_view_and_an_unknown_resource` (asserts 400 `ValidationException` for the managed view "in either direction of the tag API" — TagResource and ListTagsForResource — at :770-790, and that the refusal attached nothing at :842-858), tests/all/untag_resource.rs:286 `untagging_refuses_a_managed_view_and_an_unknown_resource` (:296-311, :376-388), tests/all/source_views_tags_and_policy.rs:547 `tagging_a_custom_view_round_trips_through_list_tags`, and tests/all/source_views_tags_and_policy.rs:682 `the_tag_store_is_bounded_per_resource`.

## Steps to Reproduce

1. Open services/service-com-amazonaws-billing-awsbilling/src/ops/tag_resource.rs:199-222 and read the managed-view refusal followed by the absent-resource refusal.
2. Open services/service-com-amazonaws-billing-awsbilling/src/ops/untag_resource.rs:201-224 and observe the same two refusals, in the same order, with the same payload fields, differing only in the error enum named.
3. Open services/service-com-amazonaws-billing-awsbilling/src/ops/list_tags_for_resource.rs:181-208 and observe the third copy, which also inlines the `ValidationException` assembly its two siblings keep in a local `invalid_field` helper.
4. Compare with the crate's own pattern for a decision shared by several operations: services/service-com-amazonaws-billing-awsbilling/src/state/enterprise_support.rs:685-699 (`enterprise_support_read_refusal`) and services/service-com-amazonaws-billing-awsbilling/src/state/billing_month.rs:145-147 + 207-218 (`BillingMonthRefusal` / `require_past_billing_month`), where the decision is in `crate::state` and each handler only maps it into its own modeled error.
5. Run the tagging tests (tests/all/source_views_tags_and_policy.rs, tests/all/untag_resource.rs): they pin the current responses, so folding the three copies onto one `crate::state` helper must leave them passing unchanged.
