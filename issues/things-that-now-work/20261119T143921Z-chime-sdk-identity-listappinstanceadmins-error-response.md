Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: a0a325cd7a8f100e0b15b3c52772a25e70aa00e8

# ListAppInstanceAdmins answers 400 NotFound for a DELETED AppInstance where every sibling answers 403 Forbidden

_Filed: 2026-11-19T14:39:21Z_
_Operation: ListAppInstanceAdmins | Class: error-response_

> _Note: this issue was identified against the code base at commit `a0a325cd7a8f100e0b15b3c52772a25e70aa00e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other handler in this crate that is addressed to an `AppInstanceArn` asks `state.unresolved_arn(..)` why the ARN resolves to nothing and splits the answer in two: an instance that has been DELETED earns the modelled `ForbiddenException` carrying `FORBIDDEN_REQUEST_MESSAGE`, and an ARN that never named an instance earns `BadRequestException` with the `NotFound` code. `ListAppInstanceAdmins` skips that existing helper entirely and returns the `NotFound` refusal for BOTH cases, so a listing of the administrators of a deleted app instance answers 400 `NotFound` while `DeleteAppInstanceAdmin`, `CreateAppInstanceAdmin`, `DescribeAppInstanceAdmin`, `ListAppInstanceUsers` and `ListAppInstanceBots` — all scoped by the same `AppInstanceArn` — answer 403 `Forbidden` for the very same instance.

## Evidence

The skipped branch, in the handler under review:

services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/list_app_instance_admins.rs:170-184
```
        if !store.app_instances.contains_key(&input.app_instance_arn) {
            // This operation models no 404 shape, so an unknown ARN is refused with the
            // modelled `BadRequestException` carrying the `NotFound` error code, as the
            // rest of this service does.
            return Err(ListAppInstanceAdminsErrorModeled::BadRequestException(
                BadRequestException {
                    code: Some("NotFound".to_string()),
```
(no `unresolved_arn` call anywhere in the file)

The sibling that administers the SAME sub-resource does make the distinction:

services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/delete_app_instance_admin.rs:203-211
```
            let store = state.lock();
            if !store.app_instances.contains_key(&input.app_instance_arn) {
                return Err(unresolved(
                    store.unresolved_arn(&input.app_instance_arn),
                    format!("AppInstance {} does not exist", input.app_instance_arn),
                ));
            }
```
with delete_app_instance_admin.rs:236-241 mapping `UnresolvedArn::DeletedAppInstance` to `ForbiddenException`.

And the sibling listings do too — services/.../src/ops/list_app_instance_bots.rs:162-172:
```
        let Some(instance) = state.app_instances.get(&input.app_instance_arn) else {
            return Err(match state.unresolved_arn(&input.app_instance_arn) {
                // The AppInstance this ARN names has been deleted, so it can no longer
                // serve a listing of the bots it held: the answer is the modelled
                // permanent refusal.
                crate::state::UnresolvedArn::DeletedAppInstance => {
                    ListAppInstanceBotsErrorModeled::ForbiddenException(ForbiddenException {
```

The crate's own test states the rule as universal — tests/all/app_instance_admin_demotion.rs:313-331:
```
    // An ARN whose app instance has been DELETED is a different absence from one that
    // never named an app instance: the instance that scoped the promotion is gone, so the
    // request is one this caller may no longer make, as every sibling operation answers.
    ...
    assert_eq!(
        refused, 403,
        "demoting under a deleted app instance must answer 403, got {refused} {refused_body:?}"
```

No test pins the deleted case for this listing: tests/all/app_instance_admin_listing.rs:439-449 exercises only `let absent = format!("{instance}x");` ("An AppInstance this service does not hold"), i.e. the NeverExisted row. The deleted-refusal table in tests/all/token_reuse_and_deleted_refusals.rs:414 (`calls_addressed_to`) claims to hold "Every operation of this service that is ADDRESSED to an app instance" and states exclusions only for `DescribeAppInstanceBot` and `DescribeAppInstanceAdmin` — `ListAppInstanceAdmins` is simply absent with no stated reason.

## Steps to Reproduce

1. CreateAppInstance -> AppInstanceArn A; CreateAppInstanceUser under A -> user U; CreateAppInstanceAdmin(A, U) -> 201
2. DELETE /app-instances/{A} -> 204
3. GET /app-instances/{A}/admins (ListAppInstanceAdmins) -> observe 400 with Code "NotFound"
4. DELETE /app-instances/{A}/admins/{U} (DeleteAppInstanceAdmin) -> observe 403 with Code "Forbidden" and the service's forbidden sentence
5. GET /app-instance-bots?app-instance-arn={A} (ListAppInstanceBots) -> observe 403 "Forbidden": the two listings disagree about the same deleted instance
