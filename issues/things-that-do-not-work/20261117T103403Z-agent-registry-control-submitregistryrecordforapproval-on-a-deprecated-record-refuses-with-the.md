Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: agent-registry-control.deprecated-record-rejects-every-later-change
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control.deprecated-record-rejects-every-later-change|run#9|AssertionError|assert message == "Cannot update registry record in DEPRECATED status (terminal state)", f"{op}: unexpected message {message!r}"
Issue-Commit: d9284a585e64f4031fc42385c92576a2c8f3605b

# SubmitRegistryRecordForApproval on a DEPRECATED record refuses with the generic wrong-status message instead of AWS's terminal-state message

> _Note: this issue was identified against the code base at commit `d9284a585e64f4031fc42385c92576a2c8f3605b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitRegistryRecordForApproval

## Description
1 cause, net-new as a replay key: the open issue 20261115T202611Z-agent-registry-control-4-net-new-causes-updateregistryrecordstatus.md (its Cause 4) describes the same terminal-DEPRECATED rule, but it is keyed on the error-CLASS assertion (`assert code == "ValidationException"`), and that part is now fixed — SubmitRegistryRecordForApproval does answer HTTP 400 / ValidationException. The check now fails one assertion later, on the message, and the suite did not report it as OWNED, so the remaining half of that rule is filed here and should be closed together with it. The sibling operation UpdateRegistryRecordStatus already carries the correct guard (services/.../src/ops/update_registry_record_status.rs, the `is_terminal_record_status` branch), so only the submit path still diverges. UpdateRegistryRecord, the third operation the check exercises, is an unimplemented stub covered by its own filed issue and is not part of this one.

Cause 1 of 1: SubmitRegistryRecordForApproval does not treat DEPRECATED as the record lifecycle's terminal status; it folds it into the generic "not a DRAFT record" precondition and emits that precondition's message.

AWS: Smithy: SubmitRegistryRecordForApproval POST /registries/{registryId}/records/{recordId}/submit-for-approval, success code 202, doc "Submits a DRAFT registry record for approval"; ValidationException is modeled with errorKind=client, httpError=400, and its `message` member is @required. RegistryRecordStatus includes DEPRECATED. The terminal rule itself is recorded behavior only: on a record whose stored status is DEPRECATED, AWS refuses SubmitRegistryRecordForApproval (and UpdateRegistryRecord and UpdateRegistryRecordStatus) with HTTP 400, Error.Code `ValidationException` and Error.Message "Cannot update registry record in DEPRECATED status (terminal state)". The message's fixed text is "Cannot update registry record in " + <the record's stored status> + " status (terminal state)"; the only variable part is the stored status name, which is DEPRECATED for the only terminal status this lifecycle has. No record id, no registry id and no restatement of the DRAFT-only rule appear in it. This terminal message takes PRECEDENCE over the generic status-precondition message "Cannot submit the record for approval in current status: <status>", which AWS emits for the non-terminal non-DRAFT statuses (e.g. REJECTED, APPROVED, PENDING_APPROVAL). The refused call must change nothing: the record stays DEPRECATED with its stored statusReason, and DeleteRegistryRecord still removes it.

Emulator: answers HTTP 400 / ValidationException — the right class — but with the generic message "Cannot submit the record for approval in current status: DEPRECATED". The handler has only one status branch, `record.status != RECORD_DRAFT`, which formats `NOT_SUBMITTABLE_IN_STATUS` + the stored status; there is no `is_terminal_record_status` check ahead of it, unlike UpdateRegistryRecordStatus which checks the terminal status first and emits `terminal_record_status_message`. So the same emulator reports two different messages for the same terminal condition depending on which operation is called.

Why: a client distinguishes "this record is retired, stop" from "this record is simply not in DRAFT, edit it and resubmit" by the error message, since both arrive as 400 ValidationException with the same code. Matching on AWS's terminal-state text never fires against the emulator, so retry/resubmit logic treats a permanently retired record as a transient wrong-status condition and loops on a submission that can never succeed; conversely, code that keys the generic message to mean "resubmittable after an edit" will try to revive a record AWS considers terminal. The inconsistency with UpdateRegistryRecordStatus, which already returns the terminal wording, also makes the emulator self-contradictory about the same record.

Fix: SubmitRegistryRecordForApproval's status precondition — check the shared terminal-record-status guard in record state BEFORE the generic non-DRAFT branch and raise ValidationException (400) with the terminal-state message template, reusing the same state helper UpdateRegistryRecordStatus already calls, so the two operations cannot drift again. Leave the generic "Cannot submit the record for approval in current status: <status>" message for the non-terminal statuses.

Traces: agent-registry-control.deprecated-record-rejects-every-later-change.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built and booted from this checkout: 11/21 passed; agent-registry-control 11/18 (7 failed). Every other agent-registry-control and agent-registry-control__cloudformation failure in this run is marked ALREADY FILED (unimplemented UpdateRegistry / UpdateRegistryRecord stubs) or OWNED by an open issue; this is the only surviving failure.

Check agent-registry-control.deprecated-record-rejects-every-later-change [mismatch]:
  AssertionError: SubmitRegistryRecordForApproval: unexpected message 'Cannot submit the record for approval in current status: DEPRECATED'
  at `assert message == "Cannot update registry record in DEPRECATED status (terminal state)"`

  Expected (AWS): HTTP 400, Error.Code 'ValidationException', Error.Message "Cannot update registry record in DEPRECATED status (terminal state)" — fixed text with the record's stored status as its only variable part — for SubmitRegistryRecordForApproval on a DEPRECATED record; the record afterwards still DEPRECATED with statusReason unchanged and no description applied; DeleteRegistryRecord still removing it and the following GetRegistryRecord returning ResourceNotFoundException "Registry record with ID <recordId> not found."
  Actual (emulator): HTTP 400, Error.Code 'ValidationException' (correct class), Error.Message 'Cannot submit the record for approval in current status: DEPRECATED' — the generic non-DRAFT precondition message.

Source of the divergence in the checkout: services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/submit_registry_record_for_approval.rs lines 226-240 — a single `if record.status != crate::state::RECORD_DRAFT` branch formatting `crate::state::NOT_SUBMITTABLE_IN_STATUS` + `record.status`, with no terminal-status check before it. Contrast services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/update_registry_record_status.rs lines 246-257, which checks `crate::state::is_terminal_record_status(&record.status)` and returns `crate::state::terminal_record_status_message(&record.status)`.

Model grounding: smithy_query(operation="SubmitRegistryRecordForApproval") — http POST /registries/{registryId}/records/{recordId}/submit-for-approval, code 202; errors include ValidationException (errorKind=client, httpError=400) and ConflictException (409); output.status targets RegistryRecordStatus which includes DEPRECATED.

## Steps to Reproduce
1. git_clone(), then build and boot the emulator from the checkout and replay the behavioral suite simparity-behavioral-aws-v1, or run the single check agent-registry-control.deprecated-record-rejects-every-later-change.
2. CreateRegistry(name=<unique>); poll GetRegistry until status == READY.
3. CreateRegistryRecord(registryId=<id>, name=<unique>, recordType='CUSTOM', descriptors={"custom": {"data": "{}"}}); poll GetRegistryRecord until status == DRAFT.
4. SubmitRegistryRecordForApproval(registryId, recordId) -> PENDING_APPROVAL; UpdateRegistryRecordStatus(status='APPROVED', statusReason='reviewed'); UpdateRegistryRecordStatus(status='DEPRECATED', statusReason='retired'). Confirm the record reads back DEPRECATED.
5. Call SubmitRegistryRecordForApproval(registryId, recordId) on the now-DEPRECATED record. Observed: HTTP 400 ValidationException with Error.Message 'Cannot submit the record for approval in current status: DEPRECATED'. Expected: HTTP 400 ValidationException with Error.Message "Cannot update registry record in DEPRECATED status (terminal state)".
6. The rest of the check then verifies the same terminal message from UpdateRegistryRecord and UpdateRegistryRecordStatus, that the record still reads DEPRECATED with statusReason 'retired' and no description, and that DeleteRegistryRecord removes it (GetRegistryRecord -> ResourceNotFoundException "Registry record with ID <recordId> not found."). UpdateRegistryRecord is a separately filed unimplemented stub, so the check cannot pass until that stub lands too; the submit-path message above is the defect filed here.
