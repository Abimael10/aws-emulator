Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: d46b9888971fbeda7693c2d785f86d7cceeb35ca

# An ingested event property is dropped even when the interaction row derives no column of that name, so a filter clause over it can never match

_Filed: 2026-11-21T14:46:34Z_
_Operation: GetPersonalizedRanking | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d46b9888971fbeda7693c2d785f86d7cceeb35ca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::ingested::merge_properties` drops every caller-supplied `properties` entry whose name appears in the single shared `DERIVED_COLUMNS` list, regardless of which row kind is being built and regardless of whether that row actually derived the column. Its own doc states the premise for the drop: the dropped names are "the columns the row takes from the stored record itself", so "a property a caller happens to name the same cannot reshape it". For three reachable cases the row takes no such column at all, so nothing is being protected and the caller's value is simply lost:

* an ITEM interaction row (`event_columns`) never derives `ACTION_ID`, yet a property named `ACTION_ID` is dropped;
* an ITEM interaction row derives `EVENT_VALUE` only when the event carried `eventValue` (`if let Some(event_value) = event.event_value`), yet a property named `EVENT_VALUE` is dropped even when the event carried none;
* an ACTION interaction row (`action_interaction_columns`) never derives `ITEM_ID` and never derives `EVENT_VALUE`, yet properties named either are dropped.

Because the column ends up absent from the row, `filter::holds_for` returns `false` for it under every predicate, so a filter clause over that column can never hold: an `INCLUDE` expression keeps nothing and an `EXCLUDE` expression removes nothing, and the ingested value is invisible to inference even though the ingestion plane stored it. The fix the premise already states is to drop only the names the row actually derived (the keys `event_columns` / `action_interaction_columns` inserted), not a fixed list shared by both row kinds.

This is distinct from the already-open issue on the same list (`20261119T030916Z-personalize-runtime-getactionrecommendations-wrong-behavior--r2.md`), which is the inverse defect — `RECOMMENDATION_ID` is MISSING from the list, so a property of that name DOES reshape a column the row derives. That one says the list is too short; this one says it is applied too broadly. Also note the names are not screened upstream: `service-com-amazonaws-personalizeevents-amazonpersonalizeevents/src/state.rs:94` reserves only `userId, sessionId, eventType, timestamp, recommendationId, impression`, so `ITEM_ID`, `ACTION_ID` and `EVENT_VALUE` properties are accepted by `PutEvents`/`PutActionInteractions` and reach this code.

## Evidence

The premise, in this crate — services/.../src/state/ingested.rs:85-95:
```
/// The columns an interaction row takes from the stored record itself, which a property
/// of the same name therefore cannot reshape.
const DERIVED_COLUMNS: [&str; 7] = [
    USER_ID_COLUMN,
    ITEM_ID_COLUMN,
    ACTION_ID_COLUMN,
    TIMESTAMP_COLUMN,
    EVENT_TYPE_COLUMN,
    EVENT_VALUE_COLUMN,
    IMPRESSION_COLUMN,
];
```
and services/.../src/state/ingested.rs:436-453:
```
/// Merge a record's `properties` into `columns`, DROPPING any whose name is one of the
/// columns the row takes from the record itself ([`DERIVED_COLUMNS`]): what identifies a
/// row and when it happened come from the record, so a property a caller happens to name
/// the same cannot reshape it.
fn merge_properties(...) {
    for (name, value) in properties(raw) {
        if DERIVED_COLUMNS
            .iter()
            .any(|column| column.eq_ignore_ascii_case(&name))
        { continue; }
        columns.insert(name, value);
    }
}
```
The code under it, which does NOT take those columns from the record — services/.../src/state/ingested.rs:373-399 (`event_columns`, an ITEM interaction row) inserts only `USER_ID_COLUMN`, `ITEM_ID_COLUMN`, `TIMESTAMP_COLUMN`, `EVENT_TYPE_COLUMN`, `IMPRESSION_COLUMN` (only `if !event.impression.is_empty()`), `RECOMMENDATION_ID_COLUMN`, and `EVENT_VALUE_COLUMN` only conditionally:
```
    if let Some(event_value) = event.event_value {
        columns.insert(EVENT_VALUE_COLUMN.to_string(), event_value.to_string());
    }
