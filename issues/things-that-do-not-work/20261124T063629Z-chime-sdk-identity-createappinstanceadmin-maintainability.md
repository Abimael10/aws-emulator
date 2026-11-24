Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 2467eda56f564b6aee6366b263dced9add6c2085

# The unresolved-ARN refusal rule is written out in 18 handlers, including two byte-identical helper pairs, instead of being stated once as the crate already does for tagging

_Filed: 2026-11-24T06:36:29Z_
_Operation: CreateAppInstanceAdmin | Class: maintainability_

> _Note: this issue was identified against the code base at commit `2467eda56f564b6aee6366b263dced9add6c2085`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate has ONE rule for an ARN it holds no live resource for: ask `state.unresolved_arn(..)`, answer the modelled `ForbiddenException` (`Code` `Forbidden`, `FORBIDDEN_REQUEST_MESSAGE`) when the owning `AppInstance` was DELETED, and the modelled `BadRequestException` (`Code` `NotFound`, "<Kind> <arn> does not exist") when the ARN never named anything. That rule is spelled out, branch for branch, in 18 separate handler files — and in `create_app_instance_admin.rs` / `delete_app_instance_admin.rs` it is even extracted into a local `unresolved(..)` + `not_found(..)` pair that is byte-identical between the two files except for the name of the generated error enum.

The crate has already solved exactly this problem once, for the other rule three handlers share: `src/state/tagging_arn.rs` states the tagging-ARN refusal as a CLASS (`TaggingArnRefusal::{BadRequest,Forbidden}` carrying `code` and `message`) so each handler only wraps it in the variant of its own enum, with the module doc giving the reason: "the decision is stated here once and each handler only wraps it in the variant of its own enum. A change to the rule is then made in one place and the three answers cannot drift apart." The unresolved-ARN rule is the same shape of decision across many more call sites and has NOT been factored that way.

The copies have already drifted, which is what makes this more than taste: three handlers that address an `AppInstance` never call `unresolved_arn` at all (`list_app_instance_admins.rs:170-184`, `get_app_instance_retention_settings.rs:158-174`, `put_app_instance_retention_settings.rs:157-173`) and so collapse the two cases onto one answer. Those behavioural divergences are already filed separately (`...listappinstanceadmins-error-response`, `...getappinstanceretentionsettings-error-response`) and are NOT what this issue asks to change: the refactor here must leave every response byte-identical and every existing test untouched — extract the decision into one place (an `UnresolvedRefusal` class beside `TaggingArnRefusal`, carrying the class, the `Code` and the `Message`) and have the 18 sites that already make the distinction wrap it, deleting the two duplicated local helper pairs.

## Evidence

The pattern this crate ALREADY chose for the sibling rule — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/state/tagging_arn.rs:3-11:
```
//! The ONE rule that decides whether a tagging call may address a `ResourceARN`.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` serve the family's three generic
//! tagging routes, and the question they each have to answer first is the same one: may a
//! tagging call address this ARN, and if not, which error class and which sentence does the
//! refusal carry. What those handlers cannot share is the error ENUM — each operation's
//! generated enum is its own type — so the decision is stated here once and each handler
//! only wraps it in the variant of its own enum. A change to the rule is then made in one
//! place and the three answers cannot drift apart.
```

Copy 1 of the unresolved-ARN rule — src/ops/create_app_instance_admin.rs:251-283:
```
/// Refuse the request for an ARN this service holds no resource for.
///
/// An ARN whose `AppInstance` has been DELETED is refused as a request this caller may no
/// longer make: the instance that would have authorized the promotion is gone, and that is
/// a different answer from the one an ARN that never named a resource earns.
fn unresolved(
    unresolved: crate::state::UnresolvedArn,
    absent: String,
) -> CreateAppInstanceAdminError {
    match unresolved {
        crate::state::UnresolvedArn::DeletedAppInstance => {
            CreateAppInstanceAdminErrorModeled::ForbiddenException(ForbiddenException {
                code: Some("Forbidden".to_string()),
                message: Some(crate::state::FORBIDDEN_REQUEST_MESSAGE.to_string()),
            })
            .into()
        }
        crate::state::UnresolvedArn::NeverExisted => not_found(absent),
    }
}
...
fn not_found(message: String) -> CreateAppInstanceAdminError {
    CreateAppInstanceAdminErrorModeled::BadRequestException(BadRequestException {
        code: Some("NotFound".to_string()),
        message: Some(message),
    })
    .into()
}
```

Copy 2, byte-identical but for the enum name — src/ops/delete_app_instance_admin.rs:226-258:
```
/// Refuse the request for an ARN this service holds no resource for.
...
fn unresolved(
    unresolved: crate::state::UnresolvedArn,
    absent: String,
) -> DeleteAppInstanceAdminError {
    match unresolved {
        crate::state::UnresolvedArn::DeletedAppInstance => {
            DeleteAppInstanceAdminErrorModeled::ForbiddenException(ForbiddenException {
                code: Some("Forbidden".to_string()),
                message: Some(crate::state::FORBIDDEN_REQUEST_MESSAGE.to_string()),
            })
            .into()
        }
        crate::state::UnresolvedArn::NeverExisted => not_found(absent),
    }
}
...
fn not_found(message: String) -> DeleteAppInstanceAdminError {
    DeleteAppInstanceAdminErrorModeled::BadRequestException(BadRequestException {
        code: Some("NotFound".to_string()),
        message: Some(message),
    })
    .into()
}
```

The same branch inlined again in a handler — src/ops/describe_app_instance.rs:153-178:
```
        let Some(record) = state.app_instances.get(&input.app_instance_arn) else {
            return Err(match state.unresolved_arn(&input.app_instance_arn) {
                crate::state::UnresolvedArn::DeletedAppInstance => {
                    DescribeAppInstanceErrorModeled::ForbiddenException(ForbiddenException {
                        code: Some("Forbidden".to_string()),
                        message: Some(crate::state::FORBIDDEN_REQUEST_MESSAGE.to_string()),
                    })
                    .into()
                }
                crate::state::UnresolvedArn::NeverExisted => {
                    DescribeAppInstanceErrorModeled::BadRequestException(BadRequestException {
                        code: Some("NotFound".to_string()),
                        message: Some(format!(
                            "AppInstance {} does not exist",
                            input.app_instance_arn
                        )),
                    })
                    .into()
                }
            });
        };
