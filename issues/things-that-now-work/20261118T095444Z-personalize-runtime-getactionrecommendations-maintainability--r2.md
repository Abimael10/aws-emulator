Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: aee81de152b8c3b1ca9d360ce8923315d8af9d2c

# The score-then-normalize-then-filter-then-sort pipeline is implemented twice, once per entity type, and the copies have already drifted once

_Filed: 2026-11-18T09:54:44Z_
_Operation: GetActionRecommendations | Class: maintainability_

> _Note: this issue was identified against the code base at commit `aee81de152b8c3b1ca9d360ce8923315d8af9d2c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The ordering rule all three operations state — score the WHOLE catalog, normalize the shares over it, and only THEN let the request's filter and the result count select which entries come back — is implemented as two independent statement sequences, one in `get_recommendations.rs` over `Item` and one in `get_action_recommendations.rs` over `Action`. The two spans are the same four steps in the same order, differing only in the element type and in what follows the sort (a promotion/`reported` split in one, `truncate` in the other).

This is exactly the kind of copy that drifts: it already did. The fixed issue `20261024T002543Z-personalize-runtime-getactionrecommendations-wrong-behavior` ("GetActionRecommendations renormalizes action scores over the truncated list, so an action's score changes with numResults") is one copy having the normalization step in the wrong position while the other copy did not, and it had to be repaired separately per handler. The crate has already hoisted the three smaller rules of this pipeline into one place each (`scoring::Seed`, `scoring::normalize_by`, `scoring::by_score_desc`, each after its own maintainability issue); the ORDER in which they are applied is the one part still written twice.

A single helper taking the entity id, `base_score` and attributes of each candidate plus the bound filter, returning the normalized, filtered, score-descending list, would leave every response byte-identical and every existing test unchanged.

## Evidence

The rule, stated once per handler:
- `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/ops/get_recommendations.rs:46-48`: "The scores are normalized across the whole catalog BEFORE the request's filter, `numResults` and any promotion decide which candidates come back, so one item's score is the same whatever the request asked for."
- `src/ops/get_action_recommendations.rs:28-30`: "The scores are normalized across the whole stored Actions catalog BEFORE the request's filter and `numResults` decide which of them come back, so one action's score is the same whatever the request asked for."

Copy 1 — `src/ops/get_recommendations.rs:342-370`:
```
let mut candidates: Vec<(usize, f64)> = catalog.iter().enumerate()
    .filter(..).map(|(index, item)| (index, seed.relevance(item.base_score, &item.item_id))).collect();
scoring::normalize_by(&mut candidates, |(_, score)| score);
candidates.retain(|(index, _)| request_filter.as_ref().is_none_or(|filter| { .. filter.retains(recorded.facts(..)) }));
candidates.sort_by(|(left, left_score), (right, right_score)| scoring::by_score_desc(..));
```
Copy 2 — `src/ops/get_action_recommendations.rs:262-289`:
```
let mut ranked: Vec<(&Action, f64)> = catalog.iter()
    .map(|action| { let score = seed.relevance(action.base_score, &action.action_id); (action, score) }).collect();
scoring::normalize_by(&mut ranked, |(_, score)| score);
ranked.retain(|(action, _)| request_filter.as_ref().is_none_or(|filter| filter.retains(recorded.facts(&action.action_id, &action.attributes))));
ranked.sort_by(|(left, left_score), (right, right_score)| scoring::by_score_desc(..));
ranked.truncate(num_results);
```
Proof the copies drift: the fixed issue `20261024T002543Z-personalize-runtime-getactionrecommendations-wrong-behavior` is titled "GetActionRecommendations renormalizes action scores over the truncated list, so an action's score changes with numResults" — the same step, wrong in one copy only.

Tests that cover both copies and must stay unchanged: `tests/all/score_denominator.rs` ("the denominator behind it is the resource's WHOLE scored catalog, and no member of the request that merely selects which entries come back — the ids `inputList` names, the `filterArn` applied, `numResults`, a promotion — may move the number reported for an entry") and `tests/all/tie_break_order.rs` ("Every one of these operations reports its list 'sorted in descending order by prediction score', so ONE comparator decides the order for all three").

## Steps to Reproduce

1. Read `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/ops/get_recommendations.rs:342-370` — build scored candidates, `scoring::normalize_by`, `retain` on the bound filter, `sort_by(scoring::by_score_desc)`.
2. Read `src/ops/get_action_recommendations.rs:262-289` — the identical four steps in the identical order over `Action`.
3. Note that the shared parts of the rule already live in `src/state/scoring.rs` (`Seed`, `normalize_by`, `by_score_desc`) but the ORDER is restated per handler, and that the fixed issue `20261024T002543Z-personalize-runtime-getactionrecommendations-wrong-behavior` is one copy having had that order wrong.
4. Confirm `cargo test -p service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime` passes before and after hoisting the sequence into one helper, with no response change.
