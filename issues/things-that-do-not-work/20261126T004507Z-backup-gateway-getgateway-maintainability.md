Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: bf205090aef11dede75ce528b7b57e0b8feca92b

# Hoist the duplicated MissingResource refusal rendering into one helper

_Filed: 2026-11-26T00:45:06Z_
_Operation: GetGateway | Class: maintainability_

> _Note: this issue was identified against the code base at commit `bf205090aef11dede75ce528b7b57e0b8feca92b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule "a missing-resource refusal of this service is rendered as the modelled `ResourceNotFoundException` carrying `MissingResource::error_code()` / `MissingResource::message(arn)`, answered at `MissingResource::status_override()` where it states one and at the shape's declared status otherwise" is implemented 19 separate times inside this crate's own `src/ops/`.

The crate already proves the rule belongs in a helper: two handlers wrap exactly this branch in a local function (`associate_gateway_to_server::missing_resource`, `put_hypervisor_property_mappings::hypervisor_not_found`), while the other seventeen paths that need the same branch inline it instead. Because each copy is written out per operation, one copy can silently drift — e.g. lose the `status_override()` arm and answer the shape's declared 404 where every sibling answers 400 — and `crate::state::MissingResource`'s own doc ("All three parts of a refusal — the status, the code and the sentence — are read off this one value, so they cannot be chosen apart, and no handler composes a sentence of its own") would then be false of that one handler.

One generic constructor (the modelled `ResourceNotFoundException` plus the status decision, parameterised by the per-operation error variant the caller supplies, as the two existing local helpers already are) would leave every response byte-identical and every existing test unchanged.

## Evidence

Both copies of the identical branch, in this crate:

`services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/put_hypervisor_property_mappings.rs:162-174` — already a helper:
```
fn hypervisor_not_found(hypervisor_arn: &str) -> PutHypervisorPropertyMappingsError {
    let missing = crate::state::MissingResource::Hypervisor;
    let error = PutHypervisorPropertyMappingsErrorModeled::ResourceNotFoundException(
        ResourceNotFoundException {
            error_code: missing.error_code().map(str::to_string),
            message: Some(missing.message(hypervisor_arn)),
        },
    );
    match missing.status_override() {
        Some(status) => crate::scaffolded::Scaffolded::modeled_with_status(error, status),
        None => error.into(),
    }
}
```

`services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/associate_gateway_to_server.rs:134-147` — the same, as a second local helper:
```
fn missing_resource(
    missing: crate::state::MissingResource,
    arn: &str,
) -> AssociateGatewayToServerError {
    let error =
        AssociateGatewayToServerErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
            error_code: missing.error_code().map(str::to_string),
            message: Some(missing.message(arn)),
        });
    match missing.status_override() {
        Some(status) => crate::scaffolded::Scaffolded::modeled_with_status(error, status),
        None => error.into(),
    }
}
```

`services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/get_gateway.rs:173-183` — the same branch inlined instead:
```
            let missing = crate::state::MissingResource::Gateway;
            let error = GetGatewayErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    error_code: missing.error_code().map(str::to_string),
                    message: Some(missing.message(&input.gateway_arn)),
                },
            );
            return Err(match missing.status_override() {
                Some(status) => crate::scaffolded::Scaffolded::modeled_with_status(error, status),
                None => error.into(),
            });
```

The same `match missing.status_override()` branch appears at: get_gateway.rs:180, delete_gateway.rs:179, update_gateway_information.rs:185, update_gateway_software_now.rs:189, get_bandwidth_rate_limit_schedule.rs:199, put_bandwidth_rate_limit_schedule.rs:259, put_maintenance_start_time.rs:251, disassociate_gateway_from_server.rs:204, test_hypervisor_configuration.rs:250, get_hypervisor.rs:179, update_hypervisor.rs:214, delete_hypervisor.rs:181, get_hypervisor_property_mappings.rs:198, get_virtual_machine.rs:191, tag_resource.rs:212, untag_resource.rs:196, list_tags_for_resource.rs:182 — plus the two helpers above (19 copies total).

The single rule the copies restate, from `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:3306-3308`:
```
/// All three parts of a refusal — the status, the code and the sentence ([`Self::message`])
/// — are read off this one value, so they cannot be chosen apart, and no handler composes a
/// sentence of its own for an ARN it could not resolve.
```

Existing coverage that would pin the behaviour unchanged across the refactor: `tests/all/missing_resource_refusal_is_one_answer.rs:3-15` ("a well-formed ARN of this service that names no record the store holds is refused for the KIND of resource that is missing, and that refusal — the HTTP status, the `ErrorCode` member and the sentence — is the same whichever operation resolved it"), with `fn not_found` at lines 70-77 asserting `status: 400` + `ResourceNotFoundException` + the kind's `ErrorCode`; and `tests/all/fixed_refusal_sentences.rs`.

## Steps to Reproduce

1. Open `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/` and grep for `missing.status_override()`: 19 hits across 19 files.
2. Compare `put_hypervisor_property_mappings.rs:162-174` and `associate_gateway_to_server.rs:134-147` (the branch wrapped in a local helper) with `get_gateway.rs:173-183` and the other sixteen handlers (the same branch written out inline).
3. Note that editing any one inline copy — dropping the `Some(status) => modeled_with_status(...)` arm — changes only that operation's status, contradicting `state.rs:3306-3308` and `tests/all/missing_resource_refusal_is_one_answer.rs:70-77` for that operation alone.
4. Hoist one generic constructor taking the `MissingResource`, the ARN and the per-operation error variant; confirm `cargo test -p service-com-amazonaws-backupgateway-backuponpremises-v20210101` still passes with no test or response change.
