Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-memory-record-crud-not-found-errors
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-memory-record-crud-not-found-errors
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore|GetMemoryRecord|4xx->4xx|$.message
Issue-Key: bedrock-agentcore|DeleteMemoryRecord|4xx->4xx|$.message
Issue-Commit: fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8

# Memory-record not-found refusals use the record sentence for a missing memory container: GetMemoryRecord and DeleteMemoryRecord (2 causes)

> _Note: this issue was identified against the code base at commit `fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMemoryRecord

## Description
The memory-record data-plane operations perform ONE combined (container, record) lookup and answer the record-level not-found sentence for both preconditions. When the request's `memoryId` names no memory container at all, AWS refuses with the container-level sentence "MemoryItem does not exist"; the emulator answers the record-level "The memory record was not found" instead. HTTP status (404) and error class (ResourceNotFoundException) are correct on both operations, so the message is the whole signal and it is wrong. This issue bundles the 2 net-new causes replay found, both on the same fault.

Cause 1 of 2: GetMemoryRecord against a memoryId that names no memory container.
  AWS: Smithy (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json) models GetMemoryRecord as GET /memories/{memoryId}/memoryRecord/{memoryRecordId}, success 200; `memoryId` is @required @httpLabel (MemoryId, @length min 12, @pattern admitting the bare id or the memory ARN), `memoryRecordId` is @required @httpLabel (MemoryRecordId, @length 40..50, @pattern ^mem-[a-zA-Z0-9-_]*$); ResourceNotFoundException is modeled @httpError 404 with a single `message` member ("The exception that occurs when the specified resource does not exist..."). The model does not fix the message text, so: recorded behavior only. The recording shows 404 ResourceNotFoundException with message "MemoryItem does not exist". That sentence is ENTIRELY fixed text — it has no variable part and interpolates neither the memoryId nor the memoryRecordId from the request. It is the same container-level sentence the sibling record/memory operations produce for a missing container.
  Emulator: 404 ResourceNotFoundException with message "The memory record was not found" — the sentence that belongs to a DIFFERENT precondition (a record absent from an EXISTING container). It is likewise 100% fixed text (the verbatim value of the state constant), so nothing in it is a per-run value; the defect is that it is served at all for the container-missing case. Hypothesis: the handler's single container+record lookup cannot tell the two apart, so the one `None` arm emits the record constant unconditionally.
  Why: status and error code are identical for the two preconditions, so a client classifying this 404 can only use the message. It mistakes a deleted/never-created memory container for a live container missing one record, and reconciliation diverges: the container case means re-create the container, the record case means re-ingest the record. Any SDK conformance assertion matching AWS's fixed "MemoryItem does not exist" fails here.
  Fix: the not-found path of this operation's handler — gate on the memory container's existence BEFORE the record lookup and refuse with the fixed container sentence when `memoryId` (after id normalization) names no container; reach the record sentence only when the container exists and the record does not. Keep the 404 status and ResourceNotFoundException class, which already match. The container sentence belongs in a constant shared by every memory-record handler so the two levels stay distinct service-wide.
  Traces: bedrock-agentcore-memory-record-crud-not-found-errors#task#4.

Cause 2 of 2: DeleteMemoryRecord against a memoryId that names no memory container.
  AWS: Smithy models DeleteMemoryRecord as DELETE /memories/{memoryId}/memoryRecords/{memoryRecordId}, success 200, same @required @httpLabel members and the same ResourceNotFoundException @httpError 404 with one `message` member. Message text is not modeled, so: recorded behavior only — the recording shows 404 ResourceNotFoundException, message "MemoryItem does not exist", again wholly fixed text with no request- or state-derived part.
  Emulator: 404 ResourceNotFoundException with "The memory record was not found".
  Why: same as Cause 1 — the two distinct not-found preconditions become indistinguishable on the wire, so delete-retry and idempotency logic that branches on the message takes the wrong branch.
  Fix: same component on this operation's handler — the `None` arm of the combined remove-record lookup must be preceded by a container-existence gate.
  Traces: bedrock-agentcore-memory-record-crud-not-found-errors#task#5.

Relation to already-filed work (none of these covers these two wire causes, and each is named here as required): 20261114T214757Z-bedrock-agentcore-deletememoryrecord-cannot-distinguish-a-missing-memory-container-from.md is the BEHAVIORAL filing of the same DeleteMemoryRecord wording defect; it carries a behavioral check id, not this corpus' replay key, and does not mention GetMemoryRecord. 20261108T080658Z-bedrock-agentcore-batchcreatememoryrecords-not-found-refusal-reworded-emulator-authored-sentence.md and 20261112T015454Z-bedrock-agentcore-listmemoryextractionjobs-not-found-refusal-is-reworded-generic-memory.md are the same family on DIFFERENT operations (BatchCreateMemoryRecords, ListMemoryExtractionJobs) and own those steps of this fixture, not task#4/task#5.

## Evidence
From the replay of simparity-wire-aws-v1 against the booted emulator (bootstrap_signal=false, emulator_up=true; 73/112 steps passed, score 65.2%, 12 divergence clusters).

[mismatch] fixture bedrock-agentcore-memory-record-crud-not-found-errors step task#4 — GetMemoryRecord
  expected status 404, actual 404 (match)
  $.message: value_mismatch (expected 'MemoryItem does not exist', got 'The memory record was not found')

[mismatch] fixture bedrock-agentcore-memory-record-crud-not-found-errors step task#5 — DeleteMemoryRecord
  expected status 404, actual 404 (match)
  $.message: value_mismatch (expected 'MemoryItem does not exist', got 'The memory record was not found')

Fixed vs variable text, both causes:
  - Expected 'MemoryItem does not exist' is 100% FIXED text: no member, id, name or number from the request or the emulator's state appears in it.
  - Actual 'The memory record was not found' is also 100% fixed text — the verbatim record-level constant. It is the right sentence for a record missing from a live container, so no part of it is a per-run value; serving it for a missing container is the defect.
  - The memoryId the fixture uses (a well-formed id for a container that was never created — the same family of id that step task#1 of this fixture uses) and the 'mem-'-prefixed record id fill the request; they are part of neither requirement and appear in neither message.

Same fixture, already-owned steps shown only to locate these two: task#1 (BatchCreateMemoryRecords) diverges the same way with 'Memory not found: <memoryId>' and is owned by 20261108T080658Z-...; task#4 and task#5 carry no owner.

Source of the divergence (not a request-specific value): services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/get_memory_record.rs lines 182-192 — the single `match store.memory_record(&memory_id, &input.memory_record_id)` whose `None` arm unconditionally builds ResourceNotFoundException { message: Some(crate::state::MEMORY_RECORD_NOT_FOUND_MESSAGE.to_string()) }, with no preceding check that the container named by `memory_id` exists; and the matching `match store.remove_memory_record(...)` `None` arm in src/ops/delete_memory_record.rs. The constant itself is defined in src/state.rs.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, then replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture bedrock-agentcore-memory-record-crud-not-found-errors.
3. Step task#4 issues GET /memories/<a well-formed memoryId for which no memory container was ever created>/memoryRecord/mem-<well-formed 40..50-char record id>. It is refused 404 ResourceNotFoundException (correct status and class) with message 'The memory record was not found' instead of AWS's fixed 'MemoryItem does not exist'.
4. Step task#5 issues DELETE /memories/<the same never-created memoryId>/memoryRecords/mem-<well-formed record id> and diverges identically.
5. To see the two preconditions collapse into one message: create a memory container with bedrock-agentcore-control CreateMemory, write a record with BatchCreateMemoryRecords, delete it, then GetMemoryRecord/DeleteMemoryRecord the dead record id. That (record-missing-in-a-live-container) case returns the identical 404 and the identical message, so nothing on the wire distinguishes it from steps 3-4.
6. Equivalent replay to confirm a fix: bedrock-agentcore-memory-record-crud-not-found-errors#task#4 and bedrock-agentcore-memory-record-crud-not-found-errors#task#5.
