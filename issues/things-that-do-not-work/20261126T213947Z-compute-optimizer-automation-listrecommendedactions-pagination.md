Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 797fabee68238dd5d382911900b8eef7c529eed2

# Page token rejects a re-spelled RestartNeeded/LookBackPeriodInDays filter value that selects identically

_Filed: 2026-11-26T21:39:47Z_
_Operation: ListRecommendedActions | Class: pagination_

> _Note: this issue was identified against the code base at commit `797fabee68238dd5d382911900b8eef7c529eed2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

ListRecommendedActions binds its `nextToken` to a fingerprint of `canonical_filters(&filters)`, which normalises each filter's `values` only as a sorted, de-duplicated list of the RAW strings. But two of the filter evaluators this service applies do not compare raw strings: `RestartNeeded` is matched case-insensitively after trimming, and `LookBackPeriodInDays` is matched numerically after trimming. So `values: ["true"]` and `values: ["TRUE"]` (likewise `["14"]` vs `[" 14"]`/`["014"]`, and `["true","TRUE"]` vs `["true"]`) perform the IDENTICAL selection over the identical sequence, yet digest to different fingerprints — so a token minted under one spelling is refused for the other with `InvalidParameterValueException` ("Member must be a token returned for the same filters") instead of serving the next page of the same walk.

That contradicts the rule the handler and the helper state for themselves: the token is supposed to carry "the SELECTION those filters perform" so that "another spelling of one selection continues one walk". A caller walking pages while spelling a boolean filter value `TRUE` on one page and `true` on the next is handed a 400 for a token this listing did in fact issue over exactly that sequence. The crate's own test already pins that the two spellings are one selection, which is what makes the two sides inconsistent rather than merely under-specified.

## Evidence

The rule the token is claimed to implement — services/service-com-amazonaws-computeoptimizerautomation-computeoptimizerautomationservice/src/ops/list_recommended_actions.rs:193-200:
```
// The page token carries this operation's name and the SELECTION those
// filters perform — every filter's values as the set it tests membership
// of, the filters in one order, a filter spelled twice once
// (`crate::state::canonical_filters`) — so resuming a walk with
// different filters, or with a token another operation issued, is
// refused rather than answered with a page of a different result set,
// while another spelling of one selection continues one walk:
// `crate::state::page_fingerprint`.
```

What the fingerprint actually normalises — src/state.rs:548-568 (`canonical_membership`, reached through `canonical_filters`, src/state.rs:621-637):
```
/// `values` as the SET the walk reads it as: every distinct entry once, in one
/// order.
...
pub fn canonical_membership(values: &[String]) -> Vec<String> {
    let mut canonical = values.to_vec();
    canonical.sort_unstable();
    canonical.dedup();
    canonical
}
```
(raw-string sort + dedup: `"TRUE"` and `"true"` are two distinct entries, `"014"` and `"14"` are two distinct entries)

What the selection actually compares — src/state.rs:4076-4087:
```
// A boolean member: `true`/`false`, matched without regard to case
// so neither JSON's nor the console's spelling is rejected.
"RestartNeeded" => {
    ...
    return filter
        .values
        .iter()
        .filter_map(|value| value.trim().to_ascii_lowercase().parse::<bool>().ok())
        .any(|wanted| wanted == restart_needed);
}
```
and src/state.rs:4063-4075 for `LookBackPeriodInDays` (`value.trim().parse::<i32>()`).

The refusal the mismatch produces — src/ops/list_recommended_actions.rs:210-220 (`PageEntryRefusal::OtherQuery` → `"Member must be a token returned for the same filters"`), raised by src/state.rs:4261-4263.

The crate's own test proves the two spellings are ONE selection — tests/all/recommended_actions.rs:443-457:
```
(
    "RestartNeeded-true",
    json!([{"name": "RestartNeeded", "values": ["true"]}]),
    vec![action_id(2), action_id(3)],
),
...
(
    "RestartNeeded-upper-case",
    json!([{"name": "RestartNeeded", "values": ["TRUE"]}]),
    vec![action_id(2), action_id(3)],
),
```
and the paging rule the suite states — tests/all/membership_page_token_spellings.rs:7-16: "neither the order of such a list nor an entry repeated in it can change which items a walk serves or the order it serves them in: two such spellings name ONE selection and continue ONE walk". That suite's table (membership_page_token_spellings.rs:156-249) only ever re-spells by permuting/repeating exact strings, so this case is uncovered by any test.

By contrast, ListAutomationEventSummaries DOES digest its query after the read the selection performs — src/ops/list_automation_event_summaries.rs:258-265 digests `start_date.map(crate::state::instant_key)` (the PARSED instant), so "2026-01-01" and "2026-01-01T00:00:00Z" continue one walk. The filter-value path is the one place the normalisation stops short of the comparison the evaluator makes.

## Steps to Reproduce

1. Start the emulator and opt the calling account in: POST / with X-Amz-Target: ComputeOptimizerAutomationService.UpdateEnrollmentConfiguration and body {"status":"Active"} (this records the account's recommended-action inventory).
2. POST / with X-Amz-Target: ComputeOptimizerAutomationService.ListRecommendedActions and body {"maxResults":1,"filters":[{"name":"RestartNeeded","values":["true"]}]}. Note the returned nextToken.
3. POST / with X-Amz-Target: ComputeOptimizerAutomationService.ListRecommendedActions and body {"maxResults":1,"filters":[{"name":"RestartNeeded","values":["TRUE"]}],"nextToken":"<the token from step 2>"}.
4. Expected (per list_recommended_actions.rs:193-200 and recommended_actions.rs:443-457, which pin ["TRUE"] and ["true"] as the same selection): HTTP 200 with the second page of that walk.
5. Actual: HTTP 400 InvalidParameterValueException, "1 validation error detected: Value at 'nextToken' failed to satisfy constraint: Member must be a token returned for the same filters".
6. The same happens for {"name":"LookBackPeriodInDays","values":["14"]} vs [" 14"] / ["014"], and for ["true"] vs ["true","TRUE"].
