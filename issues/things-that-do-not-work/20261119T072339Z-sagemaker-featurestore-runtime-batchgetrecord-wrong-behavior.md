Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: d3d6261999f9a11a24fded61323b830533f5b9d0

# The two batch handlers lock this service's store and then lock the shared feature-group directory underneath it — the reverse of the order the crate's five single-record handlers take, and the opposite of what the comment stating that order claims

_Filed: 2026-11-19T07:23:39Z_
_Operation: BatchGetRecord | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d3d6261999f9a11a24fded61323b830533f5b9d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::resolve_group` reaches ANOTHER store than this service's own: it locks the shared `FeatureGroupDirectory` the SageMaker control plane writes (`directory.lock().get(...)`). The crate therefore has two locks a handler can hold — this service's record store and the shared directory — and it takes them in two OPPOSITE orders.

The five single-record handlers (`PutRecord`, `GetRecord`, `UpdateRecord`, `DeleteRecord`, `ListRecords`) all resolve the feature group FIRST and only then take `state.lock()`, so the directory lock is never acquired while this service's guard is held. Both batch handlers do the reverse: `BatchGetRecord` takes `state.lock()` before the per-entry loop and calls `resolve_group` — hence the directory lock — inside it, and `BatchWriteRecord` does the same inside its single critical section.

Worse, the comment that documents the batch order asserts it is the crate-wide one: "LOCK ORDER: this service's own store first, the shared feature-group directory second (the order every other caller of the directory takes)". No caller in this crate takes that order except the two batch handlers; the other five take the exact opposite. So the one written statement of lock discipline in the crate is contradicted by five of its own handlers, and a future path that resolves a group while holding the record store (or holds the directory while taking the record store) has no correct order to follow. This is the same defect class already filed and fixed for `services/service-com-amazonaws-billing-awsbilling` ("The three Enterprise Support reads resolve the caller's identity (locking another service's store) while holding this service's own state lock, which two sibling handlers state must never happen", gap_class wrong-behavior, LOW), where the sibling handlers' rule was that resolving through another service's store "never happens while this service's own lock is held".

Nothing requires the batch handlers to resolve under their own guard: the resolution is a read of a different store keyed by the request alone, exactly as the five single-record handlers prove by doing it first. Hoisting the per-entry resolution out of the critical section (or correcting the claim) leaves every response and every existing test unchanged.

## Evidence

The directory IS a second lock — services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/state.rs:842-845:
```
    let Some(directory) = feature_group_directory(engine) else {
        return Err(Unresolved::NoDirectory);
    };
    let held = directory.lock().get(name_or_arn).cloned();
```

BatchGetRecord takes this service's store FIRST, then the directory per entry — src/ops/batch_get_record.rs:172 and 190-194:
```
        let mut store = state.lock();
...
            let refusal = match crate::state::resolve_group(
                &*self.engine,
                &region,
                &wanted.feature_group_name,
            ) {
```

The claim, in the sibling batch handler — src/ops/batch_write_record.rs:224-228 (the `resolve_group` call it covers is at 256-257):
```
        // ... LOCK ORDER: this service's own store first,
        // the shared feature-group directory second (the order every other caller of the
        // directory takes).
        {
        let mut store = state.lock();
```

The five handlers that take the OPPOSITE order (resolve, then lock):
* src/ops/put_record.rs:216 `let definition = match crate::state::resolve_group(` … src/ops/put_record.rs:257 `let mut store = state.lock();`
* src/ops/get_record.rs:183 `let definition = match crate::state::resolve_group(` … src/ops/get_record.rs:204 `let mut store = state.lock();`
* src/ops/update_record.rs:244 `let definition = match crate::state::resolve_group(` … src/ops/update_record.rs:386 `let mut store = state.lock();`
* src/ops/delete_record.rs:239 `let definition = match crate::state::resolve_group(` … src/ops/delete_record.rs:291 `let mut store = state.lock();`
* src/ops/list_records.rs:185 `let definition = match crate::state::resolve_group(` … src/ops/list_records.rs:217 `let mut store = state.lock();`

Test coverage: both batch paths are exercised end-to-end — tests/all/batch_write_record_bulk_ingestion.rs (`every_entry_of_a_batch_is_ingested_into_the_feature_group_it_names`, `an_entry_that_cannot_be_written_is_reported_in_errors_and_the_rest_of_the_batch_is_written`) and tests/all/feature_group_resolution.rs (`every_operation_refuses_a_feature_group_name_that_resolves_to_no_feature_group`), which drive BatchGetRecord's per-entry resolution refusal. No test asserts anything about lock discipline, so the contradiction is invisible to the suite.

## Steps to Reproduce

1. Read src/state.rs:842-845: `resolve_group` locks the SHARED `FeatureGroupDirectory`, a different lock from this service's own record store.
2. Read src/ops/batch_get_record.rs:172, then 190-194: the handler takes `state.lock()` and then calls `resolve_group` (the directory lock) for every entry inside that critical section; src/ops/batch_write_record.rs:228 and 256-257 do the same.
3. Read the comment at src/ops/batch_write_record.rs:224-226, which states that order is "the order every other caller of the directory takes".
4. Read src/ops/put_record.rs:216 vs 257, src/ops/get_record.rs:183 vs 204, src/ops/update_record.rs:244 vs 386, src/ops/delete_record.rs:239 vs 291 and src/ops/list_records.rs:185 vs 217: all five resolve the group BEFORE taking the store guard — the opposite order, so the quoted claim is false and the crate holds the two locks in two orders.
