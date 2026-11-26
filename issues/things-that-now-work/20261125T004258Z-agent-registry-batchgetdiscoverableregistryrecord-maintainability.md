Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: c979813813ecaacb4b4f6d32b630ab1098e9ff80

# Collapse the registry-resolution-to-modelled-error mapping copied into all three discovery reads

_Filed: 2026-11-25T00:42:58Z_
_Operation: BatchGetDiscoverableRegistryRecord | Class: maintainability_

> _Note: this issue was identified against the code base at commit `c979813813ecaacb4b4f6d32b630ab1098e9ff80`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every one of this crate's three handlers resolves its registry through `crate::state::owner::registry(..)` and then re-implements the SAME two-branch mapping of that one helper's failure modes into modelled errors: `Err(StoreUnavailable)` -> `InternalServerException { message: Some(owner::STORE_UNAVAILABLE_MESSAGE) }`, `Ok(None)` -> `ResourceNotFoundException(state::registry_not_found(<identifier as supplied>))`, `Ok(Some(..))` -> the registry. The crate already centralises both the SENTENCE (`owner::STORE_UNAVAILABLE_MESSAGE`, `state::registry_not_found`) and the reason for doing so — `state.rs:148-151` says the refusal is "Shared by every operation that resolves a registry, so one refusal is worded one way" — but the mapping/branching around it is written out three times, one copy per operation, so the three copies can drift in which failure maps to which modelled error (and they already differ in shape: the batch read funnels `Ok(None)` through a local `registry_not_found` wrapper, while the list and search reads inline the enum variant plus `crate::state::registry_not_found`). A single shared mapping (e.g. an `owner`-level resolver returning the two failure cases, which each handler lifts into its own error channel) would leave every response byte-identical, since all three copies already produce the same message, reason and status today.

## Evidence

Copy 1 — services/service-com-amazonaws-agentregistry-agentregistry/src/ops/batch_get_discoverable_registry_record.rs:207-222:
```
let registry = match crate::state::owner::registry(&*self.engine, &entry.registry_id) {
    Err(crate::state::owner::StoreUnavailable) => {
        return Err(
            BatchGetDiscoverableRegistryRecordErrorModeled::InternalServerException(
                InternalServerException {
                    message: Some(
                        crate::state::owner::STORE_UNAVAILABLE_MESSAGE.to_string(),
                    ),
                },
            )
            .into(),
        )
    }
    Ok(None) => return Err(registry_not_found(&entry.registry_id)),
    Ok(Some(registry)) => registry,
};
```

Copy 2 — services/service-com-amazonaws-agentregistry-agentregistry/src/ops/list_discoverable_registry_records.rs:232-254:
```
let registry = match crate::state::owner::registry(&*self.engine, &input.registry_id) {
    Err(crate::state::owner::StoreUnavailable) => {
        return Err(
            ListDiscoverableRegistryRecordsErrorModeled::InternalServerException(
                InternalServerException {
                    message: Some(
                        crate::state::owner::STORE_UNAVAILABLE_MESSAGE.to_string(),
                    ),
                },
            )
            .into(),
        )
    }
    Ok(None) => {
        return Err(
            ListDiscoverableRegistryRecordsErrorModeled::ResourceNotFoundException(
                crate::state::registry_not_found(&input.registry_id),
            )
            .into(),
        )
    }
    Ok(Some(registry)) => registry,
};
```

Copy 3 — services/service-com-amazonaws-agentregistry-agentregistry/src/ops/search_discoverable_registry_records.rs:221-243:
```
let registry = match crate::state::owner::registry(&*self.engine, registry_id) {
    Err(crate::state::owner::StoreUnavailable) => {
        return Err(
            SearchDiscoverableRegistryRecordsErrorModeled::InternalServerException(
                InternalServerException {
                    message: Some(
                        crate::state::owner::STORE_UNAVAILABLE_MESSAGE.to_string(),
                    ),
                },
            )
            .into(),
        )
    }
    Ok(None) => {
        return Err(
            SearchDiscoverableRegistryRecordsErrorModeled::ResourceNotFoundException(
                crate::state::registry_not_found(registry_id),
            )
            .into(),
        )
    }
    Ok(Some(registry)) => registry,
};
```

The crate's own stated convention for exactly this refusal — services/service-com-amazonaws-agentregistry-agentregistry/src/state.rs:148-151: "Shared by every operation that resolves a registry, so one refusal is worded one way. The identifier is stated exactly as the request supplied it"; and services/service-com-amazonaws-agentregistry-agentregistry/src/state/owner.rs:35-38: "What every discovery read states for [`StoreUnavailable`]: one sentence, in one place, so the three reads cannot word the same server fault differently."

Existing coverage that must stay green and unchanged: tests/all/registry_read_refusals.rs:112 `a_registry_this_service_does_not_hold_is_refused_by_every_read` (asserts 404 + `ResourceNotFoundException` + the exact sentence for all three reads, over bare-id and ARN identifier forms); tests/all/discovery_read_path.rs:731 `an_unknown_registry_is_reported_as_resource_not_found`; tests/all/registry_read_refusals.rs:159 `a_registry_that_exists_is_served_and_missing_records_are_reported_per_entry` (the served contrast).

## Steps to Reproduce

1. Open services/service-com-amazonaws-agentregistry-agentregistry/src/ops/batch_get_discoverable_registry_record.rs and read lines 207-222.
2. Open services/service-com-amazonaws-agentregistry-agentregistry/src/ops/list_discoverable_registry_records.rs and read lines 232-254 — the same two failure branches of `crate::state::owner::registry` mapped to the same message and the same shared `registry_not_found` constructor.
3. Open services/service-com-amazonaws-agentregistry-agentregistry/src/ops/search_discoverable_registry_records.rs and read lines 221-243 — the third copy.
4. Compare with src/state/owner.rs:35-38 and src/state.rs:148-151, which state that this refusal is held 'in one place' so 'the three reads cannot word the same server fault differently' — the wording is shared, the mapping is not.
5. Note the copies are already shaped differently (batch funnels `Ok(None)` through its local `registry_not_found` wrapper; list and search inline the enum variant plus `crate::state::registry_not_found`), which is how a future edit to one branch can diverge from the other two.
