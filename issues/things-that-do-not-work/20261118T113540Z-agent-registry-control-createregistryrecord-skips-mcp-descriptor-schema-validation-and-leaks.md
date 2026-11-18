Issue-Source: trace-sweep-divergence
Issue-Trace: agent-registry-control-record-token-replay-rename-and-descriptor-errors
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: agent-registry-control-record-token-replay-rename-and-descriptor-errors
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control|CreateRegistryRecord|4xx->2xx|$.message
Issue-Key: agent-registry-control|CreateRegistryRecord|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: agent-registry-control|CreateRegistryRecord|4xx->2xx|status
Issue-Key: agent-registry-control|ListRegistryRecords|2xx->2xx|$.registryRecords[*]
Issue-Commit: f2831bfd9793e6f7d4548f15bac1de57747f4451

# CreateRegistryRecord skips MCP descriptor schema validation and leaks the refused record into ListRegistryRecords

> _Note: this issue was identified against the code base at commit `f2831bfd9793e6f7d4548f15bac1de57747f4451`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRegistryRecord

## Description
This issue has TWO causes, both on the CreateRegistryRecord write path of agent-registry-control. Cause 2 is the stored consequence of Cause 1 and is observed through ListRegistryRecords; a fixer must close BOTH and re-replay both steps named under Traces.

Related, and does NOT cover these wire steps: the open issue 20261117T060413Z-agent-registry-control-createregistryrecord-accepts-an-mcpserver-descriptor-that-matches-no.md describes the same defect, but it was filed from the BEHAVIORAL corpus (Issue-Corpus: simparity-behavioral-aws-v1) and carries only a behavioral check id as its replay key — it has no Issue-Trace/Issue-Step binding it to any wire fixture step. The wire replay reports both clusters below with an empty owned_by, i.e. no open issue carries their replay key, so they are filed here with the wire bindings a fixer can re-replay. Also related and not covering these: 20261111T015454Z-agent-registry-control-3-net-new-causes-createregistryrecord.md (its Cause 1, descriptor/customMetadata validation on this same write path).

Cause 1 of 2: CreateRegistryRecord performs no MCP-schema validation of descriptors.mcpServer.data, so a payload matching no supported MCP server schema version is accepted.
AWS: Smithy: CreateRegistryRecord POST /registries/{registryId}/records, success code 202; input member `descriptors` -> Descriptors is @required; Descriptors.mcpServer -> McpServerDescriptor is documented "Descriptor that defines the content of an MCP (Model Context Protocol) server registry record, including the server definition and its tool definitions. The content is validated against the MCP protocol schema."; McpServerDescriptor.data -> DescriptorData @length 1..102400, @sensitive, documented "The MCP server descriptor content, serialized as descriptor payload data."; McpServerDescriptor.dataSchemaVersion -> DataSchemaVersion, "The schema version of the descriptor payload." Modeled error ValidationException (errorKind client, httpError 400, members message @required and reason -> ValidationExceptionReason default FieldValidationFailed). So the mcpServer payload is matched against the service's supported MCP server schema versions BEFORE anything is written, and a payload matching none is refused with HTTP 400 and the x-amzn-errortype header naming ValidationException. Message template, as recorded: the entire message is fixed text — "mcp.server data does not match any supported version" — with NO variable parts (the leading "mcp.server" is the descriptor path the service names for the offending member, not a value from the request), so it must be reproduced verbatim for this error class. A payload that does carry the MCP server fields stays accepted with 202.
Emulator: the step is answered HTTP 202 with the success body ({recordArn, status}); there is no `message` member and no `x-amzn-errortype` header at all. The only descriptor check on this path is JSON parseability (services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/state/descriptor_payload.rs refuses solely with "data is not valid JSON: <reading>"), so any well-formed JSON object is admitted whatever MCP schema version it matches.
Why: a client is told an MCP server definition no MCP schema version accepts was admitted to the registry. Its correction path — catch ValidationException, repair the descriptor, resubmit — is never taken, so off-schema MCP records enter the registry and are published to consumers; the failure surfaces later, at an MCP consumer resolving the record, where the client has no handler. The accepted record also occupies the requested name, so a retry under that name then collides with an unexpected ConflictException.
Fix: CreateRegistryRecord's input validation in the operation's handler — alongside the existing descriptor_payload_refusal call, before any id is minted or state written, validate descriptors.mcpServer.data against the supported MCP server schema versions and return the modeled ValidationException (400, reason FieldValidationFailed) with the fixed message above; the same rule belongs on every other write path that accepts a descriptor (UpdateRegistryRecord), so it belongs in the shared descriptor-validation module rather than in one handler.
Traces: fixture agent-registry-control-record-token-replay-rename-and-descriptor-errors, step task#11 (expected 400, got 202).

