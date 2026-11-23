Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 039d38dc4c02de0c6d595d33e91f7f516f07aa43

# The create's five-refusal sequence is written out twice in one handler instead of behind one helper, so the pre-flight copy can drift from the deciding copy

_Filed: 2026-11-23T00:27:40Z_
_Operation: CreateWebFunctionEndpoint | Class: maintainability_

> _Note: this issue was identified against the code base at commit `039d38dc4c02de0c6d595d33e91f7f516f07aa43`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateWebFunctionEndpoint::handle` takes the same five refusals twice — once under a pre-flight guard (so a doomed request does not bind a port) and once under the guard that writes. The sequence is not expressed once and called twice: both copies spell out the same five steps in the same order, including the `home_region(function, &signed_for)` derivation and the `endpoint_config_violation` arguments. Nothing ties them together, so a check added, reordered or re-argued in one copy silently weakens the other (a pre-flight that drifts starts opening a listener for a request the deciding guard will refuse; a deciding guard that drifts refuses later than the pre-flight claims).

The sibling create already shows the shape that avoids this: `CreateWebFunction` names each refusal once as a function (`refuse_if_taken`, `refuse_if_at_the_quota`) and calls those same functions from both its pre-flight and its deciding guard. Extracting this handler's sequence into one local function returning `(revision_weights, region)` and calling it from both places changes no response and no test.

## Evidence

Copy 1 (pre-flight) — services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_endpoint.rs:407-425:
```
            let mut store = state.lock();
            store.materialise_due_transitions(::std::time::SystemTime::now());
            let function = require_function(&store, &function_name, &input.function_name)?;
            refuse_if_taken(function, &input.endpoint_name)?;
            refuse_if_at_the_quota(function)?;
            revision_weights_for(function, input.revision_weights.as_deref())?;
            if let Some(message) = crate::state::endpoint_config_violation(
                &input.endpoint_name,
                &input.endpoint_type,
                input.regions.as_deref(),
                &home_region(function, &signed_for),
            ) {
                return Err(validation(message));
            }
```
Copy 2 (deciding) — same file:452-467:
```
        let (revision_weights, region) = {
            let function = require_function(&store, &function_name, &input.function_name)?;
            refuse_if_taken(function, &input.endpoint_name)?;
            refuse_if_at_the_quota(function)?;
            let weights = revision_weights_for(function, input.revision_weights.as_deref())?;
            let region = home_region(function, &signed_for);
            if let Some(message) = crate::state::endpoint_config_violation(
                &input.endpoint_name,
                &input.endpoint_type,
                input.regions.as_deref(),
                &region,
            ) {
                return Err(validation(message));
            }
            (weights, region)
        };
```
The sibling that states each refusal once and calls it twice — services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function.rs:308-313 and :391-392:
```
            refuse_if_taken(&state, &input.function_name)?;
            refuse_if_at_the_quota(&state)?;
```

Existing tests that pin every response the two copies produce: tests/all/web_function_endpoint.rs:1387 `a_create_that_cannot_own_its_name_is_refused_and_changes_nothing`, :1479 `a_configuration_outside_the_documented_rules_is_refused_on_this_path_too`, :1554 `a_create_past_the_endpoint_quota_is_refused`, :1209 `revision_weights_name_the_functions_own_revisions`, :1004 `an_endpoint_created_on_a_stored_function_is_persisted_and_read_back`.

## Steps to Reproduce

1. Read services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_endpoint.rs:407-425 and :452-467 side by side: the same five refusals, in the same order, with the same arguments
2. Compare with src/ops/create_web_function.rs:308-313 and :391-392, where the same double-check is done by calling one named helper per refusal
3. Add a hypothetical sixth refusal to one block only to see that nothing in the crate forces the other block to follow
