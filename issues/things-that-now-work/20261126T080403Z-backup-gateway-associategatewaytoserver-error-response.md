Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: aca3b1088fe80adec75979513445136e31dcd1e4

# state.rs says AssociateGatewayToServer carries the not-found code in ValidationException, while the handler answers ResourceNotFoundException

_Filed: 2026-11-26T08:04:03Z_
_Operation: AssociateGatewayToServer | Class: error-response_

> _Note: this issue was identified against the code base at commit `aca3b1088fe80adec75979513445136e31dcd1e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state`'s `HYPERVISOR_NOT_FOUND` doc states the crate's rule for which modelled shape carries a missing-resource `ErrorCode`: `ResourceNotFoundException$ErrorCode` for an operation whose model declares that shape, and `ValidationException$ErrorCode` for one that does not — naming `AssociateGatewayToServer` as the operation that does not. `GATEWAY_NOT_FOUND`'s doc restates the same rule in the abstract ("the same code inside whatever other modelled shape an operation that does not model it has to carry the condition in").

The only operation that sentence is about does the opposite, deliberately: `associate_gateway_to_server.rs` widens its generated error enum with `ResourceNotFoundException` through `extra_errors` precisely because the model declares none for it, and both of its missing-resource arms (`NoSuchGateway`, `NoSuchServer`) are answered in that shape by the local `missing_resource` helper. No handler in the crate ever puts `GATEWAY_NOT_FOUND` or `HYPERVISOR_NOT_FOUND` into a `ValidationException`: the two constants are reached only through `MissingResource::error_code()`, and every one of its call sites builds a `ResourceNotFoundException`.

The wire answer is the half this crate pins (`tests/all/missing_resource_refusal_is_one_answer.rs` asserts `AssociateGatewayToServer` answers exactly the `ResourceNotFoundException` + 400 + `GatewayNotFound` refusal `GetGateway` gives for the same ARN), so the defect is the stale rule sentence in `state.rs`. A reader who trusts it — it sits directly above the constant every handler's refusal reads — would conclude `associate_gateway_to_server.rs` is the odd one out and "fix" it to answer `ValidationException`, breaking that test and splitting one condition into two answers. The sentence should state the shape the handler really uses (the `ResourceNotFoundException` the crate carries this condition in everywhere, even where the operation shape does not list it).

## Evidence

The rule, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:246-255`:
```
/// `ErrorCode` for a well-formed hypervisor ARN that names no hypervisor, and the
/// sentence that states it. See [`MissingResource`].
///
/// It is `ResourceNotFoundException$ErrorCode` for an operation that models that shape,
/// and `ValidationException$ErrorCode` — "a description of what caused the validation
/// error" — for one that does not: `AssociateGatewayToServer` models only
/// `ConflictException` beside this service's common errors
/// (`smithy_query(query="operation",operation="AssociateGatewayToServer")`), so the
/// condition is named by the same code whichever modelled shape carries it.
pub const HYPERVISOR_NOT_FOUND: &str = "HypervisorNotFound";
```
and the same rule restated at `src/state.rs:260-268`:
```
/// Read like [`HYPERVISOR_NOT_FOUND`]: it is `ResourceNotFoundException$ErrorCode` — "a
/// description of which resource wasn't found" — for an operation that models that shape,
/// and the same code inside whatever other modelled shape an operation that does not model
/// it has to carry the condition in, so one condition is named one way whichever shape
/// carries it.
pub const GATEWAY_NOT_FOUND: &str = "GatewayNotFound";
```

The handler under it does the opposite — `src/ops/associate_gateway_to_server.rs:91-110`:
```
// `extra_errors` widens the set with the errors this SERVICE models for every
// operation (`smithy_query(query="service_info")` -> `common_errors`) but that
// `AssociateGatewayToServer` does not repeat in its own `errors` list, plus the
// `ResourceNotFoundException` it declares none of
// (`smithy_query(query="operation",operation="AssociateGatewayToServer")`): an ARN that
// resolves to no resource is a NOT-FOUND condition whichever operation resolved it, so it
// is carried by that shape here as it is everywhere else in this service rather than
// re-told as an input-validation failure.
#[smithy_macros::operation_error(
    "com.amazonaws.backupgateway#AssociateGatewayToServer",
    extra_errors = ["ResourceNotFoundException", "ValidationException", "InternalServerException"]
)]
```
and the refusal it builds, same file `:134-147`:
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
```
reached for both missing ends, same file `:221-227`:
```
                crate::state::AssociationRefusal::NoSuchGateway => {
                    missing_resource(crate::state::MissingResource::Gateway, &input.gateway_arn)
                }
                crate::state::AssociationRefusal::NoSuchServer => missing_resource(
                    crate::state::MissingResource::Hypervisor,
                    &input.server_arn,
                ),
```

No `ValidationException` anywhere in this crate carries either code: `grep -n 'HYPERVISOR_NOT_FOUND|GATEWAY_NOT_FOUND' src/` returns only the two constants, doc cross-references, `src/state.rs:3366` / `:3368` (`MissingResource::error_code`), `src/state.rs:3377` / `:3379` (`MissingResource::message`) and `src/ops/test_hypervisor_configuration.rs:247` — and every caller of `error_code()` builds a `ResourceNotFoundException`.

Existing coverage that pins the handler's answer and contradicts the doc — `tests/all/missing_resource_refusal_is_one_answer.rs:249-273`:
```
/// The same condition inside the operation whose model declares no
/// `ResourceNotFoundException` at all (`smithy_query(query="operation",
/// operation="AssociateGatewayToServer")`): an ARN that resolves to nothing is a not-found
/// condition, so it is answered in the same shape, with the same status, code and sentence
/// every operation that DOES declare it answers with.
...
        assert_eq!(
            refusal_of(&refused),
            not_found(GATEWAY_NOT_FOUND, GATEWAY_NOT_FOUND_MESSAGE),
```
with `fn not_found` at `:70-77` asserting `status: 400`, `shape: Some("ResourceNotFoundException")`.

## Steps to Reproduce

1. Read `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:246-255`: the rule says the missing-resource `ErrorCode` is carried in `ValidationException$ErrorCode` for an operation that models no `ResourceNotFoundException`, and names `AssociateGatewayToServer` as that operation.
2. Read `src/ops/associate_gateway_to_server.rs:91-110`: the handler adds `ResourceNotFoundException` to its error enum via `extra_errors` *because* the operation declares none, with a comment stating the opposite rule ("carried by that shape here as it is everywhere else in this service rather than re-told as an input-validation failure").
3. Read the same file `:134-147` and `:221-227`: both `NoSuchGateway` and `NoSuchServer` are answered as `ResourceNotFoundException`, never as `ValidationException`.
4. Confirm no other path honours the doc: grep `HYPERVISOR_NOT_FOUND|GATEWAY_NOT_FOUND` under `src/` — the codes are produced only by `MissingResource::error_code()` (`src/state.rs:3366`, `:3368`), whose every call site constructs a `ResourceNotFoundException`.
5. Confirm the wire answer is the pinned half: `tests/all/missing_resource_refusal_is_one_answer.rs:249-273` (+ `fn not_found` at `:70-77`) asserts `AssociateGatewayToServer` answers 400 `ResourceNotFoundException` with `ErrorCode` `GatewayNotFound` for an ARN naming no gateway — so the `state.rs` sentence is the stale side.
