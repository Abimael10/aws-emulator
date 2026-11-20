Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: e0a124297232ff094559dd74e057f6d8fc0e48e9

# A failed registration leaves Application$StatusMessage empty although the handler computed the refusal that explains it

_Filed: 2026-11-20T15:01:01Z_
_Operation: RegisterApplication | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e0a124297232ff094559dd74e057f6d8fc0e48e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`RegisterApplication` runs the discovery hop, keeps its refusal in `discovery_refusal`, and schedules the application to settle into `FAILED` / `REGISTRATION_FAILED` when that refusal is present. It hands the refusal to the operation record (`record_operation_outcome(..., discovery_refusal)`) but the `PendingChange::Status` it schedules for the APPLICATION carries only a status and a discovery status, so `StoredApplication::settle` never sets `status_message`.

`StoredApplication::status_message` is therefore written exactly once in the whole crate — to `None`, at registration — yet `StoredApplication::view` reports it as `Application$StatusMessage`. The result is that `GetApplication` on an application whose registration FAILED answers `Status: FAILED`, `DiscoveryStatus: REGISTRATION_FAILED` and NO `StatusMessage`, even though the service computed the sentence that explains that status in the very same handler invocation.

That contradicts the comment the `None` sits under ("`StatusMessage` explains a failed status"), and it contradicts the two sibling records of this crate, which both DO attach their explanation when the clock brings them to a non-success terminal status: `StoredOperation::settle` copies `terminal_status_message` into `status_message`, and `StoredConfigurationCheckOperation::settle` sets `status_message = Some(reason)`. The application is the one record of the three whose failure explains nothing.

## Evidence

The comment and the field it justifies, `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/register_application.rs:373-375`:
```
            // `StatusMessage` explains a failed status; a registration under way has
            // nothing to explain.
            status_message: None,
```

The same handler schedules the FAILED status WITHOUT a message, `register_application.rs:421-435`:
```
            pending: Some(crate::state::PendingTransition {
                due_at: now + crate::state::TRANSITION_SPAN,
                change: crate::state::PendingChange::Status {
                    status: if discovery_refusal.is_some() {
                        crate::state::APPLICATION_STATUS_FAILED.to_string()
```
while the refusal IS in hand and is given to the operation record two dozen lines later, `register_application.rs:463-469`:
```
        let operation_id = region_state.record_operation_outcome(
            crate::state::OPERATION_TYPE_REGISTER_APPLICATION,
            &application,
            properties,
            now,
            discovery_refusal,
        );
```

`StoredApplication::settle` can only move the two statuses — `services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:858-871`:
```
            PendingChange::Status {
                status,
                discovery_status,
            } => {
                ...
                self.status = status;
                self.discovery_status = discovery_status;
            }
```

The sibling records DO explain themselves at the same moment — `state.rs:2491-2493`:
```
                // `StatusMessage` explains a non-success status; an operation that is
                // still running has nothing to explain.
                status_message: None,
```
and `state.rs:1612-1613`:
```
        self.status = self.terminal_status.clone();
        self.status_message = self.terminal_status_message.clone();
```
plus `src/state/configuration_checks.rs:886-887`:
```
                self.status = OPERATION_STATUS_ERROR.to_string();
                self.status_message = Some(reason);
```

The field is reported on the wire, `state.rs:1060`: `status_message: self.status_message.clone(),` — and a grep of `src/` for `status_message` shows the application's copy is assigned nowhere except the `None` at `register_application.rs:375`.

Test coverage: `tests/all/registration_outcome.rs:320-331` asserts the explanation for the OPERATION record only —
```
        match status_message {
            // A record that did its work explains nothing.
            None => assert_eq!(
                row.operation_status, "SUCCESS",
                "case {case:?}: a failed registration must explain itself"
            ),
```
and `reported_outcome` (`registration_outcome.rs:230-238`) reads only `Status` and `DiscoveryStatus` off the application, so no test pins `Application$StatusMessage` either way.

## Steps to Reproduce

1. Mount this service beside the EC2 emulator and POST /register-application naming an Amazon EC2 instance id EC2 holds no record of, so `discovery::run_document` answers a refusal (the setup `tests/all/registration_outcome.rs::register_failed` uses).
2. Let the clock pass `TRANSITION_SPAN` and POST /get-application for that ApplicationId.
3. Observe `Application.Status == "FAILED"` and `Application.DiscoveryStatus == "REGISTRATION_FAILED"` with NO `StatusMessage` member.
4. POST /get-operation with the `OperationId` the registration returned: the record answers `Status == "ERROR"` with a `StatusMessage` carrying the relayed discovery refusal.
5. So the refusal that explains the application's FAILED status exists in the store and is reported for the operation record, while the application whose status it explains reports nothing — against the rule stated at register_application.rs:373-375 and against the two sibling records that attach their own explanation on settle.