```
— there is no `ACTION_ID_COLUMN` insert anywhere in it. And services/.../src/state/ingested.rs:404-434 (`action_interaction_columns`) inserts `USER_ID_COLUMN`, `ACTION_ID_COLUMN`, `TIMESTAMP_COLUMN`, `EVENT_TYPE_COLUMN`, `IMPRESSION_COLUMN`, `RECOMMENDATION_ID_COLUMN` — no `ITEM_ID_COLUMN` and no `EVENT_VALUE_COLUMN`.

Why the loss is observable — services/.../src/state/filter.rs:1126-1129:
```
fn holds_for(clause: &BoundClause, columns: &BTreeMap<String, String>) -> bool {
    let Some(stored) = columns.get(&clause.column) else {
        return false;
    };
```
and filter.rs:70-72: "An entry with no value for an entity column, and one no interaction of this user mentions at all, is therefore kept by an `EXCLUDE` expression and dropped by an `INCLUDE` one".

The handler that reaches it — services/.../src/ops/get_personalized_ranking.rs:260-266:
```
        let ingested = registry::ingested::live_view(
            &*self.engine,
            campaign.dataset_group_arn.as_deref(),
            campaign.fitted_at,
        );
        let catalog = ingested.items(&campaign.items);
        let recorded = ingested.interactions_of(&input.user_id);
```
and lines 322-324, where the bound filter decides on those facts:
```
                    if request_filter.as_ref().is_some_and(|filter| {
                        !filter.retains(recorded.facts(&item.item_id, &item.attributes))
                    }) {
```
`GetRecommendations` (get_recommendations.rs:299-313) and `GetActionRecommendations` (get_action_recommendations.rs:241-247) read the same helper.

Upstream does not screen these names — services/service-com-amazonaws-personalizeevents-amazonpersonalizeevents/src/state.rs:94-101:
```
pub const RESERVED_PROPERTY_KEYS: [&str; 6] = [
    "userId", "sessionId", "eventType", "timestamp", "recommendationId", "impression",
];
```

Test coverage: tests/all/event_propagation.rs:83 pins the working case ("interaction row through the event's own `properties`", the `DEVICE` column at line 333 / 496) and line 281 pins a clause over the DERIVED `EVENT_VALUE` column; no test in tests/all/ covers a property whose name matches a column the row does not derive, which is why the over-broad drop is unpinned.

## Steps to Reproduce

1. Register a filter whose expression is `INCLUDE ItemID WHERE Interactions.ACTION_ID = "a1"` and a PERSONALIZED_RANKING campaign fitted to a dataset group (`runtime::Campaign::fitted_to`), with `item-1` in its Items catalog.
2. Record an item interaction for `user-1` on `item-1` through Personalize Events `PutEvents`, with `properties` = `{"ACTION_ID":"a1"}` (accepted: `ACTION_ID` is not in `RESERVED_PROPERTY_KEYS`).
3. Call `GetPersonalizedRanking` with that `campaignArn`, `userId: user-1`, `inputList: ["item-1"]` and the filter's ARN.
4. Observe `personalizedRanking` is empty: `merge_properties` dropped the `ACTION_ID` property, the interaction row has no `ACTION_ID` column, `holds_for` returns false and the INCLUDE expression keeps nothing — even though the row derives no `ACTION_ID` of its own for the property to have reshaped.
5. Repeat with an event carrying no `eventValue` and `properties` = `{"EVENT_VALUE":"42"}` against `INCLUDE ItemID WHERE Interactions.EVENT_VALUE = 42`, and with `PutActionInteractions` + `properties` = `{"ITEM_ID":"item-1"}` against `INCLUDE ActionID WHERE Action_Interactions.ITEM_ID = "item-1"` on `GetActionRecommendations`: both lists come back empty for the same reason.
