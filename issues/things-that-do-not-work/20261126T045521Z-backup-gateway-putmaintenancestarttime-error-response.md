Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: e382557720e5e8d4102aca7118b81227b54e1b7d

# PutMaintenanceStartTime's own module doc states the missing-gateway refusal carries no ErrorCode, while the handler under it sends GatewayNotFound

_Filed: 2026-11-26T04:55:21Z_
_Operation: PutMaintenanceStartTime | Class: error-response_

> _Note: this issue was identified against the code base at commit `e382557720e5e8d4102aca7118b81227b54e1b7d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `PutMaintenanceStartTime` handler file documents its `ResourceNotFoundException` contract as carrying an *absent* `ErrorCode`, and claims that is what `get_gateway`, `delete_gateway` and `update_gateway_information` do too. The code immediately below that doc does the opposite: it reads the refusal off `crate::state::MissingResource::Gateway`, whose `error_code()` returns `Some(GATEWAY_NOT_FOUND)` (`"GatewayNotFound"`), and sets it on the response — exactly as the three sibling handlers it names do. So the one sentence in this crate that describes this operation's not-found refusal is false of this operation and of all three siblings it cites, and the crate's own end-to-end test asserts the opposite of it.

The defect is the stale contract sentence, not the wire answer: the `ErrorCode` the handler sends is the one `crate::state::MissingResource` chooses for every operation of this service, and `tests/all/missing_resource_refusal_is_one_answer.rs` pins it for `PutMaintenanceStartTime` by name. A reader who trusts the module doc — the first thing anyone editing this operation reads — would conclude the handler is the odd one out and "fix" it by dropping the `ErrorCode`, which would break that test and contradict `MissingResource`'s own invariant ("All three parts of a refusal — the status, the code and the sentence — are read off this one value, so they cannot be chosen apart"). The sentence should state the `GatewayNotFound` code the handler really sends.

## Evidence

The doc, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/put_maintenance_start_time.rs:81-85`:
```
//! * `ResourceNotFoundException` (HTTP 404) — A resource that is required for the action
//!   wasn't found: the refusal for a well-formed `GatewayArn` the store holds no gateway
//!   under, whether it was never created or has been deleted — the same answer, and the
//!   same status and absent `ErrorCode`, every other gateway operation of this service
//!   gives (`get_gateway`, `delete_gateway`, `update_gateway_information`).
```

The code under it, same file `:243-250` — the `ErrorCode` is SET, not absent:
```
        let Some(updated) = written else {
            let missing = crate::state::MissingResource::Gateway;
            let error = PutMaintenanceStartTimeErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    error_code: missing.error_code().map(str::to_string),
                    message: Some(missing.message(&input.gateway_arn)),
                },
            );
```

What that call returns, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:3364-3372`:
```
    pub fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Hypervisor => Some(HYPERVISOR_NOT_FOUND),
            Self::PropertyMappings => Some(MAPPINGS_NOT_FOUND),
            Self::Gateway => Some(GATEWAY_NOT_FOUND),
```
with `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:268`:
```
pub const GATEWAY_NOT_FOUND: &str = "GatewayNotFound";
```

The three siblings the doc cites do exactly the same thing, so the "every other gateway operation" half is false too — `src/ops/get_gateway.rs:173-178`, `src/ops/delete_gateway.rs:172-178`, `src/ops/update_gateway_information.rs:178-184` all build `error_code: missing.error_code().map(str::to_string)` from `MissingResource::Gateway`.

Existing coverage that contradicts the doc and pins the real answer — `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/tests/all/missing_resource_refusal_is_one_answer.rs:70-77`:
```
fn not_found(error_code: &str, message: &str) -> Refusal {
    Refusal {
        status: 400,
        shape: Some("ResourceNotFoundException".to_string()),
        error_code: Some(error_code.to_string()),
        message: message.to_string(),
    }
}
```
`PutMaintenanceStartTime` is one of the operations that table drives (`tests/all/missing_resource_refusal_is_one_answer.rs:92` lists `"PutMaintenanceStartTime"` in `gateway_requests`), and the expectation is `not_found(GATEWAY_NOT_FOUND, GATEWAY_NOT_FOUND_MESSAGE)` (`:208`, `:269`).

## Steps to Reproduce

1. Read `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/put_maintenance_start_time.rs:81-85`: the operation's documented `ResourceNotFoundException` contract says the refusal carries an "absent `ErrorCode`".
2. Read the handler branch at the same file `:243-250`: it sets `error_code: missing.error_code().map(str::to_string)` from `crate::state::MissingResource::Gateway`.
3. Follow that to `src/state.rs:3364-3372` and `:268`: `MissingResource::Gateway::error_code()` is `Some("GatewayNotFound")`, so the member is present on every such refusal.
4. Confirm the wire answer: call `PutMaintenanceStartTime` with a well-formed `GatewayArn` the store holds no gateway under; the body carries `"ErrorCode": "GatewayNotFound"`, as `tests/all/missing_resource_refusal_is_one_answer.rs:70-77` + `:92` + `:208` assert.
5. Note the same doc's claim about `get_gateway`, `delete_gateway` and `update_gateway_information` is false of those files too (`get_gateway.rs:173-178`, `delete_gateway.rs:172-178`, `update_gateway_information.rs:178-184`).
