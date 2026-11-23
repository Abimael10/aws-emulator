Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 7de16721fc47223af60b1bba7682783f47cdc260

# CreateAppInstanceAdmin releases the store lock before writing the shared admin directory, so a concurrent delete turns its own refusal into a recorded promotion

_Filed: 2026-11-23T05:29:53Z_
_Operation: CreateAppInstanceAdmin | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `7de16721fc47223af60b1bba7682783f47cdc260`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateAppInstanceAdmin` does its existence/shape checks in a SCOPED block that drops the store guard (`src/ops/create_app_instance_admin.rs:187-230`), and only afterwards takes the shared app-instance admin directory lock to write the promotion (lines 235-239). The check and the write are therefore two non-overlapping critical sections.

Every other writer in this crate holds the store guard ACROSS the shared-directory write — `CreateAppInstanceUser` and `CreateAppInstanceBot` keep `let mut state = state.lock();` alive while they call `name_directory.lock().register(..)` / `tag_directory.lock().tag(..)`, `RegisterAppInstanceUserEndpoint` states outright that "The whole register runs under ONE lock", and `ListAppInstanceAdmins` says the store-then-directory order is "the order every writer in this crate takes them in". The documented lock order (store first, directory second) PERMITS holding both, so nothing forces this handler to drop the guard.

Consequence: a `DeleteAppInstanceUser` (or `DeleteAppInstance`) that runs in the window between the two sections turns this handler's own refusal into a success — the handler has already decided the user exists, so it records a promotion for an identity, or an app instance, that no longer exists. That is exactly the stale state the crate's readers expose (`DescribeAppInstanceAdmin`/`ListAppInstanceAdmins` project whatever the directory holds, and Chime SDK Messaging reads `administers` directly). Worse, if the losing delete was `DeleteAppInstance`, the orphan can never be cleaned up: `DeleteAppInstanceAdmin` refuses any promotion whose app instance has been deleted with 403 Forbidden before it ever reaches `demote`, so the record survives for the life of the process.

The fix is local and changes no response and no existing test: keep the store guard alive across `directory.lock().promote(..)`, which is the order the state module already documents and which `ListAppInstanceAdmins` already takes.

## Evidence

The check runs under a guard that is dropped before the write — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/create_app_instance_admin.rs:187-239:
```
        let name = {
            let store = state.lock();
            if !store.app_instances.contains_key(&input.app_instance_arn) {
                return Err(unresolved(
                    store.unresolved_arn(&input.app_instance_arn),
                    format!("AppInstance {} does not exist", input.app_instance_arn),
                ));
            }
...
                None => store.identity_name(&input.app_instance_admin_arn),
            }
        };

        // Recording the promotion is idempotent: an identity already promoted keeps the
        // first record, so repeating the request neither duplicates it nor moves the
        // instant `DescribeAppInstanceAdmin` reports.
        directory.lock().promote(
            &input.app_instance_arn,
            &input.app_instance_admin_arn,
            crate::state::now_millis(),
        );
```
(the `store` guard dies at line 230; the directory is written at 235.)

The sibling creates of the SAME crate hold the store guard across their shared-directory writes — src/ops/create_app_instance_user.rs:187 and 341-344:
```
        let mut state = state.lock();
...
        name_directory.lock().register(&arn, &input.name);
        if !tags.is_empty() {
            tag_directory.lock().tag(&arn, tags);
        }
```

The crate states the one-lock rule for the analogous check-then-mint — src/ops/register_app_instance_user_endpoint.rs:179-182:
```
        // The whole register runs under ONE lock — the token lookup, the state the
        // `ResourceArn` implies, and the insert — so two registers carrying the same
        // token cannot each find no endpoint and then each mint one.
        let mut state = state.lock();
```

And that holding BOTH in this order is the convention, not a new rule — src/ops/list_app_instance_admins.rs:167-169 (the `store` guard is still alive when `directory.lock()` is taken at line 194):
```
        // LOCK ORDER: the store first, the shared admin directory second — the order every
        // writer in this crate takes them in.
        let store = state.lock();
```
and src/state.rs:710-711 for the same discipline on the shared tag directory: "LOCK ORDER: a writer takes [`service_state`] first and this directory second, and no reader holds this one while taking another store, so the order cannot invert."

Why the orphan is unreachable when the racing delete was `DeleteAppInstance` — src/ops/delete_app_instance_admin.rs:203-211 refuses before `demote` is ever called:
```
        {
            let store = state.lock();
            if !store.app_instances.contains_key(&input.app_instance_arn) {
                return Err(unresolved(
                    store.unresolved_arn(&input.app_instance_arn),
                    format!("AppInstance {} does not exist", input.app_instance_arn),
                ));
            }
        }
```
with `unresolved(..)` mapping `UnresolvedArn::DeletedAppInstance` to `ForbiddenException` (delete_app_instance_admin.rs:231-245).

Test coverage: the admin suites exercise only SEQUENTIAL calls — tests/all/app_instance_admin_demotion.rs:119-293 (promote/demote tables) and tests/all/app_instance_admin.rs:67-112 — so no test interleaves a promotion with a delete; concurrency IS a tested dimension elsewhere in the crate (tests/all/user_endpoint_registration.rs:500-529 drives eight concurrent registers against one engine), which is why the window is reachable in this harness.

## Steps to Reproduce

1. Start the emulator on one shared engine and drive two concurrent requests.
2. CreateAppInstance -> AppInstanceArn A; CreateAppInstanceUser under A -> U.
3. Request X: POST /app-instances/{A}/admins with {"AppInstanceAdminArn": U}. Let it pass the end of the scoped store-lock block (src/ops/create_app_instance_admin.rs:230) — the check has seen U alive — but not yet reach directory.lock().promote(..) at line 235.
4. Request Y: DELETE /app-instances/{A} (or DELETE /app-instance-users/{U}) runs to completion.
5. Let X resume: it records a promotion in the shared app-instance admin directory for an identity (and, in the DeleteAppInstance case, an app instance) that no longer exists, and answers 201 instead of the refusal its own check exists to produce.
6. DELETE /app-instances/{A}/admins/{U} to clean up -> 403 Forbidden (delete_app_instance_admin.rs:203-211 refuses a deleted app instance before demote), so the orphan promotion stays in the shared directory, where Chime SDK Messaging's `administers(A, U)` reader still sees it.
