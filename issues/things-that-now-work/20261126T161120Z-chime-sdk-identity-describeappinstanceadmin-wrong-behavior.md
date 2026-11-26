Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: ca98a3fe46012add62adc6847612b0abdc870e70

# DescribeAppInstanceAdmin releases the store lock between its deleted-instance refusal and the promotion read, so a concurrent DeleteAppInstance turns its own 403 into a 200

_Filed: 2026-11-26T16:11:20Z_
_Operation: DescribeAppInstanceAdmin | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `ca98a3fe46012add62adc6847612b0abdc870e70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler decides THREE things from THREE separate acquisitions of this service's store/directory locks instead of one snapshot: (1) it locks the store, asks `unresolved_arn` whether the `AppInstance` has been deleted, and releases the lock; (2) it then locks the shared admin directory and reads the promotion; (3) it then locks the store a third time to read the administering identity's name.

Because `DeleteAppInstance` needs the same store lock to remove the instance, a delete that lands between (1) and (2) makes this read answer `200` with the promotion of an app instance that no longer exists — precisely the answer the handler's own comment says it must not give ("once that instance has been deleted the read is refused ... rather than answered from a record the delete left behind"). The surviving record is reachable because `DeleteAppInstance`'s cascade does not drop promotions, so step (2) still finds the grant; the only thing standing between the caller and a stale `200` is the check in step (1), which is evaluated against a store state that is no longer held.

The sibling reader of the same two stores does it the other way: `ListAppInstanceAdmins` holds the store guard across the directory read and the whole projection, under an explicit `LOCK ORDER` comment, so its deleted-instance decision and the records it reports come from one consistent snapshot. `src/state.rs` states that lock order for the directory as the crate's rule. Holding the store guard across the directory read here is both the sibling's pattern and the documented order (store first, directory second), so nothing about the response or the existing tests changes in the serial case.

## Evidence

The rule the handler states for itself, and the lock it releases before acting on it — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/describe_app_instance_admin.rs:169-183:
```
        // The `AppInstance` an administrator is an identity OF is what serves a read of
        // that promotion, so once that instance has been deleted the read is refused as a
        // request this caller may no longer make rather than answered from a record the
        // delete left behind. A reporting operation: nothing here writes.
        if state.lock().unresolved_arn(&input.app_instance_arn)
            == crate::state::UnresolvedArn::DeletedAppInstance
        {
            return Err(
                DescribeAppInstanceAdminErrorModeled::ForbiddenException(ForbiddenException {
```
(the guard from `state.lock()` in an `if` condition is dropped at the end of the condition, so the store is unlocked before the body or anything after it runs)

The second and third acquisitions — describe_app_instance_admin.rs:195-208:
```
        let record = directory
            .lock()
            .record(&input.app_instance_arn, &input.app_instance_admin_arn)
            .cloned()
            .ok_or_else(|| {
...
        let name = state.lock().identity_name(&record.admin_arn);
```

The sibling reader of the SAME two stores keeps one snapshot, and says so — src/ops/list_app_instance_admins.rs:167-170:
```
        // LOCK ORDER: the store first, the shared admin directory second — the order every
        // writer in this crate takes them in.
        let store = state.lock();
        if !store.app_instances.contains_key(&input.app_instance_arn) {
```
with that same `store` guard still held when the directory is read (list_app_instance_admins.rs:205-213 `directory.lock().admins_of(...)`) and when `store.identity_name(&record.admin_arn)` is projected (line 246).

The crate states the order as a rule — src/state.rs:714-715:
```
/// LOCK ORDER: a writer takes [`service_state`] first and this directory second, and no
/// reader holds this one while taking another store, so the order cannot invert.
```

Why the stale record is there to be read: `DeleteAppInstance` cascades tags and published names but not promotions — src/ops/delete_app_instance.rs:226-258 withdraws tag ARNs and calls `name_directory.lock().forget_identities_of(..)`, and no `app_instance_admins(..)`/`demote(..)` call appears in the file.

Test coverage: the serial deleted-instance refusal is pinned for every instance-keyed operation EXCEPT this one — tests/all/token_reuse_and_deleted_refusals.rs:415-417 excludes it ("`DescribeAppInstanceAdmin` is out for another reason: the grant it addresses is the resource it resolves"), and tests/all/app_instance_admin_demotion.rs pins only explicit demotion, so no test exercises a delete concurrent with this read.

## Steps to Reproduce

1. CreateAppInstance -> A
2. CreateAppInstanceUser under A -> U
3. POST /app-instances/{A}/admins with AppInstanceAdminArn=U -> 201 (the promotion is now in the shared admin directory)
4. Issue GET /app-instances/{A}/admins/{U} (DescribeAppInstanceAdmin) and DELETE /app-instances/{A} (DeleteAppInstance) concurrently, so the delete takes the store lock after the describe's `unresolved_arn` check released it and before the describe locks the admin directory
5. Observe the describe answer 200 carrying AppInstanceAdmin.AppInstanceArn = A for an AppInstance that no longer exists, instead of the 403 ForbiddenException its own comment and every other instance-keyed read of a deleted instance give
6. For contrast, run the same race against GET /app-instances/{A}/admins (ListAppInstanceAdmins): it holds the store guard across the directory read, so it answers either the full listing or 403, never a listing for a deleted instance
