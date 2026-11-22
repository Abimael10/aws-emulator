Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 9497a24a09e201b48639ba78ca3a8a2f64e2323a

# The inputList bound is fixed from a metadataColumns map the handler has not yet validated, so a wrong map key is reported as an inputList length violation

_Filed: 2026-11-21T22:24:02Z_
_Operation: GetPersonalizedRanking | Class: error-response_

> _Note: this issue was identified against the code base at commit `9497a24a09e201b48639ba78ca3a8a2f64e2323a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both `GetPersonalizedRanking` and `GetRecommendations` derive the "50 vs 500" bound from `registry::asks_for_metadata`, which answers "this request includes metadata" for ANY map key whose column list is non-empty. The rule that decides whether the map asks for metadata at all — "The map key is ITEMS" — is only applied afterwards, by `registry::requested_metadata_columns`.

So for a request whose `metadataColumns` names a key other than `ITEMS` (the modelled `MetadataColumns` is `length 0..1`, so exactly one key reaches the handler, and a non-`ITEMS` key is ALWAYS refused by `requested_metadata_columns`), the handler first measures `inputList` against the metadata-bound 50 and refuses with `Length of 'inputList' cannot be greater than 50` — a clause naming a member the caller did not get wrong, carrying a bound derived from the very value the handler is about to reject. The caller has to fix `inputList` down to 50 before it is told what is actually wrong with the request (`the map key must be ITEMS`).

Two things in the crate say this is the wrong clause: `asks_for_metadata`'s own doc claims it reads the map exactly as `requested_metadata_columns` does (it does not — that function reads only the `ITEMS` key and refuses every other), and the sibling handler states the rule for this whole step as "a refusal whose every input is the request is InvalidInputException (400) naming the member the caller got wrong". Per the model, a map that names no `ITEMS` key includes no metadata in recommendations, so the maximum that applies to such a request is the "Otherwise" one.

The same ordering is in `GetRecommendations`, where it bounds `numResults` instead (`src/ops/get_recommendations.rs:248-260`); the fix belongs in the shared step, so both call sites are corrected at once.

## Evidence

The model's condition for the lower bound — `smithy_query(query="doc",shape="GetPersonalizedRankingRequest",member="inputList")`, mirrored in the handler doc at `src/ops/get_personalized_ranking.rs:88-91`:
```
//! * `inputList` (InputList) **required** — A list of items (by itemId) to rank. If an item
//!   was not included in the training dataset, the item is appended to the end of the
//!   reranked list. If you are including metadata in recommendations, the maximum is 50.
//!   Otherwise, the maximum is 500.
```
and what "including metadata" means — `src/ops/get_personalized_ranking.rs:92-97` (`GetPersonalizedRankingRequest$metadataColumns`):
```
//! * `metadataColumns` (MetadataColumns, len 0..1) — If you enabled metadata in
//!   recommendations when you created or updated the campaign, specify metadata columns from
//!   your Items dataset to include in the personalized ranking. The map key is ITEMS and the
//!   value is a list of column names from your Items dataset.
```

The handler fixes the bound BEFORE the map key is checked — `src/ops/get_personalized_ranking.rs:214-219` and `:229-232`:
```
        let max_input_list = registry::metadata_bounded_max(registry::asks_for_metadata(
            input.metadata_columns.as_ref(),
        ));
        if input.input_list.len() > max_input_list {
            return Err(deny(registry::too_long("inputList", max_input_list)));
        }
...
        //    So are the `metadataColumns` rules that need nothing but the request: the
        //    map key, and how many columns it may name.
        let requested_metadata =
            registry::requested_metadata_columns(input.metadata_columns.as_ref()).map_err(deny)?;
```

The predicate that fixes the bound accepts ANY key, while claiming to agree with the function that accepts only `ITEMS` — `src/state.rs:793-802`:
```
/// Whether the request asks for metadata in recommendations at all.
///
/// This reads the REQUEST only — it is the condition in "If you are including metadata
/// in recommendations, the maximum is 50. Otherwise, the maximum is 500."
/// (`GetPersonalizedRankingRequest$inputList` / `GetRecommendationsRequest$numResults`
/// docs), which decides a bound before any resource is resolved. A map that names no
/// column asks for nothing, exactly as [`requested_metadata_columns`] reports it.
pub fn asks_for_metadata(metadata_columns: Option<&HashMap<String, Vec<String>>>) -> bool {
    metadata_columns.is_some_and(|columns| columns.values().any(|names| !names.is_empty()))
}
```
against the function it points at, which refuses every key but `ITEMS` — `src/state.rs:851-857`:
```
    for (dataset, columns) in entries {
        if dataset != METADATA_DATASET_ITEMS {
            return Err(Denied::invalid(format!(
                "Value '{dataset}' at 'metadataColumns' failed to satisfy constraint: the map key \
                 must be {METADATA_DATASET_ITEMS}"
            )));
        }
```

The rule this step is supposed to follow, stated by the sibling handler — `src/ops/get_recommendations.rs:9-17`:
```
//! 1. **Settle what the REQUEST alone decides.** A refusal whose every input is the
//!    request is `InvalidInputException` (400) naming the member the caller got wrong,
//!    BEFORE anything is resolved, so one malformed request earns one refusal whatever
//!    the registry holds. That covers `numResults` against the bound the same request
//!    fixes (...), the `metadataColumns` rules that read nothing else ("The map key is `ITEMS`" ...)
```
and the same ordering at the sibling call site — `src/ops/get_recommendations.rs:248-260`:
```
        let max_num_results = registry::metadata_bounded_max(registry::asks_for_metadata(
            input.metadata_columns.as_ref(),
        ));
        let num_results = registry::effective_num_results(
            input.num_results,
            registry::DEFAULT_NUM_RESULTS,
            max_num_results,
        )
        .map_err(deny)?;
        //    The `metadataColumns` rules that need nothing but the request: the map key
        //    and how many columns it may name.
        let requested_metadata =
            registry::requested_metadata_columns(input.metadata_columns.as_ref()).map_err(deny)?;
```

Test coverage: `tests/all/request_only_refusal_order.rs` and `tests/all/metadata_member_presence.rs` exercise the request-only refusals and the `metadataColumns` rules, but no row combines a non-`ITEMS` map key with an `inputList` (or `numResults`) past 50, so nothing pins which of the two clauses such a request earns.

## Steps to Reproduce

1. Register an ACTIVE PERSONALIZED_RANKING campaign (`crate::state::register_campaign`, or the control plane's CreateCampaign path).
2. POST /personalize-ranking with that `campaignArn`, `userId: "u1"`, an `inputList` of 60 item ids, and `metadataColumns: {"USERS": ["AGE"]}`.
3. Observe HTTP 400 `InvalidInputException` with the message `Length of 'inputList' cannot be greater than 50` — a bound taken from a `metadataColumns` map the service never accepts (src/ops/get_personalized_ranking.rs:214-219).
4. Shorten `inputList` to 50 entries and repeat: the same request now earns the refusal that was true all along — `Value 'USERS' at 'metadataColumns' failed to satisfy constraint: the map key must be ITEMS` (src/state.rs:851-857).
5. Repeat steps 2-4 against POST /recommendations with `numResults: 200` and the same `metadataColumns`, for the identical pair of refusals (src/ops/get_recommendations.rs:248-260).
