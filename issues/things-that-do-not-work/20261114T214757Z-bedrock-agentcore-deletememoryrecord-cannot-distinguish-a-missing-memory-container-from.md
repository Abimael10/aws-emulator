Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.delete-memory-record-invalid-input-malformed-id
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.delete-memory-record-invalid-input-malformed-id|run#13|AssertionError|assert contrast_message == "MemoryItem does not exist", (
Issue-Commit: cf4f60c5fc13ba8044ce457fda63f711f871a11a

# DeleteMemoryRecord cannot distinguish a missing memory container from a missing record: the container-not-found refusal is worded "The memory record was not found" instead of AWS's fixed "MemoryItem does not exist"

> _Note: this issue was identified against the code base at commit `cf4f60c5fc13ba8044ce457fda63f711f871a11a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteMemoryRecord

## Description
Cause 1 of 1: DeleteMemoryRecord serves ONE not-found sentence for two distinct preconditions. When the `memoryId` in the path names no existing memory container at all, the emulator answers the record-level sentence instead of the container-level sentence AWS produces, so the refusal no longer identifies WHICH resource is absent.

AWS: Smithy (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json) models DeleteMemoryRecord as DELETE /memories/{memoryId}/memoryRecords/{memoryRecordId} with success code 200; `memoryId` is a @required @httpLabel (MemoryId, @length min 12, @pattern admitting the bare id or the memory ARN), `memoryRecordId` is a @required @httpLabel (MemoryRecordId, @length 40..50, @pattern ^mem-[a-zA-Z0-9-_]*$), and ResourceNotFoundException is modeled with @httpError 404 carrying a single `message` member ("The exception that occurs when the specified resource does not exist..."). The model does not fix the message text, so: recorded behavior only. The recording shows TWO distinct refusals on this one operation, and they are the requirement:
  (a) the `memoryId` names no memory container -> 404 ResourceNotFoundException, message "MemoryItem does not exist". This sentence is ENTIRELY fixed text: it has no variable part at all, and in particular it names neither the memoryId nor the memoryRecordId from the request. It is the same container-level sentence the sibling record operations produce for a missing container (see the open issue 20261108T080658Z-bedrock-agentcore-batchcreatememoryrecords-not-found-refusal-reworded-emulator-authored-sentence.md, which records AWS answering exactly "MemoryItem does not exist" for BatchCreateMemoryRecords into a container that does not exist).
  (b) the container EXISTS but holds no record with that `memoryRecordId` (including one a prior DeleteMemoryRecord removed) -> 404 ResourceNotFoundException, message "The memory record was not found" — also entirely fixed text with no variable part.
The status (404) and the error class (ResourceNotFoundException) are the same in both; only the message distinguishes them, so the message is the whole signal.

Emulator: for case (a) it answers 404 ResourceNotFoundException with "The memory record was not found" — case (b)'s sentence. The handler at services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/delete_memory_record.rs (the `handle` body, the `match store.remove_memory_record(&memory_id, &input.memory_record_id)` at lines 183-193) performs ONE combined lookup: its `None` arm unconditionally emits `crate::state::MEMORY_RECORD_NOT_FOUND_MESSAGE` (defined in src/state.rs, the constant "The memory record was not found"). There is no existence gate on the memory container before that lookup, so a missing container and a missing record inside a live container are indistinguishable on the wire.

Note on scope: the same check's FIRST assertion passes — a `memoryRecordId` violating ^mem-[a-zA-Z0-9-_]*$ is correctly refused 400 ValidationException with AWS's unanchored, value-free "1 validation error detected: Value at 'memoryRecordId' failed to satisfy constraint: Member must satisfy regular expression pattern: mem-[a-zA-Z0-9-_]*". Only the container-not-found wording diverges.

Relation to already-filed work (neither covers this): the FIXED issue 20261102T104223Z-bedrock-agentcore-memory-data-plane-refusal-messages-still-diverge-getevent.md (Cause 2) correctly changed this operation's record-level message to "The memory record was not found" for case (b); that half is right and this is NOT a regression of it — the defect is that the fix made that constant the answer for case (a) too, where AWS says something else. The open issues 20261108T080658Z-...batchcreatememoryrecords-not-found-refusal-reworded... and 20261112T015454Z-bedrock-agentcore-listmemoryextractionjobs-not-found-refusal-is-reworded-generic-memory.md are the same family of container-not-found wording defects on DIFFERENT operations (BatchCreateMemoryRecords, ListMemoryExtractionJobs); neither carries DeleteMemoryRecord or this check's replay key.

Why: a client that classifies this 404 by message — the only thing that differs between the two causes, since status and error code are identical — mistakes a deleted/never-created memory container for a live container missing one record. Retry and reconciliation logic diverges: case (a) means re-create the container, case (b) means re-ingest the record. Any equality or substring match on AWS's fixed "MemoryItem does not exist" fails here, and an SDK conformance test for the container-missing path cannot pass.

Fix: the not-found path of this operation's handler (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/delete_memory_record.rs, the `None` arm of the `remove_memory_record` match). It must gate on the memory container's existence first — when `memory_id` (after `crate::state::normalize_memory_id`) names no container, refuse 404 ResourceNotFoundException with AWS's fixed container sentence; reach `crate::state::MEMORY_RECORD_NOT_FOUND_MESSAGE` only when the container exists and the record does not. Keep the 404 status and the ResourceNotFoundException class, which already match on both paths. The container sentence should come from a constant shared with the other memory-record handlers so the two levels stay distinct service-wide.

Traces: bedrock-agentcore.delete-memory-record-invalid-input-malformed-id (its contrast assertion, check line 56).

## Evidence
From the replay of simparity-behavioral-aws-v1 against the booted emulator (bootstrap_signal=false; 33/68 checks passed, bedrock-agentcore 32/64).

[mismatch] bedrock-agentcore.delete-memory-record-invalid-input-malformed-id
  File "check_bedrock_agentcore_delete_memory_record_invalid_input_malformed_id.py", line 56, in run
    assert contrast_message == "MemoryItem does not exist", (
  AssertionError: expected message 'MemoryItem does not exist', got 'The memory record was not found'

The failing call is DeleteMemoryRecord against a memory container that does not exist (the check uses a well-formed memoryId that it never created) with a well-formed memoryRecordId ('mem-' + 44 chars, satisfying @length 40..50 and @pattern ^mem-[a-zA-Z0-9-_]*$). Expected and actual agree on HTTP 404 and on the error code ResourceNotFoundException; ONLY the message diverges.

Fixed vs variable text:
  - Expected 'MemoryItem does not exist' is 100% fixed text with NO variable part — it interpolates neither the memoryId nor the memoryRecordId from the request.
  - Actual 'The memory record was not found' is likewise 100% fixed text: it is the verbatim value of crate::state::MEMORY_RECORD_NOT_FOUND_MESSAGE. It is the correct sentence for a DIFFERENT precondition (a record missing from an EXISTING container), so no part of it is a per-run value — the defect is that this constant is served for the container-missing case at all.
  - The concrete ids this run used (the generated memoryId and the 'mem-'-prefixed record id) fill the request; they are not part of either requirement and appear in neither message.

Same check, passing half (for contrast, showing the defect is isolated to the not-found arm): DeleteMemoryRecord with a memoryRecordId of 44 'x' characters is correctly refused 400 ValidationException with '1 validation error detected: Value at \'memoryRecordId\' failed to satisfy constraint: Member must satisfy regular expression pattern: mem-[a-zA-Z0-9-_]*'.

Source of the divergence (not a request-specific value): services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/delete_memory_record.rs lines 183-193 — the single `match store.remove_memory_record(&memory_id, &input.memory_record_id)` whose `None` arm unconditionally builds ResourceNotFoundException { message: Some(crate::state::MEMORY_RECORD_NOT_FOUND_MESSAGE.to_string()) }, with no preceding check that the memory container named by `memory_id` exists.

## Steps to Reproduce
1. Build and boot the emulator from this checkout, then replay the behavioral suite simparity-behavioral-aws-v1.
2. Run check bedrock-agentcore.delete-memory-record-invalid-input-malformed-id.
3. Its first call — DELETE /memories/<well-formed memoryId>/memoryRecords/<44 'x' characters> — is correctly refused 400 ValidationException with AWS's unanchored, value-free pattern message. This assertion passes.
4. Its contrast call — DELETE /memories/<the SAME well-formed memoryId, for which no memory container was ever created>/memoryRecords/mem-<44-char well-formed suffix> — is refused 404 ResourceNotFoundException (correct status and class) with the message 'The memory record was not found' instead of AWS's fixed 'MemoryItem does not exist' for a memory container that does not exist. The check fails at line 56.
5. To see the two preconditions collapse into one message: create a memory container via bedrock-agentcore-control CreateMemory, write a record with BatchCreateMemoryRecords, delete it with DeleteMemoryRecord, then DeleteMemoryRecord the same dead id again — that (record-missing-in-a-live-container) case returns the identical 404 and the identical 'The memory record was not found' message, so nothing on the wire distinguishes it from step 4's missing container.
6. Equivalent replay: re-run bedrock-agentcore.delete-memory-record-invalid-input-malformed-id.
