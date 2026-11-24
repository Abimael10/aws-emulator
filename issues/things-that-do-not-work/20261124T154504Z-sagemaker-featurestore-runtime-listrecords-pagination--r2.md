Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 7d054efe5feda12f84d3d712f5f3c4f97dd16355

# A truncated ListRecords page cursor falls back to a COUNT, so a listing silently drops records it never returned once the cursor's record and a record before it are both gone

_Filed: 2026-11-24T15:45:04Z_
_Operation: ListRecords | Class: pagination_

> _Note: this issue was identified against the code base at commit `7d054efe5feda12f84d3d712f5f3c4f97dd16355`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

For a record identifier too long for a page token to carry whole, `ListRecords` mints a cursor made of the identifier's leading characters (the prefix), the SHA-256 digest of the whole identifier, and `skipped` — how many listable records of that prefix the page had already passed. On the next page `FeatureGroupStore::records_to_skip` looks for the record whose digest matches among the records sharing the prefix and, when it does not find it, falls back to the stored `skipped` count and skips that many records from the prefix on.

That fallback is a POSITION, not a record, so it is only correct while the records ordered before the cursor are still there. If the cursor's own record is gone AND at least one record ordered before it (within the prefix) is also gone, the walk skips past records the listing has never returned: those identifiers are never served and the response carries no `NextToken`, so the listing ends short and the caller has no way to notice or resume. The same over-skip happens when the cursor's record stops being listable for a second reason — it was soft-deleted and this page is asked for without `IncludeSoftDeletedRecords` — because the digest search is made over the LISTABLE records only (`.filter(|record| listable(record, include_soft_deleted))`), so the search misses and the stale count is used again.

The whole-identifier form of the cursor has no such hole: it names the identifier, and a cursor naming an identifier the group no longer holds resumes at the next one above it. The module's own header claims the truncated form behaves the same way ("so the page boundary survives records being added or removed between calls just as the whole form does"), and `page_identifiers` claims "the pages of a listing are the group's records in ascending identifier order, each once" — both of which this path breaks.

## Evidence

The claim, services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/state/page_cursor.rs:17-21:
```
//! what makes a cursor name an identifier and not a position. A longer one travels as its
//! leading characters plus the digest of the whole identifier and how many records of that
//! prefix the page had already passed: the resumed walk starts at the prefix, recognises
//! the record the digest names wherever it now sits among the records sharing that prefix,
//! and falls back on the count when that record is gone — so the page boundary survives
//! records being added or removed between calls just as the whole form does.
```
and page_cursor.rs:159-161 (`page_identifiers`): "the cursor handed back names the first record this page did NOT return, so the pages of a listing are the group's records in ascending identifier order, each once."

The code that breaks it, page_cursor.rs:225-234:
```
    fn records_to_skip(&self, cursor: &PageCursor, include_soft_deleted: bool) -> usize {
        let Some(truncated) = &cursor.truncated else {
            return 0;
        };
        self.records_from(Some(&cursor.resume_at))
            .take_while(|record| record.identifier.starts_with(&cursor.resume_at))
            .filter(|record| listable(record, include_soft_deleted))
            .position(|record| identifier_digest(&record.identifier) == truncated.digest)
            .unwrap_or(truncated.skipped)
    }
```
and the walk it drives, page_cursor.rs:168-178:
```
        let skipped = cursor.map_or(0, |cursor| self.records_to_skip(cursor, include_soft_deleted));
        let mut remaining = self
            .records_from(cursor.map(PageCursor::resume_at))
            .filter(|record| listable(record, include_soft_deleted))
            .skip(skipped);
```

The other side of the inconsistency — the WHOLE form's guarantee, src/state.rs:151-152: "A cursor naming an identifier the group no longer holds resumes at the next one above it."

The crate's own test states the rule as a general one and exercises only ONE removal per row, so the combination is untested — tests/all/online_store_record_lifecycle.rs:1873-1898:
```
async fn a_cursor_at_an_identifier_too_long_for_a_token_still_names_its_record() {
    // ... a cursor names a RECORD, not a position, so removing a record that sorts BEFORE the
    // cursor must not move the resume point, and removing the cursor's own record resumes
    // at the records above it.
    let written = identifiers_sharing_a_prefix(5_000, 4);
    let cases: Vec<(&str, Option<usize>, Vec<usize>)> = vec![
        ("nothing deleted", None, vec![1, 2, 3]),
        ("the record before the cursor hard-deleted", Some(0), vec![1, 2, 3]),
        ("the cursor's own record hard-deleted", Some(1), vec![2, 3]),
        ("the record after the cursor hard-deleted", Some(2), vec![1, 3]),
    ];
```
(each row deletes at most one record, `deleted: Option<usize>` at :1930).

Arithmetic of the reproduction: `MAX_WHOLE_IDENTIFIER_BYTES = MAX_PAYLOAD_BYTES - KEY_LENGTH - 3` = 4077 (page_cursor.rs:36,55), so a 5 000-character identifier takes the truncated form (`cursor_at`, page_cursor.rs:192-212), and the 4 identifiers of the test fixture share the 3 991-character prefix.

## Steps to Reproduce

1. Register a feature group `g` with an OnlineStore in the shared feature-group directory (as tests/all/online_store_record_lifecycle.rs does).
2. PutRecord four records whose `customer_id` values are the 5 000-character identifiers of `identifiers_sharing_a_prefix(5_000, 4)` — i.e. they share their first 4 997 characters and end 001, 002, 003, 004 — each with an event-time feature.
3. POST /FeatureGroup/g/ListRecords with {"MaxResults": 1}: the page carries identifier …001 and a NextToken whose cursor is the truncated form (prefix = the first 3 991 characters, digest = digest(…002), skipped = 1).
4. DELETE /FeatureGroup/g?RecordIdentifierValueAsString=<…001>&EventTime=2024-01-02T00:00:00Z&DeletionMode=HardDelete and the same for <…002>.
5. POST /FeatureGroup/g/ListRecords with that NextToken and {"MaxResults": 1}: `records_to_skip` finds no digest match (…002 is gone), falls back to skipped = 1, and the walk skips …003 — the response is {"RecordIdentifiers": ["…004"]} with NO NextToken, so …003 is never returned by any page although it was never returned before either. Expected: …003 then …004, which is what the whole-identifier form of the cursor gives for the same deletions.
6. Variant reaching the same over-skip with one hard delete and one soft delete: after step 3, SoftDelete <…002> (default DeletionMode) and HardDelete <…001>, then resume with the same token and IncludeSoftDeletedRecords absent — the digest search runs over listable records only, misses …002, falls back to 1, and drops …003.
