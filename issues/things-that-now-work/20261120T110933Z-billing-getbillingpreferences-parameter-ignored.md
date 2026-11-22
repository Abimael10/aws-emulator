Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 30d927f99e8e54f81e8ebf641df74b5be52ff769

# A filters entry that carries value but omits the optional name has its values dropped, so the read answers every key of the feature

_Filed: 2026-11-20T11:09:33Z_
_Operation: GetBillingPreferences | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `30d927f99e8e54f81e8ebf641df74b5be52ff769`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`BillingFeatureFilter$name` is NOT `@required` in the model and has exactly one legal value (`PREFERENCE_KEY`), so a request may legally send `filters: [{ "value": ["<key>"] }]` with no `name` at all. `GetBillingPreferences` accepts such a request (its `validate_filters` pass only refuses a name that is PRESENT and unequal to `PREFERENCE_KEY`), and then silently DROPS the values: `filtered_keys` skips every filter whose `name` is not exactly `Some("PREFERENCE_KEY")`, so `selected` stays `None`, `matches_filters` selects everything, and the answer is every preference key stored under the feature — a strictly larger set than the caller asked for, reported with HTTP 200 and no hint that the filter was ignored.

The handler's own analogy for this ("a filter naming no member … selects everything — as `ListBillingViews` treats an omitted filter") does not hold: `ListBillingViews` widens only for a filter that NAMES NOTHING (an absent or empty `arns` list), whereas here the caller did name the values to match and only left out a member that can carry one value. The crate's own test states the rule the code then breaks for this input shape: `the_preference_key_filter_selects_only_the_keys_it_names` — with `name` omitted the read selects keys it does not name.

## Evidence

The model makes `name` optional with a single legal value, and documents the filter as NARROWING the result — `smithy_query(query="shape", shape="BillingFeatureFilter")`:
```
"BillingFeatureFilter": {"type": "structure", "members": {
  "name": {"target": "BillingFeatureFilterName", "type": "enum", "doc": "<p>The filter name. Currently the only supported value is <code>PREFERENCE_KEY</code>.</p>"},
  "value": {"target": "BillingFeatureFilterValues", "length": {"min": 1, "max": 10}, "type": "list", "doc": "<p>The filter values to match. For <code>PREFERENCE_KEY</code>, supply 1 to 10 preference key values to match.</p>"}},
  "doc": "<p>A filter that narrows the set of preferences returned by <code>GetBillingPreferences</code>.</p>"}
```
(no `"required": true` on `name`, so a nameless filter is a legal request).

The handler accepts a nameless filter — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_preferences.rs:270-283:
```
fn validate_filters(input: &GetBillingPreferencesInput) -> Result<(), GetBillingPreferencesError> {
    for filter in input.filters.iter().flatten() {
        if filter
            .name
            .as_deref()
            .is_some_and(|name| name != FILTER_PREFERENCE_KEY)
        {
            return Err(invalid_field(
                "filters",
                "name is not a filter this service supports",
            ));
```
…and then discards its values — get_billing_preferences.rs:297-312:
```
fn filtered_keys(input: &GetBillingPreferencesInput) -> Option<BTreeSet<&str>> {
    let mut selected: Option<BTreeSet<&str>> = None;
    for filter in input.filters.iter().flatten() {
        if filter.name.as_deref() != Some(FILTER_PREFERENCE_KEY) {
            continue;
        }
```
with the un-narrowed `None` then selecting everything — get_billing_preferences.rs:316-321:
```
fn matches_filters(
    entry: &crate::state::PreferenceEntry,
    selected: Option<&BTreeSet<&str>>,
) -> bool {
    selected.is_none_or(|keys| keys.contains(entry.key.as_str()))
}
```

The rule the crate states for this filter, and the test that pins it only for the named form — services/service-com-amazonaws-billing-awsbilling/tests/all/billing_preferences.rs:671 and :696-699:
```
async fn the_preference_key_filter_selects_only_the_keys_it_names() {
...
            &json!({
                "features": ["CREDIT_SHARING"],
                "filters": [{ "name": "PREFERENCE_KEY", "value": values }],
            }),
```
Every row of that suite (and of tests/all/billing_preference_pagination.rs:124) sends `name`, so no test covers the nameless filter the model admits.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (CREDIT_SHARING needs no access activation — it is not in `state::ACCESS_ACTIVATION_FEATURES`).
2. Call UpdateBillingPreferences(feature="CREDIT_SHARING", billingPreferencesPerKey=[{key:"key/0",value:"ENABLED"},{key:"key/1",value:"ENABLED"}]) and observe 200.
3. Call GetBillingPreferences(features=["CREDIT_SHARING"], filters=[{name:"PREFERENCE_KEY", value:["key/0"]}]) and observe 200 with exactly one entry, key/0 (the behaviour tests/all/billing_preferences.rs:671 pins).
4. Call GetBillingPreferences(features=["CREDIT_SHARING"], filters=[{value:["key/0"]}]) — legal, because the model does not mark `BillingFeatureFilter$name` @required.
5. Observe 200 with BOTH key/0 and key/1: the `value` list the request supplied was dropped and the filter narrowed nothing, instead of selecting only the key it named (or refusing the request).
