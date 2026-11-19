Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 888a7dd41752985eb1de1aec0bdb6cbbaaae5598

# An entry only the ingestion plane holds is given a popularity of zero, because the fitted-at cutoff is applied to entries the fitted catalog never counted

_Filed: 2026-11-18T20:32:19Z_
_Operation: GetPersonalizedRanking | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `888a7dd41752985eb1de1aec0bdb6cbbaaae5598`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`state/ingested.rs::merge` is the one place that builds the catalog an inference call recommends from, and all three handlers reach it (`GetPersonalizedRanking` through `registry::ingested::live_view(..)` + `ingested.items(&campaign.items)`). It splits the live interaction rows at the resource's `fitted_at` instant, and the module states exactly one reason for that split: the interactions the fitted catalog ALREADY counted must not be counted twice.

That reason cannot apply to an entry the fitted catalog does not hold at all: there is no stored popularity for it, so none of its interactions were ever counted. The code nevertheless applies the same cutoff to those added entries, giving each one a popularity of only its post-`fitted_at` interaction count (`ingested.rs:551`, `:556-561`). So an item (or action) a client streamed interactions for BEFORE the campaign/recommender was created is recommendable but carries `base_score = 0`, which `scoring::Seed::relevance` turns into a relevance of `0.0` (`base_score.max(0.0) * weight`), i.e. a reported score of 0 and a place at the very end of every response — the same answer as an entry nobody ever interacted with.

The same count is also the sort key that decides WHICH added entries survive `MAX_LIVE_CATALOG_ENTRIES`, although both that constant's doc and `merge`'s own doc say the entries kept are "the most interacted-with" ones: an entry with 50 pre-`fitted_at` interactions sorts below one with a single post-`fitted_at` interaction (`ingested.rs:553-555`) and is the first to be dropped when the bound bites, so the bounded catalog is NOT "the head of the same ordering a full one would report".

## Evidence

The rule the crate states, `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state/ingested.rs:32-34`:
```
//! * each entry's popularity: the fitted popularity plus the interactions recorded AFTER
//!   the catalog was fitted. The interactions the fitted catalog already counted are not
//!   counted twice, which is the only reason the fitted instant is kept.
```
and `src/state/ingested.rs:517-519` (inside `merge`):
```
    // The interactions recorded since the catalog was fitted, per entry: the ones it
    // already counted are part of the stored popularity, so counting them again would
    // weigh one interaction twice.
```
The code that applies that cutoff to entries the fitted catalog never held, `src/state/ingested.rs:544-562`:
```
    let mut added: Vec<(&str, usize)> = records.keys().map(String::as_str)
        .chain(rows.iter().map(|row| row.entity_id.as_str()))
        .filter(|id| !held.contains(*id))
        .collect::<BTreeSet<&str>>().into_iter()
        .map(|id| (id, since_fitted.get(id).copied().unwrap_or_default()))
        .collect();
    added.sort_by(|(left_id, left), (right_id, right)| right.cmp(left).then_with(|| left_id.cmp(right_id)));
    for (id, recorded) in added.into_iter().take(MAX_LIVE_CATALOG_ENTRIES) {
        merged.push((id.to_string(), recorded as f64, records.get(id).cloned().unwrap_or_default()));
```
`held` is filled only from the FITTED catalog (`:529-537`), so every `added` entry is one the fitted catalog does not hold and whose interactions it therefore never counted — the `since_fitted` filter at `:521-523` (`row.received_at > fitted_at`) discards them for nothing.

The second side of the same code, against the ordering the crate promises — `src/state/ingested.rs:98-101`:
```
/// The most entries ingestion may ADD to a resource's fitted catalog for one call: the
/// most interacted-with ones, so a bounded catalog is the head of the same ordering a
/// full one would report.
pub const MAX_LIVE_CATALOG_ENTRIES: usize = 1_000;
```
and `src/state/ingested.rs:507-510`:
```
/// `stored` is the fitted catalog in its stored order, which is kept: the entries
/// ingestion ADDS are appended, most interacted-with first and bounded by
/// [`MAX_LIVE_CATALOG_ENTRIES`], ...
```
The sort key is `since_fitted`, not how interacted-with the entry is.

Where the zero popularity becomes a zero score: `src/state/scoring.rs:101-104` — `fn relevance(base_score: f64, parts: &[&str]) -> f64 { let weight = ..; base_score.max(0.0) * weight }` — reached from `src/ops/get_personalized_ranking.rs:294-298` over the catalog built at `:260-265`.

Test coverage: `tests/all/event_propagation.rs` only ever records interactions AFTER the campaigns are registered (`seeded()` then `record_seed_events(&emulator)`, e.g. `:786-851`), and `recorded_interactions_raise_an_item_s_score` (`:853-884`) boosts an item the FITTED catalog already holds, so no row covers an ingestion-only entry whose interactions predate `fitted_at`.

## Steps to Reproduce

1. Mount Amazon Personalize Events and this service on one engine (`Emulator::with_ingestion`, as `tests/all/event_propagation.rs` does), with an event tracker and an ITEMS dataset in dataset group `arn:aws:personalize:us-east-1:000000000000:dataset-group/propagation`.
2. Through `PutEvents`, record 50 interactions for `userId=user-1` on `item-new` — an id the campaign's fitted catalog does not hold — and note the instant T0 at which they were ingested.
3. At an instant T1 > T0, register the ranking campaign: `registry::register_campaign(engine, Campaign::new(arn, "demo", RecipeType::PersonalizedRanking, items(8)).fitted_to(GROUP, T1))`.
4. Call `GetPersonalizedRanking` with that `campaignArn`, `userId=user-1` and `inputList` holding the 8 fitted ids plus `item-new`.
5. Observe that `item-new` comes back with `score` 0.0 and last in `personalizedRanking`: `merge` added it with `base_score = since_fitted.get("item-new") = 0` because all 50 rows have `received_at <= T1`, even though the fitted catalog holds no entry for `item-new` and therefore never counted any of them — the only reason `ingested.rs:32-34` gives for the cutoff.
6. Repeat with one extra interaction recorded AFTER T1: the same item now carries a non-zero popularity, so the 50 earlier interactions are the only difference between a zero score and a non-zero one.
7. For the ordering half: ingest more than `MAX_LIVE_CATALOG_ENTRIES` (1000) ids that the fitted catalog does not hold, all with post-`fitted_at` interactions, plus `item-new` with its 50 pre-`fitted_at` ones; `item-new` sorts last in `added` (`ingested.rs:553-555`) and is dropped, although `ingested.rs:98-101` says the kept entries are "the most interacted-with ones".
