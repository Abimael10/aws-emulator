Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1af09e54dece88a0c8b706a05433430d41813211

# The DeleteAppInstance cascade drops tags and published names but leaves every AppInstanceAdmin promotion of the instance

_Filed: 2026-11-23T15:08:36Z_
_Operation: DeleteAppInstance | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1af09e54dece88a0c8b706a05433430d41813211`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteAppInstance` is documented in this crate as deleting everything the instance owns, and the handler duly cascades into the two shared directories it resolves: it forgets the instance's and every child's tags, and forgets the published names of all identities of the instance. It never touches the shared app-instance ADMIN directory, so every promotion recorded against the deleted instance survives the delete with no live resource behind it.

Consequences inside this crate: `DescribeAppInstanceAdmin` is the only admin reader that guards on `unresolved_arn`, and that guard only works while the deletion is still remembered — the store's deleted-instance set is bounded and evicts the oldest deletion (`record_app_instance_deletion`), after which the orphaned promotion is readable again, and `AppInstance` ARNs are never re-minted only because they carry a fresh uuid. More importantly the directory is a cross-service authorization record read by Chime SDK Messaging, which has no way to know the instance was deleted. The handler already enumerates the instance's children for the tag cascade and the directory already exposes `admins_of` + `demote`, so nothing new is needed.

## Evidence

The handler states the cascade and performs it for two of the three shared directories — src/ops/delete_app_instance.rs:189-190, 219-233:
```
        // Deleting an AppInstance deletes everything it owns, so the users and bots it
        // holds go with it and a later Describe of one of them resolves to nothing.
...
        // The cascade reaches the tags too: the instance's own and those of every child
        // it took with it.
        let mut tags = tag_directory.lock();
        tags.forget(&input.app_instance_arn);
        for child_arn in removed.users.keys().chain(removed.bots.keys()) {
            tags.forget(child_arn);
        }
...
        let _dropped_names = name_directory
            .lock()
            .forget_identities_of(&input.app_instance_arn);
```
The file never resolves the admin directory (no `crate::state::app_instance_admins` call in delete_app_instance.rs), while the sibling that owns promotions does — src/ops/delete_app_instance_admin.rs:218-220: `directory.lock().demote(&input.app_instance_arn, &input.app_instance_admin_arn);`.

The state module's own cascade doc lists the children and the tokens, not the promotions — src/state.rs:504-508:
```
    /// Remove an app instance and cascade to everything it owns: its users and bots go
    /// with it, so a later `DescribeAppInstanceUser` or `ListAppInstanceBots` on one of
    /// them resolves to nothing. The idempotency tokens that minted the instance and
    /// its children are released too.
```
The surviving record is an authorization grant another service reads — src/state.rs:673-676: "An `AppInstanceAdmin` is promoted here and the role it grants is read by Chime SDK Messaging, which reserves an app instance's private-channel listing and its message deletion for an administrator."

And the only reader that compensates does so only while the deletion is remembered — src/ops/describe_app_instance_admin.rs:173-183 (`if state.lock().unresolved_arn(&input.app_instance_arn) == UnresolvedArn::DeletedAppInstance`), against src/state.rs:536-551 `record_app_instance_deletion`, which drops the oldest remembered deletion once the set is full.

Test coverage: tests/all/token_reuse_and_deleted_refusals.rs pins the deleted-instance refusals for the operations it enumerates, but its table (lines 414-554) contains no admin-directory call, so no test observes the orphaned promotions.

## Steps to Reproduce

1. CreateAppInstance -> AppInstanceArn A; CreateAppInstanceUser under A -> user ARN U.
2. CreateAppInstanceAdmin (POST /app-instances/{A}/admins, AppInstanceAdminArn=U) -> 201.
3. DeleteAppInstance (DELETE /app-instances/{A}) -> 204.
4. Inspect the shared `AppInstanceAdminDirectory`: the promotion (A, U) is still recorded — nothing in the cascade demoted it, so any reader of the directory (Chime SDK Messaging) still treats U as an administrator of A.
5. Delete MAX_DELETED_APP_INSTANCES further app instances so A's deletion is evicted from `deleted_app_instances`; GET /app-instances/{A}/admins/{U} then answers 200 again from the orphaned record.
