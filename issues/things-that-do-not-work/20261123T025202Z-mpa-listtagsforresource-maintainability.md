Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: e206ea8354e119eed3b59498ff2b9b9926595f25

# The ordered `ResourceArn` screen (malformed-ARN 400, then account boundary 403) is written out five times, although the crate states the identical ordered screen once for `IdentitySourceArn`

_Filed: 2026-11-23T02:52:02Z_
_Operation: ListTagsForResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `e206ea8354e119eed3b59498ff2b9b9926595f25`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Five handlers of this crate open with byte-for-byte the same two-step refusal LOGIC for the `ResourceArn` member the model gives no pattern: first `crate::state::is_well_formed_arn` -> `ValidationException(crate::state::invalid_arn())` (400), then `crate::state::authorize_resource_arn(Self::NAME, &input.resource_arn)` -> `AccessDeniedException` (403), each lifted into that operation's own error enum. The ORDER of those two decisions is the rule (the malformed-ARN class is decided before the account boundary, and both before any lookup), and it is restated five times instead of once.

The crate already has the precedent for stating exactly this kind of ordered screen ONCE: the sibling member that addresses an identity source is screened by `crate::state::screen_identity_source_arn`, which fixes the order in one function and hands the caller an `IdentitySourceArnRefusal` enum whose variants carry the modelled error of each class, so `get_identity_source.rs` and `delete_identity_source.rs` only map the refusal onto their own error channel. The `ResourceArn` family has no such function, so the five copies can drift in which check comes first, or in which message/error a class gets — and one of the five (`UntagResource`) is not exercised by the cross-reader table that pins the other four, so a drift there would not be caught.

The fix is a single `state`-level screen for this member (shaped like `screen_identity_source_arn`, returning a refusal value per class) called from the five handlers. Every status, error code and message stays exactly as it is, and no existing test changes.

## Evidence

Copy 1 — services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/list_tags_for_resource.rs:165-176:
```
        if !crate::state::is_well_formed_arn(&input.resource_arn) {
            return Err(ListTagsForResourceErrorModeled::ValidationException(
                crate::state::invalid_arn(),
            )
            .into());
        }
...
        crate::state::authorize_resource_arn(Self::NAME, &input.resource_arn)
            .map_err(ListTagsForResourceErrorModeled::AccessDeniedException)?;
```

Copy 2 — src/ops/tag_resource.rs:180-190:
```
        if !crate::state::is_well_formed_arn(&input.resource_arn) {
            return Err(
                TagResourceErrorModeled::ValidationException(crate::state::invalid_arn()).into(),
            );
        }
...
        crate::state::authorize_resource_arn(Self::NAME, &input.resource_arn)
            .map_err(TagResourceErrorModeled::AccessDeniedException)?;
```

Copy 3 — src/ops/untag_resource.rs:165-175:
```
        if !crate::state::is_well_formed_arn(&input.resource_arn) {
            return Err(
                UntagResourceErrorModeled::ValidationException(crate::state::invalid_arn()).into(),
            );
        }
...
        crate::state::authorize_resource_arn(Self::NAME, &input.resource_arn)
            .map_err(UntagResourceErrorModeled::AccessDeniedException)?;
```

Copy 4 — src/ops/get_resource_policy.rs:192-203:
```
        if !crate::state::is_well_formed_arn(&input.resource_arn) {
            return Err(GetResourcePolicyErrorModeled::ValidationException(
                crate::state::invalid_arn(),
            )
            .into());
        }
...
        crate::state::authorize_resource_arn(Self::NAME, &input.resource_arn)
            .map_err(GetResourcePolicyErrorModeled::AccessDeniedException)?;
```

Copy 5 — src/ops/list_resource_policies.rs:195-202:
```
        if !crate::state::is_well_formed_arn(&input.resource_arn) {
            return Err(ListResourcePoliciesErrorModeled::ValidationException(
                crate::state::invalid_arn(),
            )
            .into());
        }
        crate::state::authorize_resource_arn(Self::NAME, &input.resource_arn)
            .map_err(ListResourcePoliciesErrorModeled::AccessDeniedException)?;
```

The in-crate precedent that states the same ordered screen ONCE — src/state.rs:2524-2537:
```
pub fn screen_identity_source_arn(action: &str, arn: &str) -> Result<(), IdentitySourceArnRefusal> {
    if !is_identity_source_arn(arn) {
        return Err(IdentitySourceArnRefusal::Invalid(
            invalid_identity_source_arn(),
        ));
    }
    authorize_resource_arn(action, arn).map_err(IdentitySourceArnRefusal::Denied)?;
```
and its doc at src/state.rs:2500-2503: "Whether an operation addressed by an identity source's ARN may act on `arn`, decided in the ONE order both such operations apply." Its refusal value is src/state.rs:2490-2498 (`IdentitySourceArnRefusal::Invalid(ValidationException)` / `::Denied(AccessDeniedException)`), and src/ops/get_identity_source.rs:222-231 and src/ops/delete_identity_source.rs:239-248 only map it onto their own error channel — which is exactly what the five handlers above would do.

Test coverage that must stay unchanged: tests/all/resource_arn_reads.rs:256 `a_resource_arn_that_is_not_an_arn_is_refused_by_every_reader` (asserts 400 + `state::INVALID_ARN_MESSAGE` for ListTagsForResource, TagResource, ListResourcePolicies and GetResourcePolicy — note `UntagResource`, copy 3, is absent from that table) and tests/all/resource_arn_reads.rs:321 `a_well_formed_resource_arn_earns_the_account_or_existence_answer` (asserts the 403/404 split), plus tests/all/resource_scoped_listing_refusal_order.rs:216, tests/all/resource_tags.rs and tests/all/tag_write_rule_order.rs.

## Steps to Reproduce

1. Open src/ops/list_tags_for_resource.rs:165-176, src/ops/tag_resource.rs:180-190, src/ops/untag_resource.rs:165-175, src/ops/get_resource_policy.rs:192-203 and src/ops/list_resource_policies.rs:195-202 and compare the opening of each `handle`: the same two decisions, in the same order, over the same member, five times.
2. Open src/state.rs:2490-2537 and see the crate already states this kind of ordered screen once — `screen_identity_source_arn` plus the `IdentitySourceArnRefusal` enum — for the `IdentitySourceArn` member, with both reading handlers (src/ops/get_identity_source.rs:222, src/ops/delete_identity_source.rs:239) doing nothing but mapping its refusal onto their own error channel.
3. Note that tests/all/resource_arn_reads.rs:266-279 drives only four of the five operations (ListTagsForResource, TagResource, ListResourcePolicies, GetResourcePolicy), so copy 3 in untag_resource.rs is not pinned by the cross-reader table and could drift unnoticed.
4. Add one `state`-level screen for `ResourceArn` shaped like `screen_identity_source_arn`, call it from all five handlers, and re-run `tests/all/resource_arn_reads.rs`, `resource_scoped_listing_refusal_order.rs`, `resource_tags.rs` and `tag_write_rule_order.rs`: every status, error code and message must be identical.
