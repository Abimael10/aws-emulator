Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: agent-registry-control.invalid-descriptor-rejected-without-creating-record
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control.invalid-descriptor-rejected-without-creating-record|_error#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 36b4752657a9c8fc6bd8a889740d640d6f7662fd

# CreateRegistryRecord accepts an mcpServer descriptor that matches no supported MCP server schema version and creates the record

> _Note: this issue was identified against the code base at commit `36b4752657a9c8fc6bd8a889740d640d6f7662fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRegistryRecord

## Description
This issue has ONE cause.

Related, and does NOT cover this check: the open issue 20261111T015454Z-agent-registry-control-3-net-new-causes-createregistryrecord.md (its Cause 1) and the open issue 20261110T223112Z-agent-registry-control-registry-configuration-blocks-dropped-custom-metadata-schema-never.md describe descriptor/metadata validation on this same write path. The replay machinery does not list this check as owned by either: the CUSTOM-descriptor half of the check now passes (a non-JSON `custom.data` IS refused with the expected ValidationException), and the failure has moved to the mcpServer half, which is still accepted. That remaining, un-owned half is what is filed here.

Cause 1 of 1: CreateRegistryRecord performs no MCP schema validation on `descriptors.mcpServer.data`.
AWS: Smithy: CreateRegistryRecord POST /registries/{registryId}/records, success code 202; input member `descriptors` -> Descriptors is @required, and member `mcpServer` -> McpServerDescriptor is documented "Descriptor that defines the content of an MCP (Model Context Protocol) server registry record, including the server definition and its tool definitions. The content is validated against the MCP protocol schema." `data` -> DescriptorData is @length 1..102400, @sensitive. Modeled error ValidationException (errorKind client, httpError 400). So the mcpServer payload is parsed and matched against the supported MCP server schema versions BEFORE the record is written; a payload that matches no supported version is refused with HTTP 400 and x-amzn-errortype: ValidationException, and NO record is created (a subsequent ListRegistryRecords filtered on the requested name returns no entry). Message template, as recorded: the whole message is the fixed text "mcp.server data does not match any supported version" — it has no variable parts, so it must be reproduced verbatim for this error class. A payload that does carry the MCP server fields (recorded form: an object with `name`, `description`, `version`, e.g. name "io.example/weather", version "1.0.0" — these values fill the template, they are not part of it) is accepted, the record reaches DRAFT, and GetRegistryRecord returns recordType MCP with `descriptors.mcpServer.data` byte-for-byte equivalent JSON to what was submitted.
Emulator: the create of recordType=MCP with descriptors={'mcpServer': {'data': '{"x": 1}'}} — an object carrying none of the MCP server fields — returns HTTP 202 with body {recordArn, status: "CREATING"} and persists the record. No 400, no ValidationException, no error message, no x-amzn-errortype header. The payload is stored unvalidated.
Why: a client is told an MCP server definition that no MCP schema version accepts was admitted to the registry. Its correction path (catch ValidationException, repair the descriptor, resubmit) is never taken, so malformed MCP server records enter the registry and are published to consumers; on real AWS the same code fails at create with a 400 the client handles, while here it fails later — at an MCP consumer resolving the record — where the client has no handler. The accepted record also occupies the requested name, so a retry under the same name then collides with a ConflictException the client did not expect.
Fix: CreateRegistryRecord's input validation (the operation's handler, before the record is allocated/written) — parse `descriptors.mcpServer.data` as JSON and validate it against the supported MCP server schema versions, raising ValidationException (400) with the fixed message above and writing no state; the same validation belongs on any other write path that accepts a descriptor.
Traces: agent-registry-control.invalid-descriptor-rejected-without-creating-record.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (12/21 checks passed; agent-registry-control answered 12/18, so the emulator is serving).

[mismatch] agent-registry-control.invalid-descriptor-rejected-without-creating-record, at check line 136 (the second rejection case in the check):

  code, message = _error(lambda: arc.create_registry_record(
      registryId=registry_id, name=off_schema, recordType="MCP",
      descriptors={"mcpServer": {"data": json.dumps({"x": 1})}},
  ))
  assert code == "ValidationException"
  assert message == "mcp.server data does not match any supported version"

Harness output:
  AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 202, 'HTTPHeaders': {'content-type': 'application/json', ...}}, 'recordArn': 'arn:aws:agent-registry:<region>:<account>:registry/<registryId>/record/<recordId>', 'status': 'CREATING'}

Expected: HTTP 400, Error.Code "ValidationException", Error.Message "mcp.server data does not match any supported version" (entirely fixed text), and no record created.
Actual: HTTP 202, body {recordArn, status: "CREATING"}, record persisted.

The check's preceding case — recordType=CUSTOM with descriptors={'custom': {'data': 'not json'}}, expecting ValidationException "data is not valid JSON: Unrecognized token 'not': was expecting (JSON String, Number, Array, Object or token 'null', 'true' or 'false')" — now passes, which is why the failure surfaces only on the mcpServer branch. The two trailing assertions the check never reaches also belong to this cause: ListRegistryRecords filtered by the refused record's name must return no recordId, and a valid mcpServer descriptor ({name, description, version}) must create a DRAFT record whose GetRegistryRecord body has recordType "MCP" and round-trips the submitted descriptor JSON.

## Steps to Reproduce
1. git clone the repo; build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 (run_behavioral_suite) and look at check agent-registry-control.invalid-descriptor-rejected-without-creating-record.
3. Or by hand against the emulator's agent-registry-control endpoint:
   a. CreateRegistry(name=<unique>) and poll GetRegistry until status == READY.
   b. CreateRegistryRecord(registryId=<id>, name=<unique>, recordType="MCP", descriptors={"mcpServer": {"data": "{\"x\": 1}"}}).
      Expected: HTTP 400, x-amzn-errortype: ValidationException, Error.Message "mcp.server data does not match any supported version".
      Actual: HTTP 202 with {"recordArn": ..., "status": "CREATING"}.
   c. ListRegistryRecords(registryId=<id>, filters=[{"name": "name", "values": [<that name>]}]).
      Expected: registryRecords == [] (nothing was created). Actual: the refused record is listed.
   d. Control case that must keep working: CreateRegistryRecord(recordType="MCP", descriptors={"mcpServer": {"data": "{\"name\": \"io.example/weather\", \"description\": \"Weather MCP server\", \"version\": \"1.0.0\"}"}}) -> 202, record reaches DRAFT, GetRegistryRecord returns recordType "MCP" and the same descriptor JSON.
