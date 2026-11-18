Issue-Source: trace-sweep-divergence
Issue-Trace: schemas-code-binding-lifecycle
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: schemas-code-binding-lifecycle schemas-registry-tagging
Issue-Shape-Id: com.amazonaws.schemas#schemas
Issue-Key: schemas|GetCodeBindingSource|2xx->2xx|$.__wire_length__
Issue-Key: schemas|TagResource|2xx->2xx|$headers.content-length
Issue-Commit: 5e1ce66aaf40efbb8bcb903be4505a65ffac087e

# schemas response payload framing still diverges from AWS: GetCodeBindingSource archive is half the published size and the TagResource 204 is unframed

> _Note: this issue was identified against the code base at commit `5e1ce66aaf40efbb8bcb903be4505a65ffac087e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCodeBindingSource

## Description
Two net-new causes in how the schemas service fills and frames its HTTP response payloads. Both are response-serialization defects of the same service, both are reported UNOWNED by the replay, and each must be re-replayed after the fix.

Both causes were previously filed together as 20261110T135450Z-schemas-response-payloads-diverge-from-aws-getcodebindingsource-serves.md, which is now RESOLVED while both steps still diverge on replay (the archive moved from 400 to 1695 bytes against a published 3060, and the TagResource 204 still carries no content-length). A resolved issue covers nothing, so both are filed here with their own replay keys. The sibling empty-204 issue 20261029T203553Z-schemas-empty-body-204-responses-omit-the-content-length.md is still open but its replay key names only UntagResource (schemas-registry-tagging#task#5), DeleteSchemaVersion and DeleteSchema (schemas-schema-lifecycle#task#8 / #task#10) - TagResource's step is not carried by it and would be closed unrun.

Cause 1 of 2: GetCodeBindingSource serves a code-binding archive that is still only a fraction of the package AWS publishes. Fixture schemas-code-binding-lifecycle, step task#6.
AWS: Smithy (services/service-com-amazonaws-schemas-schemas/smithy-model/com-amazonaws-schemas-schemas.json) models GetCodeBindingSource as GET /v1/registries/name/{RegistryName}/schemas/name/{SchemaName}/language/{Language}/source, http code 200, output_shape GetCodeBindingSourceResponse whose single member Body is a blob bound with @httpPayload (errors BadRequestException 400, UnauthorizedException 401, ForbiddenException 403, NotFoundException 404, TooManyRequestsException 429, InternalServerErrorException 500). The model says nothing about the archive's contents, so this is recorded behavior only: AWS answers 200 with the ZIP of the COMPLETE source package its generator produced for that (schema version, language) pair - a declaration for every type the document implies PLUS the package's own supporting files - and for the document this fixture stores that payload is 3060 bytes. The requirement is that the archive is the whole generated package for the stored document; the recorded 3060 is one document's value, not the rule.
Emulator: the status is right (200 expected, 200 actual) and the body is a well-formed ZIP, but the payload is 1695 bytes where AWS's is 3060 - the replay reports $.__wire_length__: value_mismatch (expected 3060, got 1695). The payload is compared by length, so what is pinned is that the archive is still a little over half the published package, i.e. generated source files are still absent. The earlier resolution added the types a document IMPLIES (declared_structures now hoists inline/nested objects and list items, see the doc comment at get_code_binding_source.rs lines ~501-524), which raised the archive from 400 to 1695 bytes but did not close the gap.
Hypothesis: what is still missing is not implied TYPES but the rest of the package - the per-language supporting/build files and helper declarations AWS's generator emits beside the model classes - so package_files answers fewer files than the published package holds for the same document.
Why: the whole point of a code binding is that a client unpacks it and compiles against it. Against the emulator the client gets a package missing part of itself, and a client that compares the artifact it downloaded with AWS's (size, entry list, or a build of it) sees a different artifact; the response is not interchangeable with AWS's.
Fix: the generation/serialization path of this operation's handler - package_files / code_binding_archive / declared_structures in the GetCodeBindingSource handler, which must emit the full generated package for the stored document rather than only the model-class files.
Traces: fixture schemas-code-binding-lifecycle, step task#6 (divergence_key service=schemas|GetCodeBindingSource|status=200->200|keys=$.__wire_length__).

Cause 2 of 2: TagResource's empty 204 reaches the client with no Content-Length header. Fixture schemas-registry-tagging, step task#3.
AWS: Smithy models TagResource as POST /tags/{ResourceArn}, input {ResourceArn @httpLabel @required, Tags jsonName 'tags' @required map}, output_shape Unit, http code 204; no output member is bound to a header, so the header is pure framing of the empty body. The recording shows the 204 carrying content-length with the fixed value '0' - that value has no variable part.
Emulator: status is right (204 expected, 204 actual) and the body is empty, but the response as delivered on the wire carries no content-length header at all - the replay reports $headers.content-length: missing (expected '0', got None). The earlier resolution claims the service-level protocol seam now frames an empty serialized body as content-length: 0, and records that the vendored HTTP/1.1 writer (vendor/server/src/lib.rs, the hyper connection builder) drops the header again on a 204; whatever the seam sets, the replayed socket still does not deliver it.
Why: a client or proxy that frames a response by its declared length sees an unframed empty response rather than an explicitly zero-length one. Clients differ in how they treat a 204 with neither Content-Length nor Transfer-Encoding and some log or retry it as truncated, so the reply is not byte-interchangeable with AWS's.
Fix: the empty-response framing path this service's responses are written through - the Unit-output response serializer together with whatever rewrites its headers before the bytes leave the transport; the header must survive onto the wire for a 204, not only be set at the protocol seam.
Traces: fixture schemas-registry-tagging, step task#3 (divergence_key service=schemas|TagResource|status=204->204|keys=$headers.content-length).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, steps_total 289, steps_passed 75, steps_mismatched 214, score_pct 26.0, divergence_count 58. Every other divergence in the run is service=schemas ListSchemas and is owned by 20261110T235024Z-schemas-listschemas-omits-aws-managed-schemas-from-the.md; these two clusters are the only ones reported with owned_by [] and stub_path "".

Cause 1 - service=schemas, operation GetCodeBindingSource, fixture schemas-code-binding-lifecycle step task#6:
  expected_status 200, actual_status 200
  detail: $.__wire_length__: value_mismatch (expected 3060, got 1695)
  divergence_key: service=schemas|GetCodeBindingSource|status=200->200|keys=$.__wire_length__
  affected: schemas-code-binding-lifecycle#task#6 (1 step)
  Smithy: GetCodeBindingSource http {method GET, uri /v1/registries/name/{RegistryName}/schemas/name/{SchemaName}/language/{Language}/source, code 200}; output member Body target Body, httpPayload, type blob; input Language/RegistryName/SchemaName @httpLabel @required, SchemaVersion @httpQuery schemaVersion.
  Emulator source: services/service-com-amazonaws-schemas-schemas/src/ops/get_code_binding_source.rs - code_binding_archive (lines ~466-499) zips exactly the files package_files answers for the structures declared_structures reads; declared_structures (lines ~501-660) now hoists implied/nested types, which is why the payload is 1695 rather than the 400 bytes recorded when this was first filed, and still 1365 bytes short of the published 3060.

Cause 2 - service=schemas, operation TagResource, fixture schemas-registry-tagging step task#3:
  expected_status 204, actual_status 204
  detail: $headers.content-length: missing (expected '0', got None)
  divergence_key: service=schemas|TagResource|status=204->204|keys=$headers.content-length
  affected: schemas-registry-tagging#task#3 (1 step)
  Smithy: TagResource input {ResourceArn httpLabel required, Tags jsonName 'tags' required map}, output_shape Unit, http {method POST, uri /tags/{ResourceArn}, code 204}; no header-bound output member.
  The resolved issue 20261110T135450Z-... states the framing is handled by Service::override_protocol in services/service-com-amazonaws-schemas-schemas/src/service.rs and that the vendored writer in vendor/server/src/lib.rs removes the header on a 204; the replayed response confirms the header is absent as served.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it; replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture schemas-code-binding-lifecycle up to step task#6 - CreateRegistry, CreateSchema with the fixture's document, PutCodeBinding for the fixture's language, poll DescribeCodeBinding until CREATE_COMPLETE, then GET /v1/registries/name/<registry>/schemas/name/<schema>/language/<language>/source. The response is 200 with a ZIP body of 1695 bytes; AWS's recorded body for the same step is 3060 bytes. Unzip both and diff the entry lists: the emulator's archive holds the model-class files only.
3. Cause 2: replay fixture schemas-registry-tagging step task#3 - POST /tags/<registry ARN> with a tags map. Inspect the response headers as delivered: status 204, empty body, NO content-length header; AWS sends 'content-length: 0'.
4. After the fix, re-replay BOTH schemas-code-binding-lifecycle#task#6 and schemas-registry-tagging#task#3 - they are served by two different paths (the code-binding package generator and the empty-204 framing path) and neither closes the other.
