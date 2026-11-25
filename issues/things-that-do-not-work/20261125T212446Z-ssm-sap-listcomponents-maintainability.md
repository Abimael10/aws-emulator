Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 78dbb54d13267bc68bcd71edb0782b9ab6fcde88

# The optional-ApplicationId resolution and its 404 refusal are written twice (ListComponents and ListDatabases)

_Filed: 2026-11-25T21:24:46Z_
_Operation: ListComponents | Class: maintainability_

> _Note: this issue was identified against the code base at commit `78dbb54d13267bc68bcd71edb0782b9ab6fcde88`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListComponents` and `ListDatabases` both implement the same rule in their own handler bodies: an `ApplicationId` the request names narrows the listing to that one application, which must be registered in this Region, an id that names none being the modelled `ResourceNotFoundException` built from `not_found(application_not_found_message(id))`; an absent `ApplicationId` reaches every application of the Region. The two copies are byte-for-byte the same computation apart from the error enum they lift the refusal into, so they can drift — one could change the refusal it builds, or the candidate set it falls back to, without the other.

This crate already factors its other shared listing rules into `state.rs` and lets each handler lift the refusal into its own modelled error (`validate_filters` for the filter rule, `application_control_available` for the control gate), so the one place this rule is missing is the per-handler copy. A shared resolver on `RegionState` returning the candidate applications (or the `application_not_found_message` to wrap) keeps both responses byte-identical and leaves both existing tests unchanged.

## Evidence

Copy 1 — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/list_components.rs:173-187`:
```
let applications: Vec<&crate::state::StoredApplication> =
    match input.application_id.as_deref() {
        Some(application_id) => {
            let application =
                region_state.applications.get(application_id).ok_or_else(|| {
                    ListComponentsErrorModeled::ResourceNotFoundException(
                        crate::state::not_found(
                            crate::state::application_not_found_message(application_id),
                        ),
                    )
                })?;
            vec![application]
        }
        None => region_state.applications.values().collect(),
    };
```
Copy 2 — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/list_databases.rs:166-180`:
```
let applications: Vec<&crate::state::StoredApplication> =
    match input.application_id.as_deref() {
        Some(application_id) => {
            let application =
                region_state.applications.get(application_id).ok_or_else(|| {
                    ListDatabasesErrorModeled::ResourceNotFoundException(
                        crate::state::not_found(
                            crate::state::application_not_found_message(application_id),
                        ),
                    )
                })?;
            ::std::vec![application]
        }
        None => region_state.applications.values().collect(),
    };
```
The crate's own practice for a rule shared by several listings — `src/state.rs:4047-4070`: "Refuse a `Filters` member this listing cannot apply, whatever is being listed. / This is the validation every filtered listing of this service shares, so one rule decides what makes a `Filter` usable ... and lifts the refusal into its own modelled `ValidationException`."

Test coverage that pins both copies' responses (so a dedup must keep them identical): `tests/all/component_listing.rs:442` ("registration there is refused with the 404 `ResourceNotFoundException` the operation ...") asserting `reply.status, 404` at `tests/all/component_listing.rs:565` and `:727`; `tests/all/list_databases.rs:660-661` asserting `expected 404 from ListDatabases`.

## Steps to Reproduce

1. Open `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/list_components.rs:173` and `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/list_databases.rs:166` side by side and compare the two `match input.application_id.as_deref()` blocks: the lookup, the `not_found(application_not_found_message(..))` refusal and the `None` fallback are the same logic twice.
2. Note that `services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs` already holds the shared counterparts for the other cross-listing rules (`validate_filters`, `application_control_available`) but no resolver for this one.
3. Run `cargo test -p service-com-amazonaws-ssmsap-ssmsap` and observe `tests/all/component_listing.rs` and `tests/all/list_databases.rs` pinning the 404 on both paths, so the duplication is the only thing that can drift.
