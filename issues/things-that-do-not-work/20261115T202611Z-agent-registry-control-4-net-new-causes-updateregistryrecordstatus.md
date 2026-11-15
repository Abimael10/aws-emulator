Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: agent-registry-control.approve-draft-record-directly-rejected agent-registry-control.update-registry-record-status-enforces-workflow-transitions agent-registry-control.rejected-record-resubmittable-only-after-edit agent-registry-control.deprecated-record-rejects-every-later-change
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control.approve-draft-record-directly-rejected|_error#4|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: agent-registry-control.update-registry-record-status-enforces-workflow-transitions|_error#12|AssertionError|raise AssertionError(f"expected the status change to be rejected, but it succeeded: {result!r}")
Issue-Key: agent-registry-control.rejected-record-resubmittable-only-after-edit|run#8|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message!r}"
Issue-Key: agent-registry-control.deprecated-record-rejects-every-later-change|run#9|AssertionError|assert code == "ValidationException", f"{op}: expected ValidationException, got {code}: {message!r}"
Issue-Commit: 65202ee683bcb66d82ebd52a38b6336bbaa34d8b

# agent-registry-control: 4 net-new causes — UpdateRegistryRecordStatus enforces no workflow transitions and SubmitRegistryRecordForApproval refuses with the wrong error class

