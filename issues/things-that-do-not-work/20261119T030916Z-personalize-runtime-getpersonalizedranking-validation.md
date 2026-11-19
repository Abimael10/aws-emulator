Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 79023c4f507908bc4e0b94b04ba9cd6b317395a7

# An empty-string inputList entry is reported back as a ranked item, bypassing the crate's own "empty ItemID names no item" rule

_Filed: 2026-11-19T03:09:16Z_
_Operation: GetPersonalizedRanking | Class: validation_

> _Note: this issue was identified against the code base at commit `79023c4f507908bc4e0b94b04ba9cd6b317395a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`InputList`'s member targets `ItemID` (`length 0..256`, no pattern), so the boundary admits the empty string inside `inputList` and it reaches the handler. This crate has ONE rule for such a value, stated in `state::present` and enforced by every other path that reads a `UserID`/`ItemID`: an empty string identifies no user and no item, so a handler reads it as the member not being supplied. `GetRecommendations` applies it to `userId`/`itemId` (`registry::present(input.user_id.as_deref()).unwrap_or_default()`), `GetActionRecommendations` applies it to `userId`, and all three apply it to the ARN members.

`GetPersonalizedRanking`'s ranking loop never applies it to `inputList`'s entries. An entry that is the empty string is inserted into the `reported` dedup set, looked up in the catalog (which holds no entry under an empty id), fails that lookup and is therefore APPENDED to the ranking as a `PredictedItem` with `itemId: Some("")` and `score: Some(0.0)`. So a request whose `inputList` carries an empty string gets back a recommended item that identifies nothing — an entry the catalog never held and that names no item at all — and the response length counts it, while the same value in `userId` or `itemId` on any of the three operations is read as absent and refused by name.

## Evidence

The crate's rule — `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state.rs:925-939`:
```
/// A request member that is present and not empty, as a `&str`.
///
/// `userId` and `itemId` are bounded only by a `@length` that starts at 0, so the
/// boundary validator accepts an empty string for either; an empty string identifies no
/// user and no item, so every handler reads it as the member not being supplied.
...
pub fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}
```
The same rule, pinned by this crate's own suite — `tests/all/empty_string_member_constraints.rs:18-20`:
```
//! * `UserID` / `ItemID` — `length 0..256` and no pattern. The empty string satisfies
//!   every constraint they carry, so it reaches the handler, which reads it as the member
//!   not being supplied (`crate::state::present`) and refuses NAMING THE MEMBER.
```
The shape the rule is about is exactly what an `inputList` entry is — `smithy_query(query="shape",shape="InputList")`:
```
"InputList": {"type": "list", "member": {"target": "ItemID", "length": {"min": 0, "max": 256}}}
```
The path that skips the helper — `src/ops/get_personalized_ranking.rs:315-338`:
```
        for (position, item_id) in input.input_list.iter().enumerate() {
            if !reported.insert(item_id.as_str()) {
                continue;
            }
            match catalog.iter().position(|item| &item.item_id == item_id) {
                ...
                None => {
                    ...
                    appended.push(item_id.clone());
                }
```
and what it emits — `src/ops/get_personalized_ranking.rs:364-370`:
```
        personalized_ranking.extend(appended.into_iter().map(|item_id| PredictedItem {
            item_id: Some(item_id),
            score: Some(0.0),
```
The sibling paths that DO apply the helper to the same shape: `src/ops/get_recommendations.rs:271-272` (`let item_id = registry::present(input.item_id.as_deref()).unwrap_or_default();`) and `src/ops/get_action_recommendations.rs:227` (`let user_id = registry::present(input.user_id.as_deref()).unwrap_or_default();`).

Test coverage: no test sends an empty `inputList` entry — `tests/all/inference.rs:907-924` (`personalized_ranking_is_a_permutation_of_the_input_list`) spans known ids, unknown ids and a repeated id only, and `tests/all/empty_string_member_constraints.rs:395-452` lists every length-only member it exercises (`userId`, `itemId`) and does not include an `inputList` entry.

## Steps to Reproduce

1. Register a `PERSONALIZED_RANKING` campaign through the seam, as `tests/all/empty_string_member_constraints.rs:75-87` does: `registry::register_campaign(engine, Campaign::new(CAMPAIGN_RANKING, "demo", RecipeType::PersonalizedRanking, items(6)))`.
2. POST `/personalize-ranking` with `{"campaignArn": CAMPAIGN_RANKING, "userId": "user-1", "inputList": ["item-1", ""]}`.
3. Observe HTTP 200 whose `personalizedRanking` carries TWO entries, the second being `{"itemId": "", "score": 0.0}` — the empty string was treated as an item id the catalog never held and appended (`get_personalized_ranking.rs:329-336`, `:364-370`).
4. Compare with the same empty string sent in `userId` on the same operation, or in `itemId` on `GetRecommendations`: `state::present` reads it as absent and the handler refuses naming the member (`tests/all/empty_string_member_constraints.rs:455-504`). The one path that reads an `ItemID` without `present` is the `inputList` loop.
