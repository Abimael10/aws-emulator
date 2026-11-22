Issue-Source: trace-sweep-divergence
Issue-Trace: agent-registry-control-edits-return-pending-and-rejected-records-to-draft
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: agent-registry-control-edits-return-pending-and-rejected-records-to-draft
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control|GetRegistryRecord|2xx->2xx|$
Issue-Related: 20261104T045821Z-agent-registry-control-agentregistrycontrol-record-reads-ignore-every-mutation-after-create.md
Issue-Related: 20261110T223112Z-agent-registry-control-record-lifecycle-status-transitions-and-listing-results-diverge.md
Issue-Commit: 38e24911c56fad04803701729ab45a93a1394fb2

# UpdateRegistryRecord does not withdraw an edited PENDING_APPROVAL record to DRAFT

> _Note: this issue was identified against the code base at commit `38e24911c56fad04803701729ab45a93a1394fb2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRegistryRecord

## Description
This bundle has ONE cause.

Cause 1 of 1: an edit applied to a registry record that is inside the approval workflow does not take the record back out of it, and the edited field is not reflected on the read path (fixture agent-registry-control-edits-return-pending-and-rejected-records-to-draft, step task#9).

AWS: Smithy (com.amazonaws.agentregistrycontrol#AgentRegistryControl): UpdateRegistryRecord PATCH /registries/{registryId}/records/{recordId}, success 202, documented as "Updates a registry record. The update is asynchronous: the record is returned with the UPDATING status while it is processed. Fields that use update wrappers follow PATCH semantics: omit the field to leave it unchanged." GetRegistryRecord GET /registries/{registryId}/records/{recordId}, 200, output member status required (enum RegistryRecordStatus = DRAFT, PENDING_APPROVAL, APPROVED, REJECTED, DEPRECATED, CREATING, UPDATING, CREATE_FAILED, UPDATE_FAILED) and optional description (Description, @length 1..4096, @sensitive). For the transition rule itself the model says nothing, so: recorded behavior only — the recording shows that updating a record whose status is PENDING_APPROVAL (and likewise REJECTED) withdraws it from the approval workflow, so the record settles at status DRAFT and the subsequent GetRegistryRecord returns the newly supplied description. The recorded GetRegistryRecord is a 200 whose body carries status DRAFT plus the edited description; no error code or message is involved in this step.

Emulator: GetRegistryRecord answers 200 but reports status 'PENDING_APPROVAL' and omits the description member entirely, so the fixture's poll for status DRAFT never settles (the replay reports "$.status poll: not in ['DRAFT'] after 31 attempt(s)", poll capped at 60s). Every other member of the record body is served.

Why: a client that edits a record sitting in PENDING_APPROVAL and then waits for the record to come back to DRAFT before re-submitting it waits forever — the emulator never leaves the workflow state — and any client that branches on the record's status after an edit reads a stale workflow state and will drive the wrong next call. Because the edited description is also not served, a read-after-write of an updated record returns pre-edit content.

Fix: UpdateRegistryRecord's handler and the record state transition it performs (services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/update_registry_record.rs and the record state it writes): applying an update to a record in PENDING_APPROVAL or REJECTED must reset that record's status to DRAFT (through UPDATING) in addition to applying the PATCH-semantics field changes, and the updated members must be persisted so the GetRegistryRecord read path serves them.

Related already-open issues, named here because neither owns this cluster's replay key: 20261110T223112Z-agent-registry-control-record-lifecycle-status-transitions-and-listing-results-diverge.md describes this same status-reset cause as its "Cause 1 of 4" against the same fixture and step, so a fixer should close the two together rather than treating them as independent work; 20261104T045821Z-agent-registry-control-agentregistrycontrol-record-reads-ignore-every-mutation-after-create.md covers mutations being dropped from reads generally and overlaps the missing description half of this cluster; 20261111T015454Z-agent-registry-control-updateregistryrecord-stub.md records that UpdateRegistryRecord is still an unimplemented scaffold, which is the likely upstream reason no edit takes effect (Hypothesis).

Traces: agent-registry-control-edits-return-pending-and-rejected-records-to-draft#task#9 (cluster also covers #task#15).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200; 112 steps, 82 passed, 30 mismatched, score 73.2%).

Cluster: service=agent-registry-control|GetRegistryRecord|status=200->200|keys=$,$.description,$.status, verdict mismatch, representative fixture agent-registry-control-edits-return-pending-and-rejected-records-to-draft step task#9, affected_count=2 (also #task#15), owned_by=[] and stub_path="" (not covered by any open issue's replay key).

Expected (recorded AWS response, 200): status 'DRAFT', description 'Storm surge alerts', with name/recordId/recordArn/registryArn/recordType/descriptors/createdAt/updatedAt as recorded.
Actual (emulator, 200): status 'PENDING_APPROVAL', no description member; replay detail: "$.description: missing (expected 'Storm surge alerts', got None)", "$.status: value_mismatch (expected 'DRAFT', got 'PENDING_APPROVAL')", "$.status: poll: not in ['DRAFT'] after 31 attempt(s)", "$: poll capped at 60s".

The recorded record ids, ARNs, names and timestamps in that detail are per-run values and are not part of the requirement; the requirement is the status transition to DRAFT and the presence of the edited description.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
3. Fixture agent-registry-control-edits-return-pending-and-rejected-records-to-draft: create a registry, create a CUSTOM record, SubmitRegistryRecordForApproval so the record is PENDING_APPROVAL, then UpdateRegistryRecord to change the record's description.
4. Step task#9: GetRegistryRecord on that record and poll for status DRAFT. Expected: 200 with status 'DRAFT' and the edited description. Observed: 200 with status 'PENDING_APPROVAL' and no description member; the poll never settles (31 attempts, capped at 60s). Step task#15 of the same fixture reproduces it on the REJECTED-then-edited record.
