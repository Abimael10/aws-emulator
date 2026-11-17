Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.schema-json-content-model-compatibility-pairs glue.schema-protobuf-backward-field-additions-and-removals glue.schema-protobuf-forward-field-additions-and-removals glue.get-schema-version-by-never-registered-version-number-rejected glue.get-schema-version-unminted-id-rejected glue.list-schema-versions-missing-schema-rejected glue.list-schema-versions-max-results-zero-rejected glue.schema-definition-max-length-boundary glue.delete-schema-versions-checkpoint-refused glue.update-schema-requires-moving-checkpoint-to-change-compatibility
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.schema-json-content-model-compatibility-pairs|run#16|AssertionError|assert bo_version_status == "FAILURE" and bo_def_status == "FAILURE", (
Issue-Key: glue.schema-protobuf-backward-field-additions-and-removals|run#18|AssertionError|assert status_add_required == "FAILURE", (
Issue-Key: glue.schema-protobuf-forward-field-additions-and-removals|run#32|AssertionError|assert status_remove_required == "FAILURE", (
Issue-Key: glue.get-schema-version-by-never-registered-version-number-rejected|run#11|AssertionError|assert message == (
Issue-Key: glue.get-schema-version-unminted-id-rejected|run#12|AssertionError|assert message == "Schema is not found", f"expected message 'Schema is not found', got {message!r}"
Issue-Key: glue|ListSchemaVersions|EntityNotFoundException
Issue-Key: glue|ListSchemaVersions|ValidationException
Issue-Key: glue|CreateSchema|ValidationException
Issue-Key: glue.delete-schema-versions-checkpoint-refused|run#14|AssertionError|assert moved["SchemaCheckpoint"] == 3, f"checkpoint did not move to 3: {moved['SchemaCheckpoint']!r}"
Issue-Key: glue.update-schema-requires-moving-checkpoint-to-change-compatibility|_error#13|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 666f7954233f9ddfcb1a0dcc5f57d6c1921794e2

# glue schema registry: 8 net-new causes — JSON/PROTOBUF compatibility is never evaluated, four not-found/limit refusals are mis-worded or raised by the wrong class, and UpdateSchema neither moves the checkpoint nor gates a compatibility change

