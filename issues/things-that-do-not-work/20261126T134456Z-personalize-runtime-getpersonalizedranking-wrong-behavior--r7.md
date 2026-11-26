Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 026158241d903674989907608971730578a493f7

# An inputList id the catalog does not hold is still decided by the filter's interaction clauses, which the handler's own rule says cannot happen

_Filed: 2026-11-26T13:44:56Z_
_Operation: GetPersonalizedRanking | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `026158241d903674989907608971730578a493f7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `GetPersonalizedRanking` handler states, in the comment directly above the code that implements it, that an `inputList` entry the catalog does not hold is decided by the request's filter on an EMPTY attribute set, and draws two absolute conclusions from that: "an INCLUDE filter cannot keep it, an EXCLUDE filter cannot remove it."

The code under that comment does not supply an empty fact set. It calls `recorded.facts(item_id, &no_attributes)`, which pairs the empty attribute map with the interaction rows the request's user recorded for that id (`RecordedInteractions::facts` -> `EntryFacts::new(attributes, self.of(entry_id))`). The crate's own filter evaluator reads exactly those rows for any clause over an interaction dataset (`ClauseSource::Interactions` in `evaluate`), independently of whether the entry has stored attributes. So an expression whose clause reads `Interactions.<COLUMN>` CAN hold for an un-catalogued entry: an `EXCLUDE` expression removes it (it is never appended) and an `INCLUDE` expression keeps it — the two outcomes the comment says are impossible.

The two in-crate texts disagree about what decides such an entry: the handler comment says only the (empty) attribute set decides it, while `filter.rs`'s module doc says an interaction clause "holds when ANY of that user's interactions with the entry satisfies it". Exactly one of the comment and the code can be right; either the comment overstates the rule, or the appended path should decide on facts with no interaction rows the way it already decides on no attributes.

## Evidence

The claim, in the handler, immediately above the code that breaks it:

services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/ops/get_personalized_ranking.rs:308-311
```
// 5. Report the entries `inputList` names, and collect the ones the catalog
//    does not hold. An entry with no catalog record carries no stored
//    attributes, so the request's filter decides on an empty attribute set: an
//    INCLUDE filter cannot keep it, an EXCLUDE filter cannot remove it.
```

The code, which hands the filter the user's recorded interactions as well:

services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/ops/get_personalized_ranking.rs:338-341
```
                    if request_filter.as_ref().is_some_and(|filter| {
                        !filter.retains(recorded.facts(item_id, &no_attributes))
                    }) {
                        continue;
```

What `facts` actually builds — attributes AND interaction rows:

services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state/ingested.rs:154-160
```
    pub fn facts<'a>(
        &'a self,
        entry_id: &str,
        attributes: &'a BTreeMap<String, String>,
    ) -> EntryFacts<'a> {
        EntryFacts::new(attributes, self.of(entry_id))
    }
```

The sibling rule the evaluator applies to those rows, regardless of the entry's attributes:

services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state/filter.rs:1102-1109
```
fn evaluate(clause: &BoundClause, facts: EntryFacts<'_>) -> bool {
    match clause.source {
        ClauseSource::Entity => holds_for(clause, facts.attributes),
        ClauseSource::Interactions => facts
            .interactions
            .iter()
            .any(|recorded| holds_for(clause, recorded)),
    }
}
```

and filter.rs:33-37 (module doc): "the INTERACTION dataset — `Interactions.EVENT_TYPE`, `Action_Interactions.EVENT_TYPE` — whose columns are the columns of an interaction RECORDED for that entry by the user the request personalizes for ... A clause over it holds when ANY of that user's interactions with the entry satisfies it, so an entry the user never interacted with satisfies no such clause."

Test coverage: the rule is pinned only for ENTITY-dataset clauses. `tests/all/absent_attribute_columns.rs:320` (`a_ranked_id_the_catalog_never_held_is_decided_by_no_filter_clause`) builds its expressions from `Items.GENRE` / `Items.YEAR` rows only (`item_rows()`, same file, lines 240-313), so no test exercises an `Interactions.<COLUMN>` clause against an un-catalogued `inputList` id, and neither the comment's claim nor the code's behaviour is pinned for that case.

## Steps to Reproduce

1. Read services/.../src/ops/get_personalized_ranking.rs:308-311 — the handler states that for an `inputList` id the catalog does not hold, "an INCLUDE filter cannot keep it, an EXCLUDE filter cannot remove it", because the filter "decides on an empty attribute set".
2. Read lines 338-341 of the same file: the filter is instead handed `recorded.facts(item_id, &no_attributes)`.
3. Follow `facts` to src/state/ingested.rs:154-160: it returns `EntryFacts::new(attributes, self.of(entry_id))` — the empty attribute map PLUS every interaction row this request's user recorded for `item_id`.
4. Follow `BoundFilter::retains` -> `matches` -> `evaluate` at src/state/filter.rs:1102-1109: a clause whose `source` is `ClauseSource::Interactions` is evaluated over those rows and ignores the attribute map entirely, so it can hold for an entry with no catalog record.
5. Conclude: with a filter whose expression is over the Interactions dataset, an un-catalogued `inputList` id is dropped by an EXCLUDE expression (never appended) or kept by an INCLUDE one — both outcomes the handler's own comment declares impossible. Either the comment or the code is wrong; the crate contradicts itself about which.
