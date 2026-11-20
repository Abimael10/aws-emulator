Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: db4cad1c26cf8d6e381b1c5a713894234123e1ec

# ListRecords clones the identifier of every record it skips before applying the page cursor, though the comment above the walk states nothing linear in the whole store is ever copied

_Filed: 2026-11-20T05:17:16Z_
_Operation: ListRecords | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `db4cad1c26cf8d6e381b1c5a713894234123e1ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The page walk maps each stored record to a cloned `String` identifier BEFORE the cursor filter runs, so resuming a page at identifier X allocates and copies the identifier of every record ordered before X and then throws each one away. The comment directly above the walk states the opposite invariant — that only the page (plus the one identifier the token names) is reached, so "nothing linear in the whole store is ever copied" — and the store's own doc explains that the records are held in a `BTreeMap` precisely so a page token can name the identifier to resume from. The ordered map is then walked from its first key with a clone per skipped record instead of being resumed at the cursor.

The page CONTENTS and the emitted `NextToken` are correct; what is wrong is the stated invariant and the work done to honour it. The exposure scales with the store: a group may hold up to `MAX_RECORDS_PER_GROUP` (10 000) records and each `RecordIdentifierValueAsString` is modelled `len 0..358400`, so paging to the end of a full group copies the bytes of nearly every identifier in the group once per page. Swapping the `map(clone)` to after the cursor filter (or resuming the `BTreeMap` at the cursor) leaves every response and every existing test unchanged.

## Evidence

The claim — services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/ops/list_records.rs:218-224:
```
        // `records()` iterates in ascending identifier order (a `BTreeMap`), which is
        // what makes the cursor below a correct resume point: `filter` keeps every
        // identifier at or after it regardless of iteration order, and the token the
        // page emits is the first identifier it did not return. The walk stops at the
        // page (plus that one identifier), so nothing linear in the whole store is ever
        // copied.
```

The code under it — src/ops/list_records.rs:225-240:
```
        let (record_identifiers, next_identifier) = store
            .group_mut(&input.feature_group_name)
            .map(|group| {
                let mut remaining = group
                    .records()
                    .filter(|record| include_soft_deleted || !record.soft_deleted)
                    .map(|record| record.identifier.clone())
                    .filter(|identifier| {
                        resume_at
                            .as_deref()
                            .is_none_or(|resume| identifier.as_str() >= resume)
                    });
                let page: Vec<String> = remaining.by_ref().take(page_size).collect();
                (page, remaining.next())
            })
            .unwrap_or_default();
```
`.map(|record| record.identifier.clone())` sits ABOVE the `resume_at` filter, so every record the cursor later discards has its identifier cloned first.

The data structure was chosen for cursor resumption — src/state.rs:110-113:
```
    /// Records by `RecordIdentifierValueAsString`, at most [`MAX_RECORDS_PER_GROUP`] of
    /// them. A `BTreeMap` so `ListRecords` enumerates in one deterministic ASCENDING
    /// identifier order — which is what lets a page token name the identifier to resume
    /// from rather than a positional index that insertions would shift.
```

The size the skipped copies can reach is the crate's own: `pub const MAX_RECORDS_PER_GROUP: usize = 10_000;` (src/state.rs:49) and the modelled identifier bound `RecordIdentifierValueAsString` (ValueAsString, len 0..358400) quoted in the handler docs at src/ops/list_records.rs (OPERATIONS.md:43,50).

Test coverage: pagination is exercised end-to-end by tests/all/online_store_record_lifecycle.rs:625 `list_records_pages_through_the_identifiers_in_state`, which asserts the page contents and the token round-trip only — no test asserts anything about what the walk copies, so moving the clone below the cursor filter changes no assertion.

## Steps to Reproduce

1. Read src/ops/list_records.rs:218-224: the comment states "The walk stops at the page (plus that one identifier), so nothing linear in the whole store is ever copied."
2. Read src/ops/list_records.rs:225-240: `.map(|record| record.identifier.clone())` is applied to every record the iterator pulls, BEFORE the `resume_at` filter that decides whether the identifier belongs to this page.
3. Create a feature group with an OnlineStore through the SageMaker control plane and PutRecord N records into it (N up to MAX_RECORDS_PER_GROUP = 10 000, src/state.rs:49).
4. POST /FeatureGroup/<group>/ListRecords with MaxResults=1 repeatedly, following NextToken: page k resumes at the k-th identifier, and the handler clones (then discards) the identifier of each of the k-1 records ordered before it — linear in the prefix of the store, with each clone up to the modelled 358 400-character identifier length.
5. Compare with src/state.rs:110-113, which says the BTreeMap exists so a page token can name the identifier to resume from: the ordered map is never resumed at that identifier, it is re-walked from the start.