> _Note: this issue was identified against the code base at commit `65202ee683bcb66d82ebd52a38b6336bbaa34d8b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRegistryRecordStatus

## Description
4 causes, all in the agentregistrycontrol service crate's record curation-workflow path, all net-new: none of them is owned by an open issue and none is blocked by an unimplemented stub. All four must be fixed and re-replayed.

Cause 1 of 4 — UpdateRegistryRecordStatus applies any curation status to a record regardless of the status the record is in: a DRAFT record that was never submitted is moved straight to APPROVED (and to REJECTED) with 202.
AWS: Smithy: UpdateRegistryRecordStatus PATCH /registries/{registryId}/records/{recordId}/status, success code 202, status member targets RegistryRecordStatus (DRAFT | PENDING_APPROVAL | APPROVED | REJECTED | DEPRECATED | CREATING | UPDATING | CREATE_FAILED | UPDATE_FAILED); operation doc: "Updates the status of a registry record as part of the registry's curation workflow, for example to approve or reject a record that is pending approval" — approval/rejection is a step OUT of PENDING_APPROVAL, not out of DRAFT. Modeled error ValidationException (errorKind=client, httpError=400, message @required). Recorded behavior: AWS refuses the move with HTTP 400 / ValidationException and the message "Invalid status transition from DRAFT to APPROVED. Valid transitions from DRAFT: PENDING_APPROVAL, DEPRECATED, DRAFT, UPDATING, UPDATE_PENDING_AUTH". Fixed text of that message: "Invalid status transition from " <current status> " to " <requested target status> ". Valid transitions from " <current status> ": " <comma-separated list of the targets that status allows>. The variable parts are the record's stored status and the request's status member; the recorded DRAFT row lists PENDING_APPROVAL, DEPRECATED, DRAFT, UPDATING, UPDATE_PENDING_AUTH. The refused call must leave the record's stored status untouched.
Emulator: returns HTTP 202 with status 'APPROVED' (and on the second attempt 'REJECTED') and writes that status into the record, so a DRAFT record is approved without ever being reviewed.
Why: the approval workflow is not a workflow at all — any caller with UpdateRegistryRecordStatus permission can approve or reject a record that no one submitted, and a client that relies on ValidationException to detect an out-of-order curation step sees a success instead, so audit logic built on "a record can only be approved from PENDING_APPROVAL" silently passes on records that skipped review.
Fix: UpdateRegistryRecordStatus's handler/validation — add the source-status→target-status transition matrix and raise ValidationException (400) with AWS's message template before mutating record state. Today the handler checks only the registry's READY status and the four asynchronous statuses and then assigns input.status unconditionally.
Traces: agent-registry-control.approve-draft-record-directly-rejected.

Cause 2 of 4 — UpdateRegistryRecordStatus accepts PENDING_APPROVAL as an explicit target status.
AWS: recorded behavior only for the rule; the model grounds the surface (status targets RegistryRecordStatus and the enum includes PENDING_APPROVAL, so the value passes framework validation, and ValidationException httpError=400 is modeled). PENDING_APPROVAL is reached only by SubmitRegistryRecordForApproval ("Submits a DRAFT registry record for approval"); AWS refuses it as a curation target with HTTP 400 / ValidationException and the message "Invalid target status: PENDING_APPROVAL". Fixed text: "Invalid target status: "; the variable part is the requested status value from the request. The record must stay in the status it had (DRAFT in the check).
Emulator: returns HTTP 202 with status 'PENDING_APPROVAL' and moves the record into the approval workflow through the status API.
Why: a caller can place a record in PENDING_APPROVAL without going through submission, bypassing the registry's approval configuration (auto-approval rules are evaluated only on submit), so a record that AWS would have auto-approved sits pending — and a client testing the documented submit-then-approve path cannot distinguish a legal from an illegal entry into the workflow.
Fix: UpdateRegistryRecordStatus's validation — reject PENDING_APPROVAL as a request target with ValidationException (400) and that message, separately from the transition matrix of Cause 1.
Traces: agent-registry-control.update-registry-record-status-enforces-workflow-transitions.

Cause 3 of 4 — SubmitRegistryRecordForApproval refuses a non-DRAFT record with ConflictException (409) and an emulator-authored message instead of ValidationException (400) with AWS's wording.
AWS: Smithy: SubmitRegistryRecordForApproval POST /registries/{registryId}/records/{recordId}/submit-for-approval, success 202, doc "Submits a DRAFT registry record for approval"; both ValidationException (400) and ConflictException (409) are modeled, and recorded behavior shows the record's status precondition refused as HTTP 400 / ValidationException with the message "Cannot submit the record for approval in current status: REJECTED". Fixed text: "Cannot submit the record for approval in current status: "; the only variable part is the record's stored RegistryRecordStatus. No record id and no restatement of the rule appear in it. The refused submit must leave the record in its stored status.
Emulator: answers HTTP 409 / ConflictException with "record <recordId> is in status REJECTED; only a DRAFT record can be submitted for approval" — wrong status class, wrong modeled error shape, and a message that interpolates the generated record id and appends a rule clause AWS does not emit.
Why: SDK error handling branches on the error code and on the 4xx class; a client catching ValidationException for an out-of-state submission never matches, while generic retry/conflict-resolution paths keyed on ConflictException fire on what AWS treats as a non-retryable validation failure. The open issue 20261105T120615Z-agent-registry-control-submitregistryrecordforapproval-responses-diverge-from-aws-on-timestamp-encoding.md records the same wrong error class for this precondition from the wire corpus (its keys are wire keys for status APPROVED); this behavioral check is not carried by it, so the cause is filed here and should be closed together with it.
Fix: SubmitRegistryRecordForApproval's status precondition (services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/submit_registry_record_for_approval.rs, the `record.status != RECORD_DRAFT` branch around lines 222-234) — raise ValidationException (400) with AWS's template, interpolating only the current status.
Traces: agent-registry-control.rejected-record-resubmittable-only-after-edit.

Cause 4 of 4 — a DEPRECATED record is not treated as terminal: every later change is refused with the generic wrong-status error instead of the terminal-state ValidationException.
AWS: recorded behavior only for the terminal rule; the model grounds it (UpdateRegistryRecordStatus doc: deprecation makes a record "no longer discoverable"; RegistryRecordStatus includes DEPRECATED; ValidationException httpError=400). AWS refuses SubmitRegistryRecordForApproval, UpdateRegistryRecord and UpdateRegistryRecordStatus on a DEPRECATED record with HTTP 400 / ValidationException and the single message "Cannot update registry record in DEPRECATED status (terminal state)" — that text is entirely fixed (the only variable part is the status name DEPRECATED, taken from the record's stored status). The record keeps its status and its statusReason, a refused edit applies nothing, and DeleteRegistryRecord still removes it.
Emulator: SubmitRegistryRecordForApproval on the deprecated record answers HTTP 409 / ConflictException "record <recordId> is in status DEPRECATED; only a DRAFT record can be submitted for approval"; UpdateRegistryRecordStatus has no terminal check at all and would re-approve the deprecated record (it assigns input.status unconditionally — see Cause 1).
Why: deprecation is the service's retirement step; if it is not terminal, a retired record can be dragged back into the workflow and re-approved, so consumers see a record the registry owner retired become discoverable again. Clients that detect "this record is retired" by catching ValidationException with the terminal-state message instead get a 409 conflict they will retry.
Fix: a shared terminal-state guard in record state, checked by SubmitRegistryRecordForApproval, UpdateRegistryRecordStatus and UpdateRegistryRecord before any mutation — raise ValidationException (400) with the terminal-state message whenever the stored status is DEPRECATED, and take precedence over the generic status-precondition message of Cause 3.
Traces: agent-registry-control.deprecated-record-rejects-every-later-change.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built and booted from this checkout: 4/21 passed; agent-registry-control 4/18 (14 failed). The clusters marked ALREADY FILED (UpdateRegistry, UpdateRegistryRecord stubs) and OWNED by open issues are excluded; the four below are the surviving net-new failures.

Cause 1 — check agent-registry-control.approve-draft-record-directly-rejected [mismatch]:
  AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 202, ...}, 'recordId': '<generated>', 'status': 'APPROVED', 'statusReason': 'skip review', 'updatedAt': ...}
  Expected: HTTP 400, Error.Code 'ValidationException', Error.Message "Invalid status transition from DRAFT to APPROVED. Valid transitions from DRAFT: PENDING_APPROVAL, DEPRECATED, DRAFT, UPDATING, UPDATE_PENDING_AUTH" (and the same for target REJECTED), with the record still DRAFT afterwards.
  Actual: 202 with status APPROVED/REJECTED written to the record. (The registry/record ids and updatedAt in the capture are this run's generated values, not part of the requirement.)

Cause 2 — check agent-registry-control.update-registry-record-status-enforces-workflow-transitions [mismatch]:
  AssertionError: expected the status change to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 202, ...}, 'status': 'PENDING_APPROVAL', 'statusReason': 'skip review', ...}
  Expected: HTTP 400 ValidationException with Error.Message "Invalid target status: PENDING_APPROVAL", record still DRAFT.
  Actual: 202, record moved to PENDING_APPROVAL.

Cause 3 — check agent-registry-control.rejected-record-resubmittable-only-after-edit [mismatch]:
  AssertionError: expected ValidationException, got ConflictException: 'record c1bcfe3b8ac1 is in status REJECTED; only a DRAFT record can be submitted for approval'
  Expected: HTTP 400 ValidationException, Error.Message "Cannot submit the record for approval in current status: REJECTED".
  Actual: HTTP 409 ConflictException with the emulator's own sentence ('c1bcfe3b8ac1' is this run's generated record id and is not part of AWS's template).

Cause 4 — check agent-registry-control.deprecated-record-rejects-every-later-change [mismatch]:
  AssertionError: SubmitRegistryRecordForApproval: expected ValidationException, got ConflictException: 'record 9ea63682279c is in status DEPRECATED; only a DRAFT record can be submitted for approval'
  Expected: HTTP 400 ValidationException, Error.Message "Cannot update registry record in DEPRECATED status (terminal state)" for SubmitRegistryRecordForApproval, UpdateRegistryRecord and UpdateRegistryRecordStatus on a DEPRECATED record; status and statusReason unchanged; DeleteRegistryRecord still succeeds.
  Actual: HTTP 409 ConflictException with the wrong-status message; UpdateRegistryRecordStatus has no terminal guard (src/ops/update_registry_record_status.rs assigns `record.status = input.status` at line 275 after checking only the registry status and the four asynchronous statuses).

## Steps to Reproduce
1. git_clone(), then build and boot the emulator from the checkout and replay the behavioral suite simparity-behavioral-aws-v1 (or run the four checks named below).
2. Cause 1 — CreateRegistry, wait READY, CreateRegistryRecord (recordType CUSTOM, descriptors {"custom": {"data": "{}"}}), wait DRAFT. Call UpdateRegistryRecordStatus(status='APPROVED', statusReason='skip review') on the never-submitted DRAFT record: the emulator answers 202 and approves it; AWS answers 400 ValidationException "Invalid status transition from DRAFT to APPROVED. Valid transitions from DRAFT: PENDING_APPROVAL, DEPRECATED, DRAFT, UPDATING, UPDATE_PENDING_AUTH". Repeat with status='REJECTED'.
3. Cause 2 — on the same DRAFT record call UpdateRegistryRecordStatus(status='PENDING_APPROVAL', statusReason='skip review'): the emulator answers 202 and moves the record to PENDING_APPROVAL; AWS answers 400 ValidationException "Invalid target status: PENDING_APPROVAL" and leaves it DRAFT.
4. Cause 3 — submit a DRAFT record for approval, then UpdateRegistryRecordStatus(status='REJECTED'), then call SubmitRegistryRecordForApproval again: the emulator answers 409 ConflictException 'record <id> is in status REJECTED; only a DRAFT record can be submitted for approval'; AWS answers 400 ValidationException "Cannot submit the record for approval in current status: REJECTED".
5. Cause 4 — submit a record, approve it, then UpdateRegistryRecordStatus(status='DEPRECATED', statusReason='retired'). Now call SubmitRegistryRecordForApproval, UpdateRegistryRecord(description=...) and UpdateRegistryRecordStatus(status='APPROVED') on it: each must be refused with 400 ValidationException "Cannot update registry record in DEPRECATED status (terminal state)", the record must stay DEPRECATED with statusReason 'retired' and no description, and DeleteRegistryRecord must then remove it (GetRegistryRecord → ResourceNotFoundException "Registry record with ID <recordId> not found.").
