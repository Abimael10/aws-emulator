Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: agent-registry.edited-approved-record-keeps-published-revision-until-reapproved
Issue-Shape-Id: com.amazonaws.agentregistry#AgentRegistry
Issue-Key: agent-registry.edited-approved-record-keeps-published-revision-until-reapproved|_poll#9|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: ce79cfcc2d5bdf31295b2bed10a3822c9ac41993

# UpdateRegistryRecord does not withdraw an edited APPROVED record to DRAFT: the record settles back at APPROVED, so an edit-then-reapprove flow never completes

> _Note: this issue was identified against the code base at commit `ce79cfcc2d5bdf31295b2bed10a3822c9ac41993`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRegistryRecord

## Description
This bundle has ONE cause.

Cause 1 of 1: editing a registry record that is in status APPROVED does not take it back out of the approval workflow. The emulator settles the edited record back at APPROVED, so a client waiting for the record to return to DRAFT (the precondition for re-submitting it for approval, and for the data plane to keep serving the previously published revision) waits forever. Failing check: agent-registry.edited-approved-record-keeps-published-revision-until-reapproved.

AWS: recorded behavior only for the transition rule. Smithy (com.amazonaws.agentregistrycontrol#AgentRegistryControl) UpdateRegistryRecord is documented as "Updates a registry record. The update is asynchronous: the record is returned with the UPDATING status while it is processed. Fields that use update wrappers follow PATCH semantics: omit the field to leave it unchanged."; its output member `status` is required, of enum RegistryRecordStatus = APPROVED | CREATE_FAILED | CREATING | DEPRECATED | DRAFT | PENDING_APPROVAL | REJECTED | UPDATE_FAILED | UPDATING. The model states no rule about which status an edit settles at, so the requirement comes from the recording the check replays: an edit applied to an APPROVED record withdraws it from the published/approved state — the record passes through UPDATING and settles at DRAFT, carrying the edited descriptor content — while the data plane keeps serving the last APPROVED revision until the edit is submitted and approved again. No error code or message is involved: UpdateRegistryRecord is a success (202) and the following GetRegistryRecord is a 200 whose body carries status DRAFT and the edited descriptors.

Emulator: UpdateRegistryRecord accepts the edit on an APPROVED record and sets status UPDATING, but the status it owes the record after the asynchronous span is computed as DRAFT only when the record was PENDING_APPROVAL or REJECTED; for every other status (including APPROVED) it is `record.status.clone()`, i.e. the record settles back at APPROVED. GetRegistryRecord therefore reports UPDATING and then APPROVED forever and never reports DRAFT, so the check's poll for status DRAFT cannot converge (it ends on the harness's budget rather than on a single wrong response; every individual response is a 200 whose `status` is APPROVED). Because the poll never converges, the check's later assertions — that the data plane serves the pre-edit revision while the edit is a draft, and the re-approved revision after re-approval — are never reached and are NOT graded here.

Why: a client that edits an approved record and then waits for it to come back to DRAFT before calling SubmitRegistryRecordForApproval blocks indefinitely; a client that branches on the record's status after an edit reads a stale workflow state (APPROVED) and drives the wrong next call. Worse, the record is reported APPROVED while holding un-reviewed content, so the emulator presents edited-but-unapproved descriptors as approved — the opposite of the curation guarantee the approval workflow exists to provide.

Fix: the UpdateRegistryRecord handler's record state transition in the agent-registry-control crate — the set of statuses an edit withdraws to DRAFT must include APPROVED (an edit decides nothing about curation, but it does invalidate the approval), with the record passing through UPDATING and settling at DRAFT while the data plane continues to serve the last approved revision. Related already-open issue, named because it does not cover this case: 20261115T111315Z-agent-registry-control-updateregistryrecord-does-not-withdraw-an-edited-pending-approval.md covers PENDING_APPROVAL and REJECTED only (both of which the current code does handle); 20261110T223112Z-agent-registry-control-record-lifecycle-status-transitions-and-listing-results-diverge.md covers lifecycle transitions generally and should be closed together with this one if the fixer touches the same transition table. Neither carries this check's replay key, and the suite marked this check neither ALREADY FILED nor OWNED.

