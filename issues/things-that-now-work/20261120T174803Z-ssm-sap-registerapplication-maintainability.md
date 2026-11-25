Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 0fd0c8a38859f6782ef39e69b19293525b727c9f

# The "value outside the model's enum catalogue" refusal is implemented three times in three handlers

_Filed: 2026-11-20T17:48:03Z_
_Operation: RegisterApplication | Class: maintainability_

> _Note: this issue was identified against the code base at commit `0fd0c8a38859f6782ef39e69b19293525b727c9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Three handlers each re-state the SAME rule in their own words: take the member's catalogue constant from `ops::types`, test `!CATALOGUE.contains(&value)`, and refuse with a `ValidationException` built as `"<Member> must be one of {joined catalogue}, but was {value}"`. Nothing in `state.rs` owns that rule, even though this crate otherwise keeps one construction site per refusal (`not_registered_successfully`, `application_control_available`, `validate_filters` are each documented as "the ONE place"). The three copies can drift: a change to the sentence, to the join separator, or to what counts as outside the catalogue has to be made in three files, and the next handler that re-states an enum writes a fourth copy. The same rule for filter members already lives in one place (`validate_filters` / `FilterOperators::accepts`), which is the shape the handler copies could take.

A single helper in `state.rs` taking (member name, catalogue, value) and returning the existing `ValidationException` leaves every response byte-identical and every existing test unchanged.

## Evidence

Copy 1 — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/register_application.rs:211-220`:
```
        if !APPLICATION_TYPE_VALUES.contains(&input.application_type.as_str()) {
            return Err(RegisterApplicationErrorModeled::ValidationException(
                crate::state::validation(format!(
                    "ApplicationType must be one of {}, but was {}",
                    APPLICATION_TYPE_VALUES.join(", "),
                    input.application_type
                )),
            )
            .into());
        }
```
Copy 2 — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/stop_application.rs:262-270`:
```
        if !CONNECTED_ENTITY_TYPE_VALUES.contains(&entity) {
            return Err(StopApplicationErrorModeled::ValidationException(
                crate::state::validation(format!(
                    "StopConnectedEntity must be one of {}, but was {entity}",
                    CONNECTED_ENTITY_TYPE_VALUES.join(", ")
                )),
            )
            .into());
        }
```
Copy 3 — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/update_application_settings.rs:179-188`:
```
            if !BACKINT_MODE_VALUES.contains(&backint.backint_mode.as_str()) {
                return Err(UpdateApplicationSettingsErrorModeled::ValidationException(
                    crate::state::validation(format!(
                        "BackintMode must be one of {}, but was {}",
                        BACKINT_MODE_VALUES.join(", "),
                        backint.backint_mode
                    )),
                )
                .into());
            }
```
The single-site version of the same rule for the filter members, `src/state.rs:3993-4008`:
```
pub fn validate_filters(
    filters: &[Filter],
    operators: FilterOperators<'_>,
    allowed_names: &[&str],
) -> Result<(), ValidationException> {
    filters_within_bounds(filters)?;
    for filter in filters {
        operators.accepts(&filter.operator)?;
        if !allowed_names.contains(&filter.name.as_str()) {
            return Err(validation(format!(
                "Filter Name must be one of {}, but was {}",
```

Tests that drive all three branches on their accepting side, so a refactor is covered: `tests/all/register_application_conditional_requirements.rs:119` and `tests/all/application_lifecycle.rs:854` loop over `APPLICATION_TYPE_VALUES`; `tests/all/stop_connected_entity.rs:261` loops over `CONNECTED_ENTITY_TYPE_VALUES` and `tests/all/application_reference_refusals.rs:156` sends `CONNECTED_ENTITY_TYPE_VALUES[0]`; `tests/all/update_application_settings.rs:297` and `tests/all/settings_destroyed_databases.rs:270` send `BACKINT_MODE_VALUES[0]`. `tests/all/filter_validation.rs:101,113` pins the wording of the single-site filter version.

## Steps to Reproduce

1. grep -n "must be one of" services/service-com-amazonaws-ssmsap-ssmsap/src/ops/*.rs — three handler copies of the same refusal (register_application.rs:214, stop_application.rs:265, update_application_settings.rs:182).
2. Read each of the three blocks: identical structure (catalogue `.contains`, `.join(", ")`, same sentence shape), only the member name and the catalogue constant differ.
3. Compare with src/state.rs:3993-4008, where the same rule for filter names/operators has exactly one construction site that every filtered listing calls.
