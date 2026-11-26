Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 9c31bf6152f020b6d0683107981107b2a4f16ac3

# The store's region-scope rule says every single-ARN operation refuses an out-of-region ARN "as the missing resource it is", while this handler answers AccessDeniedException/MappingsNotConfigured by design

_Filed: 2026-11-26T10:22:47Z_
_Operation: StartVirtualMachinesMetadataSync | Class: error-response_

> _Note: this issue was identified against the code base at commit `9c31bf6152f020b6d0683107981107b2a4f16ac3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state`'s module doc generalises one rule over EVERY operation of this service that resolves a single ARN: an ARN of another region "names no resource that endpoint holds and is refused as the missing resource it is, with nothing read and nothing written".

Two handlers deliberately do not do that, and both say so under the sentence. `StartVirtualMachinesMetadataSync` folds the out-of-region case into its property-mappings precondition — `if !in_request_region || !store.property_mappings_configured(...)` — so an out-of-region `HypervisorArn` is answered with the modelled `AccessDeniedException`, `ErrorCode` `MappingsNotConfigured`, at `MAPPINGS_NOT_CONFIGURED_STATUS` (400), never as a missing hypervisor. `TestHypervisorConfiguration` folds its own out-of-region `GatewayArn` into the invalid-request refusal — `if !gateway_in_request_region || store.gateway(...).is_none()` → `ValidationException` with `InvalidParameter`.

Both of those answers are the halves this crate pins (`MAPPINGS_NOT_CONFIGURED`'s doc records the gate firing before any existence lookup as recorded behaviour, and `INVALID_PARAMETER`'s doc records the test-configuration refusal), so the defect is the over-broad sentence, not the wire answers. A reader who trusts `state.rs:103-108` — the one place the region-scope rule is stated for the whole crate — would conclude `start_virtual_machines_metadata_sync.rs:225` is the odd one out and "fix" it to refuse an out-of-region ARN as `MissingResource::Hypervisor`, which splits the one gate refusal into two answers and reveals the existence of a record the gate is documented never to reveal. The sentence should carve out the two operations whose own docs already carve themselves out, exactly as the `MissingResource::Other` arm's doc carves itself out of the type-level rule above it.

## Evidence

The rule, generalised over every single-ARN operation — `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:103-108`:
```
//! That scope is not the listings' alone: an operation that resolves
//! ONE ARN reaches only the records of the region ITS request was addressed to
//! ([`arn_is_in_request_region`]), so an ARN of another region names no resource that
//! endpoint holds and is refused as the missing resource it is, with nothing read and
//! nothing written — a region never reports, writes or deletes a record another region's
//! listing owns.
```

The handler that does not — `src/ops/start_virtual_machines_metadata_sync.rs:198-199` and `:221-227`:
```
        let in_request_region =
            crate::state::arn_is_in_request_region(&input.hypervisor_arn, ctx.parts());
```
```
        // virtual-machine metadata can be synced. The gate is asked BEFORE any
        // resource-existence lookup, so an ARN naming no record at all, an ARN naming a
        // record of another region and an ARN naming a record with no mappings take the one
        // refusal and it reveals none of them.
        if !in_request_region || !store.property_mappings_configured(&input.hypervisor_arn) {
            return Err(mappings_not_configured());
        }
```
whose refusal is not a `ResourceNotFoundException` at all — same file `:144-154`:
```
fn mappings_not_configured() -> StartVirtualMachinesMetadataSyncError {
    crate::scaffolded::Scaffolded::modeled_with_status(
        StartVirtualMachinesMetadataSyncErrorModeled::AccessDeniedException(
            AccessDeniedException {
                error_code: crate::state::MAPPINGS_NOT_CONFIGURED.to_string(),
                message: Some(crate::state::MAPPINGS_NOT_CONFIGURED_MESSAGE.to_string()),
            },
        ),
        crate::state::MAPPINGS_NOT_CONFIGURED_STATUS,
    )
}
```

The second handler that does not — `src/ops/test_hypervisor_configuration.rs:186-187` and `:218-226`:
```
        let gateway_in_request_region =
            crate::state::arn_is_in_region(&input.gateway_arn, &region);
```
```
        if !gateway_in_request_region || store.gateway(&input.gateway_arn).is_none() {
            return Err(TestHypervisorConfigurationErrorModeled::ValidationException(
                ValidationException {
                    error_code: Some(crate::state::INVALID_PARAMETER.to_string()),
                    message: Some(crate::state::INVALID_PARAMETER_MESSAGE.to_string()),
                },
            )
            .into());
        }
```

The two carve-outs the crate already states elsewhere, which `state.rs:103-108` omits — `src/state.rs:380-383`:
```
/// whose check asserts an `AccessDeniedException` with this code for a nonexistent
/// `HypervisorArn`, before any resource-existence lookup), so neither the value nor the
/// answer being a refusal of that kind is this emulator's choice — and the gate firing
/// before the existence lookup is why one ARN naming nothing and one naming an
/// unconfigured record take the same answer.
```
and `src/state.rs:294-299`:
```
/// request is the configuration under test, so a `GatewayArn` that names no gateway makes
/// the REQUEST invalid rather than naming a resource the caller asked to read — AWS
/// answers it with the common `ValidationException` and this code rather than resolving the
/// ARN.
```

Existing coverage that pins the gate's single answer (so a "fix" toward the module-doc sentence would break it) — `tests/all/metadata_sync_requires_property_mappings.rs:11-17`:
```
//! The rule under test: a metadata sync of a hypervisor's virtual machines requires that
//! hypervisor's property mappings to be CONFIGURED — a `HypervisorArn` this service holds
//! no mappings under is refused and nothing is written, with the modelled
//! `AccessDeniedException`, `ErrorCode` `MappingsNotConfigured` and the sentence AWS states
//! for the condition, answered at `MAPPINGS_NOT_CONFIGURED_STATUS`. The gate is asked
//! BEFORE any resource-existence lookup, so a record whose mappings were never put and an
//! ARN naming no record at all take the one answer and it reveals neither.
```
plus `tests/all/fixed_refusal_sentences.rs`.

## Steps to Reproduce

1. Read `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:103-108`: the crate's one statement of the region-scope rule says every operation resolving ONE ARN refuses an out-of-region ARN "as the missing resource it is".
2. Read `src/ops/start_virtual_machines_metadata_sync.rs:225-227`: the out-of-region test is `||`-ed into the property-mappings gate, so the answer comes from `mappings_not_configured()` (`:144-154`) — `AccessDeniedException` + `ErrorCode` `MappingsNotConfigured` at HTTP 400, not `ResourceNotFoundException`.
3. Read `src/ops/test_hypervisor_configuration.rs:218-226`: the same shape for its `GatewayArn` — the out-of-region test is `||`-ed into the unknown-gateway refusal, answered `ValidationException` with `InvalidParameter`.
4. Confirm both departures are the pinned halves: `src/state.rs:380-383` records the mappings gate firing before any existence lookup, and `src/state.rs:294-299` records the test-configuration refusal as a `ValidationException`; `tests/all/metadata_sync_requires_property_mappings.rs:11-17` asserts the single gate answer.
5. Note that `MissingResource`'s own `Other` arm (`src/state.rs:3345-3346`) shows the convention for stating such a carve-out, which `state.rs:103-108` omits — so that sentence is the stale side and should name the two operations that answer the out-of-region ARN through their own refusal instead.
