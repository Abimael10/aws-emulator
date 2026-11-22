Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 48db053985eafc982eac97a794d88fb53a9779ea

# Application$StatusMessage is never populated, so a FAILED registration reports a failed status it explains nothing about

_Filed: 2026-11-22T09:13:20Z_
_Operation: GetApplication | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `48db053985eafc982eac97a794d88fb53a9779ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StoredApplication::status_message` is written exactly once in this crate — as `None`, by `RegisterApplication` — and is never assigned again, so `Application$StatusMessage` is ALWAYS absent in every response that carries an `Application` (`GetApplication`, `RegisterApplication`).

That contradicts the crate's own statement of what the field is for and the way its two sibling records behave. The comment above the only write says `StatusMessage` explains a FAILED status, and a registration whose discovery cannot be carried out does settle to `FAILED`/`REGISTRATION_FAILED` (`register_application.rs:424-432`). But `StoredApplication::settle` materialises that transition by assigning `status` and `discovery_status` only — it never writes `status_message` — so the application reaches the very status the comment says the field explains while the field stays `None`.

The refusal that explains it is in hand at the failure site: `register_application.rs:195-196` computes `discovery_refusal`, and line 468 hands it to `record_operation_outcome`, which stores it as the operation record's `terminal_status_message`. The two sibling settle paths of this crate DO carry such a reason onto the record they settle: `StoredOperation::settle` copies `terminal_status_message` into `status_message` (state.rs:1613) and `StoredConfigurationCheckOperation::settle` sets `self.status_message = Some(reason)` for its non-success outcome (configuration_checks.rs:887). Only the application drops it, so the one record whose own comment names `StatusMessage` as the explanation of a failed status is the one record that never carries one, and `StoredApplication::status_message` is an inhabited-by-`None` field that `view()` faithfully reports as absent.

## Evidence

The rule the crate states, at the ONLY site that ever writes the field — services/service-com-amazonaws-ssmsap-ssmsap/src/ops/register_application.rs:373-375:
```
            // `StatusMessage` explains a failed status; a registration under way has
            // nothing to explain.
            status_message: None,
```
The application DOES reach that failed status — register_application.rs:421-435 (abridged):
```
            pending: Some(crate::state::PendingTransition {
                due_at: now + crate::state::TRANSITION_SPAN,
                change: crate::state::PendingChange::Status {
                    status: if discovery_refusal.is_some() {
                        crate::state::APPLICATION_STATUS_FAILED.to_string()
```
and the code that materialises it never writes the field — services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:857-871:
```
        match pending.change {
            PendingChange::Status {
                status,
                discovery_status,
            } => {
                ...
                self.status = status;
                self.discovery_status = discovery_status;
            }
```
The explanation exists and is kept elsewhere — register_application.rs:195-196 and 463-469:
```
        let discovery_refusal =
            crate::state::discovery::run_document(ctx, &region, &input.instances).await;
...
        let operation_id = region_state.record_operation_outcome(
            crate::state::OPERATION_TYPE_REGISTER_APPLICATION,
            &application, properties, now, discovery_refusal,
        );
```
Both SIBLING records carry their terminal reason, this one does not — src/state.rs:1612-1613 (`StoredOperation::settle`):
```
        self.status = self.terminal_status.clone();
        self.status_message = self.terminal_status_message.clone();
```
src/state/configuration_checks.rs:885-888 (`StoredConfigurationCheckOperation::settle`):
```
            Err(reason) => {
                self.status = OPERATION_STATUS_ERROR.to_string();
                self.status_message = Some(reason);
            }
```
And the projection reports whatever the field holds — src/state.rs:1060: `status_message: self.status_message.clone(),` while src/state.rs:791 (`pub status_message: Option<String>,`) is only ever set to `None` (`grep -n status_message src/state.rs src/ops/register_application.rs` shows the single write at register_application.rs:375).

Test coverage: tests/all/registration_outcome.rs pins the FAILED outcome (`OUTCOMES` rows with `status: "FAILED"`, `discovery_status: "REGISTRATION_FAILED"`) and asserts the relayed refusal only on the OPERATION record (`reported_operation`, lines 241-263, and the assertion at 326-330); `reported_outcome` (lines 225-239) reads only `Status`/`DiscoveryStatus`, so no test pins the application's `StatusMessage` either way.

## Steps to Reproduce

1. POST /register-application with {"ApplicationId":"outcome-0","ApplicationType":"HANA","Sid":"HDB","SapInstanceNumber":"00","Credentials":[{"DatabaseName":"HDB","CredentialType":"ADMIN","SecretId":"sap/hdb/admin"}],"Instances":["i-0ffffffffffffffff"]} — an instance id EC2 holds no record of, so the discovery cannot be carried out. Note the returned OperationId.
2. Wait out crate::state::TRANSITION_SPAN so the registration's outcome falls due.
3. POST /get-operation with that OperationId — Status is ERROR and StatusMessage carries the discovery refusal (the relayed 'Error executing SSM document - AccessDeniedException …').
4. POST /get-application with {"ApplicationId":"outcome-0"} — Application.Status is FAILED and Application.DiscoveryStatus is REGISTRATION_FAILED, but Application.StatusMessage is absent: the status the crate's own comment says the member explains is reported with nothing explaining it, while the same reason is held on the operation record.
