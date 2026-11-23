Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 35d7082131f9491ad89c2e4b5f0079c864ac9dfa

# TagResource decides "may this ARN be tagged?" under the store lock, then releases it before writing the shared tag directory, so a concurrent delete lets tags be written onto an ARN that no longer exists

_Filed: 2026-11-23T07:41:54Z_
_Operation: TagResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `35d7082131f9491ad89c2e4b5f0079c864ac9dfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`TagResource` resolves the ARN-admissibility question (`taggable` + `unresolved_arn`) inside a SCOPED block that drops the store guard, and only afterwards takes the shared Chime tag directory lock to write the tags. The check and the write are two non-overlapping critical sections, so a `DeleteAppInstanceUser` / `DeleteAppInstanceBot` / `DeleteAppInstance` that runs in the window between them turns this handler's own refusal into a success: the handler has already decided the resource exists, and then writes tags into the shared, ARN-keyed directory for a resource that is gone — AFTER that delete's own cascade (`tag_directory.lock().forget(..)`) has already dropped the resource's entry.

Every other writer in this crate holds the store guard ACROSS the shared-directory write: `CreateAppInstance`, `CreateAppInstanceUser` and `CreateAppInstanceBot` keep `let mut state = state.lock();` alive while they call `tag_directory.lock().tag(..)` / `name_directory.lock().register(..)`, and the crate states outright that store-then-directory is "the order every writer in this crate takes them in". The documented lock order PERMITS holding both, so nothing forces this handler to drop the guard.

Two observable consequences. (1) The tag entry is unreachable but permanent: `ListTagsForResource` now refuses the ARN (`taggable` is false), so no caller can see or remove the tags, and `UntagResource` refuses them too — the directory keeps them for the life of the process. (2) Worse, a user ARN is DETERMINISTIC (`format!("{app_instance_arn}/user/{app_instance_user_id}")`), so re-creating the user with the same `AppInstanceUserId` re-mints the identical ARN and `ListTagsForResource` then reports tags that were never applied to the new user. `UntagResource` has the identical released-lock shape and can likewise remove tags from a resource the store no longer holds.

The fix is local and changes no response and no existing test: keep the store guard alive across the `directory.lock().tag(..)` write, which is the order the state module already documents.

## Evidence

The check runs under a guard that is dropped before the write — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/tag_resource.rs:185-191 (the `store` guard dies at line 191):
```
        // `None` when the ARN names a resource a tagging call may address; otherwise WHY
        // it does not, which is what decides the refusal.
        let unresolved = {
            let store = state.lock();
            (!store.taggable(&input.resource_arn))
                .then(|| store.unresolved_arn(&input.resource_arn))
        };
```
and the write happens with no store guard held — tag_resource.rs:243-248:
```
        let pairs = input
            .tags
            .into_iter()
            .map(|tag| (tag.key, tag.value))
            .collect::<Vec<_>>();
        let outcome = directory.lock().tag(&input.resource_arn, pairs);
```

The sibling creates of the SAME crate hold the store guard across their shared-directory writes — src/ops/create_app_instance_user.rs:187 and 341-344:
```
        let mut state = state.lock();
...
        name_directory.lock().register(&arn, &input.name);
        if !tags.is_empty() {
            tag_directory.lock().tag(&arn, tags);
        }
```
and src/ops/create_app_instance.rs:173 and 260-263:
```
        let mut state = state.lock();
...
        state.app_instances.insert(arn.clone(), record);
        if !tags.is_empty() {
            tag_directory.lock().tag(&arn, tags);
        }
```

The crate states that holding BOTH in this order is the convention, not a new rule — src/ops/list_app_instance_admins.rs:167-169 (the `store` guard is still alive when `directory.lock()` is taken at line 194):
```
        // LOCK ORDER: the store first, the shared admin directory second — the order every
        // writer in this crate takes them in.
        let store = state.lock();
```
and src/ops/update_app_instance_user.rs:160-163:
```
        // A rename is visible to the sibling that REPORTS the name, so the family's shared
        // identity NAME directory is kept in step with the record below; resolved before the
        // store is locked, so the lock order (this service's store first, the shared
        // directory second) cannot invert.
```
and src/ops/register_app_instance_user_endpoint.rs:179-182 for the analogous check-then-write:
```
        // The whole register runs under ONE lock — the token lookup, the state the
        // `ResourceArn` implies, and the insert — so two registers carrying the same
        // token cannot each find no endpoint and then each mint one.
        let mut state = state.lock();
```

The losing delete has already dropped the entry this handler then re-writes — src/ops/delete_app_instance_user.rs:221-222:
```
        tag_directory.lock().forget(&input.app_instance_user_arn);
        name_directory.lock().forget(&input.app_instance_user_arn);
```
and src/ops/delete_app_instance.rs:221-226 for the cascade.

The re-minted ARN that makes the stale entry observable — src/state.rs:867-869:
```
pub fn app_instance_user_arn(app_instance_arn: &str, app_instance_user_id: &str) -> String {
    format!("{app_instance_arn}/user/{app_instance_user_id}")
}
```

The stale entry cannot be read or removed afterwards, because both remaining tagging paths refuse the ARN first — src/ops/list_tags_for_resource.rs:159-187 and src/ops/untag_resource.rs:180-208 run the same `taggable`/`unresolved_arn` screen before touching the directory.

Test coverage: the tagging suite exercises only SEQUENTIAL calls — tests/all/resource_tagging.rs `tags_round_trip_through_the_shared_tagging_routes` (line 104), `tags_supplied_at_create_time_are_listed_and_can_be_changed` (line 231) and `a_tagging_call_this_service_cannot_serve_is_refused_and_writes_nothing` (line 333) — so no test interleaves a tag write with a delete; concurrency IS a tested dimension elsewhere in this crate (tests/all/user_endpoint_registration.rs `one_client_request_token_registers_one_endpoint`, line 484, drives concurrent registers against one engine), which is why the window is reachable in this harness.

## Steps to Reproduce

1. Start the emulator on one shared engine and drive two concurrent requests.
2. CreateAppInstance -> AppInstanceArn A; CreateAppInstanceUser under A with AppInstanceUserId = "alice" -> U = <A>/user/alice.
3. Request X: POST /tags?operation=tag-resource with {"ResourceARN": U, "Tags": [{"Key":"k","Value":"v"}]}. Let it pass the end of the scoped store-lock block (src/ops/tag_resource.rs:191) — the check has seen U taggable — but not yet reach directory.lock().tag(..) at line 248.
4. Request Y: DELETE /app-instance-users/{U} runs to completion (its cascade forgets U's tags at delete_app_instance_user.rs:221).
5. Let X resume: it answers 204 and writes k=v into the shared tag directory for an ARN the store no longer holds, instead of the refusal its own check exists to produce.
6. GET /tags?arn={U} (ListTagsForResource) -> 400 BadRequest/NotFound, and POST /tags?operation=untag-resource -> 400 as well, so the entry can neither be read nor removed.
7. CreateAppInstanceUser under A again with AppInstanceUserId = "alice" -> the same ARN U is re-minted; GET /tags?arn={U} now reports Tags=[{k,v}], tags no caller ever applied to this user.
