Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 954f40bdc8c8f59a0f2a74042f78050b7862fc3f

# The control-plane read pipeline (settle -> lock -> resolve_in -> served_in -> drop guard -> source_content) is implemented twice in state.rs, so the two copies can drift

_Filed: 2026-11-26T08:57:19Z_
_Operation: StartConfigurationSession | Class: maintainability_

> _Note: this issue was identified against the code base at commit `954f40bdc8c8f59a0f2a74042f78050b7862fc3f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`resolve_deployed_triple` (what StartConfigurationSession resolves a session with) and `served_configuration` (what each GetLatestConfiguration poll resolves with) each spell out the same six-step read of the AppConfig control plane, in the same order, with the same locking rule: settle the alarm monitors with no guard held, take the control-plane state, lock it, resolve the triple through `resolve_in`, ask `served_in` what is in force for the session's rollout target, drop the guard, then read an external source through `source_content`. Only the surrounding decisions differ (the mint path additionally requires a deployment to exist and discards a resolution error; the poll path propagates it).

Because the sequence is written twice, the invariant both copies exist to uphold — "the two stores are taken one at a time, never nested", and the settle call being made with no AppConfig guard held — has to be maintained in two places. A change made to one copy (moving the settle, reordering the guard release, adding a step) silently leaves the other behaving differently for the same triple, which is exactly the class of divergence between a session's mint and its polls this crate takes pains to avoid elsewhere (`served_in`'s doc: "the one read both the session's mint and its polls answer from, so neither decides what 'in force' means for itself").

Extracting the shared sequence into one helper that both call leaves every response and every existing test unchanged.

## Evidence

Both copies are in this crate's own state module.

Copy 1 — services/service-com-amazonaws-appconfigdata-appconfigdata/src/state.rs:826-879 (inside `resolve_deployed_triple`):
```
::service_com_amazonaws_appconfig_amazonappconfig::state::alarm_monitors::settle(engine, now);
let state = app_config_state(engine)?;
let store = state.lock();
let (resolved, learned_names) = match resolve_in(
    &store, bindings, application_identifier, environment_identifier,
    configuration_profile_identifier,
) { ... };
...
let pending = served_in(
    &store, &resolved, application_identifier, environment_identifier,
    configuration_profile_identifier, target, now,
)
.and_then(Result::ok);
// The AppConfig guard is released before an EXTERNAL source is read, so the two stores
// are taken one at a time, never nested.
drop(store);
let configuration = pending
    .and_then(|pending| {
        source_content(engine, region, configuration_profile_identifier, pending).ok()
    })
```

Copy 2 — src/state.rs:938-971 (inside `served_configuration`):
```
::service_com_amazonaws_appconfig_amazonappconfig::state::alarm_monitors::settle(engine, now);
let state = app_config_state(engine)?;
let store = state.lock();
let (resolved, _learned) = match resolve_in(
    &store, bindings, application_identifier, environment_identifier,
    configuration_profile_identifier,
) { ... };
let pending = match served_in(
    &store, &resolved, application_identifier, environment_identifier,
    configuration_profile_identifier, target, now,
)? { ... };
// The AppConfig guard is released before an EXTERNAL source is read, so the two stores
// are taken one at a time, never nested.
drop(store);
Some(source_content(engine, region, configuration_profile_identifier, pending))
```
The duplicated comment ("The AppConfig guard is released before an EXTERNAL source is read, so the two stores are taken one at a time, never nested.", src/state.rs:868-869 and :963-964) is the same rule stated twice over the same code.

The rule the duplication threatens is stated at src/state.rs:1048-1050 (`served_in`): "the one read both the session's mint and its polls answer from, so neither decides what 'in force' means for itself".

TEST COVERAGE for both copies (which must stay green and unchanged): tests/all/session_requires_a_deployment.rs:370 `a_session_is_minted_for_a_deployment_that_put_a_configuration_in_service` and :129 `a_session_over_a_triple_with_no_deployment_is_refused_until_one_exists` (copy 1); tests/all/configuration_session_poll.rs:693 `a_poll_returns_content_only_when_the_deployed_version_changed` and tests/all/feature_flag_resolution.rs:212 `a_feature_flags_profile_serves_the_resolved_flag_document` (copy 2); tests/all/rollout_percentage_splits_sessions.rs:128/:198 and tests/all/zz_timing.rs:38 exercise both through the rollout target.

## Steps to Reproduce

1. Read services/service-com-amazonaws-appconfigdata-appconfigdata/src/state.rs:826-885 (`resolve_deployed_triple`), the path StartConfigurationSession's handler calls at src/ops/start_configuration_session.rs:358.
2. Read src/state.rs:922-972 (`served_configuration`), the path GetLatestConfiguration's handler calls at src/ops/get_latest_configuration.rs:428.
3. Observe the identical six-step sequence (alarm_monitors::settle -> app_config_state -> lock -> resolve_in -> served_in -> drop(store) -> source_content) and the identical guard-ordering comment written out twice.
4. Note that a change to the sequence in one function (e.g. moving the settle call, or releasing the guard at a different point) would leave the other resolving the same triple by a different procedure, with no test asserting the two stay in step.
