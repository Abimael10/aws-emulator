Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 8148416be87f6b2e89717c854691e87b6c3cc940

# UpdateRecord counts repeated feature names against its documented 100-feature ceiling, though the crate reads the same list as one value per name

_Filed: 2026-11-24T08:41:19Z_
_Operation: UpdateRecord | Class: validation_

> _Note: this issue was identified against the code base at commit `8148416be87f6b2e89717c854691e87b6c3cc940`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

UpdateRecord enforces the operation's documented ceiling ("You can update up to 100 features per call") on the RAW `Features` list (`input.features.len()`), three lines before it reads that very list through the crate's one-value-per-feature-name reading (`crate::state::held_feature_values`). `Features` is modelled with no `uniqueItems`, and this crate states repeatedly — in the helper's own doc, in this handler's own comment, and in its tests — that a name repeated in one write names ONE feature whose value is the last one named. So a call posting, say, 51 distinct feature names of which 51 are repeated (102 list entries) updates 51 features but is refused 400, while the per-record bound of the store itself (`MAX_FEATURES_PER_RECORD`) is applied to the deduplicated list in `insert_record`. The two feature counts of this crate therefore count differently, and the one that reports a documented API limit to the client counts something the crate says is not a feature count.

Second symptom of the same class in the same handler: the group's RECORD-IDENTIFIER feature is read with a hand-rolled `find(...)` plus `posted.value_as_string.as_deref() != Some(identifier)`, so a posted identifier feature whose value sits in `ValueAsStringList` (or in neither value member) is refused with this operation's "must carry the RecordIdentifierValueAsString of the record being updated" wording rather than with `crate::state::identifier_not_scalar`, which the state module declares is "The refusal every write of this data plane answers" for exactly that case. The EVENT-TIME feature two statements earlier in the same handler is read the shared way (`held_scalar_feature` + `event_time_not_scalar`), whose doc says it is "stated once here, so a whole-record write and a partial update refuse the same posted value the same way" — so the handler applies the crate's single reading to one of the two features the group's definition fixes and not to the other.

## Evidence

Smithy, `smithy_query(query="doc", shape="UpdateRecord")`: "Updates one or more feature values for an existing record in the specified feature group. Features that you do not include in the request remain unchanged. You can update up to 100 features per call."

The ceiling, read off the raw list — services/.../src/ops/update_record.rs:277-283:
```
        if input.features.len() > MAX_FEATURES_PER_UPDATE {
            return Err(validation(format!(
                "Features: at most {MAX_FEATURES_PER_UPDATE} features can be updated per \
                 call, but {} were supplied",
                input.features.len()
            )));
```
with services/.../src/ops/update_record.rs:513-518: "The most feature values one `UpdateRecord` call may carry. … \"You can update up to 100 features per call\", from this operation's own documentation … A documented API limit, not an emulator capacity bound."

The same list, read as one value per name thirteen lines later — services/.../src/ops/update_record.rs:291-296:
```
        // One value per feature name ([`crate::state::held_feature_values`]): `Features`
        // is a list with no `uniqueItems`, so a call may name a feature more than once,
        // and the value it then updates that feature to is the LAST it names …
        let posted_features = crate::state::held_feature_values(&input.features);
```
services/.../src/state.rs:419-429 (the helper's own doc): "The feature values a record HOLDS, out of the list one write posts: exactly ONE value per feature name. … The list the wire carries is modelled with no `uniqueItems`, though, so a write may legitimately name the same feature more than once; the value the record then holds for that feature is the LAST the write names".
The crate's OTHER feature count, applied to the HELD list — services/.../src/state.rs:181-183:
```
    pub fn insert_record(&mut self, record: StoredRecord) -> bool {
        if record.features.len() > MAX_FEATURES_PER_RECORD {
            return false;
```
(`record.features` comes from `held` in `prepare_record`, services/.../src/state.rs:629-633.)

Second symptom — services/.../src/ops/update_record.rs:366-377:
```
        if let Some(posted) = posted_features
            .iter()
            .copied()
            .find(|feature| feature.feature_name == identifier_feature)
        {
            if posted.value_as_string.as_deref() != Some(identifier.as_str()) {
                return Err(validation(format!(
                    "Features: the record identifier feature '{identifier_feature}' must carry \
                     the RecordIdentifierValueAsString of the record being updated \
                     ('{identifier}')"
```
against the rule the crate states once — services/.../src/state.rs:496-506: "The refusal every write of this data plane answers for the group's RECORD-IDENTIFIER feature carrying no value this plane can read as one value ([`ScalarFeature::Unreadable`]) — a value in `ValueAsStringList`, or in neither value member …" / `pub fn identifier_not_scalar(...)`; and the sibling the same handler DOES use — services/.../src/state.rs:508-512: "The refusal every write of this data plane answers for the group's EVENT-TIME feature carrying no value this plane can read as one value ([`ScalarFeature::Unreadable`]): stated once here, so a whole-record write and a partial update refuse the same posted value the same way", called at services/.../src/ops/update_record.rs:306-314.

Test coverage: tests/all/update_record_semantics.rs:587-626 (`an_update_past_the_documented_feature_ceiling_is_refused`) builds its 1/100/101/250-feature rows from DISTINCT names (`feature(&format!("feature_{index}"), "v")`), so no row pins the count of a repeated name; tests/all/update_record_partial_update.rs:1121+ (`an_update_that_names_a_feature_twice_settles_on_the_last_value_named`) and :1376+ (`the_last_identifier_value_an_update_names_is_the_one_it_is_checked_against`) pin the one-value-per-name rule with two- and three-entry lists, and the 400 row of the latter asserts only the error CODE, not the message. tests/all/rejection_statements.rs:488-494 + :523 (`a_key_feature_without_one_readable_value_is_refused_by_every_whole_record_write`) pins `identifier_not_scalar` for PutRecord and BatchWriteRecord only. Counting the held values instead of the raw list, and routing the unreadable identifier through the shared refusal, leaves all of these passing.

## Steps to Reproduce

1. Create a feature group through the SageMaker control plane with an OnlineStore, `RecordIdentifierFeatureName=customer_id` and `EventTimeFeatureName=event_time` (as tests/all/update_record_partial_update.rs does).
2. `PUT /FeatureGroup/customers` (PutRecord) with a record carrying `customer_id=C1` and an `event_time`.
3. `POST /FeatureGroup/customers/Record` (UpdateRecord) with `RecordIdentifierValueAsString=C1` and a `Features` list of 102 entries that name only 51 distinct features (each name listed twice, e.g. `feature_0..feature_50` repeated): the handler reaches update_record.rs:277 with `input.features.len() == 102` and answers 400 ValidationError "Features: at most 100 features can be updated per call, but 102 were supplied", although the call updates 51 features — while the identical update spelled with 51 entries is served 200, and update_record.rs:296 would have reduced the 102 entries to 51 values.
4. Second symptom: send the same UpdateRecord with `Features` = `[{"FeatureName":"customer_id","ValueAsStringList":["C1"]}]`. The handler refuses at update_record.rs:371 with "Features: the record identifier feature 'customer_id' must carry the RecordIdentifierValueAsString of the record being updated ('C1')", whereas PutRecord/BatchWriteRecord refuse the same posted value with the crate's single statement "the record identifier feature 'customer_id' must carry its value in ValueAsString" (state.rs:501-506), and this same handler refuses the event-time feature posted that way with the shared `event_time_not_scalar`.
