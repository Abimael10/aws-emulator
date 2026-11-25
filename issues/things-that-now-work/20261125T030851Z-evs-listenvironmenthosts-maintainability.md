Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: a451b2494ad63ebcb0a59d0c4e30e275403e3b3d

# The "resolve the environment of an environment-scoped list, then refuse it while initializing" two-step is written out once per list handler, four times over

_Filed: 2026-11-25T03:08:51Z_
_Operation: ListEnvironmentHosts | Class: maintainability_

> _Note: this issue was identified against the code base at commit `a451b2494ad63ebcb0a59d0c4e30e275403e3b3d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Each of the four environment-scoped paginated reads (ListEnvironmentVlans, ListEnvironmentHosts, ListEnvironmentConnectors, ListVmEntitlements) spells out the SAME two-step rule inline: resolve the addressed environment with `state.environment_in_region(&region, &input.environment_id)` and answer `environment_not_found` when it resolves to nothing, then call `crate::state::list_refused_while_initializing(environment)` and answer that `ValidationException` when it returns one. The four copies are verbatim apart from the generated per-operation error enum name, and the explanatory comment above each copy is the same two sentences.

This crate otherwise single-homes exactly this kind of shared step: `crate::state::page_size` for the page bound, `crate::state::page_cursor` for "resolve the nextToken or refuse it", `StoredEnvironment::addressed_public_hcx_vlan` for the Elastic IP VLAN gate, `StoredConnector::is_live` for "this connector is already going away", and `live_environment_in_region` for "a DELETED environment is gone for addressing purposes". The lookup half and the refusal half are each already single-homed here too — what is duplicated is the COMBINATION the four handlers apply, so a change to which records an environment-scoped list resolves, or to which states it refuses, has to be made in four places.

That the copies can drift is not hypothetical: the still-open issue 20261119T112959Z-evs-listenvironmentvlans-error-response.md is precisely about these four copies resolving an environment that has reached DELETED while the state module names only GetEnvironment and ListEnvironments as deliberate exceptions — one rule, four edits.

The fix is behaviour-preserving: one helper beside `list_refused_while_initializing` in `crate::state` that resolves the environment and answers the two outcomes (a not-found `ResourceNotFoundException` or an initializing `ValidationException`), which each handler lifts into its own modelled error. Every response body, status and message stays byte-identical, so every existing test keeps passing.

## Evidence

Copy 1 — services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/ops/list_environment_vlans.rs:198-214:
```
        // Regional resource: the VLANs of an environment are only listable at the
        // endpoint of the Region that environment was created in.
        let environment = state
            .environment_in_region(&region, &input.environment_id)
            .ok_or_else(|| {
                ListEnvironmentVlansErrorModeled::ResourceNotFoundException(
                    crate::state::environment_not_found(&input.environment_id),
                )
            })?;

        // "You will see this exception ... if a list operation is performed on an
        // environment resource that is still initializing" (`ValidationException` doc):
        // a list is refused while the environment it addresses is still being built,
        // rather than reporting the VLANs of a half-built environment.
        if let Some(refusal) = crate::state::list_refused_while_initializing(environment) {
            return Err(ListEnvironmentVlansErrorModeled::ValidationException(refusal).into());
        }
```

Copy 2 — src/ops/list_environment_hosts.rs:200-216:
```
        // Regional resource: the hosts of an environment are only listable at the
        // endpoint of the Region that environment was created in.
        let environment = state
            .environment_in_region(&region, &input.environment_id)
            .ok_or_else(|| {
                ListEnvironmentHostsErrorModeled::ResourceNotFoundException(
                    crate::state::environment_not_found(&input.environment_id),
                )
            })?;

        // "You will see this exception ... if a list operation is performed on an
        // environment resource that is still initializing" (`ValidationException` doc):
        // a list is refused while the environment it addresses is still being built,
        // rather than reporting the hosts of a half-built environment.
        if let Some(refusal) = crate::state::list_refused_while_initializing(environment) {
            return Err(ListEnvironmentHostsErrorModeled::ValidationException(refusal).into());
        }
```

Copy 3 — src/ops/list_environment_connectors.rs:206-222 (`ListEnvironmentConnectorsErrorModeled::ResourceNotFoundException(crate::state::environment_not_found(...))` then `if let Some(refusal) = crate::state::list_refused_while_initializing(environment) { return Err(ListEnvironmentConnectorsErrorModeled::ValidationException(refusal).into()); }`).

Copy 4 — src/ops/list_vm_entitlements.rs:217-233 (the same body with `ListVmEntitlementsErrorModeled`).

The halves that ARE single-homed, which is the convention the combination departs from — src/state.rs:640-654:
```
pub fn list_refused_while_initializing(
    environment: &StoredEnvironment,
) -> Option<ValidationException> {
```
and src/state.rs:3262-3270:
```
    pub fn environment_in_region(
        &self,
        region: &str,
        environment_id: &str,
    ) -> Option<&StoredEnvironment> {
```
beside the crate's own statement of why such a step is single-homed — src/state.rs:3911-3918 (on `page_cursor`): "The whole step from the member a request carries to the position a scan resumes at, stated once ... It lives beside the two halves it joins, so no two paginated operations of this service can disagree".

Drift already recorded against these four copies: issues/things-that-do-not-work/20261119T112959Z-evs-listenvironmentvlans-error-response.md — "The four environment-scoped list operations resolve an environment that has reached DELETED, although the state module names only GetEnvironment and L...".

Test coverage that pins both outcomes for all four operations (so a refactor cannot change behaviour unnoticed) — tests/all/list_while_initializing.rs:86-101 drives the four operations by name:
```
            "ListEnvironmentVlans",
...
            "ListEnvironmentHosts",
...
            "ListEnvironmentConnectors",
...
            "ListVmEntitlements",
```
and tests/all/environment_addressing_rule.rs covers the not-found half of the same lookup.

## Steps to Reproduce

1. Open src/ops/list_environment_vlans.rs:198-214, src/ops/list_environment_hosts.rs:200-216, src/ops/list_environment_connectors.rs:206-222 and src/ops/list_vm_entitlements.rs:217-233 side by side: the lookup-then-refuse block is identical in all four apart from the generated error enum name.
2. Note that both halves already live in crate::state (src/state.rs:3262 `environment_in_region`, src/state.rs:640 `list_refused_while_initializing`) while the combination does not, unlike the sibling shared steps `page_size` (src/state.rs:3715) and `page_cursor` (src/state.rs:3924) that the same four handlers call.
3. Try to change which records an environment-scoped list resolves — the change the open issue 20261119T112959Z-evs-listenvironmentvlans-error-response.md asks for (swap `environment_in_region` for the live form): four separate handler edits are needed, and three of them can be forgotten.
4. Confirm the behaviour is pinned: `cargo test -p service-com-amazonaws-evs-amazonelasticvmwareservice list_while_initializing` exercises the refusal for all four operations and environment_addressing_rule covers the not-found half, so extracting the two-step into one state helper must leave every response unchanged.
