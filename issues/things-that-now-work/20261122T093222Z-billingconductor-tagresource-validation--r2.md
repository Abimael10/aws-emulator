Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: c610ade0f4302ea06a3f7bb8be16d080321de51c

# The tagging ARN reader makes the resource-type separator optional for ALL four kinds, although its own comment and the modelled pattern make it optional only for `billinggroup`

_Filed: 2026-11-22T09:32:22Z_
_Operation: TagResource | Class: validation_

> _Note: this issue was identified against the code base at commit `c610ade0f4302ea06a3f7bb8be16d080321de51c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The three tagging operations resolve their `ResourceArn` through `crate::state::parse_taggable_arn` (the `@pattern` is deliberately stripped from the schema via `skip_validation = ["ResourceArn"]`, so this helper is the only thing deciding what names a resource). The helper's own documentation and the comment directly above the code state that the `/` between the resource-type segment and the id is optional for `billinggroup` ONLY — which is exactly what the modelled `Arn` pattern says. The code under that comment, however, strips an optional `/` for every kind: `rest.strip_prefix('/').unwrap_or(rest)` runs after `kind` has been resolved to any of the four kinds.

Consequence: `arn:aws:billingconductor::123456789012:pricingplanABCDEFGHIJ` — a value the modelled `Arn` pattern refuses, because the `pricingplan` alternative spells the separator as mandatory — is resolved to `arn:aws:billingconductor::123456789012:pricingplan/ABCDEFGHIJ` and the tagging surface WRITES to that stored pricing plan (HTTP 204). The same holds for `pricingruleXXXXXXXXXX` and for the full-ARN `customlineitemXXXXXXXXXX` form. So this service accepts, and mutates state under, a resource identifier its own ARN grammar never defines, while the crate's comment claims the lenient separator belongs to the billing-group alternative alone.

This is a different gap from the already-fixed reserved-`aws:`-tag-key validation issue on the same operation: that one is about the `Tags` map, this one is about `ResourceArn` resolution.

## Evidence

Smithy model — the separator is optional only after `billinggroup` (`smithy_query(query="shape",shape="Arn")`):

  "pattern": "^arn:aws(-cn)?:billingconductor::[0-9]{12}:billinggroup/?[0-9]{12}$|^arn:aws(-cn)?:billingconductor::[0-9]{12}:pricingplan/[a-zA-Z0-9]{10}$|^arn:aws(-cn)?:billingconductor::[0-9]{12}:pricingrule/[a-zA-Z0-9]{10}$|^(arn:aws(-cn)?:billingconductor::[0-9]{12}:customlineitem/)?[a-zA-Z0-9]{10}$"

The crate states the same rule — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state/tagging.rs:283-285:

  /// The modelled `Arn` accepts the full ARN of any of the four taggable kinds — with the
  /// `billinggroup` separator OPTIONAL — or a bare custom-line-item id, so one value has
  /// several spellings and they must all resolve to the one key the maps use.

and repeats it immediately above the code — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state/tagging.rs:319-325:

      let kind = TaggableResourceKind::ALL
          .into_iter()
          .find(|kind| resource.starts_with(kind.segment()))?;
      // `billinggroup/?` — the separator the pattern makes optional, so both spellings of
      // one resource resolve to the same id.
      let rest = &resource[kind.segment().len()..];
      let id = rest.strip_prefix('/').unwrap_or(rest);

`kind` here is any of `TaggableResourceKind::ALL` (tagging.rs:189-194: `BillingGroup, PricingPlan, PricingRule, CustomLineItem`), so the `billinggroup/?`-only rule is applied to all four, and the resolved key is rebuilt WITH the separator at tagging.rs:332-335:

      arn: format!(
          "arn:{partition}:{ARN_SERVICE}::{account}:{}/{id}",
          kind.segment()
      ),

The pattern cannot stop it, because all three tagging inputs strip it — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/types/tag_resource_types.rs:22-25:

  #[smithy_macros::smithy(
      shape = "com.amazonaws.billingconductor#TagResourceRequest",
      skip_validation = ["ResourceArn"]
  )]

and the handler resolves the ARN itself — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/tag_resource.rs:209-223:

      let Some(addressed) = crate::state::parse_taggable_arn(&input.resource_arn) else {
          return Err(crate::state::tagging_not_found());
      };
      ...
      let Some(stored) = state.resource_tags_mut(&addressed.arn) else {

Test coverage: `tests/all/tagging_surface.rs`, `tests/all/aws_managed_pricing_plan_tagging.rs` and `tests/all/untag_created_tags.rs` exercise the tagging surface; none of them sends a separator-less `pricingplan`/`pricingrule`/`customlineitem` ARN, so nothing pins the current leniency.

## Steps to Reproduce

1. Designate the emulator account as the payer so the control plane answers, then CreatePricingPlan with Name="plan1" and note the returned Arn, e.g. arn:aws:billingconductor::123456789012:pricingplan/ABCDEFGHIJ.
2. POST /tags/arn%3Aaws%3Abillingconductor%3A%3A123456789012%3ApricingplanABCDEFGHIJ (the same ARN with the `/` after `pricingplan` removed — a value the modelled `Arn` pattern refuses) with body {"Tags":{"k":"v"}}.
3. Observe HTTP 204 instead of a refusal: the separator-less spelling resolved to the stored plan.
4. GET /tags/arn%3Aaws%3Abillingconductor%3A%3A123456789012%3Apricingplan%2FABCDEFGHIJ and observe {"Tags":{"k":"v"}} — the write landed on the real pricing plan under an identifier spelling the service's own ARN grammar (and the comment at state/tagging.rs:283-285) does not define.