> _Note: this issue was identified against the code base at commit `666f7954233f9ddfcb1a0dcc5f57d6c1921794e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateSchema

## Description
Eight distinct root causes in the Glue Schema Registry surface of service-com-amazonaws-glue-awsglue. All ten failing checks below are listed under service heading 'glue' with NO 'OWNED by' owner and NO 'ALREADY FILED' marker. EIGHT causes must be closed before this issue is submitted.

Cause 1 of 8: schema compatibility is never evaluated for the JSON and PROTOBUF data formats — every registered version settles AVAILABLE.
AWS: Smithy: RegisterSchemaVersion output {SchemaVersionId, VersionNumber, Status: SchemaVersionStatus}; SchemaVersionStatus = [AVAILABLE, PENDING, FAILURE, DELETING]; Compatibility = [NONE, DISABLED, BACKWARD, BACKWARD_ALL, FORWARD, FORWARD_ALL, FULL, FULL_ALL]. RegisterSchemaVersion is documented to return an error when the new version does not meet the schema set's compatibility requirements, and directs callers to GetSchemaVersion to check the verdict; the verdict is a persisted property of the version (GetSchemaVersion and GetSchemaByDefinition both project Status), reported at HTTP 200. Recorded behavior for the content models these checks exercise: for a JSON schema with additionalProperties=true (open content model) under BACKWARD, adding an optional property is INCOMPATIBLE and the version settles Status='FAILURE' (while the closed-content-model sibling, additionalProperties=false, is compatible and settles AVAILABLE); for PROTOBUF proto2 under BACKWARD, adding a 'required' field settles FAILURE while adding an 'optional' field settles AVAILABLE; under FORWARD, removing a 'required' field settles FAILURE while removing an 'optional' field settles AVAILABLE.
Emulator: every registered version settles Status='AVAILABLE' and GetSchemaByDefinition resolves it AVAILABLE, so no version ever reaches FAILURE (replay details: "BACKWARD open schema ... adding an optional property: version status 'AVAILABLE', GetSchemaByDefinition status 'AVAILABLE', expected both FAILURE"; "adding a required field under BACKWARD gave 'AVAILABLE', expected FAILURE"; "removing a required field under FORWARD gave 'AVAILABLE', expected FAILURE").
Why: Status is the ONLY channel AWS uses to report a failed evolution (the call itself is a 200). A producer or CI pipeline that gates deployment on the verdict sees every candidate accepted, so a definition real Glue marks FAILURE is promoted and breaks every existing consumer; and a client can never observe the FAILURE state it must handle.
Fix: the RegisterSchemaVersion handler plus the schema-registry state — the stored version record must carry a status produced by an actual compatibility evaluation against the schema set under the schema's Compatibility mode, for JSON (honouring additionalProperties) and PROTOBUF (honouring proto2 required/optional) as well as AVRO; GetSchemaVersion and GetSchemaByDefinition must then project that stored status. Registration must stay a 200, still consume a VersionNumber and keep the bytes resolvable when the verdict is FAILURE. Nearest open issues, which do NOT carry these checks: 20261108T184111Z-glue-2-net-new-causes-backward-schema-compatibility.md (keyed to glue.e2e-schema-registry-compatibility-enforced-across-versions, AVRO/BACKWARD only) and 20261108T174217Z-glue-schema-registry-version-registration-never-validates-or.md (wire corpus).
Traces: glue.schema-json-content-model-compatibility-pairs, glue.schema-protobuf-backward-field-additions-and-removals, glue.schema-protobuf-forward-field-additions-and-removals

Cause 2 of 8: GetSchemaVersion by SchemaId + VersionNumber — the not-found message drops the lookup context AWS echoes.
AWS: Smithy: GetSchemaVersion input {SchemaId, SchemaVersionId, SchemaVersionNumber}; declares EntityNotFoundException (client, 400). Recorded behavior: a VersionNumber no RegisterSchemaVersion ever minted is refused with the message 'Schema version is not found. RegistryName: <registry>, SchemaName: <schema>, SchemaArn: null, VersionNumber: <n>, isLatestVersion: <bool>' — the fixed text is that sentence and its five labels; the variable parts are the registry name, schema name, SchemaArn (null when the lookup was by name), the VersionNumber from the request and the isLatestVersion flag of the request's SchemaVersionNumber.
Emulator: correct code, but the message is the bare 'Schema version is not found.' — every label/value pair is missing (replay detail: "AssertionError: unexpected message 'Schema version is not found.'").
Why: with several schemas in flight the refusal does not say which registry/schema/version was queried, so callers cannot attribute the failure and exact-match assertions fail.
Fix: the GetSchemaVersion not-found branch — compose the message from the resolved SchemaId and the requested SchemaVersionNumber.
Traces: glue.get-schema-version-by-never-registered-version-number-rejected

Cause 3 of 8: GetSchemaVersion by an unminted SchemaVersionId uses the version-not-found sentence instead of the schema-not-found one.
AWS: Smithy: GetSchemaVersion.SchemaVersionId targets SchemaVersionIdString (@length 36, uuid pattern). Recorded behavior: a well-formed SchemaVersionId that was never minted is refused 400 EntityNotFoundException with the fixed message 'Schema is not found' (no trailing period, no variable part) — AWS resolves the id to a schema first, and it is the SCHEMA lookup that fails.
Emulator: 400 EntityNotFoundException with 'Schema version is not found.' (replay detail: "AssertionError: expected message 'Schema is not found', got 'Schema version is not found.'").
Why: the two sentences distinguish 'that id belongs to no schema' from 'that schema has no such version'; collapsing them loses the distinction and breaks message-keyed handling.
Fix: the GetSchemaVersion handler's by-SchemaVersionId lookup branch (message selection).
Traces: glue.get-schema-version-unminted-id-rejected

Cause 4 of 8: ListSchemaVersions for a schema name that does not exist — the message drops the lookup context.
AWS: Smithy: ListSchemaVersions input {SchemaId @required, MaxResults, NextToken}; declares EntityNotFoundException (client, 400). Recorded behavior: the message is 'Schema is not found. RegistryName: <registry>, SchemaName: <schema>, SchemaArn: null' — fixed text plus the registry/schema from the request's SchemaId (SchemaArn null when the lookup was by name).
Emulator: 400 EntityNotFoundException with the bare 'Schema is not found.' (replay detail: expected '... RegistryName: <registry>, SchemaName: <schema>, SchemaArn: null', got 'Schema is not found.').
Why: same as Cause 2 — the refusal no longer identifies what was looked up.
Fix: the ListSchemaVersions not-found branch message composition.
Traces: glue.list-schema-versions-missing-schema-rejected

Cause 5 of 8: ListSchemaVersions MaxResults out of range is refused by the generic constraint layer instead of Glue's own refusal.
AWS: Smithy: ListSchemaVersions.MaxResults targets MaxResultsNumber (@range min 1 max 100). Recorded behavior: Glue answers 400 InvalidInputException with 'MaxResults cannot be greater than 100 (or) less than or equal to 0. MaxResults: <value>' — fixed text plus the MaxResults value from the request.
Emulator: 400 ValidationException with the framework sentence "1 validation error detected: Value '0' at 'maxResults' failed to satisfy constraint: Member must have value greater than or equal to 1" — wrong error class and wrong message (the '0' is this run's requested value filling the template).
Why: the modeled error list for this operation does not include ValidationException, so botocore raises an unexpected class and pagination code that handles InvalidInputException does not catch it.
Fix: the ListSchemaVersions input validation — raise Glue's InvalidInputException with its own sentence rather than letting the generic range constraint fire.
Traces: glue.list-schema-versions-max-results-zero-rejected

Cause 6 of 8: CreateSchema SchemaDefinition over the length limit is refused by the generic constraint layer instead of Glue's own refusal.
AWS: Smithy: CreateSchema.SchemaDefinition targets SchemaDefinitionString (@length min 1 max 170000, pattern \\S); CreateSchema declares InvalidInputException (client, 400). Recorded behavior: a 170000-character definition is ACCEPTED and stored byte-for-byte (GetSchemaVersion returns it unchanged); a 170001-character one is refused 400 InvalidInputException with the fixed message 'The resource name contains too many or too few characters. Parameter Name: schemaDefinition', and the schema name stays unreadable (GetSchema -> EntityNotFoundException 'Schema is not found. RegistryName: <r>, SchemaName: <s>, SchemaArn: null').
Emulator: 400 ValidationException with "1 validation error detected: Value '<the definition, elided>' at 'schemaDefinition' failed to satisfy constraint: Member must have length less than or equal to 170000" — wrong class, wrong sentence, and it echoes the entire rejected 170001-byte value.
Why: callers handling Glue's InvalidInputException see an unmodeled class; and echoing the whole oversized document in the error body is behavior AWS never exhibits.
Fix: the CreateSchema input validation path for SchemaDefinition — emit Glue's InvalidInputException sentence naming the parameter instead of the generic constraint violation.
Traces: glue.schema-definition-max-length-boundary

Cause 7 of 8: UpdateSchema with SchemaVersionNumber does not move the schema's checkpoint.
AWS: Smithy: UpdateSchema input {SchemaId @required, SchemaVersionNumber, Compatibility, Description}; GetSchema projects SchemaCheckpoint (SchemaCheckpointNumber @range 1..100000). Recorded behavior: UpdateSchema(SchemaId, SchemaVersionNumber={VersionNumber: n}) moves the checkpoint to n, and GetSchema then reports SchemaCheckpoint = n; the checkpoint version is the one DeleteSchemaVersions refuses with InvalidInputException 'Cannot delete checkpoint version.' while a non-checkpoint version deletes with no SchemaVersionErrors.
Emulator: after UpdateSchema(SchemaVersionNumber={VersionNumber: 3}) on a 3-version schema, GetSchema still reports SchemaCheckpoint = 1 (replay detail: "AssertionError: checkpoint did not move to 3: 1"), so the checkpoint-delete guard is tested against the wrong version and the rest of the recorded behavior (refusing the checkpoint delete, deleting the non-checkpoint version, the post-delete 'Schema version is not found. ... VersionNumber: 2, isLatestVersion: false' message, the surviving checkpoint version's definition/Status) is unreachable.
Why: the checkpoint is what pins a consumer's compatibility baseline; if it never moves, callers cannot advance it and the delete protection applies to the wrong version — a version AWS protects can be deleted here.
Fix: the UpdateSchema handler and the schema-registry state — apply SchemaVersionNumber to the stored SchemaCheckpoint, and have GetSchema/DeleteSchemaVersions read that stored value.
Traces: glue.delete-schema-versions-checkpoint-refused

Cause 8 of 8: UpdateSchema accepts a Compatibility change that AWS refuses.
AWS: Smithy: UpdateSchema declares InvalidInputException (client, 400). Recorded behavior: a Compatibility change submitted WITHOUT a SchemaVersionNumber is refused 400 InvalidInputException 'Schema version checkpoint is missing.', and a change to the setting the schema already has is refused 400 InvalidInputException 'Cannot update to the same compatibility setting.' — both sentences fixed text with no variable part; GetSchema is left unchanged by each refusal, and only an UpdateSchema that also names a later SchemaVersionNumber moves the checkpoint and applies the new Compatibility.
Emulator: the call succeeds — HTTP 200 with {SchemaArn, SchemaName, RegistryName} (replay detail: "AssertionError: expected the call to be rejected, but it succeeded: {'SchemaArn': ..., 'SchemaName': ..., 'RegistryName': ...}").
Why: this is an overly permissive write: the compatibility mode of a schema set can be changed silently without moving the checkpoint, so every later version is judged under a mode real Glue would not have let the caller set, and the two distinct refusals a client handles never occur.
Fix: the UpdateSchema handler's compatibility-change gate (require a SchemaVersionNumber; reject a no-op change) in the glue crate.
Traces: glue.update-schema-requires-moving-checkpoint-to-change-compatibility

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 503/724 passed. All ten checks appear under service heading 'glue', unowned and unmarked.

Cause 1 — glue.schema-json-content-model-compatibility-pairs [mismatch], line 132: "AssertionError: BACKWARD open schema 'sim2real-behav-schema-bo-<id>' adding an optional property: version status 'AVAILABLE', GetSchemaByDefinition status 'AVAILABLE', expected both FAILURE". glue.schema-protobuf-backward-field-additions-and-removals [mismatch], line 150: "adding a required field under BACKWARD gave 'AVAILABLE', expected FAILURE". glue.schema-protobuf-forward-field-additions-and-removals [mismatch], line 184: "removing a required field under FORWARD gave 'AVAILABLE', expected FAILURE". The compatible siblings in the same checks (optional add under BACKWARD, optional remove under FORWARD, closed-content-model add) settle AVAILABLE correctly and must not regress.
Cause 2 — glue.get-schema-version-by-never-registered-version-number-rejected [mismatch], line 61: expected 'Schema version is not found. RegistryName: <registry>, SchemaName: <schema>, SchemaArn: null, VersionNumber: 5, isLatestVersion: false'; got 'Schema version is not found.' (the 5 is the request's VersionNumber filling the template).
Cause 3 — glue.get-schema-version-unminted-id-rejected [mismatch], line 60: "expected message 'Schema is not found', got 'Schema version is not found.'" (code EntityNotFoundException already correct).
Cause 4 — glue.list-schema-versions-missing-schema-rejected [mismatch]: expected ('EntityNotFoundException', 'Schema is not found. RegistryName: <registry>, SchemaName: <missing schema>, SchemaArn: null'); got ('EntityNotFoundException', 'Schema is not found.').
Cause 5 — glue.list-schema-versions-max-results-zero-rejected [mismatch]: expected ('InvalidInputException', 'MaxResults cannot be greater than 100 (or) less than or equal to 0. MaxResults: 0'); got ('ValidationException', "1 validation error detected: Value '0' at 'maxResults' failed to satisfy constraint: Member must have value greater than or equal to 1"). The check disables botocore parameter validation so the server-side refusal is reached; MaxResults=1 on the same schema must still list its one version.
Cause 6 — glue.schema-definition-max-length-boundary [mismatch]: expected code 'InvalidInputException' with 'The resource name contains too many or too few characters. Parameter Name: schemaDefinition'; got 'ValidationException' with "1 validation error detected: Value '{...}... [170001 bytes]' at 'schemaDefinition' failed to satisfy constraint: Member must have length less than or equal to 170000". The 170000-char accept-and-store half of the check passes.
Cause 7 — glue.delete-schema-versions-checkpoint-refused [mismatch], line 96: "AssertionError: checkpoint did not move to 3: 1" after CreateSchema (checkpoint 1) + two RegisterSchemaVersion + UpdateSchema(SchemaVersionNumber={VersionNumber:3}).
Cause 8 — glue.update-schema-requires-moving-checkpoint-to-change-compatibility [mismatch]: "AssertionError: expected the call to be rejected, but it succeeded: {'SchemaArn': 'arn:aws:glue:us-west-2:000000000000:schema/<registry>/<schema>', 'SchemaName': ..., 'RegistryName': ..., HTTP 200}" where ('InvalidInputException', 'Schema version checkpoint is missing.') was required.

Model evidence (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): SchemaVersionStatus = [AVAILABLE, PENDING, FAILURE, DELETING]; Compatibility = [NONE, DISABLED, BACKWARD, BACKWARD_ALL, FORWARD, FORWARD_ALL, FULL, FULL_ALL]; CreateSchema.SchemaDefinition -> SchemaDefinitionString @length 1..170000; ListSchemaVersions.MaxResults -> MaxResultsNumber @range 1..100 with errors [AccessDenied, EntityNotFound, InternalService, InvalidInput] (no ValidationException); CreateSchema errors include InvalidInputException (no ValidationException); UpdateSchema input {SchemaId, SchemaVersionNumber, Compatibility, Description} with InvalidInputException; GetSchemaVersion/RegisterSchemaVersion outputs carry Status: SchemaVersionStatus.

## Steps to Reproduce
1. Clone this checkout and boot the emulator built from it.
2. Cause 1: CreateRegistry; CreateSchema(DataFormat='JSON', Compatibility='BACKWARD', definition with "additionalProperties":true and one property) then RegisterSchemaVersion adding an optional property and poll GetSchemaVersion -> Status must be FAILURE (and GetSchemaByDefinition on those bytes must report FAILURE); repeat with "additionalProperties":false, which must stay AVAILABLE. Then CreateSchema(DataFormat='PROTOBUF', Compatibility='BACKWARD', proto2 message) and RegisterSchemaVersion adding a 'required' field -> FAILURE, adding an 'optional' field -> AVAILABLE; and with Compatibility='FORWARD', removing a 'required' field -> FAILURE, removing an 'optional' field -> AVAILABLE.
3. Cause 2: CreateRegistry + CreateSchema(AVRO, NONE); GetSchemaVersion(SchemaId={RegistryName,SchemaName}, SchemaVersionNumber={VersionNumber:5}) and read Error.Message.
4. Cause 3: GetSchemaVersion(SchemaVersionId=<a random uuid never minted>) and read Error.Message.
5. Cause 4: CreateRegistry; ListSchemaVersions(SchemaId={RegistryName:<registry>, SchemaName:<never created>}) and read Error.Message.
6. Cause 5: with botocore parameter_validation=False, ListSchemaVersions(SchemaId=<existing>, MaxResults=0); then MaxResults=1 must still list the one version.
7. Cause 6: CreateSchema with a 170000-char JSON SchemaDefinition (must succeed and read back byte-for-byte), then with a 170001-char one and read Error.Code/Message; GetSchema on the rejected name must be EntityNotFoundException.
8. Cause 7: CreateSchema(JSON, NONE) -> RegisterSchemaVersion twice -> UpdateSchema(SchemaId, SchemaVersionNumber={VersionNumber:3}) -> GetSchema and read SchemaCheckpoint (must be 3); then DeleteSchemaVersions(Versions='3') must be refused ('Cannot delete checkpoint version.') and DeleteSchemaVersions(Versions='2') must succeed with empty SchemaVersionErrors.
9. Cause 8: CreateSchema(AVRO, BACKWARD) then UpdateSchema(SchemaId, Compatibility='FORWARD') with NO SchemaVersionNumber -> must be InvalidInputException 'Schema version checkpoint is missing.'; and UpdateSchema with the current Compatibility -> 'Cannot update to the same compatibility setting.'
10. Re-replay the ten checks listed in trace_ids to confirm the fix.
