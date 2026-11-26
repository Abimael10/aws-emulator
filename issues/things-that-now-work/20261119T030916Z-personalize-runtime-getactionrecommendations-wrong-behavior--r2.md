Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 79023c4f507908bc4e0b94b04ba9cd6b317395a7

# A caller-supplied properties key can reshape the RECOMMENDATION_ID interaction column, which the crate states a property cannot do

_Filed: 2026-11-19T03:09:16Z_
_Operation: GetActionRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `79023c4f507908bc4e0b94b04ba9cd6b317395a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`state/ingested.rs` builds the interaction rows a filter clause over an interaction dataset reads. It declares eight column names, inserts seven of them AND `RECOMMENDATION_ID` from the stored record, and then merges the record's free-form `properties` over the result — dropping any property whose name is a column the row took from the record. That drop list, `DERIVED_COLUMNS`, holds only SEVEN entries: `RECOMMENDATION_ID` is missing from it, although `RECOMMENDATION_ID_COLUMN` is taken from the record exactly like the other seven (`event.recommendation_id` / `interaction.recommendation_id`).

So a property named `RECOMMENDATION_ID` survives the screen and overwrites (or invents) the column, which is the opposite of the rule the module states. The value is observable: a filter clause over `Action_Interactions.RECOMMENDATION_ID` (for `GetActionRecommendations`) or `Interactions.RECOMMENDATION_ID` (for `GetRecommendations`/`GetPersonalizedRanking`) is evaluated against the caller's property instead of the recommendation id the ingested record actually carries, so an entry is excluded (or kept) on a value the event never recorded. The ingestion side does not block the key either: the Personalize Events crate reserves `recommendationId` case-insensitively, which does not match the `RECOMMENDATION_ID` spelling a schema field — and therefore a filter clause — uses.

## Evidence

The rule and the short list, `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state/ingested.rs:82-92`:
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
The column it omits is one of those taken from the record — `src/state/ingested.rs:79-80`:
```
/// The interaction column carrying `Event$recommendationId`.
pub const RECOMMENDATION_ID_COLUMN: &str = "RECOMMENDATION_ID";
```
Taken from the record, then overwritten by a property — `src/state/ingested.rs:423-430` (`action_interaction_columns`, read only by `GetActionRecommendations` through `action_interactions_of`):
```
    if let Some(recommendation_id) = &interaction.recommendation_id {
        columns.insert(
            RECOMMENDATION_ID_COLUMN.to_string(),
            recommendation_id.clone(),
        );
    }
    merge_properties(&mut columns, interaction.properties.as_ref());
```
The identical pattern for item interactions at `src/state/ingested.rs:389-395`. And the screen that lets it through — `src/state/ingested.rs:433-451`:
```
/// Merge a record's `properties` into `columns`, DROPPING any whose name is one of the
/// columns the row takes from the record itself ([`DERIVED_COLUMNS`]): what identifies a
/// row and when it happened come from the record, so a property a caller happens to name
/// the same cannot reshape it. Names are compared case-insensitively, ...
fn merge_properties(...) {
    for (name, value) in properties(raw) {
        if DERIVED_COLUMNS.iter().any(|column| column.eq_ignore_ascii_case(&name)) {
            continue;
        }
        columns.insert(name, value);
```
Nothing upstream blocks the key — `services/service-com-amazonaws-personalizeevents-amazonpersonalizeevents/src/state.rs:94-101` reserves `"recommendationId"` and compares with `reserved.eq_ignore_ascii_case(key)` (`:123-126`), which does not match `RECOMMENDATION_ID`.

Where the spoofed column is read: `src/state/filter.rs:1126-1129` — `fn holds_for(clause: &BoundClause, columns: &BTreeMap<String, String>) -> bool { let Some(stored) = columns.get(&clause.column) else { return false; };` — reached for an interaction-dataset clause through `src/state/filter.rs:1105-1108` and `src/ops/get_action_recommendations.rs:247,278-282`.

Test coverage: no test in `tests/` mentions `RECOMMENDATION_ID` at all (grep over `tests/` finds only `recommendationId` assertions about the response member), so the omission is uncovered.

## Steps to Reproduce

1. Mount Amazon Personalize Events and this service on one engine (as `tests/all/event_propagation.rs` does), with an event tracker for dataset group `G`.
2. Register a `PERSONALIZED_ACTIONS` campaign fitted to `G`: `Campaign::new_actions(CAMPAIGN, "actions", actions(6)).fitted_to(G, t)`, and a filter `EXCLUDE ActionID WHERE Action_Interactions.RECOMMENDATION_ID = $rid` through `registry::register_filter`.
3. Call `PutActionInteractions` for `userId=user-1`, `actionId=action-1`, with NO `recommendationId` and `properties = {"RECOMMENDATION_ID": "RID-spoof"}` — accepted, because the events crate reserves only `recommendationId` (`personalizeevents/src/state.rs:94-101`).
4. Call `GetActionRecommendations` with that `campaignArn`, `userId=user-1`, the filter ARN and `filterValues = {"rid": "\"RID-spoof\""}`.
5. Observe `action-1` excluded from `actionList`: `merge_properties` wrote the caller's property into the `RECOMMENDATION_ID` column (`ingested.rs:429,443-449`) although `ingested.rs:82-84` states a property of the same name cannot reshape a column the row takes from the record.
6. Repeat with a record whose real `recommendationId` is `RID-real` plus `properties = {"RECOMMENDATION_ID": "other"}`: filtering on `RID-real` no longer excludes the action, because the derived value was overwritten.
