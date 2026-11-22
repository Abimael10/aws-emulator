Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 861999866ea9ac28e07c2c6c1f5e841f1e39e080

# The score/normalize/filter/order pipeline that scoring.rs says it states once is re-implemented inline in GetPersonalizedRanking

_Filed: 2026-11-22T04:07:58Z_
_Operation: GetPersonalizedRanking | Class: maintainability_

> _Note: this issue was identified against the code base at commit `861999866ea9ac28e07c2c6c1f5e841f1e39e080`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::scoring` declares that the ORDER in which a catalog is scored, normalized over the whole catalog, filtered by the request's bound filter, and then ordered by score descending is stated in ONE place — `scoring::ranked` — precisely so the operations cannot drift apart on it. Two of the three handlers do call it (`get_recommendations.rs:364-371`, `get_action_recommendations.rs:272-277`). `GetPersonalizedRanking` re-implements the same four steps inline over the same whole catalog: it maps `Seed::relevance` over every catalog entry, calls `scoring::normalize`, then applies `filter.retains(recorded.facts(..))` per entry, then sorts through `scoring::by_score_desc`. The only thing that genuinely differs is the tie-break key (the caller's `inputList` position instead of the entry's own id) and the fact that the handler reports a subset the caller named.

So the rule the module says lives in one place lives in two, and the copies can drift: a change to WHEN normalization happens relative to filtering (the property `tests/all/score_denominator.rs` asserts for all three operations) has to be made twice, and the second copy is the one no shared helper protects. The fix is to give `ranked` the tie-break seam the comparator already takes (`by_score_desc` is generic over the key) — or to have the handler select its reported entries out of `ranked`'s output — so the composition exists once. Every response and every existing test stays byte-identical: the handler already computes the denominator over the whole catalog before filtering, exactly as `ranked` does, and the appended "not in the training dataset" entries stay in the handler where they belong.

## Evidence

The claim that the order is stated once — `src/state/scoring.rs:5-9`:
```
//! Each stored catalog entry's prediction score is composed here ([`Seed`]), each
//! entry's share of the reported certainty is settled here ([`normalize`]), and the
//! order in which those two and the request's filter are applied to a whole catalog is
//! stated once here as well ([`ranked`]), so the operations that report a catalog of
//! their own choosing cannot drift apart on it.
```
and on the helper itself — `src/state/scoring.rs:309-316`:
```
/// `catalog` scored for `seed`, each entry's share settled over the WHOLE scored
/// catalog, then the request's `filter` applied, then ordered by score descending —
/// the ORDER in which an operation that chooses its own list applies the three rules
/// above, stated once.
///
/// The order is the rule, not an implementation detail of one operation: the
/// denominator behind a reported score is the whole scored catalog, so normalization
/// is settled BEFORE any member of the request that merely selects which entries come
/// back ([`normalize_by`]) ...
```
Copy #1, the helper body — `src/state/scoring.rs:334-353`:
```
    let mut ranked: Vec<(&'a Entry, f64)> = catalog
        .into_iter()
        .map(|entry| {
            let score = seed.relevance(entry.base_score(), entry.entry_id());
            (entry, score)
        })
        .collect();
    normalize_by(&mut ranked, |(_, score)| score);
    ranked.retain(|(entry, _)| {
        filter.is_none_or(|filter| {
            filter.retains(recorded.facts(entry.entry_id(), entry.attributes()))
        })
    });
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        by_score_desc(
            (left.entry_id(), *left_score),
            (right.entry_id(), *right_score),
        )
    });
```
Copy #2, the same four steps inline in the handler — `src/ops/get_personalized_ranking.rs:294-298`:
```
        let mut catalog_shares: Vec<f64> = catalog
            .iter()
            .map(|item| seed.relevance(item.base_score, &item.item_id))
            .collect();
        scoring::normalize(&mut catalog_shares);
```
then the filter step — `src/ops/get_personalized_ranking.rs:322-327`:
```
                    if request_filter.as_ref().is_some_and(|filter| {
                        !filter.retains(recorded.facts(&item.item_id, &item.attributes))
                    }) {
                        continue;
                    }
                    ranked.push((index, catalog_shares[index], position));
```
and the ordering step — `src/ops/get_personalized_ranking.rs:342`:
```
        ranked.sort_by(|left, right| scoring::by_score_desc((left.2, left.1), (right.2, right.1)));
```
The two handlers that do go through the helper — `src/ops/get_recommendations.rs:364-371`:
```
        let candidates = scoring::ranked(
            catalog.iter().filter(|item| {
                !(resolved.required_input == RequiredInput::Item && item.item_id == item_id)
            }),
            &seed,
            request_filter.as_ref(),
            &recorded,
        );
```
and `src/ops/get_action_recommendations.rs:272-277`:
```
        let mut ranked = scoring::ranked(
            catalog.iter(),
            &seed,
            request_filter.as_ref(),
            &recorded,
        );
```
Test coverage that pins both copies and must stay unchanged: `tests/all/score_denominator.rs:3-17` ("the denominator behind it is the resource's WHOLE scored catalog, and no member of the request that merely selects which entries come back — the ids `inputList` names, the `filterArn` applied, `numResults`, a promotion — may move the number reported for an entry"), with GetPersonalizedRanking rows at `score_denominator.rs:265`, `:394` and `:595-607`; `tests/all/shared_score_seed.rs:443-496`; `tests/all/tie_break_order.rs:228` and `:347-367`; `tests/all/repeated_input_list_ids.rs:368-426`.

## Steps to Reproduce

1. Read `src/state/scoring.rs:5-9` and `:309-316`: the crate states that the order of (score, normalize over the whole catalog, apply the request filter, order by score descending) lives in `scoring::ranked` alone.
2. Read `src/state/scoring.rs:334-353`: that order, implemented once in the helper.
3. Read `src/ops/get_personalized_ranking.rs:294-298`, `:322-327` and `:342`: the same four steps implemented a second time inline, differing only in the tie-break key handed to the shared `by_score_desc`.
4. Note that `src/ops/get_recommendations.rs:364-371` and `src/ops/get_action_recommendations.rs:272-277` reach the rule only through the helper, so a change to the helper (for example moving `normalize_by` after `retain`) silently leaves the GetPersonalizedRanking copy behind, while `tests/all/score_denominator.rs` asserts the identical property for all three routes.