```
and `grep -n 'crate::state::UnresolvedArn::DeletedAppInstance' src/ops/*.rs` lists the same branch in 18 handler files: create_app_instance_admin.rs:261, create_app_instance_bot.rs:278, create_app_instance_user.rs:283, delete_app_instance.rs:196, delete_app_instance_admin.rs:236, delete_app_instance_bot.rs:198, delete_app_instance_user.rs:198, deregister_app_instance_user_endpoint.rs:184, describe_app_instance.rs:158, describe_app_instance_admin.rs:174, describe_app_instance_user.rs:157, describe_app_instance_user_endpoint.rs:166, list_app_instance_bots.rs:167, list_app_instance_user_endpoints.rs:166, list_app_instance_users.rs:166, put_app_instance_user_expiration_settings.rs:175, register_app_instance_user_endpoint.rs:282, update_app_instance.rs:168, update_app_instance_user.rs:181, update_app_instance_user_endpoint.rs:177.

The rule itself, stated once in the state module (src/state.rs:16-19) that the copies each re-implement:
```
//! * `deleted_app_instances` — the ARN of every app instance this store has deleted, so
//!   a request addressed to a dead instance (or to a resource the delete cascaded away
//!   with it) is refused as one this caller may no longer make rather than as an ARN
//!   that never named anything.
```

Test coverage that pins the behaviour the refactor must preserve: tests/all/token_reuse_and_deleted_refusals.rs:557-637 — `a_request_addressed_to_a_deleted_app_instance_is_forbidden`, whose table walks 18 addressed calls and asserts `(403, Some("Forbidden"), Some(FORBIDDEN_MESSAGE))` for a deleted instance and `(400, Some("NotFound"))` for ARNs that never existed; tests/all/app_instance_admin_demotion.rs:313-341 pins the same pair of answers for the two admin handlers that hold the duplicated helpers; tests/all/app_instance_lifecycle.rs:1028-1040, tests/all/app_instance_bots.rs:1008 and tests/all/user_endpoint_registration.rs:1348-1396 pin the remaining per-operation rows.

Precedent in this same crate: the backlog entry "The tagging-ARN refusal rule is implemented three times verbatim across TagResource, UntagResource and ListTagsForResource" (id 20261119T143922Z-chime-sdk-identity-tagresource-maintainability) is marked FIXED, and `src/state/tagging_arn.rs` is that fix — so the shape of the fix asked for here is the one the crate already accepted.

## Steps to Reproduce

1. Open services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/create_app_instance_admin.rs:251-283 and src/ops/delete_app_instance_admin.rs:226-258 and diff them: the `unresolved(..)` and `not_found(..)` helpers are identical except for the generated enum name.
2. Run `grep -n 'crate::state::UnresolvedArn::DeletedAppInstance' services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/*.rs` — the same two-arm refusal is written out in 18 handler files.
3. Compare that with src/state/tagging_arn.rs, where the crate states the analogous tagging rule ONCE as a class each handler wraps in its own enum, for the reason given in its module doc (lines 3-11).
4. Observe the drift the duplication already allowed: list_app_instance_admins.rs:170-184, get_app_instance_retention_settings.rs:158-174 and put_app_instance_retention_settings.rs:157-173 never call `unresolved_arn`, so they answer 400 NotFound where the 18 copies answer 403 Forbidden (behaviour tracked by separate open issues; unchanged by this refactor).
5. Extract the decision once (an `UnresolvedRefusal` class beside `TaggingArnRefusal`), have each of the 18 sites wrap it, delete the two duplicated local helper pairs, and confirm `cargo test -p service-com-amazonaws-chimesdkidentity-chimeidentityservice` passes with no test or response text changed.