Traces: agent-registry.edited-approved-record-keeps-published-revision-until-reapproved. Source: services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/update_registry_record.rs lines 337-351 (`settles_at` is DRAFT only for RECORD_PENDING_APPROVAL / RECORD_REJECTED, else `record.status.clone()`) and lines 417-421 (status set to RECORD_UPDATING, `withdrawn` false for an APPROVED record).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 against this checkout: Total 6/12 passed (50.0%); mismatched 3, errored 3, skipped 0, unloadable 0; bootstrap_signal false. Per-service: agent-registry 6/12 (50%), 6 failed. Five of the six failures are marked OWNED by 20261116T002027Z-agent-registry-3-net-new-causes-searchdiscoverableregistryrecords-never.md and are excluded from this issue. The sixth is this one:

[error] agent-registry.edited-approved-record-keeps-published-revision-until-reapproved
  File "check_agent_registry_edited_approved_record_keeps_published_revision_until_reapproved.py", line 82, in _wait_record
    return _poll(ctx, lambda: arc.get_registry_record(registryId=registry_id, recordId=record_id), ...)
  File "...", line 63, in _poll
    raise TimeoutError(f"{what}: the run budget ended first; not graded")
  TimeoutError: record DRAFT: the run budget ended first; not graded

The failing poll is the check's line 543, `draft = _wait_record(ctx, arc, registry_id, record_id, "DRAFT")`, reached after: create registry (READY), create CUSTOM record with descriptors {"custom": {"data": "{\"rev\": 1}"}}, SubmitRegistryRecordForApproval + UpdateRegistryRecordStatus(status=APPROVED) (both succeeded — the check asserts status APPROVED and that assertion passed), the data-plane poll "approved revision served" (passed: BatchGetDiscoverableRegistryRecord served {"rev": 1}), then UpdateRegistryRecord with descriptors patched to {"rev": 2}. Expected: GetRegistryRecord reaches status 'DRAFT' holding descriptors {"rev": 2}. Observed: the status never leaves UPDATING→APPROVED, so the wait cannot converge.

That non-convergence is the emulator's response content, not a runner fault — no tool, TLS, name-resolution or live-account value is involved, and the emulator's own source shows the poll's target is unreachable by construction: services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/update_registry_record.rs

    let settles_at = if [crate::state::RECORD_PENDING_APPROVAL, crate::state::RECORD_REJECTED]
        .contains(&record.status.as_str()) { crate::state::RECORD_DRAFT.to_string() }
        else { record.status.clone() };

An APPROVED record therefore has settles_at == APPROVED and `withdrawn == false`; the handler sets record.status = RECORD_UPDATING and arms a transition back to APPROVED. The check's own distinction between its two timeout messages confirms nothing was mis-graded: an emulator that simply answered slowly raises AssertionError("... not reached within Ns"), whereas this run raised the TimeoutError variant, so the single-response verdict was never taken — the divergence is established from the transition rule itself, and the record id, registry id and timestamps in any one run are per-run values that are not part of the requirement.

Model evidence: UpdateRegistryRecord (PATCH /registries/{registryId}/records/{recordId}, async, 202) output member `status` required, enum RegistryRecordStatus includes DRAFT, PENDING_APPROVAL, APPROVED, UPDATING; the model prescribes no post-edit status, so the DRAFT withdrawal is recorded behavior only.

## Steps to Reproduce
1. Clone the repo and let the behavioral suite build and boot the emulator from this checkout; confirm bootstrap_signal false and that agent-registry/agent-registry-control answer (6 of agent-registry's 12 checks pass).
2. Replay agent-registry.edited-approved-record-keeps-published-revision-until-reapproved.
3. Manually: through AgentRegistryControl create a READY registry; create a CUSTOM record with descriptors {"custom": {"data": "{\"rev\": 1}"}} and wait for DRAFT; SubmitRegistryRecordForApproval then UpdateRegistryRecordStatus(status=APPROVED) and confirm status APPROVED; through AgentRegistry confirm BatchGetDiscoverableRegistryRecord serves {"rev": 1}.
4. Call UpdateRegistryRecord on that APPROVED record, patching descriptors to {"rev": 2} (descriptors={"optionalValue": {"custom": {"optionalValue": {"data": {"optionalValue": "{\"rev\": 2}"}}}}}). Then poll GetRegistryRecord. Expected: status passes through UPDATING and settles at DRAFT, with descriptors holding {"rev": 2}. Observed: status settles back at APPROVED and DRAFT is never reached, so the poll never converges.
5. After the fix, re-replay the whole check, not only the poll: it goes on to assert that while the edit is a draft the data plane still serves the APPROVED revision ({"rev": 1}) via BatchGetDiscoverableRegistryRecord, and that after SubmitRegistryRecordForApproval + UpdateRegistryRecordStatus(APPROVED) the data plane serves {"rev": 2}. Those two assertions were never reached in this run and are not graded by it.
