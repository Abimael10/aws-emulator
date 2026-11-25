Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 7c1a32607181dcc252ace77444601da30e81bd5f

# An empty $and / $or operand array is accepted and silently changes the result set, while every sibling empty operand list in the same walk is refused 400

_Filed: 2026-11-25T07:08:25Z_
_Operation: SearchDiscoverableRegistryRecords | Class: validation_

> _Note: this issue was identified against the code base at commit `7c1a32607181dcc252ace77444601da30e81bd5f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`SearchDiscoverableRegistryRecords`'s metadata-filter walk guards three operand lists. Two of them refuse an EMPTY list outright — the field-level operator object (`operators.is_empty()`) and `$in`'s candidate array (`candidates.is_empty()`) — and both guards spell a lower bound of 1 in their own refusal sentence. The third, `branches()`, which validates the operand array of `$and` and `$or`, checks ONLY the upper bound (`branches.len() > MAX_FILTER_TERMS`), so an empty array passes validation.

Because validation passes, the expression is then EVALUATED, and the degenerate operand silently decides the answer rather than being refused:

* `{"$or": []}` — the `$or` arm seeds `any = false` and iterates zero branches, so `holds` becomes `false` for every record. The search answers HTTP 200 with an EMPTY `registryRecords`, as if the registry held no matching record.
* `{"$and": []}` — the `$and` arm seeds `all = true` and iterates zero branches, so `holds` stays `true` for every record. The filter the caller supplied is silently a no-op and the search answers records the caller asked to have narrowed away.

So the same class of malformed operand — an operator whose operand list carries nothing — is a 400 `ValidationException` at two of three sites in one function and a silently-wrong 200 at the third. `{"recordType": {"$in": []}}` is pinned as a 400 by the crate's own refusal table, while `{"$or": []}` and `{"$and": []}` are pinned by nothing and answered 200. The guard's own doc sentence already states the intent the code does not implement ("an array of expressions"); an empty array carries no expression.

This also contradicts the handler's stated reason for validating the filter up front: a `$or: []` request is reported as "no record matched", which is indistinguishable from a well-formed query that genuinely matched nothing.

## Evidence

The guard that does NOT refuse an empty operand list — services/service-com-amazonaws-agentregistry-agentregistry/src/ops/search_discoverable_registry_records.rs:454-466:
```
/// The branches of a `$and` / `$or`, which must be a bounded array of expressions.
fn branches<'a>(operator: &str, value: &'a Document) -> Result<&'a Vec<Document>, String> {
    let Document::Array(branches) = value else {
        return Err(format!("{operator} takes an array of filter expressions."));
    };
    if branches.len() > MAX_FILTER_TERMS {
        return Err(format!(
            "{operator} takes at most {MAX_FILTER_TERMS} filter expressions, got {}.",
            branches.len()
        ));
    }
    Ok(branches)
}
```
(only `> MAX_FILTER_TERMS` — no lower bound)

The two SIBLING guards in the same file that DO refuse an empty operand list, each naming a lower bound of 1 — search_discoverable_registry_records.rs:522-526:
```
    if operators.is_empty() || operators.len() > MAX_FILTER_TERMS {
        return Err(format!(
            "'{field}' takes an object naming between 1 and {MAX_FILTER_TERMS} of $eq, $ne or $in."
        ));
    }
```
and search_discoverable_registry_records.rs:537-542:
```
                if candidates.is_empty() || candidates.len() > MAX_FILTER_TERMS {
                    return Err(format!(
                        "$in takes between 1 and {MAX_FILTER_TERMS} values, got {}.",
                        candidates.len()
                    ));
                }
```

The evaluation that turns the unrefused empty array into a wrong answer — search_discoverable_registry_records.rs:433-449:
```
        holds &= match key.as_str() {
            "$and" => {
                let mut all = true;
                for branch in branches(key, value)? {
                    all &= walk(branch, record, depth + 1)?;
                }
                all
            }
            "$or" => {
                let mut any = false;
                for branch in branches(key, value)? {
                    any |= walk(branch, record, depth + 1)?;
                }
                any
            }
```
with zero branches: `$and` yields `true` (filter ignored), `$or` yields `false` (every record dropped at search_discoverable_registry_records.rs:245-247 `if !matches_metadata_filter(input.filters.as_ref(), record)? { continue; }`).

Test coverage, showing the inconsistency is unpinned on the `$and`/`$or` side: the empty `$in` IS pinned as a refusal — tests/all/discovery_read_path.rs:1213-1218:
```
        (
            "filters carries an empty $in",
            "/discoverable-records-search",
            json!({ "searchQuery": "", "registryIds": [REGISTRY], "maxResults": 10,
                    "filters": { "recordType": { "$in": [] } } }),
        ),
```
while the `$and`/`$or` operator test only ever supplies NON-empty arrays — tests/all/discovery_read_path.rs:1108-1130 (`"$or": [ { "recordType": { "$eq": "MCP" } }, { "recordType": { "$eq": "SKILL" } } ]`, `"$and": [ ... two branches ... ]`). No case in the suite posts `{"$or": []}` or `{"$and": []}`, so nothing pins the current behaviour.

## Steps to Reproduce

1. Build the seeded router of tests/all/discovery_read_path.rs (`seeded_router()`), whose REGISTRY `abc123def456` holds three discoverable records (DISCOVERABLE = ["rec000000001", "rec000000002", "rec000000004"]).
2. POST /discoverable-records-search with {"searchQuery": "", "registryIds": ["abc123def456"], "maxResults": 20, "filters": {"recordType": {"$in": []}}} — answered 400 ValidationException, as tests/all/discovery_read_path.rs:1213-1218 asserts.
3. POST the same request with "filters": {"$or": []} — answered HTTP 200 with registryRecords: [], instead of refusing the empty operand array the way step 2 refuses it. The caller cannot tell this from a query that genuinely matched nothing.
4. POST the same request with "filters": {"$and": []} — answered HTTP 200 with all three discoverable records, i.e. the supplied filter narrowed nothing at all.
5. POST the same request with "filters": {"name": {}} — answered 400 (search_discoverable_registry_records.rs:522-526), confirming that an empty operand object IS refused everywhere except inside branches().
