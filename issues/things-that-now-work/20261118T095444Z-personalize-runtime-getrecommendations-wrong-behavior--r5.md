Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: aee81de152b8c3b1ca9d360ce8923315d8af9d2c

# A spurious userId still changes which items GetRecommendations returns, through the interaction-reading filter path the crate says it cannot reach

_Filed: 2026-11-18T09:54:44Z_
_Operation: GetRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `aee81de152b8c3b1ca9d360ce8923315d8af9d2c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states, in three places, the rule that the personalization input a resolved resource does NOT use is ignored and cannot change which entries come back. The score path honours it (it goes through `resolved.required_input.personalization(user_id, item_id)`), but the FILTER path does not: `recorded` is built from the request's raw `user_id` at `get_recommendations.rs:302`, one line before the handler asks `resolved.required_input` which input the resource actually uses, and `recorded` is what decides whether a candidate survives the request's filter (`:359-364`) and whether it is taken by a promotion (`:391-394`).

So for a resolved resource that does NOT use `userId` — a `RELATED_ITEMS` campaign (`RequiredInput::Item`) or a domain recommender whose use case needs neither input (`RequiredInput::Nothing`, e.g. "Most viewed", "Best sellers", "Trending now") — a request that also names a filter with a clause over the `Interactions` dataset returns a DIFFERENT item list depending on the spurious `userId` it carries: with no `userId` the clause reads no interaction rows and matches nothing, with `userId=user-1` it reads that user's rows and includes/excludes entries accordingly. The same spurious value also moves which entries a promotion's filter selects.

The response is therefore a function of an input the crate (and the model sentence it quotes) says is "not used", and a resource that is "not personalized at all" stops answering "every caller the same way". The crate already owns the decision of which input a resolved resource uses (`RequiredInput`), so the filter path reads a value the handler has, one line later, already decided to discard.

## Evidence

The rule, stated by the crate:
- `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state.rs:196-200` (`RequiredInput::personalization` doc): "The input a resource does not need is the input it does not use, so it never reaches a score and therefore cannot change which entries come back or in what order: sending it is simply ignored. A resource that needs NEITHER is not personalized at all (a domain use case that asks for no user and no item), so it uses neither and answers every caller the same way."
- `src/ops/get_recommendations.rs:330-335`: "Only the personalization input the resolved resource USES reaches the score: \"USER_PERSONALIZATION - userId required, itemId not used RELATED_ITEMS - itemId required, userId not used\". So a spurious `itemId` on a user-personalization request, or a spurious `userId` on a related-items one, leaves the scored catalog — and therefore the items returned and their order — exactly as it was."
- `src/state/filter.rs:33-37` (module doc): "the INTERACTION dataset — `Interactions.EVENT_TYPE`, `Action_Interactions.EVENT_TYPE` — whose columns are the columns of an interaction RECORDED for that entry by the user the request personalizes for".
- `src/state/ingested.rs:217-219` (`Ingested::interactions_of` doc): "A request that personalizes for no user (an empty `user_id`) recorded none, so a clause over an interaction dataset matches nothing for it."
- Smithy model, `GetRecommendations` operation doc (`smithy_query(query="doc",shape="GetRecommendations")`): "USER_PERSONALIZATION - userId required, itemId not used RELATED_ITEMS - itemId required, userId not used".

The code that breaks it:
- `src/ops/get_recommendations.rs:302`: `let recorded = ingested.interactions_of(user_id);` — the raw request value from `:271`, NOT `resolved.required_input.personalization(...)`, which the very next step uses at `:336`: `let personalization = resolved.required_input.personalization(user_id, item_id);`
- `src/ops/get_recommendations.rs:359-364`: `candidates.retain(|(index, _)| { request_filter.as_ref().is_none_or(|filter| { let item = &catalog[*index]; filter.retains(recorded.facts(&item.item_id, &item.attributes)) }) });` — which entries come back depends on `recorded`.
- `src/ops/get_recommendations.rs:391-394`: the promotion's share is likewise chosen with `promotion.filter.retains(recorded.facts(&item.item_id, &item.attributes))`.

Test coverage: none. `tests/all/unused_personalization_input.rs` is the suite that pins the rule ("a resolved resource reads exactly the ONE personalization input it needs, so the other one is ignored — sending it, or changing it, leaves the returned items, their order and their scores untouched"), and a grep of that file for `filter|filterArn|Interactions|register_filter` returns no matches, so no row of it names a filter. `tests/all/event_propagation.rs`, which exercises interaction-reading clauses, contains no `RELATED_ITEMS` campaign and no `Recommender::new` (grep for `RelatedItems|RELATED_ITEMS|use_case|Recommender::new`: no matches), so the combination is untested from both sides.

## Steps to Reproduce

1. Register a `RELATED_ITEMS` campaign fitted to a dataset group: `Campaign::new(arn, "demo", RecipeType::RelatedItems, items(8)).fitted_to(group_arn, fitted_at)` through `state::register_campaign`.
2. Through Amazon Personalize Events on the same engine, record an interaction for `userId=user-1` on `item-3` with `EVENT_TYPE=purchase` in that dataset group (as `tests/all/event_propagation.rs` does).
3. Register a filter: `state::register_filter(engine, filter_arn, "EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN (\"purchase\")")`.
4. Call `GetRecommendations` with that `campaignArn`, `itemId=item-1`, `filterArn`, `numResults=10` and NO `userId`. Observed: `item-3` is present (the clause reads no interaction rows).
5. Repeat the identical call with `userId=user-1` added. Observed: `item-3` is now absent and the rest of the list shifts up. Expected, per `state.rs:196-200` and `get_recommendations.rs:330-335`: the two responses are identical, because `userId` is not an input a `RELATED_ITEMS` resource uses.
6. Repeat steps 1-5 with a recommender whose use case needs neither input (`Recommender::new(arn, "most-viewed", "Most viewed", items(8)).fitted_to(..)`) and `recommenderArn`: the response again differs with and without `userId`, although the crate says such a resource "answers every caller the same way".