Cause 2 of 2: because the off-schema create is not refused, the record it should never have written is persisted and reported by ListRegistryRecords.
AWS: Smithy: ListRegistryRecords POST /registries/{registryId}/records-list, success code 200; output member `registryRecords` -> RegistryRecordSummaryList is @required; input member `filters` -> RegistryRecordFilterList selects records by name/status/recordType. A create refused with ValidationException writes no state, so a subsequent list of the registry (filtered on the refused record's name) returns an EMPTY registryRecords list.
Emulator: the list answers 200 but registryRecords[0] is present — a summary for the record the refused create wrote (recordType MCP, status CREATING, createdByAutoDetection false, customMetadataSchemaComplianceStatus COMPLIANT), where the recording has no element at that index. The specific recordId/recordArn/name/timestamps in the actual body are per-run values and are not the requirement; the requirement is that the list carries no entry for a create that must have been refused.
Why: a client that probes for the record after handling the refusal sees it exist, so it treats an off-schema descriptor as published; reconciliation and clean-up loops that list a registry to decide what was created diverge permanently from AWS, and the leaked record keeps the name reserved.
Fix: the same handler/validation seam as Cause 1 — the refusal must happen before the record is inserted into the registry's record map and before the idempotency answer is remembered, so no state is left behind; nothing in ListRegistryRecords' own filtering or paging needs changing.
Traces: fixture agent-registry-control-record-token-replay-rename-and-descriptor-errors, step task#13 (ListRegistryRecords, status 200, $.registryRecords[0] extra).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 112 steps, 108 passed, 4 mismatched, 2 divergence clusters, score 96.4% (emulator_up true, bootstrap_signal false, so the emulator is serving the corpus).

Cluster 1 — fixture agent-registry-control-record-token-replay-rename-and-descriptor-errors, step task#11, operation CreateRegistryRecord, verdict mismatch:
  detail: status: expected 400, got 202
          $.message: missing (expected 'mcp.server data does not match any supported version', got None)
          $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  Expected: HTTP 400, x-amzn-errortype: ValidationException, Error.Message exactly "mcp.server data does not match any supported version" (entirely fixed text), no record written.
  Actual: HTTP 202 with the CreateRegistryRecord success body and no error members or header.

Cluster 2 — same fixture, step task#13, operation ListRegistryRecords, verdict mismatch:
  detail: $.registryRecords[0]: extra (expected None, got a RegistryRecordSummary with recordType 'MCP', recordVersion '1', status 'CREATING', createdByAutoDetection false, customMetadataSchemaComplianceStatus 'COMPLIANT')
  Expected: HTTP 200 with an empty registryRecords list (the refused create wrote nothing).
  Actual: HTTP 200 with one summary — the record the unrefused create persisted.

Code: services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/create_registry_record.rs lines 190-203 run only crate::state::descriptor_payload_refusal, and services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/state/descriptor_payload.rs lines 37-52 show that check is JSON parseability only; the record is then inserted at line 449.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus) and look at fixture agent-registry-control-record-token-replay-rename-and-descriptor-errors, steps task#11 and task#13.
3. Or by hand against the emulator's agent-registry-control endpoint:
   a. CreateRegistry(name=<unique>), poll GetRegistry until status == READY.
   b. CreateRegistryRecord(registryId=<id>, name=<unique>, recordType="MCP", descriptors={"mcpServer": {"data": "{\"x\": 1}"}}) — well-formed JSON that carries none of the MCP server fields.
      Expected: HTTP 400, x-amzn-errortype: ValidationException, Error.Message "mcp.server data does not match any supported version".
      Actual: HTTP 202 with {"recordArn": ..., "status": "CREATING"}.
   c. ListRegistryRecords(registryId=<id>, filters=[{"name": "name", "values": [<that name>]}]).
      Expected: registryRecords == [].
      Actual: one summary for the record step (b) should never have created.
   d. Control case that must keep working: CreateRegistryRecord with recordType="MCP" and an mcpServer payload carrying the MCP server fields (name/description/version) -> 202, record settles to DRAFT, GetRegistryRecord round-trips the descriptor; and a non-JSON custom.data still refuses with "data is not valid JSON: ...".
