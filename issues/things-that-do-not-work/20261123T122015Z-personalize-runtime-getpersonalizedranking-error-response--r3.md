Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 27adb121eb59767e1dcf6a4e065660a1b2826a94

# The inputList/numResults bound is still lowered by a metadataColumns map the handler refuses one line later, when the map names more than 10 columns

_Filed: 2026-11-23T12:20:15Z_
_Operation: GetPersonalizedRanking | Class: error-response_

> _Note: this issue was identified against the code base at commit `27adb121eb59767e1dcf6a4e065660a1b2826a94`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both inference handlers fix the "50 vs 500" maximum from `registry::asks_for_metadata` BEFORE `registry::requested_metadata_columns` has applied the two request-only `metadataColumns` rules the crate owns (the map key, and the 10-column cap). After the earlier fix for the map-KEY trigger, `asks_for_metadata` reads only the `ITEMS` key — so a non-`ITEMS` key no longer lowers the bound — but the COLUMN-COUNT rule is still applied only afterwards.

So for a request whose `metadataColumns` is `{"ITEMS": [11 columns]}`, `asks_for_metadata` answers true (the `ITEMS` key names a non-empty list), the handler measures `inputList` against 50, and a 60-entry `inputList` earns `Length of 'inputList' cannot be greater than 50` — a bound taken from the very map the next statement refuses with `at most 10 columns can be provided`. The caller has to shorten a member it did not get wrong before it is told what is actually wrong with the request. The comment directly above the check promises the opposite: "the lower bound is never taken from a map this handler is about to refuse".

Both rules here are request-only (neither needs the campaign), so the crate's own stated precedence — the refusal naming "the member the caller got wrong" — is the one the column-count clause should win under. The same ordering is in `GetRecommendations`, where it bounds `numResults` (src/ops/get_recommendations.rs:251-263); the two call sites share one step, so one fix (validating `requested_metadata_columns` before fixing the bound) corrects both and leaves the already-fixed map-key outcome exactly as it is.

## Evidence

The claim, and the code under it that breaks it — `src/ops/get_personalized_ranking.rs:215-223`:
```
        //    maxima applies is the `metadataColumns` map read as the model states it
        //    ("The map key is `ITEMS`"), so a map that names no column of the Items
        //    dataset includes no metadata and gets the "Otherwise" maximum — the lower
        //    bound is never taken from a map this handler is about to refuse.
        let max_input_list = registry::metadata_bounded_max(registry::asks_for_metadata(
            input.metadata_columns.as_ref(),
        ));
        if input.input_list.len() > max_input_list {
            return Err(deny(registry::too_long("inputList", max_input_list)));
        }
```
The column cap is applied only AFTER that refusal can fire — `src/ops/get_personalized_ranking.rs:233-236`:
```
        //    So are the `metadataColumns` rules that need nothing but the request: the
        //    map key, and how many columns it may name.
        let requested_metadata =
            registry::requested_metadata_columns(input.metadata_columns.as_ref()).map_err(deny)?;
```
The predicate that fixes the bound answers true for ANY non-empty `ITEMS` list, including one past the cap — `src/state.rs:822-823` with `src/state.rs:802-809`:
```
pub fn asks_for_metadata(metadata_columns: Option<&HashMap<String, Vec<String>>>) -> bool {
    items_metadata_columns(metadata_columns).is_some()
```
```
fn items_metadata_columns(
    metadata_columns: Option<&HashMap<String, Vec<String>>>,
) -> Option<&[String]> {
    metadata_columns?
        .get(METADATA_DATASET_ITEMS)
        .map(Vec::as_slice)
        .filter(|columns| !columns.is_empty())
}
```
while the function the comment defers to refuses such a map outright — `src/state.rs:885-891`:
```
    if requested.len() > MAX_METADATA_COLUMNS {
        return Err(Denied::invalid(format!(
            "Value '{}' at 'metadataColumns' failed to satisfy constraint: at most \
             {MAX_METADATA_COLUMNS} columns can be provided",
            requested.len()
        )));
    }
```
The same ordering at the sibling call site — `src/ops/get_recommendations.rs:248-263`:
```
        //    metadata, so it gets the "Otherwise" maximum rather than lowering the bound
        //    with a map the handler is about to refuse.
        let max_num_results = registry::metadata_bounded_max(registry::asks_for_metadata(
            input.metadata_columns.as_ref(),
        ));
        let num_results = registry::effective_num_results(
            input.num_results,
            registry::DEFAULT_NUM_RESULTS,
            max_num_results,
        )
        .map_err(deny)?;
```
Test coverage: `tests/all/request_only_refusal_order.rs:277-290` and `:393-399` exercise `{"ITEMS": column_names(MAX_METADATA_COLUMNS + 1)}` and expect `metadata_columns_past(..)`, but every such row carries a short `inputList`/`numResults`, so no row combines a past-the-cap column list with a list/count past 50 — nothing pins which of the two clauses that request earns.

## Steps to Reproduce

1. Register an ACTIVE PERSONALIZED_RANKING campaign with metadata in recommendations enabled (`crate::state::register_campaign`, or the control plane's CreateCampaign path).
2. POST /personalize-ranking with that `campaignArn`, `userId: "u1"`, an `inputList` of 60 item ids, and `metadataColumns: {"ITEMS": ["C1",...,"C11"]}` (11 columns).
3. Observe HTTP 400 `InvalidInputException` with `Length of 'inputList' cannot be greater than 50` — the lower bound taken from a map the handler refuses at src/ops/get_personalized_ranking.rs:235-236.
4. Shorten `inputList` to 50 entries and repeat: the same request now earns the refusal that was true all along — `Value '11' at 'metadataColumns' failed to satisfy constraint: at most 10 columns can be provided` (src/state.rs:885-891).
5. Repeat against POST /recommendations with `numResults: 200` and the same `metadataColumns` for the identical pair of refusals (src/ops/get_recommendations.rs:251-263).
